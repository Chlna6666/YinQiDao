use std::{
    collections::HashMap,
    f32::consts::FRAC_PI_2,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::model::{
    EqSettings, PlaybackState, PlayerSnapshot, SpatialSettings, Track, TrackData, TrackId,
};

use super::engine::{
    AudioEngine as BlockingAudioEngine, OutputDeviceInfo, PlayerCommand, PlayerEvent,
};

const REQUEST_QUEUE_CAPACITY: usize = 128;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(25);
const TRANSPORT_FADE_DURATION: Duration = Duration::from_millis(120);
const TRANSPORT_FADE_SEND_INTERVAL: Duration = Duration::from_millis(8);
const TRANSIENT_TRACK_SWEEP_INTERVAL: Duration = Duration::from_secs(1);
// The blocking player currently caps crossfade/fade transitions at 12 seconds. Keep the Host-owned
// playback backing for a few extra seconds after transport release so a detached preloader/crossfade
// decoder cannot race cache GC. This is control-plane state; the realtime callback never touches it.
const TRANSIENT_TRACK_RELEASE_GRACE: Duration = Duration::from_secs(15);
const NO_STATE_OVERRIDE: u8 = u8::MAX;
const NO_POSITION_OVERRIDE: u64 = u64::MAX;
const SEEK_ACK_TOLERANCE_MS: u64 = 50;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioUiEvent {
    SnapshotChanged,
    Error(String),
}

/// UI-facing audio facade.
///
/// The decoder/output engine owns blocking locks internally. None of those locks are ever touched
/// from GPUI after construction: commands cross a bounded non-blocking mailbox, hot playback
/// progress is published through atomics, structural snapshots are double-buffered by a dedicated
/// bridge thread, and only structural changes are forwarded to the GPUI event bridge.
pub struct AudioEngine {
    request_tx: Sender<EngineRequest>,
    snapshot: Arc<SnapshotCache>,
    running: Arc<AtomicBool>,
    ui_event_rx: Mutex<Option<UnboundedReceiver<AudioUiEvent>>>,
}

enum EngineRequest {
    Command(PlayerCommand),
    RegisterTracks(Vec<Track>),
    /// Register one process-local Track while retaining its opaque playback backing only in the
    /// bridge control plane, then start it through the normal blocking player transport.
    PlayTransientTrack(Track),
    Shutdown,
}

struct TransientTrackState {
    track: Track,
    observed_in_transport: bool,
    unreferenced_since: Option<Instant>,
}

impl TransientTrackState {
    fn playing(track: Track) -> Self {
        Self {
            track,
            observed_in_transport: true,
            unreferenced_since: None,
        }
    }
}

struct SnapshotCache {
    slots: [RwLock<PlayerSnapshot>; 2],
    active: AtomicUsize,
    state: AtomicU8,
    position_ms: AtomicU64,
    duration_ms: AtomicU64,
    state_override: AtomicU8,
    position_override_ms: AtomicU64,
    transport_generation: AtomicU64,
}

impl SnapshotCache {
    fn new(initial: PlayerSnapshot) -> Self {
        Self {
            slots: [RwLock::new(initial.clone()), RwLock::new(initial.clone())],
            active: AtomicUsize::new(0),
            state: AtomicU8::new(encode_state(initial.state)),
            position_ms: AtomicU64::new(initial.position_ms),
            duration_ms: AtomicU64::new(initial.duration_ms),
            state_override: AtomicU8::new(NO_STATE_OVERRIDE),
            position_override_ms: AtomicU64::new(NO_POSITION_OVERRIDE),
            transport_generation: AtomicU64::new(0),
        }
    }

    fn store(&self, snapshot: PlayerSnapshot) {
        self.store_progress(snapshot.state, snapshot.position_ms, snapshot.duration_ms);

        let current = self.active.load(Ordering::Acquire) & 1;
        let inactive = 1 - current;
        if let Ok(mut slot) = self.slots[inactive].write() {
            *slot = snapshot;
            self.active.store(inactive, Ordering::Release);
        }
    }

    fn store_progress(&self, state: PlaybackState, position_ms: u64, duration_ms: u64) {
        let raw_state = encode_state(state);
        self.state.store(raw_state, Ordering::Release);
        self.position_ms.store(position_ms, Ordering::Release);
        self.duration_ms.store(duration_ms, Ordering::Release);

        let desired = self.state_override.load(Ordering::Acquire);
        if desired != NO_STATE_OVERRIDE && desired == raw_state {
            self.state_override
                .store(NO_STATE_OVERRIDE, Ordering::Release);
        }

        let desired_position = self.position_override_ms.load(Ordering::Acquire);
        if desired_position != NO_POSITION_OVERRIDE
            && position_ms.abs_diff(desired_position) <= SEEK_ACK_TOLERANCE_MS
        {
            let _ = self.position_override_ms.compare_exchange(
                desired_position,
                NO_POSITION_OVERRIDE,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn sync_snapshot(&self, snapshot: &mut PlayerSnapshot) {
        let current = self.active.load(Ordering::Acquire) & 1;
        let other = 1 - current;
        let structural = self.slots[current]
            .try_read()
            .or_else(|_| self.slots[other].try_read());

        if let Ok(slot) = structural {
            if !tracks_equal(snapshot.current_track.as_ref(), slot.current_track.as_ref()) {
                snapshot.current_track.clone_from(&slot.current_track);
            }
            if !Arc::ptr_eq(&snapshot.queue, &slot.queue) {
                snapshot.queue = slot.queue.clone();
            }
            snapshot.volume = slot.volume;
            snapshot.repeat = slot.repeat;
            snapshot.shuffle = slot.shuffle;
            if snapshot.error.as_deref() != slot.error.as_deref() {
                snapshot.error.clone_from(&slot.error);
            }
        }

        snapshot.state = self.visible_state();
        snapshot.position_ms = self.visible_position_ms();
        snapshot.duration_ms = self.duration_ms.load(Ordering::Acquire);
    }

    fn snapshot(&self) -> PlayerSnapshot {
        let mut snapshot = PlayerSnapshot::default();
        self.sync_snapshot(&mut snapshot);
        snapshot
    }

    fn progress(&self) -> (PlaybackState, u64, u64) {
        (
            self.visible_state(),
            self.visible_position_ms(),
            self.duration_ms.load(Ordering::Acquire),
        )
    }

    fn optimistic_state(command: &PlayerCommand) -> Option<PlaybackState> {
        match command {
            PlayerCommand::Play => Some(PlaybackState::Playing),
            PlayerCommand::Pause => Some(PlaybackState::Paused),
            PlayerCommand::Stop => Some(PlaybackState::Stopped),
            PlayerCommand::RestoreTrack { play: false, .. } => Some(PlaybackState::Paused),
            _ => None,
        }
    }

    fn optimistic_position_ms(command: &PlayerCommand) -> Option<u64> {
        match command {
            PlayerCommand::Seek(position) => Some(
                position
                    .as_millis()
                    .min(u128::from(NO_POSITION_OVERRIDE - 1)) as u64,
            ),
            PlayerCommand::RestoreTrack { position, .. } => Some(
                position
                    .as_millis()
                    .min(u128::from(NO_POSITION_OVERRIDE - 1)) as u64,
            ),
            _ => None,
        }
    }

    fn resets_position_override(command: &PlayerCommand) -> bool {
        matches!(
            command,
            PlayerCommand::PlayTrack(_)
                | PlayerCommand::Next
                | PlayerCommand::Previous
                | PlayerCommand::SetQueue(_)
                | PlayerCommand::Stop
        )
    }

    fn set_optimistic_state(&self, state: PlaybackState) {
        self.state_override
            .store(encode_state(state), Ordering::Release);
    }

    fn set_optimistic_position_ms(&self, position_ms: u64) -> u64 {
        self.position_override_ms
            .swap(position_ms.min(NO_POSITION_OVERRIDE - 1), Ordering::Release)
    }

    fn clear_optimistic_position_ms(&self) -> u64 {
        self.position_override_ms
            .swap(NO_POSITION_OVERRIDE, Ordering::Release)
    }

    fn restore_optimistic_position_ms(&self, expected: u64, previous: u64) {
        let _ = self.position_override_ms.compare_exchange(
            expected,
            previous,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn bump_transport_generation(&self) {
        self.transport_generation.fetch_add(1, Ordering::AcqRel);
    }

    fn transport_generation(&self) -> u64 {
        self.transport_generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn optimistic_command(&self, command: &PlayerCommand) {
        if let Some(state) = Self::optimistic_state(command) {
            self.set_optimistic_state(state);
        }
        if let Some(position_ms) = Self::optimistic_position_ms(command) {
            self.set_optimistic_position_ms(position_ms);
        }
    }

    fn visible_state(&self) -> PlaybackState {
        let overridden = self.state_override.load(Ordering::Acquire);
        if overridden != NO_STATE_OVERRIDE {
            decode_state(overridden)
        } else {
            decode_state(self.state.load(Ordering::Acquire))
        }
    }

    fn visible_position_ms(&self) -> u64 {
        let overridden = self.position_override_ms.load(Ordering::Acquire);
        if overridden != NO_POSITION_OVERRIDE {
            overridden
        } else {
            self.position_ms.load(Ordering::Acquire)
        }
    }
}

fn tracks_equal(left: Option<&Track>, right: Option<&Track>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.ptr_eq(right)
                || (left.id == right.id
                    && left.path == right.path
                    && left.title == right.title
                    && left.artist == right.artist
                    && left.album == right.album
                    && left.year == right.year
                    && left.genre == right.genre
                    && left.duration_ms == right.duration_ms
                    && left.codec == right.codec
                    && left.sample_rate == right.sample_rate
                    && left.channels == right.channels
                    && left.artwork_key == right.artwork_key)
        }
        _ => false,
    }
}

fn detached_playback_track(track: &Track) -> Track {
    Track::new(TrackData {
        id: track.id,
        path: track.path.clone(),
        title: track.title.clone(),
        artist: track.artist.clone(),
        album: track.album.clone(),
        year: track.year,
        genre: track.genre.clone(),
        duration_ms: track.duration_ms,
        codec: track.codec.clone(),
        sample_rate: track.sample_rate,
        channels: track.channels,
        artwork_key: track.artwork_key.clone(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FadeCompletion {
    Pause,
    Stop,
}

struct TransportFade {
    master_volume: f32,
    current_gain: f32,
    start_gain: f32,
    target_gain: f32,
    started_at: Instant,
    active: bool,
    completion: Option<FadeCompletion>,
    last_sent_control: f32,
    last_sent_at: Instant,
}

impl TransportFade {
    fn new(master_volume: f32) -> Self {
        let master_volume = master_volume.clamp(0.0, 1.0);
        Self {
            master_volume,
            current_gain: 0.0,
            start_gain: 0.0,
            target_gain: 0.0,
            started_at: Instant::now(),
            active: false,
            completion: None,
            last_sent_control: master_volume,
            last_sent_at: Instant::now(),
        }
    }

    fn curve(progress: f32) -> f32 {
        let phase = progress.clamp(0.0, 1.0) * FRAC_PI_2;
        let value = phase.sin();
        value * value
    }

    fn control_volume(&self) -> f32 {
        (self.master_volume * self.current_gain.clamp(0.0, 1.0).sqrt()).clamp(0.0, 1.0)
    }

    fn send_current(&mut self, engine: &BlockingAudioEngine, force: bool) {
        let control = self.control_volume();
        let changed = (control - self.last_sent_control).abs() >= 1.0e-4;
        if force || (changed && self.last_sent_at.elapsed() >= TRANSPORT_FADE_SEND_INTERVAL) {
            let _ = engine.try_send(PlayerCommand::SetVolume(control));
            self.last_sent_control = control;
            self.last_sent_at = Instant::now();
        }
    }

    fn set_master_volume(&mut self, volume: f32, engine: &BlockingAudioEngine) {
        self.master_volume = volume.clamp(0.0, 1.0);
        self.send_current(engine, true);
    }

    fn force_gain(&mut self, gain: f32, engine: &BlockingAudioEngine) {
        let gain = gain.clamp(0.0, 1.0);
        self.current_gain = gain;
        self.start_gain = gain;
        self.target_gain = gain;
        self.active = false;
        self.completion = None;
        self.send_current(engine, true);
    }

    fn fade_to(&mut self, target_gain: f32, completion: Option<FadeCompletion>) {
        self.start_gain = self.current_gain;
        self.target_gain = target_gain.clamp(0.0, 1.0);
        self.started_at = Instant::now();
        self.active = (self.start_gain - self.target_gain).abs() > 1.0e-5;
        self.completion = completion;
    }

    fn tick(&mut self, engine: &BlockingAudioEngine) -> bool {
        if self.active {
            let progress = (self.started_at.elapsed().as_secs_f32()
                / TRANSPORT_FADE_DURATION.as_secs_f32())
                .clamp(0.0, 1.0);
            let shaped = Self::curve(progress);
            self.current_gain =
                self.start_gain + (self.target_gain - self.start_gain) * shaped;
            self.send_current(engine, false);
            if progress >= 1.0 {
                self.current_gain = self.target_gain;
                self.active = false;
                self.send_current(engine, true);
            }
        }

        if !self.active {
            if let Some(completion) = self.completion.take() {
                match completion {
                    FadeCompletion::Pause => {
                        let _ = engine.try_send(PlayerCommand::Pause);
                    }
                    FadeCompletion::Stop => {
                        let _ = engine.try_send(PlayerCommand::Stop);
                    }
                }
                return true;
            }
        }
        false
    }
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
        let (request_tx, request_rx) = bounded::<EngineRequest>(REQUEST_QUEUE_CAPACITY);
        let (ui_event_tx, ui_event_rx) = unbounded_channel::<AudioUiEvent>();
        let (init_tx, init_rx) = bounded::<Result<()>>(1);
        let snapshot = Arc::new(SnapshotCache::new(PlayerSnapshot {
            volume: volume.clamp(0.0, 1.0),
            ..PlayerSnapshot::default()
        }));
        let running = Arc::new(AtomicBool::new(true));

        let worker_snapshot = snapshot.clone();
        let worker_running = running.clone();
        let requested_device = device_id.map(str::to_owned);
        thread::Builder::new()
            .name("yinqidao-audio-bridge".into())
            .spawn(move || {
                let engine = match BlockingAudioEngine::new_with_device(
                    requested_device.as_deref(),
                    volume,
                    eq,
                    spatial,
                ) {
                    Ok(engine) => engine,
                    Err(error) => {
                        let _ = init_tx.send(Err(error));
                        return;
                    }
                };

                worker_snapshot.store(engine.snapshot());
                let _ = ui_event_tx.send(AudioUiEvent::SnapshotChanged);
                if init_tx.send(Ok(())).is_err() {
                    return;
                }
                run_bridge(
                    engine,
                    request_rx,
                    ui_event_tx,
                    worker_snapshot,
                    worker_running,
                    volume,
                );
            })
            .context("创建音频 UI 桥接线程失败")?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                request_tx,
                snapshot,
                running,
                ui_event_rx: Mutex::new(Some(ui_event_rx)),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("音频 UI 桥接线程在初始化完成前退出")),
        }
    }

    pub fn register_tracks(&self, tracks: impl IntoIterator<Item = Track>) {
        let tracks = tracks.into_iter().collect::<Vec<_>>();
        if tracks.is_empty() {
            return;
        }
        let _ = self
            .request_tx
            .try_send(EngineRequest::RegisterTracks(tracks));
    }

    /// Start one process-local remote/materialized track without making the blocking engine registry
    /// own its cache/provenance backing forever. Remote playback ids are allocated from the negative
    /// namespace; positive Library ids must continue through the normal registration path.
    pub(crate) fn try_play_transient_track(&self, track: Track) -> bool {
        if track.id >= 0 {
            return false;
        }
        let previous_position_override = self.snapshot.clear_optimistic_position_ms();
        match self
            .request_tx
            .try_send(EngineRequest::PlayTransientTrack(track))
        {
            Ok(()) => {
                self.snapshot
                    .set_optimistic_state(PlaybackState::Loading);
                true
            }
            Err(_) => {
                self.snapshot.restore_optimistic_position_ms(
                    NO_POSITION_OVERRIDE,
                    previous_position_override,
                );
                false
            }
        }
    }

    pub fn try_send(&self, command: PlayerCommand) -> bool {
        let optimistic_state = SnapshotCache::optimistic_state(&command);
        let optimistic_position_ms = SnapshotCache::optimistic_position_ms(&command);
        let advances_transport = optimistic_position_ms.is_some();
        let position_override_update = optimistic_position_ms
            .map(|position_ms| {
                (
                    position_ms,
                    self.snapshot.set_optimistic_position_ms(position_ms),
                )
            })
            .or_else(|| {
                SnapshotCache::resets_position_override(&command).then(|| {
                    (
                        NO_POSITION_OVERRIDE,
                        self.snapshot.clear_optimistic_position_ms(),
                    )
                })
            });
        match self.request_tx.try_send(EngineRequest::Command(command)) {
            Ok(()) => {
                if let Some(state) = optimistic_state {
                    self.snapshot.set_optimistic_state(state);
                }
                if advances_transport {
                    self.snapshot.bump_transport_generation();
                }
                true
            }
            Err(_) => {
                if let Some((position_ms, previous)) = position_override_update {
                    self.snapshot
                        .restore_optimistic_position_ms(position_ms, previous);
                }
                false
            }
        }
    }

    pub fn take_ui_event_receiver(&self) -> Option<UnboundedReceiver<AudioUiEvent>> {
        self.ui_event_rx.lock().ok()?.take()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.snapshot()
    }

    pub fn sync_snapshot(&self, snapshot: &mut PlayerSnapshot) {
        self.snapshot.sync_snapshot(snapshot);
    }

    pub fn progress(&self) -> (PlaybackState, u64, u64) {
        self.snapshot.progress()
    }

    /// Monotonic generation for accepted seek/restore commands.
    ///
    /// UI consumers can use this to invalidate renderer-owned timelines without polling position or
    /// inferring discontinuities from wall-clock deltas. Normal playback progress never changes it.
    pub fn transport_generation(&self) -> u64 {
        self.snapshot.transport_generation()
    }

    pub fn output_devices() -> Result<Vec<OutputDeviceInfo>> {
        BlockingAudioEngine::output_devices()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        let _ = self.request_tx.try_send(EngineRequest::Shutdown);
    }
}

fn run_bridge(
    engine: BlockingAudioEngine,
    request_rx: Receiver<EngineRequest>,
    ui_event_tx: UnboundedSender<AudioUiEvent>,
    snapshot: Arc<SnapshotCache>,
    running: Arc<AtomicBool>,
    initial_volume: f32,
) {
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut last_transient_sweep = Instant::now();
    let mut transport_fade = TransportFade::new(initial_volume);
    // Online enrichment updates the registry without touching decoder transport. The blocking
    // engine's structural snapshot can therefore still carry the Track value captured when the
    // song started. Keep only the enriched current Track as an O(1) bridge-side overlay until the
    // transport switches to a different id; never mirror the full track registry here.
    let mut current_track_override: Option<Track> = None;
    let mut queue_override: Option<Arc<Vec<TrackId>>> = None;
    let mut transient_tracks = HashMap::<TrackId, TransientTrackState>::new();

    while running.load(Ordering::Acquire) {
        let mut refresh_snapshot = false;
        match request_rx.recv_timeout(Duration::from_millis(4)) {
            Ok(request) => {
                refresh_snapshot |= request_refreshes_snapshot(&request);
                if !apply_request(
                    &engine,
                    request,
                    &mut transport_fade,
                    &mut current_track_override,
                    &mut queue_override,
                    &mut transient_tracks,
                ) {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        for _ in 0..32 {
            let Ok(request) = request_rx.try_recv() else {
                break;
            };
            refresh_snapshot |= request_refreshes_snapshot(&request);
            if !apply_request(
                &engine,
                request,
                &mut transport_fade,
                &mut current_track_override,
                &mut queue_override,
                &mut transient_tracks,
            ) {
                running.store(false, Ordering::Release);
                break;
            }
        }

        refresh_snapshot |= transport_fade.tick(&engine);

        for event in engine.drain_events() {
            match event {
                PlayerEvent::PositionChanged(_) => {}
                PlayerEvent::Error(error) => {
                    refresh_snapshot = true;
                    let _ = ui_event_tx.send(AudioUiEvent::Error(error.to_string()));
                }
                _ => refresh_snapshot = true,
            }
        }

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            let (state, position_ms, duration_ms) = engine.progress();
            snapshot.store_progress(state, position_ms, duration_ms);
            last_progress = Instant::now();
        }

        if refresh_snapshot {
            let (mut current, released) = bridge_snapshot(
                &engine,
                &mut current_track_override,
                &mut queue_override,
                &mut transient_tracks,
                Instant::now(),
            );
            current.volume = transport_fade.master_volume;
            // SnapshotCache keeps two structural slots. When transient backing is released, overwrite
            // both slots so the inactive fallback cannot keep an obsolete cache lease alive.
            if released > 0 {
                snapshot.store(current.clone());
            }
            snapshot.store(current);
            let _ = ui_event_tx.send(AudioUiEvent::SnapshotChanged);
        } else if !transient_tracks.is_empty()
            && last_transient_sweep.elapsed() >= TRANSIENT_TRACK_SWEEP_INTERVAL
        {
            let (mut current, released) = bridge_snapshot(
                &engine,
                &mut current_track_override,
                &mut queue_override,
                &mut transient_tracks,
                Instant::now(),
            );
            if released > 0 {
                current.volume = transport_fade.master_volume;
                snapshot.store(current.clone());
                snapshot.store(current);
                let _ = ui_event_tx.send(AudioUiEvent::SnapshotChanged);
            }
            last_transient_sweep = Instant::now();
        }
    }
}

fn bridge_snapshot(
    engine: &BlockingAudioEngine,
    current_track_override: &mut Option<Track>,
    queue_override: &mut Option<Arc<Vec<TrackId>>>,
    transient_tracks: &mut HashMap<TrackId, TransientTrackState>,
    now: Instant,
) -> (PlayerSnapshot, usize) {
    let mut current = engine.snapshot();
    apply_queue_override(&mut current, queue_override);
    apply_current_track_override(&mut current, current_track_override);
    let released = reconcile_transient_tracks(
        &mut current,
        current_track_override,
        transient_tracks,
        now,
    );
    (current, released)
}

fn apply_queue_override(
    current: &mut PlayerSnapshot,
    queue_override: &mut Option<Arc<Vec<TrackId>>>,
) {
    let Some(expected) = queue_override.as_ref() else {
        return;
    };
    if current.queue.as_ref() == expected.as_ref() {
        *queue_override = None;
    } else {
        current.queue = expected.clone();
    }
}

fn append_queue_override(
    engine: &BlockingAudioEngine,
    queue_override: &mut Option<Arc<Vec<TrackId>>>,
    track_id: TrackId,
) {
    let source = queue_override
        .as_ref()
        .cloned()
        .unwrap_or_else(|| engine.snapshot().queue);
    if source.contains(&track_id) {
        *queue_override = Some(source);
        return;
    }
    let mut queue = source.as_ref().clone();
    queue.push(track_id);
    *queue_override = Some(Arc::new(queue));
}

fn reconcile_transient_tracks(
    current: &mut PlayerSnapshot,
    current_track_override: &mut Option<Track>,
    transient_tracks: &mut HashMap<TrackId, TransientTrackState>,
    now: Instant,
) -> usize {
    let current_id = current.current_track.as_ref().map(|track| track.id);
    let transport_active = matches!(
        current.state,
        PlaybackState::Loading
            | PlaybackState::Playing
            | PlaybackState::Paused
            | PlaybackState::Buffering
    );
    let mut released_current: Option<Track> = None;
    let mut released_ids = Vec::new();

    transient_tracks.retain(|track_id, state| {
        let referenced = transport_active
            && (current_id == Some(*track_id) || current.queue.contains(track_id));
        if referenced {
            state.observed_in_transport = true;
            state.unreferenced_since = None;
            return true;
        }
        if !state.observed_in_transport {
            return true;
        }

        let since = state.unreferenced_since.get_or_insert(now);
        if now.saturating_duration_since(*since) < TRANSIENT_TRACK_RELEASE_GRACE {
            return true;
        }

        if current_id == Some(*track_id) {
            released_current = Some(detached_playback_track(&state.track));
        }
        released_ids.push(*track_id);
        false
    });

    if let Some(replacement) = released_current {
        current.current_track = Some(replacement);
    }
    if current_track_override
        .as_ref()
        .is_some_and(|track| released_ids.contains(&track.id))
    {
        *current_track_override = None;
    }

    if let Some(track_id) = current.current_track.as_ref().map(|track| track.id)
        && let Some(state) = transient_tracks.get(&track_id)
    {
        current.current_track = Some(state.track.clone());
    }

    released_ids.len()
}

fn apply_current_track_override(
    current: &mut PlayerSnapshot,
    current_track_override: &mut Option<Track>,
) {
    let Some(override_id) = current_track_override.as_ref().map(|track| track.id) else {
        return;
    };
    if current.current_track.as_ref().map(|track| track.id) != Some(override_id) {
        *current_track_override = None;
        return;
    }

    let replacement = current_track_override
        .as_ref()
        .expect("current track override must exist after id check");
    if !tracks_equal(current.current_track.as_ref(), Some(replacement)) {
        current.current_track = Some(replacement.clone());
    }
}

fn request_refreshes_snapshot(request: &EngineRequest) -> bool {
    match request {
        EngineRequest::Command(
            PlayerCommand::Seek(_)
                | PlayerCommand::SetVolume(_)
                | PlayerCommand::SetEq(_)
                | PlayerCommand::SetSpatial(_)
                | PlayerCommand::SetSmartAudio(_)
                | PlayerCommand::SetTransition(_)
                | PlayerCommand::SetOutputDevice(_),
        )
        | EngineRequest::RegisterTracks(_)
        | EngineRequest::Shutdown => false,
        EngineRequest::Command(_) | EngineRequest::PlayTransientTrack(_) => true,
    }
}

fn register_tracks_for_bridge(
    engine: &BlockingAudioEngine,
    tracks: Vec<Track>,
    current_track_override: &mut Option<Track>,
    transient_tracks: &mut HashMap<TrackId, TransientTrackState>,
) {
    let current_id = engine.snapshot().current_track.as_ref().map(|track| track.id);
    let mut engine_tracks = Vec::with_capacity(tracks.len());

    for track in tracks {
        if track.id < 0 {
            if let Some(state) = transient_tracks.get_mut(&track.id) {
                state.track = track.clone();
                if current_id == Some(track.id) {
                    *current_track_override = Some(track.clone());
                }
            } else if current_id == Some(track.id) {
                *current_track_override = Some(detached_playback_track(&track));
            }
            // A process-local/remote Track may carry a Host cache lease. Never let the blocking
            // registry become an unbounded owner of that backing after playback has ended.
            engine_tracks.push(detached_playback_track(&track));
        } else {
            if current_id == Some(track.id) {
                *current_track_override = Some(track.clone());
            }
            engine_tracks.push(track);
        }
    }
    engine.register_tracks(engine_tracks);
}

fn apply_request(
    engine: &BlockingAudioEngine,
    request: EngineRequest,
    transport_fade: &mut TransportFade,
    current_track_override: &mut Option<Track>,
    queue_override: &mut Option<Arc<Vec<TrackId>>>,
    transient_tracks: &mut HashMap<TrackId, TransientTrackState>,
) -> bool {
    match request {
        EngineRequest::Command(command) => {
            match command {
                PlayerCommand::SetVolume(volume) => {
                    transport_fade.set_master_volume(volume, engine);
                }
                PlayerCommand::Pause => {
                    if engine.progress().0 == PlaybackState::Playing {
                        transport_fade.fade_to(0.0, Some(FadeCompletion::Pause));
                    } else {
                        transport_fade.force_gain(0.0, engine);
                        let _ = engine.try_send(PlayerCommand::Pause);
                    }
                }
                PlayerCommand::Stop => {
                    if engine.progress().0 == PlaybackState::Playing {
                        transport_fade.fade_to(0.0, Some(FadeCompletion::Stop));
                    } else {
                        transport_fade.force_gain(0.0, engine);
                        let _ = engine.try_send(PlayerCommand::Stop);
                    }
                }
                PlayerCommand::Play => {
                    let actual_state = engine.progress().0;
                    if transport_fade.completion.is_none()
                        && actual_state != PlaybackState::Playing
                    {
                        transport_fade.force_gain(0.0, engine);
                    }
                    transport_fade.completion = None;
                    let _ = engine.try_send(PlayerCommand::Play);
                    transport_fade.fade_to(1.0, None);
                }
                PlayerCommand::PlayTrack(track_id) => {
                    transport_fade.force_gain(0.0, engine);
                    if engine.try_send(PlayerCommand::PlayTrack(track_id)) {
                        append_queue_override(engine, queue_override, track_id);
                    }
                    transport_fade.fade_to(1.0, None);
                }
                PlayerCommand::Next => {
                    transport_fade.force_gain(0.0, engine);
                    let _ = engine.try_send(PlayerCommand::Next);
                    transport_fade.fade_to(1.0, None);
                }
                PlayerCommand::Previous => {
                    transport_fade.force_gain(0.0, engine);
                    let _ = engine.try_send(PlayerCommand::Previous);
                    transport_fade.fade_to(1.0, None);
                }
                PlayerCommand::RestoreTrack {
                    track_id,
                    position,
                    play,
                } => {
                    transport_fade.force_gain(0.0, engine);
                    if engine.try_send(PlayerCommand::RestoreTrack {
                        track_id,
                        position,
                        play,
                    }) {
                        append_queue_override(engine, queue_override, track_id);
                    }
                    if play {
                        transport_fade.fade_to(1.0, None);
                    }
                }
                PlayerCommand::SetQueue(queue) => {
                    if engine.try_send(PlayerCommand::SetQueue(queue.clone())) {
                        *queue_override = Some(queue);
                    }
                }
                other => {
                    let _ = engine.try_send(other);
                }
            }
            true
        }
        EngineRequest::RegisterTracks(tracks) => {
            register_tracks_for_bridge(
                engine,
                tracks,
                current_track_override,
                transient_tracks,
            );
            true
        }
        EngineRequest::PlayTransientTrack(track) => {
            if track.id >= 0 {
                return true;
            }
            let track_id = track.id;
            let detached = detached_playback_track(&track);
            engine.register_tracks(std::iter::once(detached));
            transient_tracks.insert(track_id, TransientTrackState::playing(track.clone()));
            *current_track_override = Some(track);

            transport_fade.force_gain(0.0, engine);
            if engine.try_send(PlayerCommand::PlayTrack(track_id)) {
                append_queue_override(engine, queue_override, track_id);
                transport_fade.fade_to(1.0, None);
            } else {
                transient_tracks.remove(&track_id);
                if current_track_override
                    .as_ref()
                    .is_some_and(|track| track.id == track_id)
                {
                    *current_track_override = None;
                }
            }
            true
        }
        EngineRequest::Shutdown => false,
    }
}

fn encode_state(state: PlaybackState) -> u8 {
    match state {
        PlaybackState::Stopped => 0,
        PlaybackState::Loading => 1,
        PlaybackState::Playing => 2,
        PlaybackState::Paused => 3,
        PlaybackState::Buffering => 4,
        PlaybackState::Error => 5,
    }
}

fn decode_state(value: u8) -> PlaybackState {
    match value {
        1 => PlaybackState::Loading,
        2 => PlaybackState::Playing,
        3 => PlaybackState::Paused,
        4 => PlaybackState::Buffering,
        5 => PlaybackState::Error,
        _ => PlaybackState::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize as TestAtomicUsize;

    fn test_track(title: &str) -> Track {
        Track::new(crate::model::TrackData {
            id: 7,
            path: std::path::PathBuf::from("test.flac"),
            title: title.to_owned(),
            artist: "artist".into(),
            album: "album".into(),
            year: Some(2026),
            genre: Some("test".into()),
            duration_ms: 10_000,
            codec: "flac".into(),
            sample_rate: 48_000,
            channels: 2,
            artwork_key: Some("art".into()),
        })
    }

    fn transient_test_track(title: &str, id: TrackId) -> Track {
        let mut track = test_track(title);
        track.id = id;
        track
    }

    #[test]
    fn optimistic_transport_state_is_visible_without_waiting_for_engine() {
        let cache = SnapshotCache::new(PlayerSnapshot::default());
        cache.optimistic_command(&PlayerCommand::Play);
        assert_eq!(cache.progress().0, PlaybackState::Playing);

        let confirmed = PlayerSnapshot {
            state: PlaybackState::Playing,
            ..PlayerSnapshot::default()
        };
        cache.store(confirmed);
        assert_eq!(cache.progress().0, PlaybackState::Playing);
        assert_eq!(cache.state_override.load(Ordering::Acquire), NO_STATE_OVERRIDE);
    }

    #[test]
    fn optimistic_seek_position_survives_stale_progress_until_acknowledged() {
        let cache = SnapshotCache::new(PlayerSnapshot {
            state: PlaybackState::Playing,
            position_ms: 1_000,
            duration_ms: 10_000,
            ..PlayerSnapshot::default()
        });
        cache.optimistic_command(&PlayerCommand::Seek(Duration::from_millis(7_500)));
        assert_eq!(cache.progress().1, 7_500);

        cache.store_progress(PlaybackState::Playing, 1_025, 10_000);
        assert_eq!(cache.progress().1, 7_500);
        assert_eq!(
            cache.position_override_ms.load(Ordering::Acquire),
            7_500
        );

        cache.store_progress(PlaybackState::Playing, 7_510, 10_000);
        assert_eq!(cache.progress().1, 7_510);
        assert_eq!(
            cache.position_override_ms.load(Ordering::Acquire),
            NO_POSITION_OVERRIDE
        );
    }

    #[test]
    fn newer_seek_is_not_cleared_by_an_older_seek_acknowledgment() {
        let cache = SnapshotCache::new(PlayerSnapshot {
            duration_ms: 10_000,
            ..PlayerSnapshot::default()
        });
        cache.optimistic_command(&PlayerCommand::Seek(Duration::from_millis(2_000)));
        cache.optimistic_command(&PlayerCommand::Seek(Duration::from_millis(8_000)));
        cache.store_progress(PlaybackState::Playing, 2_000, 10_000);
        assert_eq!(cache.progress().1, 8_000);
        assert_eq!(
            cache.position_override_ms.load(Ordering::Acquire),
            8_000
        );
    }

    #[test]
    fn restoring_failed_seek_does_not_clobber_newer_target() {
        let cache = SnapshotCache::new(PlayerSnapshot::default());
        let previous = cache.set_optimistic_position_ms(4_000);
        let failed_target_previous = cache.set_optimistic_position_ms(7_500);

        cache.set_optimistic_position_ms(9_000);
        cache.restore_optimistic_position_ms(7_500, failed_target_previous);

        assert_eq!(cache.progress().1, 9_000);
        assert_eq!(previous, NO_POSITION_OVERRIDE);
    }

    #[test]
    fn accepted_seek_advances_transport_generation_but_failed_seek_does_not() {
        let (request_tx, request_rx) = bounded::<EngineRequest>(1);
        let snapshot = Arc::new(SnapshotCache::new(PlayerSnapshot::default()));
        let engine = AudioEngine {
            request_tx,
            snapshot,
            running: Arc::new(AtomicBool::new(true)),
            ui_event_rx: Mutex::new(None),
        };

        assert_eq!(engine.transport_generation(), 0);
        assert!(engine.try_send(PlayerCommand::Seek(Duration::from_millis(1_000))));
        assert_eq!(engine.transport_generation(), 1);
        assert!(!engine.try_send(PlayerCommand::Seek(Duration::from_millis(2_000))));
        assert_eq!(engine.transport_generation(), 1);
        drop(request_rx);
    }

    #[test]
    fn hot_progress_does_not_replace_structural_snapshot() {
        let structural = PlayerSnapshot {
            volume: 0.42,
            position_ms: 10,
            duration_ms: 1_000,
            ..PlayerSnapshot::default()
        };
        let cache = SnapshotCache::new(PlayerSnapshot::default());
        cache.store(structural);
        cache.store_progress(PlaybackState::Playing, 700, 1_000);

        let loaded = cache.snapshot();
        assert_eq!(loaded.volume, 0.42);
        assert_eq!(loaded.state, PlaybackState::Playing);
        assert_eq!(loaded.position_ms, 700);
        assert_eq!(loaded.duration_ms, 1_000);
    }

    #[test]
    fn sync_snapshot_refreshes_changed_track_metadata() {
        let cache = SnapshotCache::new(PlayerSnapshot {
            current_track: Some(test_track("old")),
            ..PlayerSnapshot::default()
        });
        let mut target = cache.snapshot();
        cache.store(PlayerSnapshot {
            current_track: Some(test_track("new")),
            ..PlayerSnapshot::default()
        });
        cache.sync_snapshot(&mut target);
        assert_eq!(
            target
                .current_track
                .as_ref()
                .map(|track| track.title.as_str()),
            Some("new")
        );
    }

    #[test]
    fn current_track_override_survives_stale_structural_snapshot_until_track_switch() {
        let mut current = PlayerSnapshot {
            current_track: Some(test_track("old")),
            ..PlayerSnapshot::default()
        };
        let mut current_track_override = Some(test_track("enriched"));
        apply_current_track_override(&mut current, &mut current_track_override);
        assert_eq!(
            current.current_track.as_ref().map(|track| track.title.as_str()),
            Some("enriched")
        );
        assert!(current_track_override.is_some());

        let mut other = test_track("other");
        other.id = 8;
        current.current_track = Some(other);
        apply_current_track_override(&mut current, &mut current_track_override);
        assert!(current_track_override.is_none());
        assert_eq!(current.current_track.as_ref().map(|track| track.id), Some(8));
    }

    #[test]
    fn ui_snapshot_shares_decoder_queue() {
        let queue = Arc::new(vec![1, 2, 3, 4]);
        let source = PlayerSnapshot {
            queue: queue.clone(),
            ..PlayerSnapshot::default()
        };
        let cache = SnapshotCache::new(PlayerSnapshot::default());
        cache.store(source);
        let loaded = cache.snapshot();
        assert!(Arc::ptr_eq(&loaded.queue, &queue));
    }

    #[test]
    fn queue_override_survives_stale_engine_snapshot_until_acknowledged() {
        let expected = Arc::new(vec![1, 2, -1]);
        let mut queue_override = Some(expected.clone());
        let mut stale = PlayerSnapshot {
            queue: Arc::new(vec![1, 2]),
            ..PlayerSnapshot::default()
        };
        apply_queue_override(&mut stale, &mut queue_override);
        assert_eq!(stale.queue.as_ref(), expected.as_ref());
        assert!(queue_override.is_some());

        let mut acknowledged = PlayerSnapshot {
            queue: expected,
            ..PlayerSnapshot::default()
        };
        apply_queue_override(&mut acknowledged, &mut queue_override);
        assert!(queue_override.is_none());
    }

    #[test]
    fn stopped_transient_track_releases_after_crossfade_grace() {
        let track = transient_test_track("remote", -1);
        let mut transient_tracks = HashMap::from([(
            track.id,
            TransientTrackState {
                track: track.clone(),
                observed_in_transport: true,
                unreferenced_since: None,
            },
        )]);
        let mut current = PlayerSnapshot {
            state: PlaybackState::Stopped,
            current_track: Some(detached_playback_track(&track)),
            queue: Arc::new(vec![track.id]),
            ..PlayerSnapshot::default()
        };
        let mut current_track_override = Some(track.clone());
        let first = Instant::now();
        assert_eq!(
            reconcile_transient_tracks(
                &mut current,
                &mut current_track_override,
                &mut transient_tracks,
                first,
            ),
            0
        );
        assert!(transient_tracks.contains_key(&track.id));

        let released = reconcile_transient_tracks(
            &mut current,
            &mut current_track_override,
            &mut transient_tracks,
            first + TRANSIENT_TRACK_RELEASE_GRACE,
        );
        assert_eq!(released, 1);
        assert!(transient_tracks.is_empty());
        assert!(current_track_override.is_none());
        assert_eq!(current.current_track.as_ref().map(|track| track.id), Some(-1));
    }

    #[test]
    fn paused_transient_queue_entry_keeps_playback_backing_alive() {
        let track = transient_test_track("remote", -2);
        let mut transient_tracks = HashMap::from([(
            track.id,
            TransientTrackState::playing(track.clone()),
        )]);
        let mut current = PlayerSnapshot {
            state: PlaybackState::Paused,
            current_track: Some(detached_playback_track(&track)),
            queue: Arc::new(vec![track.id]),
            ..PlayerSnapshot::default()
        };
        let mut current_track_override = None;
        let now = Instant::now();
        assert_eq!(
            reconcile_transient_tracks(
                &mut current,
                &mut current_track_override,
                &mut transient_tracks,
                now + TRANSIENT_TRACK_RELEASE_GRACE + Duration::from_secs(1),
            ),
            0
        );
        assert!(transient_tracks.contains_key(&track.id));
        assert_eq!(current.current_track.as_ref().map(|track| track.id), Some(-2));
    }

    #[test]
    fn detached_track_does_not_retain_playback_backing() {
        struct DropProbe(Arc<TestAtomicUsize>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::AcqRel);
            }
        }

        let drops = Arc::new(TestAtomicUsize::new(0));
        let backed = transient_test_track("remote", -3)
            .with_playback_backing(DropProbe(drops.clone()));
        let detached = detached_playback_track(&backed);
        drop(backed);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(detached.id, -3);
    }

    #[test]
    fn transport_curve_is_smooth_at_both_endpoints() {
        assert_eq!(TransportFade::curve(0.0), 0.0);
        assert!((TransportFade::curve(1.0) - 1.0).abs() < 1.0e-6);
        let midpoint = TransportFade::curve(0.5);
        assert!((midpoint - 0.5).abs() < 1.0e-5);
    }

    #[test]
    fn transport_gain_compensates_engine_perceptual_volume_curve() {
        let mut fade = TransportFade::new(0.4);
        fade.current_gain = 0.25;
        assert!((fade.control_volume() - 0.2).abs() < 1.0e-6);
    }
}
