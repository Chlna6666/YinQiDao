use std::{
    collections::{HashMap, HashSet},
    f32::consts::FRAC_PI_2,
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use cpal::{
    Device, SampleFormat, Stream, StreamConfig, SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::{Receiver, Sender, bounded};
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use thiserror::Error;

use crate::{
    audio_policy::audio_runtime_policy,
    model::{
        EqSettings, PlaybackState, PlayerSnapshot, RepeatMode, SmartAudioSettings,
        SpatialSettings, Track, TrackId, TrackTransitionSettings, TransitionMode,
    },
};

use super::{
    command_queue::CommandQueue,
    decoder::{DecodeError, DecoderStream},
    dsp::{AudioProcessor, perceptual_volume_gain},
    smart_profile::resolve_smart_audio,
    transition::{SmartCue, analyze_smart_cue, fade_in_gain, fade_out_gain},
};

pub type DeviceId = String;

const PRELOAD_DEBOUNCE: Duration = Duration::from_millis(40);
const MIN_TRANSITION_MS: u64 = 250;
const MAX_TRANSITION_MS: u64 = 12_000;

#[derive(Clone, Debug)]
pub enum PlayerCommand {
    Play,
    Pause,
    Seek(Duration),
    PlayTrack(TrackId),
    RestoreTrack {
        track_id: TrackId,
        position: Duration,
        play: bool,
    },
    Next,
    Previous,
    SetVolume(f32),
    SetEq(EqSettings),
    SetSpatial(SpatialSettings),
    SetSmartAudio(SmartAudioSettings),
    SetTransition(TrackTransitionSettings),
    #[allow(dead_code)]
    SetOutputDevice(DeviceId),
    SetQueue(Arc<Vec<TrackId>>),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    Stop,
}

#[derive(Clone, Debug)]
pub enum PlayerEvent {
    StateChanged(PlaybackState),
    PositionChanged(Duration),
    TrackEnded,
    #[allow(dead_code)]
    Buffering,
    Error(PlaybackError),
}

#[derive(Clone, Debug, Error)]
pub enum PlaybackError {
    #[error("音频输出错误: {0}")]
    Output(String),
    #[error("音频解码错误: {0}")]
    Decode(String),
    #[error("输出设备切换失败: {0}")]
    Device(String),
}

impl From<DecodeError> for PlaybackError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct OutputDeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub sample_rate: u32,
    pub channels: u16,
}

struct PreloadedChunk {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: u16,
    /// Playback position at the end of this chunk.
    position: Duration,
}

struct PreloadedTrack {
    track_id: TrackId,
    decoder: DecoderStream,
    first_chunk: Option<PreloadedChunk>,
    smart_cue: SmartCue,
    use_smart_cue: bool,
    max_smart_cue_ms: u64,
}

struct PreloadRequest {
    generation: u64,
    track: Track,
    use_smart_cue: bool,
    max_smart_cue_ms: u64,
}

struct PreloadCoordinator {
    request: Mutex<Option<PreloadRequest>>,
    ready: Mutex<Option<PreloadedTrack>>,
    wake: Condvar,
    generation: AtomicU64,
    closed: AtomicBool,
}

impl PreloadCoordinator {
    fn new() -> Self {
        Self {
            request: Mutex::new(None),
            ready: Mutex::new(None),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn request(&self, track: Track, transition: &TrackTransitionSettings) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let use_smart_cue = transition.enabled
            && transition.mode == TransitionMode::Crossfade
            && transition.smart_cue;
        let max_smart_cue_ms = transition.max_smart_cue_ms.clamp(0, 8_000);

        if let Ok(ready) = self.ready.lock()
            && ready.as_ref().is_some_and(|ready| {
                ready.track_id == track.id
                    && ready.use_smart_cue == use_smart_cue
                    && ready.max_smart_cue_ms == max_smart_cue_ms
            })
        {
            return;
        }
        if let Ok(pending) = self.request.lock()
            && pending.as_ref().is_some_and(|pending| {
                pending.track.id == track.id
                    && pending.track.path == track.path
                    && pending.use_smart_cue == use_smart_cue
                    && pending.max_smart_cue_ms == max_smart_cue_ms
            })
        {
            return;
        }

        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if let Ok(mut ready) = self.ready.lock() {
            *ready = None;
        }
        if let Ok(mut request) = self.request.lock() {
            *request = Some(PreloadRequest {
                generation,
                track,
                use_smart_cue,
                max_smart_cue_ms,
            });
            self.wake.notify_one();
        }
    }

    fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut request) = self.request.lock() {
            *request = None;
        }
        if let Ok(mut ready) = self.ready.lock() {
            *ready = None;
        }
    }

    fn take(&self, track_id: TrackId) -> Option<PreloadedTrack> {
        let mut ready = self.ready.lock().ok()?;
        if ready.as_ref().is_some_and(|ready| ready.track_id == track_id) {
            ready.take()
        } else {
            None
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wake.notify_all();
    }

    fn spawn(self: &Arc<Self>) -> std::io::Result<thread::JoinHandle<()>> {
        let coordinator = self.clone();
        thread::Builder::new()
            .name("yinqidao-audio-preloader".into())
            .spawn(move || coordinator.run())
    }

    fn run(&self) {
        loop {
            let mut request = {
                let Ok(mut slot) = self.request.lock() else {
                    return;
                };
                while slot.is_none() && !self.closed.load(Ordering::Acquire) {
                    let Ok(next_slot) = self.wake.wait(slot) else {
                        return;
                    };
                    slot = next_slot;
                }
                if self.closed.load(Ordering::Acquire) {
                    return;
                }
                let Some(request) = slot.take() else {
                    continue;
                };
                request
            };

            thread::sleep(PRELOAD_DEBOUNCE);
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            if let Ok(mut slot) = self.request.lock()
                && let Some(latest) = slot.take()
            {
                request = latest;
            }

            let smart_cue = if request.use_smart_cue {
                let mut analysis_decoder = match DecoderStream::open(&request.track.path) {
                    Ok(decoder) => decoder,
                    Err(_) => continue,
                };
                match analyze_smart_cue(
                    &mut analysis_decoder,
                    &request.track,
                    request.max_smart_cue_ms,
                ) {
                    Ok(cue) => cue,
                    Err(error) => {
                        tracing::debug!(
                            track_id = request.track.id,
                            error = %error,
                            "Smart Cue 分析失败，回退到歌曲起点"
                        );
                        SmartCue::default()
                    }
                }
            } else {
                SmartCue::default()
            };

            let mut decoder = match DecoderStream::open(&request.track.path) {
                Ok(decoder) => decoder,
                Err(_) => continue,
            };
            let first_chunk = match prepare_preloaded_chunk(&mut decoder, smart_cue.position) {
                Ok(chunk) => chunk,
                Err(error) => {
                    tracing::debug!(
                        track_id = request.track.id,
                        error = %error,
                        "下一曲 PCM 入点准备失败"
                    );
                    continue;
                }
            };

            if self.closed.load(Ordering::Acquire)
                || self.generation.load(Ordering::Acquire) != request.generation
            {
                continue;
            }
            if let Ok(mut ready) = self.ready.lock()
                && self.generation.load(Ordering::Acquire) == request.generation
            {
                *ready = Some(PreloadedTrack {
                    track_id: request.track.id,
                    decoder,
                    first_chunk,
                    smart_cue,
                    use_smart_cue: request.use_smart_cue,
                    max_smart_cue_ms: request.max_smart_cue_ms,
                });
            }
        }
    }
}

fn prepare_preloaded_chunk(
    decoder: &mut DecoderStream,
    cue: Duration,
) -> Result<Option<PreloadedChunk>, DecodeError> {
    let mut samples = Vec::new();
    loop {
        let chunk_start = decoder.position();
        let Some((sample_rate, channels)) = decoder.next_chunk_into(&mut samples)? else {
            return Ok(None);
        };
        let chunk_end = decoder.position();
        if cue >= chunk_end {
            continue;
        }

        if cue > chunk_start {
            let skip = cue - chunk_start;
            let skip_frames = ((skip.as_nanos().saturating_mul(u128::from(sample_rate.max(1))))
                / 1_000_000_000_u128)
                .min(usize::MAX as u128) as usize;
            let channels_usize = usize::from(channels.max(1));
            let skip_samples = skip_frames
                .saturating_mul(channels_usize)
                .min(samples.len());
            if skip_samples >= samples.len() {
                continue;
            }
            if skip_samples > 0 {
                let remaining = samples.len() - skip_samples;
                samples.copy_within(skip_samples.., 0);
                samples.truncate(remaining);
            }
        }

        return Ok(Some(PreloadedChunk {
            samples,
            sample_rate,
            channels,
            position: chunk_end,
        }));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CrossfadeMixOutcome {
    Continue,
    Complete,
    NextExhausted,
}

struct CrossfadeState {
    track_id: TrackId,
    decoder: DecoderStream,
    prefetched_chunk: Option<PreloadedChunk>,
    processor: AudioProcessor,
    decoded_samples: Vec<f32>,
    chunk_processed: Vec<f32>,
    buffered_samples: Vec<f32>,
    buffer_cursor: usize,
    transition_frames: u64,
    next_frames_consumed: u64,
    total_frames: u64,
    phase_cos: f32,
    phase_sin: f32,
    rotation_cos: f32,
    rotation_sin: f32,
    cue: SmartCue,
}

impl CrossfadeState {
    fn new(
        preloaded: PreloadedTrack,
        output_rate: u32,
        eq: EqSettings,
        spatial: SpatialSettings,
        total_frames: u64,
    ) -> Self {
        let total_frames = total_frames.max(1);
        let (phase_cos, phase_sin, rotation_cos, rotation_sin) = if total_frames <= 1 {
            (0.0, 1.0, 1.0, 0.0)
        } else {
            let step = FRAC_PI_2 / (total_frames - 1) as f32;
            let (rotation_sin, rotation_cos) = step.sin_cos();
            (1.0, 0.0, rotation_cos, rotation_sin)
        };
        Self {
            track_id: preloaded.track_id,
            decoder: preloaded.decoder,
            prefetched_chunk: preloaded.first_chunk,
            processor: AudioProcessor::new(output_rate, eq, spatial, 1.0),
            decoded_samples: Vec::new(),
            chunk_processed: Vec::new(),
            buffered_samples: Vec::new(),
            buffer_cursor: 0,
            transition_frames: 0,
            next_frames_consumed: 0,
            total_frames,
            phase_cos,
            phase_sin,
            rotation_cos,
            rotation_sin,
            cue: preloaded.smart_cue,
        }
    }

    fn available_samples(&self) -> usize {
        self.buffered_samples.len().saturating_sub(self.buffer_cursor)
    }

    fn compact_buffer(&mut self) {
        if self.buffer_cursor == 0 {
            return;
        }
        if self.buffer_cursor >= self.buffered_samples.len() {
            self.buffered_samples.clear();
            self.buffer_cursor = 0;
            return;
        }
        if self.buffer_cursor >= 8_192 {
            let remaining = self.buffered_samples.len() - self.buffer_cursor;
            self.buffered_samples.copy_within(self.buffer_cursor.., 0);
            self.buffered_samples.truncate(remaining);
            self.buffer_cursor = 0;
        }
    }

    fn ensure_samples(&mut self, needed_samples: usize) -> Result<(), DecodeError> {
        self.compact_buffer();
        while self.available_samples() < needed_samples {
            let (sample_rate, channels) = if let Some(chunk) = self.prefetched_chunk.take() {
                self.decoded_samples = chunk.samples;
                (chunk.sample_rate, chunk.channels)
            } else {
                let Some((sample_rate, channels)) =
                    self.decoder.next_chunk_into(&mut self.decoded_samples)?
                else {
                    break;
                };
                (sample_rate, channels)
            };
            self.processor.process_into(
                &self.decoded_samples,
                sample_rate,
                channels,
                &mut self.chunk_processed,
            );
            self.buffered_samples.extend_from_slice(&self.chunk_processed);
        }
        Ok(())
    }

    fn mix_into(&mut self, current: &mut [f32]) -> Result<CrossfadeMixOutcome, DecodeError> {
        self.ensure_samples(current.len())?;
        if self.available_samples() < current.len() {
            return Ok(CrossfadeMixOutcome::NextExhausted);
        }

        let frames = current.len() / 2;
        for frame in 0..frames {
            let next_index = self.buffer_cursor;
            let next_left = self.buffered_samples[next_index];
            let next_right = self.buffered_samples[next_index + 1];
            self.buffer_cursor += 2;

            let (current_gain, next_gain) = if self.transition_frames >= self.total_frames {
                (0.0, 1.0)
            } else {
                (
                    self.phase_cos * self.phase_cos,
                    self.phase_sin * self.phase_sin,
                )
            };
            let index = frame * 2;
            current[index] = mix_crossfade_sample(
                current[index],
                next_left,
                current_gain,
                next_gain,
            );
            current[index + 1] = mix_crossfade_sample(
                current[index + 1],
                next_right,
                current_gain,
                next_gain,
            );
            self.next_frames_consumed = self.next_frames_consumed.saturating_add(1);

            if self.transition_frames < self.total_frames {
                self.transition_frames = self.transition_frames.saturating_add(1);
                if self.transition_frames >= self.total_frames {
                    self.phase_cos = 0.0;
                    self.phase_sin = 1.0;
                } else {
                    let next_cos = self.phase_cos * self.rotation_cos
                        - self.phase_sin * self.rotation_sin;
                    let next_sin = self.phase_sin * self.rotation_cos
                        + self.phase_cos * self.rotation_sin;
                    self.phase_cos = next_cos;
                    self.phase_sin = next_sin;
                }
            }
        }

        if self.transition_frames >= self.total_frames {
            Ok(CrossfadeMixOutcome::Complete)
        } else {
            Ok(CrossfadeMixOutcome::Continue)
        }
    }

    fn consumed_position(&self, output_rate: u32) -> Duration {
        self.cue.position
            + Duration::from_secs_f64(
                self.next_frames_consumed as f64 / f64::from(output_rate.max(1)),
            )
    }

    fn remaining_transition_frames(&self) -> u64 {
        self.total_frames.saturating_sub(self.transition_frames)
    }

    fn continuation_gain(&self) -> f32 {
        if self.transition_frames >= self.total_frames {
            1.0
        } else {
            (self.phase_sin * self.phase_sin).clamp(0.0, 1.0)
        }
    }

    fn take_remaining_buffer(&mut self) -> Vec<f32> {
        if self.buffer_cursor >= self.buffered_samples.len() {
            return Vec::new();
        }
        self.buffered_samples.split_off(self.buffer_cursor)
    }
}

#[inline]
fn mix_crossfade_sample(current: f32, next: f32, current_gain: f32, next_gain: f32) -> f32 {
    current * current_gain + next * next_gain
}

pub struct AudioEngine {
    command_queue: Arc<CommandQueue<PlayerCommand>>,
    event_rx: Receiver<PlayerEvent>,
    tracks: Arc<RwLock<HashMap<TrackId, Track>>>,
    snapshot: Arc<RwLock<PlayerSnapshot>>,
    paused: Arc<AtomicBool>,
    flush: Arc<AtomicBool>,
    output_gain: Arc<AtomicU32>,
    audible_frames: Arc<AtomicU64>,
    output_rate: u32,
    preloader: Arc<PreloadCoordinator>,
    preload_worker: Option<thread::JoinHandle<()>>,
    _stream: Stream,
    _worker: thread::JoinHandle<()>,
}

impl AudioEngine {
    pub fn new(volume: f32, eq: EqSettings, spatial: SpatialSettings) -> Result<Self> {
        Self::new_with_device(None, volume, eq, spatial)
    }

    pub fn new_with_device(
        device_id: Option<&str>,
        volume: f32,
        eq: EqSettings,
        spatial: SpatialSettings,
    ) -> Result<Self> {
        let host = cpal::default_host();
        let device = if let Some(device_id) = device_id {
            host.output_devices()
                .context("枚举音频输出设备失败")?
                .find(|device| device.name().is_ok_and(|name| name == device_id))
                .ok_or_else(|| anyhow!("没有找到输出设备“{device_id}”"))?
        } else {
            host.default_output_device()
                .context("没有找到默认音频输出设备")?
        };
        let supported = device
            .default_output_config()
            .context("读取默认音频输出配置失败")?;
        let output_rate = supported.sample_rate().0;
        let command_queue = Arc::new(CommandQueue::new());
        let (event_tx, event_rx) = bounded::<PlayerEvent>(256);
        let tracks = Arc::new(RwLock::new(HashMap::new()));
        let snapshot = Arc::new(RwLock::new(PlayerSnapshot {
            volume: volume.clamp(0.0, 1.0),
            ..PlayerSnapshot::default()
        }));
        let ring_capacity = ((output_rate as usize / 8) * 2).max(4096);
        let ring = HeapRb::<f32>::new(ring_capacity);
        let (producer, consumer) = ring.split();
        let flush = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let output_gain = Arc::new(AtomicU32::new(volume.clamp(0.0, 1.0).to_bits()));
        let audible_frames = Arc::new(AtomicU64::new(0));
        let preloader = Arc::new(PreloadCoordinator::new());
        let stream = build_output_stream(
            &device,
            &supported,
            consumer,
            flush.clone(),
            paused.clone(),
            output_gain.clone(),
            audible_frames.clone(),
            event_tx.clone(),
        )?;
        stream.play().context("启动音频输出流失败")?;

        let worker_tracks = tracks.clone();
        let worker_snapshot = snapshot.clone();
        let worker_flush = flush.clone();
        let worker_paused = paused.clone();
        let worker_audible_frames = audible_frames.clone();
        let worker_command_queue = command_queue.clone();
        let worker_preloader = preloader.clone();
        let worker = thread::Builder::new()
            .name("yinqidao-audio-worker".into())
            .spawn(move || {
                let mut worker = AudioWorker::new_with_preloader(
                    worker_command_queue,
                    event_tx,
                    producer,
                    worker_tracks,
                    worker_snapshot,
                    worker_flush,
                    worker_paused,
                    worker_audible_frames,
                    worker_preloader,
                    AudioWorkerConfig {
                        output_rate,
                        eq,
                        spatial,
                    },
                );
                worker.run();
            })
            .context("创建音频工作线程失败")?;
        let preload_worker = match preloader.spawn() {
            Ok(worker) => worker,
            Err(error) => {
                command_queue.close();
                preloader.close();
                return Err(anyhow!("创建音频预加载线程失败: {error}"));
            }
        };

        Ok(Self {
            command_queue,
            event_rx,
            tracks,
            snapshot,
            paused,
            flush,
            output_gain,
            audible_frames,
            output_rate,
            preloader,
            preload_worker: Some(preload_worker),
            _stream: stream,
            _worker: worker,
        })
    }

    pub fn register_tracks(&self, tracks: impl IntoIterator<Item = Track>) {
        if let Ok(mut known_tracks) = self.tracks.write() {
            known_tracks.extend(tracks.into_iter().map(|track| (track.id, track)));
        }
    }

    fn set_audible_position_immediately(&self, position: Duration) {
        self.flush.store(true, Ordering::Release);
        let frames = position.as_nanos().saturating_mul(self.output_rate as u128)
            / 1_000_000_000_u128;
        self.audible_frames.store(
            frames.min(u64::MAX as u128) as u64,
            Ordering::Release,
        );
    }

    fn reset_transport_for_switch(&self, loading: bool) {
        self.set_audible_position_immediately(Duration::ZERO);
        if let Ok(mut snapshot) = self.snapshot.write() {
            snapshot.position_ms = 0;
            snapshot.error = None;
            if loading {
                snapshot.state = PlaybackState::Loading;
            }
        }
    }

    pub fn try_send(&self, command: PlayerCommand) -> bool {
        match &command {
            PlayerCommand::Pause => {
                self.paused.store(true, Ordering::Release);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.state = PlaybackState::Paused;
                }
            }
            PlayerCommand::Play => {
                self.paused.store(false, Ordering::Release);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.state = PlaybackState::Playing;
                }
            }
            PlayerCommand::Seek(position) => {
                self.set_audible_position_immediately(*position);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.position_ms = position.as_millis() as u64;
                }
            }
            PlayerCommand::PlayTrack(track_id) => {
                self.reset_transport_for_switch(true);
                let track = self
                    .tracks
                    .read()
                    .ok()
                    .and_then(|tracks| tracks.get(track_id).cloned());
                if let Some(track) = track
                    && let Ok(mut snapshot) = self.snapshot.write()
                {
                    snapshot.duration_ms = track.duration_ms;
                    snapshot.current_track = Some(track);
                }
            }
            PlayerCommand::Next => self.reset_transport_for_switch(true),
            PlayerCommand::Previous => self.reset_transport_for_switch(false),
            PlayerCommand::SetVolume(volume) => {
                let volume = volume.clamp(0.0, 1.0);
                self.output_gain.store(volume.to_bits(), Ordering::Release);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.volume = volume;
                }
            }
            PlayerCommand::Stop => {
                self.paused.store(true, Ordering::Release);
                self.set_audible_position_immediately(Duration::ZERO);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.state = PlaybackState::Stopped;
                    snapshot.position_ms = 0;
                }
            }
            PlayerCommand::RestoreTrack { play, .. } => {
                self.paused.store(!*play, Ordering::Release);
                if !*play && let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.state = PlaybackState::Paused;
                }
            }
            _ => {}
        }
        self.command_queue.push(command, can_coalesce_commands)
    }

    pub fn drain_events(&self) -> Vec<PlayerEvent> {
        self.event_rx.try_iter().collect()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let mut snapshot = self
            .snapshot
            .read()
            .map_or_else(|_| PlayerSnapshot::default(), |snapshot| snapshot.clone());
        snapshot.position_ms = self.audible_position_ms(snapshot.duration_ms);
        snapshot
    }

    pub fn progress(&self) -> (PlaybackState, u64, u64) {
        self.snapshot
            .read()
            .map_or((PlaybackState::Stopped, 0, 0), |snapshot| {
                (
                    snapshot.state,
                    self.audible_position_ms(snapshot.duration_ms),
                    snapshot.duration_ms,
                )
            })
    }

    fn audible_position_ms(&self, duration_ms: u64) -> u64 {
        let frames = self.audible_frames.load(Ordering::Acquire);
        let position_ms = frames.saturating_mul(1_000) / u64::from(self.output_rate.max(1));
        if duration_ms > 0 {
            position_ms.min(duration_ms)
        } else {
            position_ms
        }
    }

    pub fn output_devices() -> Result<Vec<OutputDeviceInfo>> {
        let host = cpal::default_host();
        let devices = host.output_devices().context("枚举音频输出设备失败")?;
        let mut result = Vec::new();
        for device in devices {
            let name = device.name().unwrap_or_else(|_| "未命名设备".into());
            let config = device.default_output_config().ok();
            result.push(OutputDeviceInfo {
                id: name.clone(),
                name,
                sample_rate: config
                    .as_ref()
                    .map_or(48_000, |value| value.sample_rate().0),
                channels: config.as_ref().map_or(2, SupportedStreamConfig::channels),
            });
        }
        Ok(result)
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.command_queue.close();
        self.preloader.close();
        if let Some(worker) = self.preload_worker.take() {
            let _ = worker.join();
        }
    }
}

struct AudioWorker {
    command_queue: Arc<CommandQueue<PlayerCommand>>,
    event_tx: Sender<PlayerEvent>,
    producer: ringbuf::HeapProd<f32>,
    tracks: Arc<RwLock<HashMap<TrackId, Track>>>,
    snapshot: Arc<RwLock<PlayerSnapshot>>,
    flush: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    audible_frames: Arc<AtomicU64>,
    output_rate: u32,
    processor: AudioProcessor,
    eq_settings: EqSettings,
    spatial_settings: SpatialSettings,
    smart_audio: SmartAudioSettings,
    transition: TrackTransitionSettings,
    decoded_samples: Vec<f32>,
    processed_samples: Vec<f32>,
    decoder: Option<DecoderStream>,
    prefetched_chunk: Option<PreloadedChunk>,
    preloader: Arc<PreloadCoordinator>,
    crossfade: Option<CrossfadeState>,
    transition_carry: Vec<f32>,
    transition_carry_position: Option<Duration>,
    fade_in_total_frames: u64,
    fade_in_elapsed_frames: u64,
    fade_in_start_gain: f32,
    preserve_ring_for_auto_next: bool,
    current_track: Option<TrackId>,
    queue: Arc<Vec<TrackId>>,
    queue_positions: HashMap<TrackId, usize>,
    queue_index: usize,
    repeat: RepeatMode,
    shuffle: bool,
    shuffle_played: HashSet<TrackId>,
    shuffle_state: u64,
    decode_generation: u64,
    state: PlaybackState,
    last_position_event: Instant,
}

struct AudioWorkerConfig {
    output_rate: u32,
    eq: EqSettings,
    spatial: SpatialSettings,
}

impl AudioWorker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        command_queue: Arc<CommandQueue<PlayerCommand>>,
        event_tx: Sender<PlayerEvent>,
        producer: ringbuf::HeapProd<f32>,
        tracks: Arc<RwLock<HashMap<TrackId, Track>>>,
        snapshot: Arc<RwLock<PlayerSnapshot>>,
        flush: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        audible_frames: Arc<AtomicU64>,
        config: AudioWorkerConfig,
    ) -> Self {
        Self::new_with_preloader(
            command_queue,
            event_tx,
            producer,
            tracks,
            snapshot,
            flush,
            paused,
            audible_frames,
            Arc::new(PreloadCoordinator::new()),
            config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_preloader(
        command_queue: Arc<CommandQueue<PlayerCommand>>,
        event_tx: Sender<PlayerEvent>,
        producer: ringbuf::HeapProd<f32>,
        tracks: Arc<RwLock<HashMap<TrackId, Track>>>,
        snapshot: Arc<RwLock<PlayerSnapshot>>,
        flush: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        audible_frames: Arc<AtomicU64>,
        preloader: Arc<PreloadCoordinator>,
        config: AudioWorkerConfig,
    ) -> Self {
        let output_rate = config.output_rate;
        let eq_settings = config.eq;
        let spatial_settings = config.spatial;
        let runtime_policy = audio_runtime_policy();
        Self {
            command_queue,
            event_tx,
            producer,
            tracks,
            snapshot,
            flush,
            paused,
            audible_frames,
            output_rate,
            processor: AudioProcessor::new(
                output_rate,
                eq_settings.clone(),
                spatial_settings.clone(),
                1.0,
            ),
            eq_settings,
            spatial_settings,
            smart_audio: runtime_policy.smart_audio,
            transition: sanitize_transition(runtime_policy.transition),
            decoded_samples: Vec::new(),
            processed_samples: Vec::new(),
            decoder: None,
            prefetched_chunk: None,
            preloader,
            crossfade: None,
            transition_carry: Vec::new(),
            transition_carry_position: None,
            fade_in_total_frames: 0,
            fade_in_elapsed_frames: 0,
            fade_in_start_gain: 0.0,
            preserve_ring_for_auto_next: false,
            current_track: None,
            queue: Arc::new(Vec::new()),
            queue_positions: HashMap::new(),
            queue_index: 0,
            repeat: RepeatMode::Off,
            shuffle: false,
            shuffle_played: HashSet::new(),
            shuffle_state: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0x9e37_79b9_7f4a_7c15, |duration| duration.as_nanos() as u64),
            decode_generation: 0,
            state: PlaybackState::Stopped,
            last_position_event: Instant::now(),
        }
    }

    fn run(&mut self) {
        loop {
            if !self.handle_commands() {
                break;
            }
            if self.state != PlaybackState::Playing || self.decoder.is_none() {
                thread::sleep(Duration::from_millis(8));
                continue;
            }
            match self.decode_next() {
                Ok(true) => {}
                Ok(false) => self.finish_track(),
                Err(error) => {
                    self.state = PlaybackState::Error;
                    self.emit(PlayerEvent::Error(error.into()));
                    self.emit(PlayerEvent::StateChanged(self.state));
                    self.decoder = None;
                    self.prefetched_chunk = None;
                    self.crossfade = None;
                }
            }
        }
    }

    fn handle_commands(&mut self) -> bool {
        loop {
            let Some(command) = self.command_queue.pop() else {
                return !self.command_queue.is_closed();
            };
            if !self.handle_command(command) {
                return false;
            }
        }
    }

    fn handle_command(&mut self, command: PlayerCommand) -> bool {
        match command {
            PlayerCommand::Play => {
                self.paused.store(false, Ordering::Release);
                if self.decoder.is_some() {
                    self.state = PlaybackState::Playing;
                    self.emit(PlayerEvent::StateChanged(self.state));
                } else {
                    self.open_queue_index(self.queue_index);
                }
            }
            PlayerCommand::Pause => {
                if self.state == PlaybackState::Playing {
                    self.state = PlaybackState::Paused;
                    self.paused.store(true, Ordering::Release);
                    self.emit(PlayerEvent::StateChanged(self.state));
                }
            }
            PlayerCommand::Seek(position) => {
                self.cancel_transition_runtime();
                self.prefetched_chunk = None;
                if let Some(decoder) = &mut self.decoder {
                    self.decode_generation = self.decode_generation.wrapping_add(1);
                    if let Err(error) = decoder.seek(position) {
                        self.emit(PlayerEvent::Error(error.into()));
                    } else {
                        self.synchronize_audible_position(position);
                        self.emit(PlayerEvent::PositionChanged(position));
                    }
                }
            }
            PlayerCommand::PlayTrack(track_id) => {
                self.cancel_transition_runtime();
                let target_index = self
                    .queue_position(track_id)
                    .unwrap_or_else(|| self.append_queue_track(track_id));
                self.open_queue_index(target_index);
            }
            PlayerCommand::RestoreTrack {
                track_id,
                position,
                play,
            } => {
                self.cancel_transition_runtime();
                let target_index = self
                    .queue_position(track_id)
                    .unwrap_or_else(|| self.append_queue_track(track_id));
                self.queue_index = target_index;
                if self.open_track(track_id) {
                    if !position.is_zero() {
                        self.prefetched_chunk = None;
                        if let Some(decoder) = &mut self.decoder {
                            if let Err(error) = decoder.seek(position) {
                                self.emit(PlayerEvent::Error(error.into()));
                            } else {
                                self.synchronize_audible_position(position);
                                self.emit(PlayerEvent::PositionChanged(position));
                            }
                        }
                    }
                    if !play {
                        self.state = PlaybackState::Paused;
                        self.paused.store(true, Ordering::Release);
                        self.emit(PlayerEvent::StateChanged(self.state));
                    }
                    self.schedule_next_preload();
                }
            }
            PlayerCommand::Next => {
                self.cancel_transition_runtime();
                self.next_track();
            }
            PlayerCommand::Previous => {
                self.cancel_transition_runtime();
                if self
                    .decoder
                    .as_ref()
                    .is_some_and(|decoder| decoder.position() > Duration::from_secs(3))
                {
                    let _ = self.handle_command(PlayerCommand::Seek(Duration::ZERO));
                } else if self.current_track.is_none() {
                    self.open_queue_index(self.queue_index);
                } else if self.queue_index > 0 {
                    self.open_queue_index(self.queue_index - 1);
                } else if !self.queue.is_empty() {
                    self.open_queue_index(self.queue.len() - 1);
                }
            }
            PlayerCommand::SetVolume(volume) => {
                let volume = volume.clamp(0.0, 1.0);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.volume = volume;
                }
            }
            PlayerCommand::SetEq(settings) => {
                self.eq_settings = settings;
                self.apply_processing_for_current_track();
            }
            PlayerCommand::SetSpatial(settings) => {
                self.spatial_settings = settings;
                self.apply_processing_for_current_track();
            }
            PlayerCommand::SetSmartAudio(settings) => {
                self.smart_audio = settings;
                self.apply_processing_for_current_track();
                self.schedule_next_preload();
            }
            PlayerCommand::SetTransition(settings) => {
                self.transition = sanitize_transition(settings);
                self.cancel_transition_runtime();
                self.schedule_next_preload();
            }
            PlayerCommand::SetOutputDevice(device) => self.emit(PlayerEvent::Error(
                PlaybackError::Device(format!("设备“{device}”将在下次启动时应用")),
            )),
            PlayerCommand::SetQueue(queue) => {
                self.queue = queue;
                self.rebuild_queue_positions();
                self.shuffle_played.clear();
                if let Some(current_track) = self.current_track {
                    if let Some(index) = self.queue_position(current_track) {
                        self.queue_index = index;
                        if self.shuffle {
                            self.shuffle_played.insert(current_track);
                        }
                        self.schedule_next_preload();
                    } else {
                        self.queue_index = 0;
                        self.preloader.cancel();
                        self.stop_playback(true);
                    }
                } else {
                    self.queue_index = 0;
                    self.preloader.cancel();
                }
            }
            PlayerCommand::SetRepeat(repeat) => {
                self.repeat = repeat;
                self.schedule_next_preload();
            }
            PlayerCommand::SetShuffle(shuffle) => {
                self.shuffle = shuffle;
                self.shuffle_played.clear();
                if let Some(track_id) = self.current_track
                    && self.shuffle
                {
                    self.shuffle_played.insert(track_id);
                }
                if self.shuffle {
                    self.preloader.cancel();
                } else {
                    self.schedule_next_preload();
                }
            }
            PlayerCommand::Stop => {
                self.cancel_transition_runtime();
                self.preloader.cancel();
                self.stop_playback(false);
            }
        }
        true
    }

    fn effective_processing_for_track(&self, track: &Track) -> (EqSettings, SpatialSettings) {
        if self.smart_audio.enabled {
            let decision = resolve_smart_audio(
                track,
                &self.eq_settings,
                &self.spatial_settings,
                self.smart_audio.intensity,
            );
            tracing::debug!(
                track_id = track.id,
                profile = decision.profile.label(),
                confidence = decision.confidence,
                "智能音效已匹配曲目"
            );
            (decision.eq, decision.spatial)
        } else {
            (self.eq_settings.clone(), self.spatial_settings.clone())
        }
    }

    fn apply_processing_for_track(&mut self, track: &Track) {
        let (eq, spatial) = self.effective_processing_for_track(track);
        self.processor.eq.set_settings(eq);
        self.processor.spatial.set_settings(spatial);
    }

    fn apply_processing_for_current_track(&mut self) {
        let track = self.current_track.and_then(|track_id| {
            self.tracks
                .read()
                .ok()
                .and_then(|tracks| tracks.get(&track_id).cloned())
        });
        if let Some(track) = track {
            self.apply_processing_for_track(&track);
        } else {
            self.processor.eq.set_settings(self.eq_settings.clone());
            self.processor
                .spatial
                .set_settings(self.spatial_settings.clone());
        }
    }

    fn cancel_transition_runtime(&mut self) {
        self.crossfade = None;
        self.transition_carry.clear();
        self.transition_carry_position = None;
        self.fade_in_total_frames = 0;
        self.fade_in_elapsed_frames = 0;
        self.fade_in_start_gain = 0.0;
        self.preserve_ring_for_auto_next = false;
    }

    fn queue_position(&self, track_id: TrackId) -> Option<usize> {
        self.queue_positions.get(&track_id).copied()
    }

    fn append_queue_track(&mut self, track_id: TrackId) -> usize {
        let queue = Arc::make_mut(&mut self.queue);
        queue.push(track_id);
        let index = queue.len() - 1;
        self.queue_positions.entry(track_id).or_insert(index);
        index
    }

    fn rebuild_queue_positions(&mut self) {
        self.queue_positions.clear();
        self.queue_positions.reserve(self.queue.len());
        for (index, track_id) in self.queue.iter().copied().enumerate() {
            self.queue_positions.entry(track_id).or_insert(index);
        }
    }

    fn linear_next_index(&self) -> Option<usize> {
        if self.queue.is_empty() || self.current_track.is_none() || self.repeat == RepeatMode::One {
            return None;
        }
        if self.queue_index + 1 < self.queue.len() {
            Some(self.queue_index + 1)
        } else if self.repeat == RepeatMode::All {
            Some(0)
        } else {
            None
        }
    }

    fn has_automatic_next(&self) -> bool {
        if self.repeat == RepeatMode::One {
            return true;
        }
        self.shuffle || self.linear_next_index().is_some()
    }

    fn schedule_next_preload(&self) {
        if self.shuffle || self.queue.is_empty() || self.current_track.is_none() {
            return;
        }
        let Some(next_index) = self.linear_next_index() else {
            self.preloader.cancel();
            return;
        };
        let Some(track_id) = self.queue.get(next_index).copied() else {
            return;
        };
        if Some(track_id) == self.current_track {
            return;
        }
        let track = self
            .tracks
            .read()
            .ok()
            .and_then(|tracks| tracks.get(&track_id).cloned());
        if let Some(track) = track {
            self.preloader.request(track, &self.transition);
        }
    }

    fn open_queue_index(&mut self, index: usize) -> bool {
        let Some(track_id) = self.queue.get(index).copied() else {
            return false;
        };
        if self.open_track(track_id) {
            self.queue_index = index;
            self.schedule_next_preload();
            true
        } else {
            false
        }
    }

    fn reset_audible_position(&self, flush_pcm: bool) {
        if flush_pcm {
            self.flush.store(true, Ordering::Release);
        }
        self.audible_frames.store(0, Ordering::Release);
    }

    fn open_track(&mut self, track_id: TrackId) -> bool {
        self.decode_generation = self.decode_generation.wrapping_add(1);
        let track = self
            .tracks
            .read()
            .ok()
            .and_then(|tracks| tracks.get(&track_id).cloned());
        let Some(track) = track else {
            self.emit(PlayerEvent::Error(PlaybackError::Decode(format!(
                "歌库中不存在歌曲 {track_id}"
            ))));
            self.emit(PlayerEvent::StateChanged(self.state));
            return false;
        };

        let previous_track = self.current_track;
        self.current_track = Some(track_id);
        self.state = PlaybackState::Loading;
        self.reset_audible_position(!self.preserve_ring_for_auto_next);
        self.emit(PlayerEvent::PositionChanged(Duration::ZERO));
        self.emit(PlayerEvent::StateChanged(self.state));

        let prepared = self.preloader.take(track_id);
        let opened = if let Some(preloaded) = prepared {
            if preloaded.smart_cue.position.is_zero() {
                self.prefetched_chunk = preloaded.first_chunk;
                Ok(preloaded.decoder)
            } else {
                self.prefetched_chunk = None;
                DecoderStream::open(&track.path)
            }
        } else {
            self.prefetched_chunk = None;
            DecoderStream::open(&track.path)
        };

        self.preserve_ring_for_auto_next = false;
        match opened {
            Ok(decoder) => {
                self.decoder = Some(decoder);
                self.apply_processing_for_track(&track);
                if self.shuffle {
                    self.shuffle_played.insert(track_id);
                }
                self.state = PlaybackState::Playing;
                self.paused.store(false, Ordering::Release);
                self.emit(PlayerEvent::StateChanged(self.state));
                true
            }
            Err(error) => {
                self.decoder = None;
                self.prefetched_chunk = None;
                self.current_track = previous_track;
                self.fade_in_total_frames = 0;
                self.fade_in_elapsed_frames = 0;
                self.fade_in_start_gain = 0.0;
                self.state = PlaybackState::Error;
                self.emit(PlayerEvent::Error(error.into()));
                self.emit(PlayerEvent::StateChanged(self.state));
                false
            }
        }
    }

    fn current_playable_duration_ms(&self) -> u64 {
        if let Some(duration) = self.decoder.as_ref().and_then(DecoderStream::duration) {
            let ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
            if ms > 0 {
                return ms;
            }
        }
        self.current_track
            .and_then(|track_id| {
                self.tracks
                    .read()
                    .ok()
                    .and_then(|tracks| tracks.get(&track_id).map(|track| track.duration_ms))
            })
            .unwrap_or(0)
    }

    fn decode_next(&mut self) -> Result<bool, DecodeError> {
        if !self.transition_carry.is_empty() {
            // `processed_samples` still contains the previous mixed chunk after it has already been
            // pushed into the CPAL ring. Swapping without clearing used to put that old chunk back
            // into `transition_carry`, so every subsequent decode iteration ping-ponged the two
            // buffers forever and replayed stale PCM after a crossfade handoff.
            //
            // Clear only the length (retain the allocation), then swap. The carry becomes empty and
            // is consumed exactly once; the following iteration resumes the next-track decoder.
            self.processed_samples.clear();
            std::mem::swap(&mut self.processed_samples, &mut self.transition_carry);
            debug_assert!(self.transition_carry.is_empty());
            self.apply_pending_fade_in();
            let position = self
                .transition_carry_position
                .take()
                .or_else(|| self.decoder.as_ref().map(DecoderStream::position))
                .unwrap_or_default();
            return self.push_processed_samples(position);
        }

        let decoded = if let Some(chunk) = self.prefetched_chunk.take() {
            self.decoded_samples = chunk.samples;
            Some((chunk.sample_rate, chunk.channels, chunk.position))
        } else {
            let Some(decoder) = &mut self.decoder else {
                return Ok(false);
            };
            match decoder.next_chunk_into(&mut self.decoded_samples)? {
                Some((sample_rate, channels)) => {
                    Some((sample_rate, channels, decoder.position()))
                }
                None => None,
            }
        };

        let Some((sample_rate, channels, position)) = decoded else {
            if self.crossfade.is_some() {
                let next_position = self.complete_crossfade(0, true);
                return Ok(next_position.is_some());
            }
            return Ok(false);
        };

        self.processor.process_into(
            &self.decoded_samples,
            sample_rate,
            channels,
            &mut self.processed_samples,
        );
        self.apply_pending_fade_in();

        if self.crossfade.is_none() {
            self.maybe_begin_crossfade(position)?;
        }

        let mut handoff_tail_frames = None;
        if let Some(mut crossfade) = self.crossfade.take() {
            let transition_before = crossfade.transition_frames;
            let total_transition_frames = crossfade.total_frames;
            let chunk_frames = self.processed_samples.len() / 2;
            match crossfade.mix_into(&mut self.processed_samples)? {
                CrossfadeMixOutcome::Continue => {
                    self.crossfade = Some(crossfade);
                }
                CrossfadeMixOutcome::Complete => {
                    let transition_frames_in_chunk = total_transition_frames
                        .saturating_sub(transition_before)
                        .min(chunk_frames as u64) as usize;
                    let frames_after_boundary =
                        chunk_frames.saturating_sub(transition_frames_in_chunk);
                    self.crossfade = Some(crossfade);
                    handoff_tail_frames = Some(frames_after_boundary);
                }
                CrossfadeMixOutcome::NextExhausted => {
                    tracing::warn!(
                        current_track = ?self.current_track,
                        next_track = crossfade.track_id,
                        "下一曲预加载流在交叉淡化前耗尽，本次保持当前曲原始 PCM 并回退到普通自动切换"
                    );
                    self.crossfade = None;
                }
            }
        } else {
            self.apply_fade_out(position);
        }

        let decode_generation = self.decode_generation;
        if !self.push_processed_samples(position)? {
            return Ok(false);
        }

        if let Some(frames_after_boundary) = handoff_tail_frames
            && self.decode_generation == decode_generation
            && self.crossfade.is_some()
            && self.state == PlaybackState::Playing
        {
            let preroll_frames = (u64::from(self.output_rate.max(1)) * 20 / 1_000).max(1) as usize;
            let target_samples = frames_after_boundary
                .saturating_add(preroll_frames)
                .saturating_mul(2);
            let deadline = Instant::now() + Duration::from_millis(500);

            while self.producer.occupied_len() > target_samples {
                if !self.handle_commands() {
                    return Ok(false);
                }
                if self.decode_generation != decode_generation
                    || self.crossfade.is_none()
                    || self.state != PlaybackState::Playing
                {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    tracing::warn!(
                        queued_frames = self.producer.occupied_len() / 2,
                        frames_after_boundary,
                        "等待交叉淡化可听边界超时，暂不提前切换下一曲状态"
                    );
                    return Ok(true);
                }
                thread::sleep(Duration::from_millis(1));
            }

            tracing::debug!(
                queued_frames = self.producer.occupied_len() / 2,
                frames_after_boundary,
                "声卡已接近交叉淡化结束边界，提交下一曲播放状态"
            );
            let _ = self.complete_crossfade(0, false);
        }

        Ok(true)
    }

    fn push_processed_samples(&mut self, position: Duration) -> Result<bool, DecodeError> {
        let decode_generation = self.decode_generation;
        for index in 0..self.processed_samples.len() {
            let sample = self.processed_samples[index];
            if index > 0 && index % 1024 == 0 {
                if !self.handle_commands() {
                    return Ok(false);
                }
                if self.decode_generation != decode_generation
                    || self.state != PlaybackState::Playing
                    || self.decoder.is_none()
                {
                    return Ok(true);
                }
            }
            loop {
                if self.producer.try_push(sample).is_ok() {
                    break;
                }
                if !self.handle_commands() {
                    return Ok(false);
                }
                if self.decode_generation != decode_generation
                    || self.state != PlaybackState::Playing
                    || self.decoder.is_none()
                {
                    return Ok(true);
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
        if self.last_position_event.elapsed() >= Duration::from_millis(100) {
            self.emit(PlayerEvent::PositionChanged(position));
            self.last_position_event = Instant::now();
        }
        Ok(true)
    }

    fn maybe_begin_crossfade(&mut self, position: Duration) -> Result<(), DecodeError> {
        if !self.transition.enabled
            || self.transition.mode != TransitionMode::Crossfade
            || self.repeat == RepeatMode::One
            || self.shuffle
        {
            return Ok(());
        }
        let Some(next_index) = self.linear_next_index() else {
            return Ok(());
        };
        let Some(next_track_id) = self.queue.get(next_index).copied() else {
            return Ok(());
        };
        if Some(next_track_id) == self.current_track {
            return Ok(());
        }

        let duration_ms = self.current_playable_duration_ms();
        if duration_ms == 0 {
            return Ok(());
        }
        let chunk_frames = self.processed_samples.len() as u64 / 2;
        let chunk_ms = chunk_frames.saturating_mul(1_000) / u64::from(self.output_rate.max(1));
        let remaining_at_chunk_start = duration_ms
            .saturating_sub(position.as_millis() as u64)
            .saturating_add(chunk_ms);
        if remaining_at_chunk_start > self.transition.duration_ms {
            return Ok(());
        }

        let Some(preloaded) = self.preloader.take(next_track_id) else {
            return Ok(());
        };
        let next_track = self
            .tracks
            .read()
            .ok()
            .and_then(|tracks| tracks.get(&next_track_id).cloned());
        let Some(next_track) = next_track else {
            return Ok(());
        };
        let cue_ms = preloaded.smart_cue.position.as_millis() as u64;
        let next_duration_ms = preloaded
            .decoder
            .duration()
            .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
            .filter(|duration| *duration > 0)
            .unwrap_or(next_track.duration_ms);
        let next_available_ms = if next_duration_ms > 0 {
            next_duration_ms.saturating_sub(cue_ms)
        } else {
            u64::MAX
        };
        if next_available_ms == 0 {
            return Ok(());
        }
        let (next_eq, next_spatial) = self.effective_processing_for_track(&next_track);
        let total_ms = self
            .transition
            .duration_ms
            .min(remaining_at_chunk_start)
            .min(next_available_ms)
            .max(1);
        let total_frames = total_ms.saturating_mul(u64::from(self.output_rate.max(1))) / 1_000;
        tracing::debug!(
            current_track = ?self.current_track,
            next_track = next_track_id,
            playable_duration_ms = duration_ms,
            duration_ms = total_ms,
            cue_ms,
            cue_confidence = preloaded.smart_cue.confidence,
            "启动自动交叉淡化"
        );
        self.crossfade = Some(CrossfadeState::new(
            preloaded,
            self.output_rate,
            next_eq,
            next_spatial,
            total_frames,
        ));
        Ok(())
    }

    fn complete_crossfade(
        &mut self,
        pending_chunk_frames: u64,
        current_ended_early: bool,
    ) -> Option<Duration> {
        let mut crossfade = self.crossfade.take()?;
        let next_track_id = crossfade.track_id;
        let next_position = crossfade.consumed_position(self.output_rate);
        let cue_position = crossfade.cue.position;
        let remaining_transition_frames = crossfade.remaining_transition_frames();
        let continuation_gain = crossfade.continuation_gain();
        let carry = crossfade.take_remaining_buffer();
        let carry_frames = carry.len() as u64 / 2;
        let carry_end_position = next_position
            + Duration::from_secs_f64(
                carry_frames as f64 / f64::from(self.output_rate.max(1)),
            );

        self.emit(PlayerEvent::TrackEnded);
        self.decode_generation = self.decode_generation.wrapping_add(1);
        self.current_track = Some(next_track_id);
        if let Some(index) = self.queue_position(next_track_id) {
            self.queue_index = index;
        }
        self.decoder = Some(crossfade.decoder);
        self.prefetched_chunk = crossfade.prefetched_chunk;
        self.processor = crossfade.processor;
        self.transition_carry = carry;
        self.transition_carry_position =
            (!self.transition_carry.is_empty()).then_some(carry_end_position);

        if current_ended_early && remaining_transition_frames > 0 {
            self.fade_in_total_frames = remaining_transition_frames;
            self.fade_in_elapsed_frames = 0;
            self.fade_in_start_gain = continuation_gain;
            tracing::debug!(
                next_track = next_track_id,
                remaining_frames = remaining_transition_frames,
                start_gain = continuation_gain,
                "当前曲提前 EOF，下一曲从现有交叉增益继续平滑接管"
            );
        } else {
            self.fade_in_total_frames = 0;
            self.fade_in_elapsed_frames = 0;
            self.fade_in_start_gain = 0.0;
        }

        let queued_frames = self.producer.occupied_len() as u64 / 2;
        let not_yet_audible = queued_frames.saturating_add(pending_chunk_frames);
        let next_position_frames = self.duration_to_output_frames(next_position);
        let cue_frames = self.duration_to_output_frames(cue_position);
        let audible_next_frames = next_position_frames
            .saturating_sub(not_yet_audible)
            .max(cue_frames.min(next_position_frames));
        self.audible_frames
            .store(audible_next_frames, Ordering::Release);
        let audible_next_position = self.output_frames_to_duration(audible_next_frames);

        self.state = PlaybackState::Playing;
        self.paused.store(false, Ordering::Release);
        self.emit(PlayerEvent::PositionChanged(audible_next_position));
        self.emit(PlayerEvent::StateChanged(self.state));
        self.schedule_next_preload();
        Some(audible_next_position)
    }

    fn apply_fade_out(&mut self, position: Duration) {
        if !self.transition.enabled
            || self.transition.mode != TransitionMode::FadeOutIn
            || !self.has_automatic_next()
            || self.repeat == RepeatMode::One
        {
            return;
        }
        let fade_ms = (self.transition.duration_ms / 2).max(MIN_TRANSITION_MS / 2);
        let duration_ms = self.current_playable_duration_ms();
        if duration_ms == 0 {
            return;
        }
        let frames = self.processed_samples.len() / 2;
        let chunk_ms = frames as u64 * 1_000 / u64::from(self.output_rate.max(1));
        let chunk_start_ms = (position.as_millis() as u64).saturating_sub(chunk_ms);
        let fade_start_ms = duration_ms.saturating_sub(fade_ms);
        for frame in 0..frames {
            let frame_ms = chunk_start_ms
                .saturating_add(frame as u64 * 1_000 / u64::from(self.output_rate.max(1)));
            if frame_ms < fade_start_ms {
                continue;
            }
            let progress = (frame_ms.saturating_sub(fade_start_ms)) as f32 / fade_ms as f32;
            let gain = fade_out_gain(progress);
            let index = frame * 2;
            self.processed_samples[index] *= gain;
            self.processed_samples[index + 1] *= gain;
        }
    }

    fn apply_pending_fade_in(&mut self) {
        if self.fade_in_total_frames == 0 {
            return;
        }
        let frames = self.processed_samples.len() / 2;
        for frame in 0..frames {
            let progress = if self.fade_in_total_frames <= 1 {
                1.0
            } else {
                self.fade_in_elapsed_frames as f32 / (self.fade_in_total_frames - 1) as f32
            };
            let curved = fade_in_gain(progress);
            let gain = self.fade_in_start_gain
                + (1.0 - self.fade_in_start_gain) * curved;
            let index = frame * 2;
            self.processed_samples[index] *= gain;
            self.processed_samples[index + 1] *= gain;
            self.fade_in_elapsed_frames = self.fade_in_elapsed_frames.saturating_add(1);
            if self.fade_in_elapsed_frames >= self.fade_in_total_frames {
                self.fade_in_total_frames = 0;
                self.fade_in_elapsed_frames = 0;
                self.fade_in_start_gain = 0.0;
                break;
            }
        }
    }

    fn prepare_automatic_open(&mut self) {
        self.preserve_ring_for_auto_next = true;
        if self.transition.enabled
            && self.transition.mode == TransitionMode::FadeOutIn
            && self.repeat != RepeatMode::One
        {
            let fade_in_ms = (self.transition.duration_ms / 2).max(MIN_TRANSITION_MS / 2);
            self.fade_in_total_frames =
                fade_in_ms.saturating_mul(u64::from(self.output_rate.max(1))) / 1_000;
            self.fade_in_elapsed_frames = 0;
            self.fade_in_start_gain = 0.0;
            tracing::debug!(fade_in_ms, "下一曲淡入已准备");
        }
    }

    fn finish_track(&mut self) {
        self.emit(PlayerEvent::TrackEnded);
        match self.repeat {
            RepeatMode::One => {
                if let Some(track_id) = self.current_track {
                    self.preserve_ring_for_auto_next = true;
                    self.open_track(track_id);
                }
            }
            RepeatMode::All => {
                self.prepare_automatic_open();
                self.next_track();
            }
            RepeatMode::Off => {
                if self.queue_index + 1 < self.queue.len() {
                    self.prepare_automatic_open();
                    self.next_track();
                } else {
                    self.stop_playback(false);
                }
            }
        }
    }

    fn next_track(&mut self) {
        if self.queue.is_empty() {
            self.stop_playback(self.current_track.is_some());
            return;
        }
        let target_index = if self.current_track.is_none() {
            self.queue_index.min(self.queue.len() - 1)
        } else if self.shuffle {
            let mut available = self
                .queue
                .iter()
                .enumerate()
                .filter_map(|(index, track_id)| {
                    (!self.shuffle_played.contains(track_id)).then_some(index)
                })
                .collect::<Vec<_>>();
            if available.is_empty() {
                self.shuffle_played.clear();
                available = (0..self.queue.len()).collect();
            }
            let random_index = self.next_shuffle_index(available.len());
            available.swap_remove(random_index)
        } else if self.queue_index + 1 < self.queue.len() {
            self.queue_index + 1
        } else if self.repeat == RepeatMode::All {
            0
        } else {
            self.stop_playback(false);
            return;
        };
        self.open_queue_index(target_index);
    }

    fn next_shuffle_index(&mut self, length: usize) -> usize {
        self.shuffle_state ^= self.shuffle_state << 7;
        self.shuffle_state ^= self.shuffle_state >> 9;
        self.shuffle_state ^= self.shuffle_state << 8;
        (self.shuffle_state as usize) % length.max(1)
    }

    fn synchronize_audible_position(&self, position: Duration) {
        self.flush.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(50);
        while self.flush.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        self.audible_frames
            .store(self.duration_to_output_frames(position), Ordering::Release);
    }

    fn duration_to_output_frames(&self, position: Duration) -> u64 {
        let frames = position.as_nanos().saturating_mul(self.output_rate as u128)
            / 1_000_000_000_u128;
        frames.min(u64::MAX as u128) as u64
    }

    fn output_frames_to_duration(&self, frames: u64) -> Duration {
        Duration::from_secs_f64(frames as f64 / f64::from(self.output_rate.max(1)))
    }

    fn stop_playback(&mut self, clear_current_track: bool) {
        self.decode_generation = self.decode_generation.wrapping_add(1);
        self.decoder = None;
        self.prefetched_chunk = None;
        self.crossfade = None;
        self.transition_carry.clear();
        self.transition_carry_position = None;
        self.fade_in_total_frames = 0;
        self.fade_in_elapsed_frames = 0;
        self.fade_in_start_gain = 0.0;
        if clear_current_track {
            self.current_track = None;
        }
        self.state = PlaybackState::Stopped;
        self.paused.store(true, Ordering::Release);
        self.reset_audible_position(true);
        self.emit(PlayerEvent::PositionChanged(Duration::ZERO));
        self.emit(PlayerEvent::StateChanged(self.state));
    }

    fn emit(&mut self, event: PlayerEvent) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            match &event {
                PlayerEvent::StateChanged(state) => snapshot.state = *state,
                PlayerEvent::PositionChanged(position) => {
                    snapshot.position_ms = position.as_millis() as u64
                }
                PlayerEvent::Error(error) => {
                    snapshot.error = Some(error.to_string());
                    snapshot.state = PlaybackState::Error;
                }
                PlayerEvent::TrackEnded | PlayerEvent::Buffering => {}
            }
            snapshot.current_track = self.current_track.and_then(|track_id| {
                self.tracks
                    .read()
                    .ok()
                    .and_then(|tracks| tracks.get(&track_id).cloned())
            });
            snapshot.duration_ms = snapshot
                .current_track
                .as_ref()
                .map_or(0, |track| track.duration_ms);
            snapshot.queue = self.queue.clone();
            snapshot.repeat = self.repeat;
            snapshot.shuffle = self.shuffle;
        }
        let _ = self.event_tx.try_send(event);
    }
}

fn sanitize_transition(mut settings: TrackTransitionSettings) -> TrackTransitionSettings {
    settings.duration_ms = settings.duration_ms.clamp(MIN_TRANSITION_MS, MAX_TRANSITION_MS);
    settings.max_smart_cue_ms = settings.max_smart_cue_ms.min(8_000);
    settings
}

fn can_coalesce_commands(queued: &PlayerCommand, incoming: &PlayerCommand) -> bool {
    matches!(
        (queued, incoming),
        (PlayerCommand::Seek(_), PlayerCommand::Seek(_))
            | (PlayerCommand::PlayTrack(_), PlayerCommand::PlayTrack(_))
            | (PlayerCommand::Next, PlayerCommand::Next)
            | (PlayerCommand::Previous, PlayerCommand::Previous)
            | (PlayerCommand::SetVolume(_), PlayerCommand::SetVolume(_))
            | (PlayerCommand::SetEq(_), PlayerCommand::SetEq(_))
            | (PlayerCommand::SetSpatial(_), PlayerCommand::SetSpatial(_))
            | (PlayerCommand::SetSmartAudio(_), PlayerCommand::SetSmartAudio(_))
            | (PlayerCommand::SetTransition(_), PlayerCommand::SetTransition(_))
            | (
                PlayerCommand::SetOutputDevice(_),
                PlayerCommand::SetOutputDevice(_)
            )
            | (PlayerCommand::SetQueue(_), PlayerCommand::SetQueue(_))
            | (PlayerCommand::SetRepeat(_), PlayerCommand::SetRepeat(_))
            | (PlayerCommand::SetShuffle(_), PlayerCommand::SetShuffle(_))
    )
}

fn build_output_stream(
    device: &Device,
    supported: &SupportedStreamConfig,
    mut consumer: ringbuf::HeapCons<f32>,
    flush: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    output_gain: Arc<AtomicU32>,
    audible_frames: Arc<AtomicU64>,
    event_tx: Sender<PlayerEvent>,
) -> Result<Stream> {
    let config: StreamConfig = supported.clone().into();
    let channels = config.channels as usize;
    let f32_events = event_tx.clone();
    let i16_events = event_tx.clone();
    let u16_events = event_tx;
    let stream = match supported.sample_format() {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                fill_f32(
                    data,
                    channels,
                    &mut consumer,
                    &flush,
                    &paused,
                    &audible_frames,
                );
                let gain = current_output_gain(&output_gain);
                if gain != 1.0 {
                    for sample in data {
                        *sample *= gain;
                    }
                }
            },
            move |error: cpal::StreamError| {
                let _ = f32_events
                    .try_send(PlayerEvent::Error(PlaybackError::Output(error.to_string())));
            },
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| {
                fill_i16(
                    data,
                    channels,
                    &mut consumer,
                    &flush,
                    &paused,
                    &audible_frames,
                );
                let gain = current_output_gain(&output_gain);
                if gain != 1.0 {
                    for sample in data {
                        *sample = ((*sample as f32 * gain)
                            .round()
                            .clamp(i16::MIN as f32, i16::MAX as f32))
                            as i16;
                    }
                }
            },
            move |error: cpal::StreamError| {
                let _ = i16_events
                    .try_send(PlayerEvent::Error(PlaybackError::Output(error.to_string())));
            },
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _| {
                fill_u16(
                    data,
                    channels,
                    &mut consumer,
                    &flush,
                    &paused,
                    &audible_frames,
                );
                let gain = current_output_gain(&output_gain);
                if gain != 1.0 {
                    const CENTER: f32 = 32768.0;
                    for sample in data {
                        let centered = *sample as f32 - CENTER;
                        *sample = (CENTER + centered * gain)
                            .round()
                            .clamp(0.0, u16::MAX as f32) as u16;
                    }
                }
            },
            move |error: cpal::StreamError| {
                let _ = u16_events
                    .try_send(PlayerEvent::Error(PlaybackError::Output(error.to_string())));
            },
            None,
        ),
        format => return Err(anyhow!("不支持的音频输出采样格式: {format:?}")),
    }
    .map_err(|error| anyhow!("创建音频输出流失败: {error}"))?;
    Ok(stream)
}

#[inline]
fn current_output_gain(output_gain: &AtomicU32) -> f32 {
    perceptual_volume_gain(f32::from_bits(output_gain.load(Ordering::Acquire)))
}

fn read_stereo(consumer: &mut ringbuf::HeapCons<f32>) -> Option<(f32, f32)> {
    if consumer.occupied_len() < 2 {
        return None;
    }
    Some((consumer.try_pop()?, consumer.try_pop()?))
}

fn fill_f32(
    data: &mut [f32],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
    audible_frames: &AtomicU64,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(0.0);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let Some((left, right)) = read_stereo(consumer) else {
            frame.fill(0.0);
            continue;
        };
        for (index, sample) in frame.iter_mut().enumerate() {
            *sample = if channels == 1 {
                (left + right) * 0.5
            } else if index % 2 == 0 {
                left
            } else {
                right
            };
        }
        audible_frames.fetch_add(1, Ordering::Relaxed);
    }
}

fn fill_i16(
    data: &mut [i16],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
    audible_frames: &AtomicU64,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(0);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let Some((left, right)) = read_stereo(consumer) else {
            frame.fill(0);
            continue;
        };
        for (index, sample) in frame.iter_mut().enumerate() {
            let value = if channels == 1 {
                (left + right) * 0.5
            } else if index % 2 == 0 {
                left
            } else {
                right
            };
            *sample = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        }
        audible_frames.fetch_add(1, Ordering::Relaxed);
    }
}

fn fill_u16(
    data: &mut [u16],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
    audible_frames: &AtomicU64,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(32768);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let Some((left, right)) = read_stereo(consumer) else {
            frame.fill(32768);
            continue;
        };
        for (index, sample) in frame.iter_mut().enumerate() {
            let value = if channels == 1 {
                (left + right) * 0.5
            } else if index % 2 == 0 {
                left
            } else {
                right
            };
            *sample = ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16;
        }
        audible_frames.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
