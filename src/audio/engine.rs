use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
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
    traits::{Consumer, Producer, Split},
};
use thiserror::Error;

use crate::{
    audio::{
        command_queue::CommandQueue,
        decoder::{DecodeError, DecoderStream},
        dsp::AudioProcessor,
    },
    model::{
        EqSettings, PlaybackState, PlayerSnapshot, RepeatMode, SpatialSettings, Track, TrackId,
    },
};

pub type DeviceId = String;

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

pub struct AudioEngine {
    command_queue: Arc<CommandQueue<PlayerCommand>>,
    event_rx: Receiver<PlayerEvent>,
    tracks: Arc<RwLock<HashMap<TrackId, Track>>>,
    snapshot: Arc<RwLock<PlayerSnapshot>>,
    paused: Arc<AtomicBool>,
    flush: Arc<AtomicBool>,
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
        // The command mailbox is bounded and coalesces latest-value controls, so rapid input
        // cannot turn into unbounded allocations while the decoder is opening a track.
        let command_queue = Arc::new(CommandQueue::new());
        let (event_tx, event_rx) = bounded::<PlayerEvent>(256);
        let tracks = Arc::new(RwLock::new(HashMap::new()));
        let snapshot = Arc::new(RwLock::new(PlayerSnapshot {
            volume: volume.clamp(0.0, 1.0),
            ..PlayerSnapshot::default()
        }));
        // 收紧环形缓冲区至约 125ms (~12,000 samples at 48kHz stereo)，消除 1 秒时间超前与 Seek 清空延迟
        let ring_capacity = ((output_rate as usize / 8) * 2).max(4096);
        let ring = HeapRb::<f32>::new(ring_capacity);
        let (producer, consumer) = ring.split();
        let flush = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let stream = build_output_stream(
            &device,
            &supported,
            consumer,
            flush.clone(),
            paused.clone(),
            event_tx.clone(),
        )?;
        stream.play().context("启动音频输出流失败")?;

        let worker_tracks = tracks.clone();
        let worker_snapshot = snapshot.clone();
        let worker_flush = flush.clone();
        let worker_paused = paused.clone();
        let worker_command_queue = command_queue.clone();
        let worker = thread::Builder::new()
            .name("yinqidao-audio-worker".into())
            .spawn(move || {
                let mut worker = AudioWorker::new(
                    worker_command_queue,
                    event_tx,
                    producer,
                    worker_tracks,
                    worker_snapshot,
                    worker_flush,
                    worker_paused,
                    AudioWorkerConfig {
                        output_rate,
                        volume,
                        eq,
                        spatial,
                    },
                );
                worker.run();
            })
            .context("创建音频工作线程失败")?;

        Ok(Self {
            command_queue,
            event_rx,
            tracks,
            snapshot,
            paused,
            flush,
            _stream: stream,
            _worker: worker,
        })
    }

    pub fn register_tracks(&self, tracks: impl IntoIterator<Item = Track>) {
        if let Ok(mut known_tracks) = self.tracks.write() {
            known_tracks.extend(tracks.into_iter().map(|track| (track.id, track)));
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
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.position_ms = position.as_millis() as u64;
                }
            }
            PlayerCommand::SetVolume(volume) => {
                let volume = volume.clamp(0.0, 1.0);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.volume = volume;
                }
            }
            PlayerCommand::Stop => {
                self.paused.store(true, Ordering::Release);
                self.flush.store(true, Ordering::Release);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.state = PlaybackState::Stopped;
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
        self.snapshot
            .read()
            .map_or_else(|_| PlayerSnapshot::default(), |snapshot| snapshot.clone())
    }

    pub fn progress(&self) -> (PlaybackState, u64, u64) {
        self.snapshot
            .read()
            .map_or((PlaybackState::Stopped, 0, 0), |snapshot| {
                (snapshot.state, snapshot.position_ms, snapshot.duration_ms)
            })
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
    processor: AudioProcessor,
    decoded_samples: Vec<f32>,
    processed_samples: Vec<f32>,
    decoder: Option<DecoderStream>,
    current_track: Option<TrackId>,
    queue: Arc<Vec<TrackId>>,
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
    volume: f32,
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
        config: AudioWorkerConfig,
    ) -> Self {
        Self {
            command_queue,
            event_tx,
            producer,
            tracks,
            snapshot,
            flush,
            paused,
            processor: AudioProcessor::new(
                config.output_rate,
                config.eq,
                config.spatial,
                config.volume,
            ),
            decoded_samples: Vec::new(),
            processed_samples: Vec::new(),
            decoder: None,
            current_track: None,
            queue: Arc::new(Vec::new()),
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
                if let Some(decoder) = &mut self.decoder {
                    self.decode_generation = self.decode_generation.wrapping_add(1);
                    if let Err(error) = decoder.seek(position) {
                        self.emit(PlayerEvent::Error(error.into()));
                    } else {
                        self.flush_output();
                        self.emit(PlayerEvent::PositionChanged(position));
                    }
                }
            }
            PlayerCommand::PlayTrack(track_id) => {
                let target_index =
                    if let Some(index) = self.queue.iter().position(|id| *id == track_id) {
                        index
                    } else {
                        let queue = Arc::make_mut(&mut self.queue);
                        queue.push(track_id);
                        queue.len() - 1
                    };
                self.open_queue_index(target_index);
            }
            PlayerCommand::RestoreTrack {
                track_id,
                position,
                play,
            } => {
                let target_index =
                    if let Some(index) = self.queue.iter().position(|id| *id == track_id) {
                        index
                    } else {
                        let queue = Arc::make_mut(&mut self.queue);
                        queue.push(track_id);
                        queue.len() - 1
                    };
                self.queue_index = target_index;
                if self.open_track(track_id) {
                    if !position.is_zero()
                        && let Some(decoder) = &mut self.decoder
                    {
                        if let Err(error) = decoder.seek(position) {
                            self.emit(PlayerEvent::Error(error.into()));
                        } else {
                            self.flush_output();
                            self.emit(PlayerEvent::PositionChanged(position));
                        }
                    }
                    if !play {
                        self.state = PlaybackState::Paused;
                        self.paused.store(true, Ordering::Release);
                        self.emit(PlayerEvent::StateChanged(self.state));
                    }
                }
            }
            PlayerCommand::Next => self.next_track(),
            PlayerCommand::Previous => {
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
                self.processor.set_volume(volume);
                if let Ok(mut snapshot) = self.snapshot.write() {
                    snapshot.volume = volume;
                }
            }
            PlayerCommand::SetEq(settings) => self.processor.eq.set_settings(settings),
            PlayerCommand::SetSpatial(settings) => self.processor.spatial.set_settings(settings),
            PlayerCommand::SetOutputDevice(device) => self.emit(PlayerEvent::Error(
                PlaybackError::Device(format!("设备“{device}”将在下次启动时应用")),
            )),
            PlayerCommand::SetQueue(queue) => {
                self.queue = queue;
                self.shuffle_played.clear();
                if let Some(current_track) = self.current_track {
                    if let Some(index) = self.queue.iter().position(|id| *id == current_track) {
                        self.queue_index = index;
                        if self.shuffle {
                            self.shuffle_played.insert(current_track);
                        }
                    } else {
                        self.queue_index = 0;
                        self.stop_playback(true);
                    }
                } else {
                    self.queue_index = 0;
                }
            }
            PlayerCommand::SetRepeat(repeat) => self.repeat = repeat,
            PlayerCommand::SetShuffle(shuffle) => {
                self.shuffle = shuffle;
                self.shuffle_played.clear();
                if let Some(track_id) = self.current_track
                    && self.shuffle
                {
                    self.shuffle_played.insert(track_id);
                }
            }
            PlayerCommand::Stop => {
                self.stop_playback(false);
            }
        }
        true
    }

    fn open_queue_index(&mut self, index: usize) -> bool {
        let Some(track_id) = self.queue.get(index).copied() else {
            return false;
        };
        if self.open_track(track_id) {
            self.queue_index = index;
            true
        } else {
            false
        }
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
        self.state = PlaybackState::Loading;
        self.emit(PlayerEvent::StateChanged(self.state));
        self.flush_output();
        match DecoderStream::open(&track.path) {
            Ok(decoder) => {
                self.decoder = Some(decoder);
                self.current_track = Some(track_id);
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
                self.state = PlaybackState::Error;
                self.emit(PlayerEvent::Error(error.into()));
                self.emit(PlayerEvent::StateChanged(self.state));
                false
            }
        }
    }

    fn decode_next(&mut self) -> Result<bool, DecodeError> {
        let (sample_rate, channels, position) = {
            let Some(decoder) = &mut self.decoder else {
                return Ok(false);
            };
            let Some((sample_rate, channels)) =
                decoder.next_chunk_into(&mut self.decoded_samples)?
            else {
                return Ok(false);
            };
            (sample_rate, channels, decoder.position())
        };
        self.processor.process_into(
            &self.decoded_samples,
            sample_rate,
            channels,
            &mut self.processed_samples,
        );
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
                // A full output ring means decoding is ahead of playback, not that playback is
                // buffering. Keep the Playing state while waiting for the CPAL callback to drain
                // samples, otherwise the worker loop will suspend itself after a few seconds.
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

    fn finish_track(&mut self) {
        self.emit(PlayerEvent::TrackEnded);
        match self.repeat {
            RepeatMode::One => {
                if let Some(track_id) = self.current_track {
                    self.open_track(track_id);
                }
            }
            RepeatMode::All => self.next_track(),
            RepeatMode::Off => {
                if self.queue_index + 1 < self.queue.len() {
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

    fn flush_output(&self) {
        self.flush.store(true, Ordering::Release);
    }

    fn stop_playback(&mut self, clear_current_track: bool) {
        self.decode_generation = self.decode_generation.wrapping_add(1);
        self.decoder = None;
        if clear_current_track {
            self.current_track = None;
        }
        self.state = PlaybackState::Stopped;
        self.paused.store(true, Ordering::Release);
        self.flush_output();
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

fn can_coalesce_commands(queued: &PlayerCommand, incoming: &PlayerCommand) -> bool {
    matches!(
        (queued, incoming),
        (PlayerCommand::Seek(_), PlayerCommand::Seek(_))
            | (PlayerCommand::PlayTrack(_), PlayerCommand::PlayTrack(_))
            | (PlayerCommand::SetVolume(_), PlayerCommand::SetVolume(_))
            | (PlayerCommand::SetEq(_), PlayerCommand::SetEq(_))
            | (PlayerCommand::SetSpatial(_), PlayerCommand::SetSpatial(_))
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
            move |data: &mut [f32], _| fill_f32(data, channels, &mut consumer, &flush, &paused),
            move |error: cpal::StreamError| {
                let _ = f32_events
                    .try_send(PlayerEvent::Error(PlaybackError::Output(error.to_string())));
            },
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |data: &mut [i16], _| fill_i16(data, channels, &mut consumer, &flush, &paused),
            move |error: cpal::StreamError| {
                let _ = i16_events
                    .try_send(PlayerEvent::Error(PlaybackError::Output(error.to_string())));
            },
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |data: &mut [u16], _| fill_u16(data, channels, &mut consumer, &flush, &paused),
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

fn read_stereo(consumer: &mut ringbuf::HeapCons<f32>) -> (f32, f32) {
    (
        consumer.try_pop().unwrap_or(0.0),
        consumer.try_pop().unwrap_or(0.0),
    )
}

fn fill_f32(
    data: &mut [f32],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(0.0);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let (left, right) = read_stereo(consumer);
        for (index, sample) in frame.iter_mut().enumerate() {
            *sample = if channels == 1 {
                (left + right) * 0.5
            } else if index % 2 == 0 {
                left
            } else {
                right
            };
        }
    }
}

fn fill_i16(
    data: &mut [i16],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(0);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let (left, right) = read_stereo(consumer);
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
    }
}

fn fill_u16(
    data: &mut [u16],
    channels: usize,
    consumer: &mut ringbuf::HeapCons<f32>,
    flush: &AtomicBool,
    paused: &AtomicBool,
) {
    if flush.swap(false, Ordering::Acquire) {
        consumer.clear();
    }
    if paused.load(Ordering::Relaxed) {
        data.fill(32768);
        return;
    }
    for frame in data.chunks_exact_mut(channels.max(1)) {
        let (left, right) = read_stereo(consumer);
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
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
