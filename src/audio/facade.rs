use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::model::{EqSettings, PlaybackState, PlayerSnapshot, SpatialSettings, Track};

use super::engine::{
    AudioEngine as BlockingAudioEngine, OutputDeviceInfo, PlayerCommand, PlayerEvent,
};

const REQUEST_QUEUE_CAPACITY: usize = 128;
const EVENT_QUEUE_CAPACITY: usize = 256;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(25);
const STRUCTURAL_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);
const NO_STATE_OVERRIDE: u8 = u8::MAX;

/// UI-facing audio facade.
///
/// The decoder/output engine owns blocking locks internally. None of those locks are ever touched
/// from GPUI after construction: commands cross a bounded non-blocking mailbox, hot playback
/// progress is published through atomics, and structural snapshots are double-buffered by a
/// dedicated bridge thread. A stalled decoder therefore cannot stall input, hover, layout or paint
/// on the application thread.
pub struct AudioEngine {
    request_tx: Sender<EngineRequest>,
    event_rx: Receiver<PlayerEvent>,
    snapshot: Arc<SnapshotCache>,
    running: Arc<AtomicBool>,
}

enum EngineRequest {
    Command(PlayerCommand),
    RegisterTracks(Vec<Track>),
    Shutdown,
}

struct SnapshotCache {
    slots: [RwLock<PlayerSnapshot>; 2],
    active: AtomicUsize,
    state: AtomicU8,
    position_ms: AtomicU64,
    duration_ms: AtomicU64,
    state_override: AtomicU8,
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
        }
    }

    fn store(&self, snapshot: PlayerSnapshot) {
        self.store_progress(snapshot.state, snapshot.position_ms, snapshot.duration_ms);

        // PlayerSnapshot::queue is Arc-backed, so structural snapshots share the decoder queue
        // without copying the entire TrackId array on every bridge refresh.
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
    }

    fn snapshot(&self) -> PlayerSnapshot {
        let current = self.active.load(Ordering::Acquire) & 1;
        let other = 1 - current;
        let mut snapshot = self.slots[current]
            .try_read()
            .map(|slot| slot.clone())
            .or_else(|_| self.slots[other].try_read().map(|slot| slot.clone()))
            .unwrap_or_default();

        snapshot.state = self.visible_state();
        snapshot.position_ms = self.position_ms.load(Ordering::Acquire);
        snapshot.duration_ms = self.duration_ms.load(Ordering::Acquire);
        snapshot
    }

    fn progress(&self) -> (PlaybackState, u64, u64) {
        (
            self.visible_state(),
            self.position_ms.load(Ordering::Acquire),
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

    fn set_optimistic_state(&self, state: PlaybackState) {
        self.state_override
            .store(encode_state(state), Ordering::Release);
    }

    #[cfg(test)]
    fn optimistic_command(&self, command: &PlayerCommand) {
        if let Some(state) = Self::optimistic_state(command) {
            self.set_optimistic_state(state);
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
        let (event_tx, event_rx) = bounded::<PlayerEvent>(EVENT_QUEUE_CAPACITY);
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
                if init_tx.send(Ok(())).is_err() {
                    return;
                }
                run_bridge(
                    engine,
                    request_rx,
                    event_tx,
                    worker_snapshot,
                    worker_running,
                );
            })
            .context("创建音频 UI 桥接线程失败")?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                request_tx,
                event_rx,
                snapshot,
                running,
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
        // Registration can include thousands of Track values. Moving the Vec into a bounded
        // mailbox is O(1); the engine-side HashMap mutation happens only on the bridge thread.
        let _ = self
            .request_tx
            .try_send(EngineRequest::RegisterTracks(tracks));
    }

    pub fn try_send(&self, command: PlayerCommand) -> bool {
        // Extract only the tiny optimistic state before moving the command. PlayerCommand queue
        // payloads are Arc-backed, so cloning/transporting queue state never deep-copies TrackIds.
        let optimistic_state = SnapshotCache::optimistic_state(&command);
        match self.request_tx.try_send(EngineRequest::Command(command)) {
            Ok(()) => {
                if let Some(state) = optimistic_state {
                    self.snapshot.set_optimistic_state(state);
                }
                true
            }
            Err(_) => false,
        }
    }

    pub fn drain_events(&self) -> Vec<PlayerEvent> {
        self.event_rx.try_iter().collect()
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot.snapshot()
    }

    pub fn progress(&self) -> (PlaybackState, u64, u64) {
        self.snapshot.progress()
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
    event_tx: Sender<PlayerEvent>,
    snapshot: Arc<SnapshotCache>,
    running: Arc<AtomicBool>,
) {
    let mut last_progress = Instant::now() - PROGRESS_INTERVAL;
    let mut last_snapshot = Instant::now() - STRUCTURAL_SNAPSHOT_INTERVAL;

    while running.load(Ordering::Acquire) {
        let mut refresh_snapshot = false;
        match request_rx.recv_timeout(Duration::from_millis(4)) {
            Ok(request) => {
                refresh_snapshot |= request_refreshes_snapshot(&request);
                if !apply_request(&engine, request) {
                    break;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        // Drain a bounded burst so transport input cannot starve progress/event publication.
        for _ in 0..32 {
            let Ok(request) = request_rx.try_recv() else {
                break;
            };
            refresh_snapshot |= request_refreshes_snapshot(&request);
            if !apply_request(&engine, request) {
                running.store(false, Ordering::Release);
                break;
            }
        }

        for event in engine.drain_events() {
            if !matches!(event, PlayerEvent::PositionChanged(_)) {
                refresh_snapshot = true;
            }
            let _ = event_tx.try_send(event);
        }

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            let (state, position_ms, duration_ms) = engine.progress();
            snapshot.store_progress(state, position_ms, duration_ms);
            last_progress = Instant::now();
        }

        if refresh_snapshot || last_snapshot.elapsed() >= STRUCTURAL_SNAPSHOT_INTERVAL {
            snapshot.store(engine.snapshot());
            last_snapshot = Instant::now();
        }
    }
}

fn request_refreshes_snapshot(request: &EngineRequest) -> bool {
    match request {
        EngineRequest::Command(
            PlayerCommand::Seek(_) | PlayerCommand::SetEq(_) | PlayerCommand::SetSpatial(_),
        )
        | EngineRequest::RegisterTracks(_)
        | EngineRequest::Shutdown => false,
        EngineRequest::Command(_) => true,
    }
}

fn apply_request(engine: &BlockingAudioEngine, request: EngineRequest) -> bool {
    match request {
        EngineRequest::Command(command) => {
            let _ = engine.try_send(command);
            true
        }
        EngineRequest::RegisterTracks(tracks) => {
            engine.register_tracks(tracks);
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
}
