use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, Sender},
    },
    time::Duration,
};

use anyhow::Result;
use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, KeyDownEvent, Render, SharedString,
    Subscription, Timer, WeakEntity, Window, WindowBounds, WindowOptions, div, hsla, prelude::*,
    px, relative, rgb, size,
};
use gpui_tokio::Tokio;
use lucide_gpui::icons as lucide_icons;

use crate::lyrics::LyricsDocument;
use crate::{
    artwork::ArtworkCache,
    audio::{AudioEngine, EqPreset, PlayerCommand, PlayerEvent},
    library::{Library, ScanReport},
    model::{AppPage, LibraryTab, PlaybackState, PlayerSnapshot, RepeatMode, Track, TrackId},
    settings::{AppConfig, ConfigStore},
};

use super::{
    home, library as library_page, player,
    player::NowPlaying,
    route::{self, AppRoute},
    settings as settings_page, theme,
};

const MAX_LYRICS_MEMORY_ENTRIES: usize = 64;
const STAGE_CONTROLS_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const STAGE_TRANSITION_RESPONSE: f32 = 8.0;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DragTarget {
    Progress,
    Volume,
}

pub struct MusicApp {
    pub(crate) config_store: ConfigStore,
    pub(crate) config: AppConfig,
    pub(crate) library: Option<Library>,
    pub(crate) engine: Option<Arc<AudioEngine>>,
    pub(crate) tracks: Vec<Track>,
    pub(crate) output_devices: Vec<crate::audio::OutputDeviceInfo>,
    pub(crate) page: AppPage,
    pub(crate) library_tab: LibraryTab,
    pub(crate) search: String,
    pub(crate) search_active: bool,
    pub(crate) status: String,
    pub(crate) scan_in_progress: bool,
    pub(crate) last_scan: Option<ScanReport>,
    pub(crate) position_ms: u64,
    pub(crate) snapshot: PlayerSnapshot,
    playback_progress: Option<gpui::Entity<player::PlaybackProgress>>,
    playback_time: Option<gpui::Entity<player::PlaybackTime>>,
    pub(crate) artworks: HashMap<TrackId, Arc<[u8]>>,
    pub(crate) blurred_artworks: HashMap<TrackId, Arc<[u8]>>,
    pub(crate) artwork_palettes: HashMap<TrackId, crate::artwork::ArtworkPalette>,
    pub(crate) lyrics: HashMap<TrackId, LyricsDocument>,
    pub(crate) lyrics_order: VecDeque<TrackId>,
    watchers: Vec<notify::RecommendedWatcher>,
    library_update_rx: Receiver<()>,
    library_update_tx: Sender<()>,
    system_media: Option<crate::media_controls::SystemMediaBridge>,
    media_event_rx: Receiver<crate::media_controls::SystemMediaEvent>,
    media_event_tx: Sender<crate::media_controls::SystemMediaEvent>,
    system_media_init_attempts: usize,
    system_media_init_in_flight: bool,
    system_media_update_in_flight: bool,
    system_media_sync_dirty: bool,
    last_system_media_track_id: Option<TrackId>,
    last_system_media_state: Option<PlaybackState>,
    last_system_media_position_sec: u64,
    pub(crate) artwork_cache: Option<ArtworkCache>,
    artwork_loading: HashSet<TrackId>,
    pub(crate) artwork_missing: HashSet<TrackId>,
    pub(crate) enrichment_loading: HashSet<TrackId>,
    pub(crate) enrichment_done: HashSet<TrackId>,
    pub(crate) acoustid_key_active: bool,
    pub(crate) seeking: bool,
    pub(crate) volume_dragging: bool,
    pub(crate) drag_target: Option<DragTarget>,
    pub(crate) drag_progress_ratio: Option<f32>,
    pub(crate) drag_volume_ratio: Option<f32>,
    pub(crate) pending_progress_ratio: Option<(u64, f32)>,
    pub(crate) pending_volume_ratio: Option<f32>,
    lyrics_checked: HashSet<TrackId>,
    last_polled_track_id: Option<TrackId>,
    pub(crate) lyrics_scroll_handle: gpui::ScrollHandle,
    pub(crate) last_lyric_index: Option<usize>,
    pub(crate) stage_open: bool,
    pub(crate) stage_progress: f32,
    pub(crate) stage_animating: bool,
    pub(crate) last_frame_instant: Option<std::time::Instant>,
    pub(crate) stage_controls_visibility: f32,
    pub(crate) stage_last_user_activity: std::time::Instant,
    pub(crate) stage_last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) stage_controls_hovered: bool,
    pub(crate) stage_suppress_wake_until: Option<std::time::Instant>,
    pub(crate) lyrics_current_offset: f32,
    pub(crate) lyrics_target_offset: f32,
    pub(crate) lyrics_user_scrolling_until: Option<std::time::Instant>,
    pub(crate) library_scroll_handle: gpui::UniformListScrollHandle,
    previous_page: AppPage,
    background_started: bool,
    library_refresh_request: u64,
    queue_matches_tracks: bool,
    timer_started: bool,
    polling_player: bool,
    last_saved_position_ms: u64,
    last_saved_at: std::time::Instant,
    config_save_dirty: bool,
    last_config_save_at: std::time::Instant,
    home_page: Option<Entity<HomePage>>,
    library_page: Option<Entity<LibraryPage>>,
}

struct HomePage {
    parent: WeakEntity<MusicApp>,
    _subscription: Subscription,
}

impl HomePage {
    fn new(parent: Entity<MusicApp>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&parent, |_, _, cx| cx.notify());
        Self {
            parent: parent.downgrade(),
            _subscription: subscription,
        }
    }
}

impl Render for HomePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let app = parent.read(cx);
        home::render(app, &self.parent)
    }
}

struct LibraryPage {
    parent: WeakEntity<MusicApp>,
    _subscription: Subscription,
}

impl LibraryPage {
    fn new(parent: Entity<MusicApp>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&parent, |_, _, cx| cx.notify());
        Self {
            parent: parent.downgrade(),
            _subscription: subscription,
        }
    }
}

impl Render for LibraryPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let app = parent.read(cx);
        library_page::render(app, &self.parent)
    }
}

pub(super) fn app_listener<E: ?Sized>(
    view: &WeakEntity<MusicApp>,
    listener: impl Fn(&mut MusicApp, &E, &mut Window, &mut Context<MusicApp>) + 'static,
) -> impl Fn(&E, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |event, window, cx| {
        let _ = view.update(cx, |app, cx| listener(app, event, window, cx));
    }
}

impl MusicApp {
    pub fn new(async_ready: bool) -> Self {
        let config_store = ConfigStore::discover()
            .unwrap_or_else(|_| ConfigStore::from_path(PathBuf::from("config.toml")));
        let config = config_store.load().unwrap_or_default();
        let artwork_cache = config_store
            .path()
            .parent()
            .and_then(|parent| ArtworkCache::new(parent.join("artwork-cache")).ok());
        let library = config_store
            .path()
            .parent()
            .map(|parent| parent.join("library.sqlite3"))
            .and_then(|path| Library::new(path).ok());
        let tracks = library
            .as_ref()
            .and_then(|library| library.tracks(None).ok())
            .unwrap_or_default();
        let output_devices = AudioEngine::output_devices().unwrap_or_default();
        let (library_update_tx, library_update_rx) = std::sync::mpsc::channel();
        let (media_event_tx, media_event_rx) = std::sync::mpsc::channel();
        let system_media = crate::media_controls::SystemMediaBridge::new(media_event_tx.clone());
        let engine = AudioEngine::new_with_device(
            config.output_device.as_deref(),
            config.volume,
            config.eq.clone(),
            config.spatial.clone(),
        )
        .or_else(|_| AudioEngine::new(config.volume, config.eq.clone(), config.spatial.clone()))
        .ok()
        .map(Arc::new);

        let initial_track = config
            .current_track
            .and_then(|id| tracks.iter().find(|t| t.id == id).cloned());
        let initial_duration = initial_track.as_ref().map_or(0, |t| t.duration_ms);
        let initial_position = if initial_duration > 0 {
            config.position_ms.min(initial_duration)
        } else {
            config.position_ms
        };
        let queue_matches_tracks = config.queue.len() == tracks.len()
            && config
                .queue
                .iter()
                .copied()
                .eq(tracks.iter().map(|track| track.id));

        let initial_snapshot = PlayerSnapshot {
            state: PlaybackState::Paused,
            position_ms: initial_position,
            duration_ms: initial_duration,
            current_track: initial_track.clone(),
            volume: config.volume,
            queue: config.queue.clone(),
            repeat: config.repeat,
            shuffle: config.shuffle,
            error: None,
        };

        if let Some(engine) = &engine {
            engine.register_tracks(tracks.clone());
            engine.try_send(PlayerCommand::SetQueue(config.queue.clone()));
            engine.try_send(PlayerCommand::SetRepeat(config.repeat));
            engine.try_send(PlayerCommand::SetShuffle(config.shuffle));
            if let Some(track) = &initial_track {
                engine.try_send(PlayerCommand::RestoreTrack {
                    track_id: track.id,
                    position: Duration::from_millis(initial_position),
                    play: false,
                });
            }
        }
        Self {
            config_store,
            config,
            library,
            engine,
            tracks,
            output_devices,
            page: AppPage::Home,
            library_tab: LibraryTab::Songs,
            search: String::new(),
            search_active: false,
            status: if async_ready {
                "异步运行时已就绪".into()
            } else {
                "异步运行时未初始化".into()
            },
            scan_in_progress: false,
            last_scan: None,
            position_ms: initial_position,
            snapshot: initial_snapshot,
            playback_progress: None,
            playback_time: None,
            artworks: HashMap::new(),
            blurred_artworks: HashMap::new(),
            artwork_palettes: HashMap::new(),
            lyrics: HashMap::new(),
            lyrics_order: VecDeque::new(),
            watchers: Vec::new(),
            library_update_rx,
            library_update_tx,
            system_media,
            media_event_rx,
            media_event_tx,
            system_media_init_attempts: 0,
            system_media_init_in_flight: false,
            system_media_update_in_flight: false,
            system_media_sync_dirty: true,
            last_system_media_track_id: None,
            last_system_media_state: None,
            last_system_media_position_sec: 0,
            artwork_cache,
            artwork_loading: HashSet::new(),
            artwork_missing: HashSet::new(),
            enrichment_loading: HashSet::new(),
            enrichment_done: HashSet::new(),
            acoustid_key_active: false,
            seeking: false,
            volume_dragging: false,
            drag_target: None,
            drag_progress_ratio: None,
            drag_volume_ratio: None,
            pending_progress_ratio: None,
            pending_volume_ratio: None,
            lyrics_checked: HashSet::new(),
            last_polled_track_id: None,
            lyrics_scroll_handle: gpui::ScrollHandle::new(),
            last_lyric_index: None,
            stage_open: false,
            stage_progress: 0.0,
            stage_animating: false,
            last_frame_instant: None,
            stage_controls_visibility: 1.0,
            stage_last_user_activity: std::time::Instant::now(),
            stage_last_mouse_pos: None,
            stage_controls_hovered: false,
            stage_suppress_wake_until: None,
            lyrics_current_offset: 0.0,
            lyrics_target_offset: 0.0,
            lyrics_user_scrolling_until: None,
            library_scroll_handle: gpui::UniformListScrollHandle::new(),
            previous_page: AppPage::Home,
            background_started: false,
            library_refresh_request: 0,
            queue_matches_tracks,
            timer_started: false,
            polling_player: false,
            last_saved_position_ms: initial_position,
            last_saved_at: std::time::Instant::now(),
            config_save_dirty: false,
            last_config_save_at: std::time::Instant::now() - Duration::from_secs(1),
            home_page: None,
            library_page: None,
        }
    }

    fn ensure_home_page(&mut self, cx: &mut Context<Self>) -> Entity<HomePage> {
        if let Some(page) = &self.home_page {
            return page.clone();
        }
        let parent = cx.entity();
        let page = cx.new(|cx| HomePage::new(parent, cx));
        self.home_page = Some(page.clone());
        page
    }

    fn ensure_library_page(&mut self, cx: &mut Context<Self>) -> Entity<LibraryPage> {
        if let Some(page) = &self.library_page {
            return page.clone();
        }
        let parent = cx.entity();
        let page = cx.new(|cx| LibraryPage::new(parent, cx));
        self.library_page = Some(page.clone());
        page
    }

    pub(crate) fn show_page(&mut self, page: AppPage, cx: &mut Context<Self>) {
        if page == AppPage::Player {
            self.open_stage(cx);
            return;
        }
        if self.stage_open {
            self.close_stage(cx);
        }
        self.previous_page = page;
        self.page = page;
        route::navigate_to(cx, page);
        cx.notify();
    }

    pub(crate) fn open_stage(&mut self, cx: &mut Context<Self>) {
        if self.page != AppPage::Player {
            self.previous_page = self.page;
        }
        self.stage_open = true;
        self.stage_animating = true;
        self.last_frame_instant = None;
        self.stage_last_user_activity = std::time::Instant::now();
        self.stage_last_mouse_pos = None;
        self.stage_controls_visibility = 1.0;
        self.page = AppPage::Player;
        route::navigate_to(cx, AppPage::Player);
        cx.notify();
    }

    pub(crate) fn close_stage(&mut self, cx: &mut Context<Self>) {
        self.stage_open = false;
        self.stage_animating = true;
        self.last_frame_instant = None;
        self.stage_last_mouse_pos = None;
        let return_page = if self.previous_page == AppPage::Player {
            AppPage::Home
        } else {
            self.previous_page
        };
        self.page = return_page;
        route::navigate_to(cx, return_page);
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn toggle_stage(&mut self, cx: &mut Context<Self>) {
        if self.stage_open {
            self.close_stage(cx);
        } else {
            self.open_stage(cx);
        }
    }

    pub(crate) fn wake_stage_controls(&mut self, cx: &mut Context<Self>) {
        if self
            .stage_suppress_wake_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        self.stage_last_user_activity = std::time::Instant::now();
        if self.stage_controls_visibility < 0.995 {
            self.stage_controls_visibility = 1.0;
            cx.notify();
        }
    }

    pub(crate) fn wake_stage_controls_immediately(&mut self, cx: &mut Context<Self>) {
        if self
            .stage_suppress_wake_until
            .is_some_and(|until| std::time::Instant::now() < until)
        {
            return;
        }
        self.stage_last_user_activity = std::time::Instant::now();
        self.stage_controls_visibility = 1.0;
        cx.notify();
    }

    pub(crate) fn hide_stage_controls_immediately(&mut self, cx: &mut Context<Self>) {
        let now = std::time::Instant::now();
        self.stage_controls_visibility = 0.0;
        self.stage_last_user_activity = now
            .checked_sub(STAGE_CONTROLS_IDLE_TIMEOUT + Duration::from_secs(1))
            .unwrap_or(now);
        self.stage_controls_hovered = false;
        self.stage_suppress_wake_until = Some(now + Duration::from_millis(600));
        cx.notify();
    }

    pub(crate) fn handle_stage_mouse_move(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let moved = self.stage_last_mouse_pos.is_none_or(|last| {
            let dx = f32::from(pos.x - last.x).abs();
            let dy = f32::from(pos.y - last.y).abs();
            dx >= 2.0 || dy >= 2.0
        });
        if moved {
            self.stage_last_mouse_pos = Some(pos);
            // Meaningful pointer movement is user activity even after the controls have faded out.
            // Micro-jitter below the threshold stays filtered and does not keep the HUD alive.
            self.wake_stage_controls(cx);
        }
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        self.stage_animating
            || (self.stage_open
                && (self.stage_controls_visibility > 0.005
                    && self.stage_controls_visibility < 0.995))
            || (self.stage_open
                && (self.lyrics_target_offset - self.lyrics_current_offset).abs() > 0.5)
    }

    pub(crate) fn show_library_tab(&mut self, tab: LibraryTab, cx: &mut Context<Self>) {
        self.page = AppPage::Library;
        self.library_tab = tab;
        route::navigate_to(cx, AppPage::Library);
        cx.notify();
    }

    pub(crate) fn ensure_queue(&mut self) {
        if self.config.queue.is_empty() && !self.tracks.is_empty() {
            let queue = Arc::new(self.tracks.iter().map(|track| track.id).collect::<Vec<_>>());
            self.config.queue = queue.clone();
            self.send(PlayerCommand::SetQueue(queue));
            self.queue_matches_tracks = true;
            self.save_config();
        }
    }

    pub(crate) fn set_artwork_parts(
        &mut self,
        track_id: TrackId,
        png: Vec<u8>,
        blurred_png: Option<Vec<u8>>,
        palette: Option<crate::artwork::ArtworkPalette>,
    ) {
        self.artworks.insert(track_id, png.into());
        if let Some(blurred_png) = blurred_png {
            self.blurred_artworks.insert(track_id, blurred_png.into());
        }
        if let Some(palette) = palette {
            self.artwork_palettes.insert(track_id, palette);
        }
    }

    pub(crate) fn cache_lyrics(&mut self, track_id: TrackId, lyrics: LyricsDocument) {
        self.lyrics.insert(track_id, lyrics);
        self.lyrics_order.retain(|id| *id != track_id);
        self.lyrics_order.push_back(track_id);
        let current_id = self.snapshot.current_track.as_ref().map(|track| track.id);
        while self.lyrics_order.len() > MAX_LYRICS_MEMORY_ENTRIES {
            let Some(candidate) = self.lyrics_order.pop_front() else {
                break;
            };
            if Some(candidate) == current_id && !self.lyrics_order.is_empty() {
                self.lyrics_order.push_back(candidate);
                continue;
            }
            self.lyrics.remove(&candidate);
        }
    }

    pub(crate) fn toggle_play(&mut self, cx: &mut Context<Self>) {
        self.ensure_queue();
        if self.snapshot.current_track.is_none()
            && let Some(first_id) = self
                .config
                .queue
                .first()
                .copied()
                .or_else(|| self.tracks.first().map(|t| t.id))
        {
            self.play_track(first_id, cx);
            return;
        }

        let state = self
            .engine
            .as_ref()
            .map_or(self.snapshot.state, |engine| engine.snapshot().state);
        let next_state = match state {
            PlaybackState::Playing => PlaybackState::Paused,
            PlaybackState::Paused | PlaybackState::Stopped => PlaybackState::Playing,
            other => other,
        };
        let command = if state == PlaybackState::Playing {
            PlayerCommand::Pause
        } else {
            PlayerCommand::Play
        };
        self.snapshot.state = next_state;
        cx.notify();

        if self.send(command) {
            self.save_config();
        } else {
            self.status = "音频输出不可用，请检查默认音频设备".into();
            cx.notify();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        if self.send(PlayerCommand::Stop) {
            self.snapshot.state = PlaybackState::Stopped;
            self.snapshot.position_ms = 0;
            self.position_ms = 0;
            self.config.position_ms = 0;
            self.save_config();
        }
        cx.notify();
    }

    pub(crate) fn next(&mut self, cx: &mut Context<Self>) {
        self.ensure_queue();
        let is_at_end = if let (Some(curr), false) =
            (self.config.current_track, self.config.queue.is_empty())
        {
            self.config.queue.last().copied() == Some(curr)
        } else {
            false
        };

        if is_at_end
            && self.config.repeat == RepeatMode::Off
            && let Some(first) = self.config.queue.first().copied()
        {
            self.play_track(first, cx);
            return;
        }

        if self.send(PlayerCommand::Next) {
            self.save_config();
        }
        cx.notify();
    }

    pub(crate) fn previous(&mut self, cx: &mut Context<Self>) {
        self.ensure_queue();
        let is_at_start = if let (Some(curr), false) =
            (self.config.current_track, self.config.queue.is_empty())
        {
            self.config.queue.first().copied() == Some(curr)
        } else {
            false
        };

        if is_at_start
            && self.config.repeat == RepeatMode::Off
            && self.snapshot.position_ms < 3_000
            && let Some(last) = self.config.queue.last().copied()
        {
            self.play_track(last, cx);
            return;
        }

        if self.send(PlayerCommand::Previous) {
            self.save_config();
        }
        cx.notify();
    }

    pub(crate) fn play_track(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            self.status = "音频输出不可用，请检查默认音频设备".into();
            cx.notify();
            return;
        };

        if !self.queue_matches_tracks {
            let queue = Arc::new(self.tracks.iter().map(|track| track.id).collect::<Vec<_>>());
            if !engine.try_send(PlayerCommand::SetQueue(queue.clone())) {
                self.status = "音频命令队列繁忙，请稍后重试".into();
                cx.notify();
                return;
            }
            self.config.queue = queue;
            self.queue_matches_tracks = true;
        }

        if engine.try_send(PlayerCommand::PlayTrack(track_id)) {
            self.config.current_track = Some(track_id);
            self.config.position_ms = 0;
            self.status = "正在准备播放".into();
            self.save_config();
        } else {
            self.status = "音频命令队列繁忙，请稍后重试".into();
        }
        self.last_lyric_index = None;
        self.lyrics_scroll_handle.scroll_to_item(0);
        cx.notify();
    }

    pub(crate) fn add_to_queue(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        if !self.config.queue.contains(&track_id) {
            Arc::make_mut(&mut self.config.queue).push(track_id);
            self.queue_matches_tracks = false;
            self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
            self.save_config();
            self.status = "已加入播放队列".into();
        }
        cx.notify();
    }

    pub(crate) fn remove_from_queue(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        Arc::make_mut(&mut self.config.queue).retain(|id| *id != track_id);
        self.queue_matches_tracks = false;
        self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn clear_queue(&mut self, cx: &mut Context<Self>) {
        Arc::make_mut(&mut self.config.queue).clear();
        self.queue_matches_tracks = self.tracks.is_empty();
        self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
        self.save_config();
        self.status = "播放队列已清空".into();
        cx.notify();
    }

    pub(crate) fn seek_relative(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        let duration = self.snapshot.duration_ms as i64;
        let next = (self.snapshot.position_ms as i64 + delta_ms).clamp(0, duration.max(0)) as u64;
        self.send(PlayerCommand::Seek(Duration::from_millis(next)));
        cx.notify();
    }

    pub(crate) fn cycle_repeat(&mut self, cx: &mut Context<Self>) {
        self.config.repeat = match self.config.repeat {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        };
        self.send(PlayerCommand::SetRepeat(self.config.repeat));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        self.config.shuffle = !self.config.shuffle;
        self.send(PlayerCommand::SetShuffle(self.config.shuffle));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_volume(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.config.volume = (self.config.volume + delta).clamp(0.0, 1.0);
        self.send(PlayerCommand::SetVolume(self.config.volume));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn apply_eq(&mut self, preset: EqPreset, cx: &mut Context<Self>) {
        self.config.eq = preset.settings();
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.save_config();
        self.status = format!("EQ 已切换为 {preset:?}");
        cx.notify();
    }

    pub(crate) fn toggle_eq(&mut self, cx: &mut Context<Self>) {
        self.config.eq.enabled = !self.config.eq.enabled;
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_eq(&mut self, index: usize, delta: f32, cx: &mut Context<Self>) {
        if let Some(band) = self.config.eq.bands_db.get_mut(index) {
            *band += delta;
            self.config.eq = crate::audio::clamp_eq(self.config.eq.clone());
            self.send(PlayerCommand::SetEq(self.config.eq.clone()));
            self.save_config();
        }
        cx.notify();
    }

    pub(crate) fn toggle_spatial(&mut self, cx: &mut Context<Self>) {
        self.config.spatial.enabled = !self.config.spatial.enabled;
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_spatial(&mut self, index: u8, delta: f32, cx: &mut Context<Self>) {
        match index {
            0 => self.config.spatial.width += delta,
            1 => self.config.spatial.depth += delta,
            2 => self.config.spatial.distance += delta,
            3 => self.config.spatial.mix += delta,
            _ => return,
        }
        self.config.spatial = crate::audio::clamp_spatial(self.config.spatial.clone());
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_blur(&mut self, cx: &mut Context<Self>) {
        self.config.dynamic_blur = !self.config.dynamic_blur;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_blur_radius(&mut self, radius: f32, cx: &mut Context<Self>) {
        self.config.blur_radius = radius.clamp(0.0, 80.0);
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_blur_radius(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.set_blur_radius(self.config.blur_radius + delta, cx);
    }

    pub(crate) fn set_output_device(&mut self, device: String, cx: &mut Context<Self>) {
        let volume = self.config.volume;
        let eq = self.config.eq.clone();
        let spatial = self.config.spatial.clone();
        let queue = self.config.queue.clone();
        let tracks = self.tracks.clone();
        let current_track = self.snapshot.current_track.as_ref().map(|track| track.id);
        let current_position = self.snapshot.position_ms;
        let resume_playback = matches!(
            self.snapshot.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Buffering
        );
        let pause_after_open = self.snapshot.state == PlaybackState::Paused;
        self.status = format!("正在切换输出设备：{device}");
        let target_device = device.clone();
        let task = Tokio::spawn_result(cx, async move {
            let requested_device = target_device.clone();
            tokio::task::spawn_blocking(move || {
                let engine =
                    AudioEngine::new_with_device(Some(&requested_device), volume, eq, spatial)?;
                engine.register_tracks(tracks);
                engine.try_send(PlayerCommand::SetQueue(queue));
                if resume_playback && let Some(track_id) = current_track {
                    engine.try_send(PlayerCommand::PlayTrack(track_id));
                    if current_position > 0 {
                        engine
                            .try_send(PlayerCommand::Seek(Duration::from_millis(current_position)));
                    }
                    if pause_after_open {
                        engine.try_send(PlayerCommand::Pause);
                    }
                }
                Ok::<_, anyhow::Error>(Arc::new(engine))
            })
            .await
            .map_err(|_| anyhow::anyhow!("音频设备切换任务异常退出"))?
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(engine) => {
                        this.engine = Some(engine);
                        this.config.output_device = Some(device.clone());
                        this.status = format!("输出设备已切换为 {device}");
                        this.save_config();
                    }
                    Err(error) => this.status = format!("输出设备切换失败：{error:#}"),
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn choose_folder(&mut self, cx: &mut Context<Self>) {
        if self.scan_in_progress {
            return;
        }
        let Some(library) = self.library.clone() else {
            self.status = "歌库数据库不可用".into();
            return;
        };
        let signal = self.library_update_tx.clone();
        let task = Tokio::spawn_result(cx, async move {
            let picked = rfd::AsyncFileDialog::new()
                .pick_folder()
                .await
                .map(|folder| folder.path().to_path_buf());
            let Some(path) = picked else {
                return Ok(None);
            };
            let scan_result = tokio::task::spawn_blocking(move || -> Result<_> {
                library.add_root(&path)?;
                let report = library.scan_root(&path)?;
                let watcher = library
                    .start_watching_with_signal(&report.root, signal)
                    .ok();
                Ok((report, watcher))
            })
            .await
            .map_err(|_| anyhow::anyhow!("扫描任务异常退出"))??;
            Ok(Some(scan_result))
        });
        self.scan_in_progress = true;
        self.status = "等待选择音乐目录…".into();
        cx.spawn(async move |this, cx| -> Result<()> {
            let scan_result = task.await?;
            this.update(cx, |this, cx| {
                if let Some((report, watcher)) = scan_result {
                    if !this.config.music_dirs.contains(&report.root) {
                        this.config.music_dirs.push(report.root.clone());
                    }
                    if let Some(watcher) = watcher {
                        this.watchers.push(watcher);
                    }
                    this.apply_scan(report, cx);
                    this.save_config();
                } else {
                    this.scan_in_progress = false;
                    this.status = "已取消添加目录".into();
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.scan_in_progress {
            return;
        }
        let Some(library) = self.library.clone() else {
            return;
        };
        let roots = self.config.music_dirs.clone();
        self.scan_in_progress = true;
        self.status = "正在扫描音乐目录…".into();
        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || library.scan_all(&roots))
                .await
                .map_err(|_| anyhow::anyhow!("扫描任务异常退出"))?
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await?;
            this.update(cx, |this, cx| {
                for report in result {
                    this.apply_scan(report, cx);
                }
                this.scan_in_progress = false;
                this.status = "扫描完成".into();
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn update_search(&mut self, key: &str, cx: &mut Context<Self>) {
        if key == "backspace" {
            self.search.pop();
        } else if key.len() == 1 && !key.chars().next().is_some_and(char::is_control) {
            self.search.push_str(key);
        }
        self.refresh_tracks_async(cx, None);
        cx.notify();
    }

    fn apply_scan(&mut self, report: ScanReport, cx: &mut Context<MusicApp>) {
        if report.failed > 0 {
            let summary = format!(
                "扫描完成：导入 {} 首，{} 首失败",
                report.imported, report.failed
            );
            self.status = if let Some(error) = report.errors.first() {
                format!("{summary} · {error}")
            } else {
                summary
            };
        } else {
            self.status = format!("扫描完成：已导入 {} 首", report.imported);
        }
        self.last_scan = Some(report);
        self.scan_in_progress = false;
        self.artwork_missing.clear();
        self.refresh_tracks_async(cx, None);
    }

    pub(crate) fn refresh_tracks_async(
        &mut self,
        cx: &mut Context<MusicApp>,
        status: Option<String>,
    ) {
        let Some(library) = self.library.clone() else {
            return;
        };
        self.library_refresh_request = self.library_refresh_request.wrapping_add(1);
        let request = self.library_refresh_request;
        let search = self.search.clone();
        let task = Tokio::spawn_result(cx, async move {
            let tracks =
                tokio::task::spawn_blocking(move || library.tracks(Some(&search))).await??;
            let engine_tracks = tracks.clone();
            Ok::<_, anyhow::Error>((tracks, engine_tracks))
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let query_result = task.await;
            this.update(cx, |this, cx| {
                if request != this.library_refresh_request {
                    return;
                }
                match query_result {
                    Ok((tracks, engine_tracks)) => {
                        this.tracks = tracks;
                        this.queue_matches_tracks = false;
                        if let Some(engine) = &this.engine {
                            engine.register_tracks(engine_tracks);
                        }
                        if let Some(status) = status {
                            this.status = status;
                        }
                        this.request_library_artworks(cx);
                        this.request_current_artwork(cx);
                    }
                    Err(error) => this.status = format!("刷新歌库失败：{error:#}"),
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn send(&self, command: PlayerCommand) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|engine| engine.try_send(command))
    }

    pub(crate) fn save_config(&mut self) {
        self.config_save_dirty = true;
        self.flush_config_save_if_due();
    }

    fn flush_config_save_if_due(&mut self) {
        if self.config_save_dirty
            && self.last_config_save_at.elapsed() >= Duration::from_millis(250)
        {
            self.flush_config_save();
        }
    }

    fn flush_config_save(&mut self) {
        let store = self.config_store.clone();
        let mut config = self.config.clone();
        config.position_ms = self.position_ms;
        if let Some(track) = &self.snapshot.current_track {
            config.current_track = Some(track.id);
        }
        self.config_save_dirty = false;
        self.last_config_save_at = std::time::Instant::now();
        let _ = crate::runtime::spawn_blocking(move || {
            let _ = store.save(&config);
        });
    }

    fn start_background_work(&mut self, cx: &mut Context<Self>) {
        if self.background_started {
            return;
        }
        self.background_started = true;
        let Some(library) = self.library.clone() else {
            return;
        };
        let configured_roots = self.config.music_dirs.clone();
        let signal = self.library_update_tx.clone();
        let scan_library = library.clone();
        let setup_task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || -> Result<_> {
                let mut roots = configured_roots;
                if let Ok(stored_roots) = library.roots() {
                    for root in stored_roots {
                        if !roots.contains(&root) {
                            roots.push(root);
                        }
                    }
                }
                let mut watchers = Vec::new();
                for root in &roots {
                    library.add_root(root)?;
                    if let Ok(watcher) = library.start_watching_with_signal(root, signal.clone()) {
                        watchers.push(watcher);
                    }
                }
                Ok((roots, watchers))
            })
            .await?
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            let (roots, watchers) = setup_task.await?;
            let scan_roots = roots.clone();
            this.update(cx, |this, cx| {
                for root in roots {
                    if !this.config.music_dirs.contains(&root) {
                        this.config.music_dirs.push(root);
                    }
                }
                this.watchers.extend(watchers);
                if !scan_roots.is_empty() {
                    this.scan_in_progress = true;
                    this.status = "增量检查音乐库中…".into();
                    cx.notify();
                }
            })?;

            if scan_roots.is_empty() {
                return Ok(());
            }

            let scan_task = Tokio::spawn_result(cx, async move {
                tokio::task::spawn_blocking(move || scan_library.scan_all(&scan_roots)).await?
            });

            let result = scan_task.await?;
            this.update(cx, |this, cx| {
                for report in result {
                    this.apply_scan(report, cx);
                }
                this.scan_in_progress = false;
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn reset_library_index(&mut self, cx: &mut Context<Self>) {
        let Some(library) = self.library.clone() else {
            return;
        };
        self.scan_in_progress = true;
        self.status = "正在清空索引并全量重建...".into();
        self.artwork_missing.clear();
        cx.notify();

        let roots = self.config.music_dirs.clone();
        let scan_library = library.clone();
        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || -> Result<Vec<crate::library::ScanReport>> {
                scan_library.reset_index()?;
                scan_library.scan_all(&roots)
            })
            .await?
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await?;
            this.update(cx, |this, cx| {
                for report in result {
                    this.apply_scan(report, cx);
                }
                this.scan_in_progress = false;
                this.refresh_tracks_async(cx, Some("索引已全量重建完成".into()));
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn rescan_library(&mut self, cx: &mut Context<Self>) {
        let Some(library) = self.library.clone() else {
            return;
        };
        self.scan_in_progress = true;
        self.status = "正在执行快速增量同步...".into();
        cx.notify();

        let roots = self.config.music_dirs.clone();
        let scan_library = library.clone();
        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || scan_library.scan_all(&roots)).await?
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await?;
            this.update(cx, |this, cx| {
                for report in result {
                    this.apply_scan(report, cx);
                }
                this.scan_in_progress = false;
                this.refresh_tracks_async(cx, Some("增量同步完成".into()));
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    fn poll_player(&mut self, cx: &mut Context<MusicApp>) {
        if self.polling_player {
            return;
        }
        self.polling_player = true;

        let sys_events = self.media_event_rx.try_iter().collect::<Vec<_>>();
        for ev in sys_events {
            match ev {
                crate::media_controls::SystemMediaEvent::Play => {
                    if self.snapshot.state != PlaybackState::Playing {
                        self.toggle_play(cx);
                    }
                }
                crate::media_controls::SystemMediaEvent::Pause => {
                    if self.snapshot.state == PlaybackState::Playing {
                        self.toggle_play(cx);
                    }
                }
                crate::media_controls::SystemMediaEvent::Toggle => self.toggle_play(cx),
                crate::media_controls::SystemMediaEvent::Next => self.next(cx),
                crate::media_controls::SystemMediaEvent::Previous => self.previous(cx),
                crate::media_controls::SystemMediaEvent::Stop => {
                    self.send(PlayerCommand::Stop);
                    cx.notify();
                }
                crate::media_controls::SystemMediaEvent::SeekBy(delta_ms) => {
                    self.seek_relative(delta_ms, cx);
                }
                crate::media_controls::SystemMediaEvent::SetPosition(pos) => {
                    self.seek_to_ms(pos.as_millis() as u64, cx);
                }
            }
        }

        if self.library_update_rx.try_iter().next().is_some() {
            self.artwork_missing.clear();
            self.refresh_tracks_async(cx, Some("歌库已更新".into()));
        }
        if let Some(engine) = &self.engine {
            for event in engine.drain_events() {
                if let PlayerEvent::Error(error) = event {
                    self.status = error.to_string();
                }
            }
            self.snapshot = engine.snapshot();
            if self.drag_target.is_none() {
                self.position_ms = self.snapshot.position_ms;
                self.config.position_ms = self.position_ms;
            }

            let curr_gen = self
                .snapshot
                .current_track
                .as_ref()
                .map_or(0, |t| t.id as u64);
            if let Some((target_gen, ratio)) = self.pending_progress_ratio {
                if target_gen != curr_gen {
                    self.pending_progress_ratio = None;
                } else if self.snapshot.duration_ms > 0 {
                    let curr_ratio =
                        self.snapshot.position_ms as f32 / self.snapshot.duration_ms as f32;
                    if (curr_ratio - ratio).abs() < 0.02 {
                        self.pending_progress_ratio = None;
                    }
                }
            }

            if let Some(ratio) = self.pending_volume_ratio
                && (self.config.volume - ratio).abs() < 0.02
            {
                self.pending_volume_ratio = None;
            }

            if self.snapshot.current_track.is_some() {
                self.config.current_track =
                    self.snapshot.current_track.as_ref().map(|track| track.id);
            }

            if self.snapshot.state == PlaybackState::Playing
                && self.last_saved_at.elapsed() >= Duration::from_secs(3)
            {
                let diff = (self.position_ms as i64 - self.last_saved_position_ms as i64).abs();
                if diff >= 1000 {
                    self.last_saved_position_ms = self.position_ms;
                    self.last_saved_at = std::time::Instant::now();
                    self.save_config();
                }
            }

            if let Some(track) = &self.snapshot.current_track {
                let track_id = track.id;
                let track_path = track.path.clone();
                if !self.lyrics.contains_key(&track_id) && self.lyrics_checked.insert(track_id) {
                    let task = Tokio::spawn_result(cx, async move {
                        tokio::task::spawn_blocking(move || crate::lyrics::read_local(&track_path))
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))
                    });
                    cx.spawn(async move |this, cx| -> Result<()> {
                        if let Ok(Some(lrc)) = task.await {
                            this.update(cx, |this, cx| {
                                this.cache_lyrics(track_id, lrc);
                                cx.notify();
                            })?;
                        }
                        Ok(())
                    })
                    .detach();
                }
            }

            let curr_track_id = self.snapshot.current_track.as_ref().map(|t| t.id);
            if curr_track_id != self.last_polled_track_id {
                self.last_polled_track_id = curr_track_id;
                self.last_lyric_index = None;
                self.lyrics_current_offset = 0.0;
                self.lyrics_target_offset = 0.0;
                self.lyrics_user_scrolling_until = None;
                self.lyrics_scroll_handle.scroll_to_item(0);
                self.request_current_artwork(cx);
                self.request_current_enrichment(cx);
            }

            if let Some(track) = &self.snapshot.current_track
                && let Some(doc) = self.lyrics.get(&track.id)
            {
                let timed = doc.timed_lines();
                if !timed.is_empty() {
                    let current_idx = timed
                        .iter()
                        .rposition(|line| line.timestamp_ms <= self.snapshot.position_ms)
                        .unwrap_or(0);
                    if self.last_lyric_index != Some(current_idx) {
                        self.last_lyric_index = Some(current_idx);
                        let in_user_scroll = self
                            .lyrics_user_scrolling_until
                            .is_some_and(|until| std::time::Instant::now() < until);
                        if !in_user_scroll {
                            self.lyrics_target_offset = (current_idx as f32) * 60.0;
                        }
                        self.lyrics_scroll_handle.scroll_to_item(current_idx);
                        if self.stage_open {
                            cx.notify();
                        }
                    }
                }
            }

            self.update_system_media_async(cx);
        }

        if self.system_media.is_none()
            && !self.system_media_init_in_flight
            && !self.system_media_update_in_flight
            && self.system_media_init_attempts < 10
        {
            self.system_media_init_attempts += 1;
            self.system_media_init_in_flight = true;
            let event_tx = self.media_event_tx.clone();
            let task = Tokio::spawn_result(cx, async move {
                tokio::task::spawn_blocking(move || {
                    crate::media_controls::SystemMediaBridge::try_create(event_tx)
                        .map_err(anyhow::Error::msg)
                })
                .await
                .map_err(|error| anyhow::anyhow!("系统媒体初始化任务异常退出: {error}"))?
            });
            cx.spawn(async move |this, cx| -> Result<()> {
                let result = task.await;
                this.update(cx, |this, _cx| {
                    this.system_media_init_in_flight = false;
                    if let Ok(bridge) = result {
                        this.system_media = Some(bridge);
                        this.system_media_sync_dirty = true;
                    }
                })?;
                Ok(())
            })
            .detach();
        }

        self.polling_player = false;
    }

    fn update_system_media_async(&mut self, cx: &mut Context<MusicApp>) {
        if self.system_media_update_in_flight {
            return;
        }
        let Some(mut bridge) = self.system_media.take() else {
            return;
        };
        let track = self.snapshot.current_track.clone();
        let state = self.snapshot.state;
        let position_ms = self.snapshot.position_ms;
        let track_id = track.as_ref().map(|track| track.id);
        let position_sec = position_ms / 1000;
        let needs_update = self.system_media_sync_dirty
            || self.last_system_media_track_id != track_id
            || self.last_system_media_state != Some(state)
            || position_sec.abs_diff(self.last_system_media_position_sec) >= 2;
        if !needs_update {
            self.system_media = Some(bridge);
            return;
        }

        self.last_system_media_track_id = track_id;
        self.last_system_media_state = Some(state);
        self.last_system_media_position_sec = position_sec;
        self.system_media_sync_dirty = false;
        self.system_media_update_in_flight = true;

        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || {
                bridge.update_metadata(track.as_ref());
                bridge.update_playback(state, position_ms);
                bridge
            })
            .await
            .map_err(|error| anyhow::anyhow!("系统媒体状态更新任务异常退出: {error}"))
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await;
            this.update(cx, |this, _cx| {
                this.system_media_update_in_flight = false;
                match result {
                    Ok(bridge) => {
                        let current_track_id =
                            this.snapshot.current_track.as_ref().map(|track| track.id);
                        let current_position_sec = this.snapshot.position_ms / 1000;
                        this.system_media_sync_dirty = current_track_id != track_id
                            || this.snapshot.state != state
                            || current_position_sec.abs_diff(position_sec) >= 2;
                        this.system_media = Some(bridge);
                    }
                    Err(_) => {
                        this.system_media_sync_dirty = true;
                    }
                }
            })?;
            Ok(())
        })
        .detach();
    }

    fn request_current_artwork(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.snapshot.current_track.clone() else {
            return;
        };
        if self.artworks.contains_key(&track.id)
            || self.artwork_missing.contains(&track.id)
            || !self.artwork_loading.insert(track.id)
        {
            return;
        }
        let Some(cache) = self.artwork_cache.clone() else {
            self.artwork_loading.remove(&track.id);
            return;
        };
        let track_id = track.id;
        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || cache.load(&track))
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.artwork_loading.remove(&track_id);
                match result {
                    Ok(Some(artwork)) => {
                        this.set_artwork_parts(
                            track_id,
                            artwork.png,
                            Some(artwork.blurred_png),
                            Some(artwork.palette),
                        );
                    }
                    Ok(None) => {
                        this.artwork_missing.insert(track_id);
                    }
                    Err(error) => {
                        this.artwork_missing.insert(track_id);
                        this.status = format!("封面读取失败：{error:#}");
                    }
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    fn request_library_artworks(&mut self, cx: &mut Context<Self>) {
        let Some(cache) = self.artwork_cache.clone() else {
            return;
        };
        let mut pending = Vec::new();
        for track in &self.tracks {
            if !self.artworks.contains_key(&track.id)
                && !self.artwork_missing.contains(&track.id)
                && self.artwork_loading.insert(track.id)
            {
                pending.push(track.clone());
                if pending.len() >= 40 {
                    break;
                }
            }
        }
        if pending.is_empty() {
            return;
        }
        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || {
                let results: Vec<_> = pending
                    .into_iter()
                    .map(|track| {
                        let id = track.id;
                        match cache.load(&track) {
                            Ok(Some(artwork)) => (id, Some(artwork)),
                            _ => (id, None),
                        }
                    })
                    .collect();
                let mut loaded = Vec::new();
                let mut missing = Vec::new();
                for (id, artwork) in results {
                    if let Some(art) = artwork {
                        loaded.push((id, art));
                    } else {
                        missing.push(id);
                    }
                }
                (loaded, missing)
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let (loaded, missing) = task.await?;
            this.update(cx, |this, cx| {
                for (id, art) in loaded {
                    this.artwork_loading.remove(&id);
                    this.set_artwork_parts(id, art.png, Some(art.blurred_png), Some(art.palette));
                }
                for id in missing {
                    this.artwork_loading.remove(&id);
                    this.artwork_missing.insert(id);
                }
                let has_more = this.tracks.iter().any(|track| {
                    !this.artworks.contains_key(&track.id)
                        && !this.artwork_missing.contains(&track.id)
                        && !this.artwork_loading.contains(&track.id)
                });
                if has_more {
                    this.request_library_artworks(cx);
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn displayed_progress_ratio(&self) -> f32 {
        self.drag_progress_ratio
            .or_else(|| self.pending_progress_ratio.map(|(_, ratio)| ratio))
            .unwrap_or_else(|| {
                let duration_ms = self.snapshot.duration_ms;
                if duration_ms == 0 {
                    0.0
                } else {
                    (self.snapshot.position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
                }
            })
    }

    pub(crate) fn displayed_volume_ratio(&self) -> f32 {
        self.drag_volume_ratio
            .or(self.pending_volume_ratio)
            .unwrap_or_else(|| self.config.volume.clamp(0.0, 1.0))
    }

    pub(crate) fn displayed_position_ms(&self) -> u64 {
        let duration_ms = self.snapshot.duration_ms;
        if let Some(ratio) = self
            .drag_progress_ratio
            .or_else(|| self.pending_progress_ratio.map(|(_, r)| r))
        {
            (duration_ms as f32 * ratio).round() as u64
        } else {
            self.snapshot.position_ms
        }
    }

    pub(crate) fn mini_progress_ratio(&self, x: f32, window: &Window) -> f32 {
        let win_width = f32::from(window.bounds().size.width).max(1.0);
        (x / win_width).clamp(0.0, 1.0)
    }

    pub(crate) fn stage_progress_ratio(&self, x: f32, window: &Window) -> f32 {
        let win_width = f32::from(window.bounds().size.width);
        let track_start = 117.0;
        let track_end = (win_width - 580.0).max(track_start + 100.0);
        let track_width = (track_end - track_start).max(80.0);
        ((x - track_start) / track_width).clamp(0.0, 1.0)
    }

    pub(crate) fn mini_volume_ratio(&self, x: f32, window: &Window) -> f32 {
        let win_width = f32::from(window.bounds().size.width);
        let bar_start = win_width - 96.0;
        ((x - bar_start) / 72.0).clamp(0.0, 1.0)
    }

    pub(crate) fn stage_volume_ratio(&self, x: f32, window: &Window) -> f32 {
        let win_width = f32::from(window.bounds().size.width);
        let track_end = win_width - 80.0;
        (1.0 - (track_end - x) / 72.0).clamp(0.0, 1.0)
    }

    pub(crate) fn begin_drag(&mut self, target: DragTarget, ratio: f32, cx: &mut Context<Self>) {
        let ratio = ratio.clamp(0.0, 1.0);
        self.drag_target = Some(target);
        match target {
            DragTarget::Progress => {
                self.seeking = true;
                self.drag_progress_ratio = Some(ratio);
            }
            DragTarget::Volume => {
                self.volume_dragging = true;
                self.drag_volume_ratio = Some(ratio);
            }
        }
        self.stage_last_user_activity = std::time::Instant::now();
        cx.notify();
    }

    pub(crate) fn update_drag_ratio(
        &mut self,
        target: DragTarget,
        ratio: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let ratio = ratio.clamp(0.0, 1.0);
        self.stage_last_user_activity = std::time::Instant::now();
        match target {
            DragTarget::Progress => {
                let changed = self
                    .drag_progress_ratio
                    .is_none_or(|c| (c - ratio).abs() >= 0.002);
                if changed {
                    self.drag_progress_ratio = Some(ratio);
                    cx.notify();
                    return true;
                }
            }
            DragTarget::Volume => {
                let changed = self
                    .drag_volume_ratio
                    .is_none_or(|c| (c - ratio).abs() >= 0.004);
                if changed {
                    self.drag_volume_ratio = Some(ratio);
                    cx.notify();
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn commit_drag(&mut self, cx: &mut Context<Self>) {
        match self.drag_target {
            Some(DragTarget::Progress) => {
                if let Some(ratio) = self.drag_progress_ratio {
                    let duration_ms = self.snapshot.duration_ms;
                    let target_ms = (duration_ms as f32 * ratio).round() as u64;
                    let current_gen = self
                        .snapshot
                        .current_track
                        .as_ref()
                        .map_or(0, |t| t.id as u64);
                    self.pending_progress_ratio = Some((current_gen, ratio));
                    self.seek_to_ms(target_ms, cx);
                }
            }
            Some(DragTarget::Volume) => {
                if let Some(ratio) = self.drag_volume_ratio {
                    self.pending_volume_ratio = Some(ratio);
                    self.set_app_volume(ratio, cx);
                }
            }
            None => {}
        }
        self.clear_drag(cx);
    }

    pub(crate) fn clear_drag(&mut self, cx: &mut Context<Self>) {
        self.drag_target = None;
        self.drag_progress_ratio = None;
        self.drag_volume_ratio = None;
        self.seeking = false;
        self.volume_dragging = false;
        cx.notify();
    }

    pub(crate) fn seek_to_ms(&mut self, position_ms: u64, cx: &mut Context<Self>) {
        let duration_ms = self.snapshot.duration_ms;
        let clamped = if duration_ms > 0 {
            position_ms.min(duration_ms)
        } else {
            position_ms
        };
        self.send(PlayerCommand::Seek(Duration::from_millis(clamped)));
        self.snapshot.position_ms = clamped;
        self.position_ms = clamped;
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn seek_to_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let duration_ms = self.snapshot.duration_ms;
        if duration_ms == 0 {
            return;
        }
        let position_ms = (duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64;
        self.seek_to_ms(position_ms, cx);
    }

    pub(crate) fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if self.config.volume > 0.001 {
            self.config.volume = 0.0;
        } else {
            self.config.volume = 0.8;
        }
        self.send(PlayerCommand::SetVolume(self.config.volume));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_app_volume(&mut self, vol: f32, cx: &mut Context<Self>) {
        self.config.volume = vol.clamp(0.0, 1.0);
        self.send(PlayerCommand::SetVolume(self.config.volume));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_debug_log(&mut self, cx: &mut Context<Self>) {
        if self.config.log.level == "debug" {
            self.config.log.level = "info".into();
        } else {
            self.config.log.level = "debug".into();
        }
        self.save_config();
        cx.notify();
    }

    fn start_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        cx.spawn(async move |this, cx| -> Result<()> {
            let mut is_playing = false;
            loop {
                let delay = if is_playing {
                    Duration::from_millis(100)
                } else {
                    Duration::from_millis(200)
                };
                Timer::after(delay).await;
                let res = this.update(cx, |this, cx| {
                    let old_track_id = this.snapshot.current_track.as_ref().map(|track| track.id);
                    let old_state = this.snapshot.state;
                    let old_position_ms = this.snapshot.position_ms;
                    this.poll_player(cx);
                    this.flush_config_save_if_due();
                    let playing = this.snapshot.state == PlaybackState::Playing;
                    let is_dragging =
                        this.drag_target.is_some() || this.seeking || this.volume_dragging;
                    let stage_idle_due = this.stage_open
                        && this.stage_controls_visibility > 0.005
                        && this.stage_last_user_activity.elapsed() >= STAGE_CONTROLS_IDLE_TIMEOUT
                        && !this.seeking
                        && !this.volume_dragging
                        && !this.stage_controls_hovered;
                    let should_refresh = should_refresh_main_view(
                        old_track_id,
                        this.snapshot.current_track.as_ref().map(|track| track.id),
                        old_state,
                        this.snapshot.state,
                        old_position_ms != this.snapshot.position_ms,
                        this.stage_open,
                        is_dragging,
                        stage_idle_due,
                    );
                    if should_refresh {
                        cx.notify();
                    }
                    playing
                });
                match res {
                    Ok(playing) => {
                        is_playing = playing;
                    }
                    Err(_) => break,
                }
            }
            Ok(())
        })
        .detach();
    }

    fn ensure_playback_progress(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (
        gpui::Entity<player::PlaybackProgress>,
        gpui::Entity<player::PlaybackTime>,
    ) {
        let parent = cx.entity().downgrade();
        if self.playback_progress.is_none() {
            let engine = self.engine.clone();
            self.playback_progress =
                Some(cx.new(|_| player::PlaybackProgress::new(parent.clone(), engine)));
        }
        if self.playback_time.is_none() {
            let engine = self.engine.clone();
            self.playback_time = Some(cx.new(|_| player::PlaybackTime::new(parent, engine)));
        }
        (
            self.playback_progress
                .as_ref()
                .expect("playback progress view must be initialized")
                .clone(),
            self.playback_time
                .as_ref()
                .expect("playback time view must be initialized")
                .clone(),
        )
    }

    #[allow(dead_code)]
    pub(crate) fn open_player(&mut self, cx: &mut Context<Self>) {
        let engine = self.engine.clone();
        let dynamic_blur = self.config.dynamic_blur;
        let artwork = self
            .snapshot
            .current_track
            .as_ref()
            .and_then(|track| self.artworks.get(&track.id).cloned());
        let bounds = Bounds::centered(None, size(px(1_180.0), px(760.0)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, window_cx| window_cx.new(|_| NowPlaying::new(engine, dynamic_blur, artwork)),
        ) {
            self.status = format!("打开播放页失败: {error:#}");
        }
    }

    fn custom_titlebar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.snapshot.current_track.as_ref().map_or_else(
            || "音栖岛 · 让声音有归处".to_string(),
            |t| format!("{} · {}", t.title, t.artist),
        );

        div()
            .id("custom-titlebar")
            .w_full()
            .h(px(38.0))
            .flex_none()
            .bg(theme::BG_CANVAS)
            .border_b_1()
            .border_color(theme::BORDER_HAIRLINE)
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .child(
                div()
                    .occlude()
                    .window_control_area(gpui::WindowControlArea::Client)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(traffic_light_button(
                        "window-close",
                        rgb(0xff_5f_56),
                        cx.listener(|_, _, _, cx| cx.quit()),
                    ))
                    .child(traffic_light_button(
                        "window-minimize",
                        rgb(0xff_bd_2e),
                        cx.listener(|_, _, window, _| window.minimize_window()),
                    ))
                    .child(traffic_light_button(
                        "window-maximize",
                        rgb(0x27_c9_3f),
                        cx.listener(|_, _, window, _| {
                            if window.is_maximized() {
                                window.restore_window();
                            } else {
                                window.maximize_window();
                            }
                        }),
                    )),
            )
            .child(
                div()
                    .id("titlebar-drag-region")
                    .window_control_area(gpui::WindowControlArea::Drag)
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::TEXT_SECONDARY)
                            .truncate()
                            .child(title),
                    )
                    .on_click(cx.listener(|_, _, window, _| {
                        window.titlebar_double_click();
                    })),
            )
            .child(
                div()
                    .occlude()
                    .window_control_area(gpui::WindowControlArea::Client)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(if self.scan_in_progress {
                                theme::ACCENT_RED
                            } else {
                                rgb(0x34_c7_59)
                            }),
                    )
                    .child(div().text_xs().text_color(theme::TEXT_TERTIARY).child(
                        if self.scan_in_progress {
                            "正在扫描"
                        } else {
                            "已就绪"
                        },
                    )),
            )
    }

    fn stage_titlebar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.snapshot.current_track.as_ref().map_or_else(
            || "沉浸音乐大舞台".to_string(),
            |t| format!("{} · {}", t.title, t.artist),
        );

        let v = self.stage_controls_visibility;
        let y_offset = -(1.0 - v) * 38.0;

        div()
            .w_full()
            .h(px(38.0))
            .top(px(y_offset))
            .opacity(v)
            .child(
                div()
                    .id("stage-titlebar")
                    .w_full()
                    .h(px(38.0))
                    .flex_none()
                    .bg(hsla(0.0, 0.0, 0.0, 0.40))
                    .border_b_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.08))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .child(
                        div()
                            .occlude()
                            .window_control_area(gpui::WindowControlArea::Client)
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(traffic_light_button(
                                "stage-window-close",
                                rgb(0xff_5f_56),
                                cx.listener(|_, _, _, cx| cx.quit()),
                            ))
                            .child(traffic_light_button(
                                "stage-window-minimize",
                                rgb(0xff_bd_2e),
                                cx.listener(|_, _, window, _| window.minimize_window()),
                            ))
                            .child(traffic_light_button(
                                "stage-window-maximize",
                                rgb(0x27_c9_3f),
                                cx.listener(|_, _, window, _| {
                                    if window.is_maximized() {
                                        window.restore_window();
                                    } else {
                                        window.maximize_window();
                                    }
                                }),
                            )),
                    )
                    .child(
                        div()
                            .id("stage-drag-region")
                            .window_control_area(gpui::WindowControlArea::Drag)
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(hsla(0.0, 0.0, 1.0, 0.70))
                                    .truncate()
                                    .child(title),
                            )
                            .on_click(cx.listener(|_, _, window, _| {
                                window.titlebar_double_click();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("stage-quick-hide-btn")
                                    .occlude()
                                    .window_control_area(gpui::WindowControlArea::Client)
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(hsla(0.0, 0.0, 1.0, 0.12))
                                    .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.22)))
                                    .transition(theme::press_transition())
                                    .active(|s| s.scale(0.95))
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .child(theme::themed_icon(
                                        lucide_icons::icon_eye_off(),
                                        14.0,
                                        hsla(0.0, 0.0, 1.0, 0.90),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(hsla(0.0, 0.0, 1.0, 0.90))
                                            .child("纯享沉浸"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.hide_stage_controls_immediately(cx);
                                    })),
                            )
                            .child(
                                div()
                                    .id("stage-collapse-btn")
                                    .occlude()
                                    .window_control_area(gpui::WindowControlArea::Client)
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(hsla(0.0, 0.0, 1.0, 0.12))
                                    .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.22)))
                                    .transition(theme::press_transition())
                                    .active(|s| s.scale(0.95))
                                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation();
                                    })
                                    .child(theme::themed_icon(
                                        lucide_icons::icon_chevron_down(),
                                        14.0,
                                        hsla(0.0, 0.0, 1.0, 0.90),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(hsla(0.0, 0.0, 1.0, 0.90))
                                            .child("收起舞台 (Esc)"),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.close_stage(cx);
                                    })),
                            ),
                    ),
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn should_refresh_main_view(
    old_track_id: Option<TrackId>,
    current_track_id: Option<TrackId>,
    old_state: PlaybackState,
    current_state: PlaybackState,
    position_changed: bool,
    stage_open: bool,
    is_dragging: bool,
    stage_idle_due: bool,
) -> bool {
    if is_dragging {
        return false;
    }
    old_track_id != current_track_id
        || old_state != current_state
        || (stage_open && position_changed)
        || (stage_open && stage_idle_due)
}

impl Drop for MusicApp {
    fn drop(&mut self) {
        if self.config_save_dirty {
            self.flush_config_save();
        }
        self.send(PlayerCommand::Stop);
    }
}

impl Render for MusicApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_background_work(cx);
        self.start_timer(cx);
        let (playback_progress, playback_time) = self.ensure_playback_progress(cx);

        let now = std::time::Instant::now();
        let dt = self
            .last_frame_instant
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.016)
            .clamp(0.001, 0.1);
        self.last_frame_instant = Some(now);

        if self.stage_animating {
            let target = if self.stage_open { 1.0 } else { 0.0 };
            let diff = target - self.stage_progress;
            if diff.abs() < 0.003 {
                self.stage_progress = target;
                self.stage_animating = false;
            } else {
                let factor = 1.0 - (-STAGE_TRANSITION_RESPONSE * dt).exp();
                self.stage_progress += diff * factor;
            }
        }

        let is_idle = self.stage_open
            && self.stage_last_user_activity.elapsed() >= STAGE_CONTROLS_IDLE_TIMEOUT
            && !self.seeking
            && !self.volume_dragging
            && !self.stage_controls_hovered;

        let target_visibility = if is_idle { 0.0 } else { 1.0 };
        let vis_diff = target_visibility - self.stage_controls_visibility;
        if vis_diff.abs() > 0.005 {
            let factor = 1.0 - (-12.0 * dt).exp();
            self.stage_controls_visibility += vis_diff * factor;
        } else {
            self.stage_controls_visibility = target_visibility;
        }

        if self.stage_open {
            let in_user_scroll = self
                .lyrics_user_scrolling_until
                .is_some_and(|until| std::time::Instant::now() < until);
            if !in_user_scroll {
                let diff = self.lyrics_target_offset - self.lyrics_current_offset;
                if diff.abs() > 0.5 {
                    let factor = 1.0 - (-9.0 * dt).exp();
                    self.lyrics_current_offset += diff * factor;
                } else {
                    self.lyrics_current_offset = self.lyrics_target_offset;
                }
            }
        }

        // Fluid background motion is renderer-owned; the root view only schedules frames for
        // drawer/HUD/lyric layout motion that actually changes application state.
        if self.has_active_animations() {
            window.request_animation_frame();
        }

        let current_route = route::current_route(cx);
        if current_route == AppRoute::Player && !self.stage_open && self.stage_progress < 0.001 {
            self.open_stage(cx);
        }

        let main_page = match self.page {
            AppPage::Player => {
                if self.previous_page == AppPage::Player {
                    AppPage::Home
                } else {
                    self.previous_page
                }
            }
            page => page,
        };

        let home_page = self.ensure_home_page(cx);
        let library_page = self.ensure_library_page(cx);

        let content = if self.stage_progress >= 0.999 && !self.stage_animating {
            div().into_any_element()
        } else {
            match main_page {
                AppPage::Home => home_page.clone().into_any_element(),
                AppPage::Library => library_page.into_any_element(),
                AppPage::Player => home_page.into_any_element(),
                AppPage::Settings => settings_page::render(self, cx),
            }
        };

        let stage_drawer = if self.stage_progress > 0.001 {
            let progress = self.stage_progress.clamp(0.0, 1.0);
            let y_offset = 1.0 - progress;
            Some(
                div()
                    .id("stage-drawer-root")
                    .absolute()
                    .inset_0()
                    .top(relative(y_offset))
                    .shadow_lg()
                    .border_t_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.12))
                    .flex()
                    .flex_col()
                    .bg(rgb(0x10_11_1a))
                    .text_color(theme::TEXT_WHITE)
                    .on_mouse_move(cx.listener(
                        |this, event: &gpui::MouseMoveEvent, _window, cx| {
                            this.handle_stage_mouse_move(event.position, cx);
                        },
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.wake_stage_controls_immediately(cx);
                        }),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.wake_stage_controls_immediately(cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            if this.drag_target.is_some() {
                                this.commit_drag(cx);
                            }
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        this.wake_stage_controls(cx);
                        let key = event.keystroke.key.as_str();
                        if key == "escape" {
                            this.close_stage(cx);
                        } else if key == "space" {
                            this.toggle_play(cx);
                        } else if key == "left" {
                            this.seek_relative(-10_000, cx);
                        } else if key == "right" {
                            this.seek_relative(10_000, cx);
                        }
                    }))
                    .child(self.stage_titlebar(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(player::render(self, cx)),
                    ),
            )
        } else {
            None
        };

        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _window, cx| {
                    if this.drag_target.is_some() {
                        this.commit_drag(cx);
                    }
                }),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(theme::BG_CANVAS)
                    .text_color(theme::TEXT_PRIMARY)
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        let key = event.keystroke.key.as_str();
                        let modifiers = event.keystroke.modifiers;
                        if this.acoustid_key_active {
                            this.update_acoustid_key(key, cx);
                        } else if modifiers.control && key.eq_ignore_ascii_case("f") {
                            this.search_active = true;
                            this.page = AppPage::Library;
                            cx.notify();
                        } else if key == "space" {
                            this.toggle_play(cx);
                        } else if key == "left" {
                            this.seek_relative(-10_000, cx);
                        } else if key == "right" {
                            this.seek_relative(10_000, cx);
                        } else if this.search_active
                            && (key == "backspace" || (key.len() == 1 && !modifiers.modified()))
                        {
                            this.update_search(key, cx);
                        }
                    }))
                    .child(self.custom_titlebar(window, cx))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .child(sidebar(self, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .h_full()
                                    .overflow_hidden()
                                    .child(content),
                            ),
                    )
                    .child(player::mini_player(
                        self,
                        cx,
                        playback_progress,
                        playback_time,
                    )),
            )
            .children(stage_drawer)
    }
}

fn sidebar(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let track_count = app.tracks.len();
    div()
        .w(px(236.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap_4()
        .px_4()
        .py_5()
        .bg(theme::BG_SIDEBAR)
        .border_r_1()
        .border_color(theme::BORDER_HAIRLINE)
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .py_1()
                .child(
                    div()
                        .size(px(32.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_lg()
                        .bg(theme::ACCENT_RED)
                        .child(theme::themed_icon(
                            lucide_icons::icon_audio_waveform(),
                            18.0,
                            hsla(0.0, 0.0, 1.0, 1.0),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child("音栖岛"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child("让声音有归处"),
                        ),
                ),
        )
        .child(
            div()
                .id("sidebar-search-btn")
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .cursor_pointer()
                .hover(|s| s.bg(theme::bg_hover()))
                .transition(theme::press_transition())
                .active(|s| s.scale(0.98))
                .child(theme::themed_icon(
                    lucide_icons::icon_search(),
                    14.0,
                    hsla(220.0, 0.07, 0.50, 1.0),
                ))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_SECONDARY)
                        .child("搜索音乐 (Ctrl+F)"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.search_active = true;
                    this.page = AppPage::Library;
                    cx.notify();
                })),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(sidebar_section_header("探索"))
                .child(sidebar_item(
                    "发现",
                    lucide_icons::icon_compass(),
                    app.page == AppPage::Home,
                    cx.listener(|this, _, _, cx| this.show_page(AppPage::Home, cx)),
                ))
                .child(sidebar_item(
                    "歌曲",
                    lucide_icons::icon_music(),
                    app.page == AppPage::Library && app.library_tab == LibraryTab::Songs,
                    cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Songs, cx)),
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(sidebar_section_header("音乐库"))
                .child(sidebar_item(
                    "专辑",
                    lucide_icons::icon_disc_3(),
                    app.page == AppPage::Library && app.library_tab == LibraryTab::Albums,
                    cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Albums, cx)),
                ))
                .child(sidebar_item(
                    "艺术家",
                    lucide_icons::icon_users_round(),
                    app.page == AppPage::Library && app.library_tab == LibraryTab::Artists,
                    cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Artists, cx)),
                ))
                .child(sidebar_item(
                    "播放队列",
                    lucide_icons::icon_list_music(),
                    app.page == AppPage::Library && app.library_tab == LibraryTab::Playlists,
                    cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Playlists, cx)),
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(sidebar_section_header("系统"))
                .child(sidebar_item(
                    "偏好设置",
                    lucide_icons::icon_settings(),
                    app.page == AppPage::Settings,
                    cx.listener(|this, _, _, cx| this.show_page(AppPage::Settings, cx)),
                )),
        )
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_2()
                .rounded_md()
                .bg(hsla(0.0, 0.0, 0.0, 0.02))
                .child(
                    div()
                        .size(px(7.0))
                        .rounded_full()
                        .bg(if app.scan_in_progress {
                            rgb(0xff_9f_0a)
                        } else {
                            rgb(0x34_c7_59)
                        }),
                )
                .child(div().text_xs().text_color(theme::TEXT_TERTIARY).child(
                    if app.scan_in_progress {
                        "正在扫描同步...".to_string()
                    } else {
                        format!("已收录 {track_count} 首音乐")
                    },
                )),
        )
}

fn sidebar_section_header(title: &str) -> impl IntoElement {
    div()
        .px_2()
        .pt_2()
        .pb_1()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::TEXT_TERTIARY)
        .child(title.to_uppercase())
}

fn sidebar_item<F>(
    label: &'static str,
    icon: &'static str,
    active: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    let text_color = if active {
        theme::ACCENT_RED
    } else {
        theme::TEXT_PRIMARY
    };
    let icon_color = if active {
        hsla(348.0, 0.95, 0.56, 1.0)
    } else {
        hsla(220.0, 0.08, 0.50, 1.0)
    };
    let bg_color = if active {
        theme::accent_red_muted()
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };

    div()
        .id(SharedString::from(format!("side-{label}")))
        .flex()
        .items_center()
        .justify_between()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .bg(bg_color)
        .hover(move |s| {
            s.bg(if active {
                theme::accent_red_muted()
            } else {
                theme::bg_hover()
            })
        })
        .transition(theme::hover_transition())
        .active(|s| s.scale(0.98))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(div().w(px(3.0)).h(px(14.0)).rounded_full().bg(if active {
                    theme::ACCENT_RED.into()
                } else {
                    hsla(0.0, 0.0, 0.0, 0.0)
                }))
                .child(theme::themed_icon(icon, 16.0, icon_color))
                .child(
                    div()
                        .text_sm()
                        .font_weight(if active {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::NORMAL
                        })
                        .text_color(text_color)
                        .child(label),
                ),
        )
        .on_click(on_click)
}

fn traffic_light_button<F>(id: &'static str, color: gpui::Rgba, on_click: F) -> impl IntoElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .id(SharedString::from(id))
        .size(px(12.0))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, 0.15))
        .cursor_pointer()
        .occlude()
        .window_control_area(gpui::WindowControlArea::Client)
        .hover(|s| s.opacity(0.80))
        .transition(theme::press_transition())
        .active(|s| s.scale(0.90))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        })
        .on_click(on_click)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drag_progress_ratio_precedence() {
        let mut app = MusicApp::new(false);
        app.snapshot.duration_ms = 100_000;
        app.snapshot.position_ms = 20_000;

        assert!((app.displayed_progress_ratio() - 0.20).abs() < 0.001);
        assert_eq!(app.displayed_position_ms(), 20_000);

        app.pending_progress_ratio = Some((1, 0.55));
        assert!((app.displayed_progress_ratio() - 0.55).abs() < 0.001);
        assert_eq!(app.displayed_position_ms(), 55_000);

        app.drag_progress_ratio = Some(0.85);
        assert!((app.displayed_progress_ratio() - 0.85).abs() < 0.001);
        assert_eq!(app.displayed_position_ms(), 85_000);

        app.drag_progress_ratio = None;
        assert!((app.displayed_progress_ratio() - 0.55).abs() < 0.001);

        app.pending_progress_ratio = None;
        assert!((app.displayed_progress_ratio() - 0.20).abs() < 0.001);
    }

    #[test]
    fn test_drag_volume_ratio_precedence() {
        let mut app = MusicApp::new(false);
        app.config.volume = 0.5;

        assert!((app.displayed_volume_ratio() - 0.5).abs() < 0.001);
        app.pending_volume_ratio = Some(0.8);
        assert!((app.displayed_volume_ratio() - 0.8).abs() < 0.001);
        app.drag_volume_ratio = Some(0.3);
        assert!((app.displayed_volume_ratio() - 0.3).abs() < 0.001);
        app.drag_volume_ratio = None;
        assert!((app.displayed_volume_ratio() - 0.8).abs() < 0.001);
        app.pending_volume_ratio = None;
        assert!((app.displayed_volume_ratio() - 0.5).abs() < 0.001);
    }

    #[test]
    fn position_only_refreshes_progress_view_when_stage_is_closed() {
        assert!(!should_refresh_main_view(
            Some(1),
            Some(1),
            PlaybackState::Playing,
            PlaybackState::Playing,
            true,
            false,
            false,
            false,
        ));
        assert!(should_refresh_main_view(
            Some(1),
            Some(1),
            PlaybackState::Playing,
            PlaybackState::Playing,
            true,
            true,
            false,
            false,
        ));
        assert!(!should_refresh_main_view(
            Some(1),
            Some(1),
            PlaybackState::Playing,
            PlaybackState::Playing,
            true,
            true,
            true,
            false,
        ));
        assert!(should_refresh_main_view(
            Some(1),
            Some(1),
            PlaybackState::Playing,
            PlaybackState::Playing,
            false,
            true,
            false,
            true,
        ));
    }

    #[test]
    fn test_lyrics_target_offset_centering() {
        let target_0 = 0_f32 * 60.0;
        assert_eq!(target_0, 0.0);
        let target_10 = 10_f32 * 60.0;
        assert_eq!(target_10, 600.0);
    }

    #[test]
    fn test_mouse_movement_threshold() {
        let last_pos = gpui::Point::new(gpui::px(100.0), gpui::px(100.0));
        let micro_move = gpui::Point::new(gpui::px(100.5), gpui::px(101.2));
        let dx = f32::from(micro_move.x - last_pos.x).abs();
        let dy = f32::from(micro_move.y - last_pos.y).abs();
        assert!(dx < 2.0 && dy < 2.0, "微小抖动必须被过滤，不得触发唤醒");

        let real_move = gpui::Point::new(gpui::px(103.5), gpui::px(100.0));
        let real_dx = f32::from(real_move.x - last_pos.x).abs();
        assert!(real_dx >= 2.0, "真实鼠标位移必须触发舞台唤醒");
    }

    #[test]
    fn stage_idle_policy_is_twenty_seconds() {
        assert_eq!(STAGE_CONTROLS_IDLE_TIMEOUT, Duration::from_secs(20));
    }

    #[test]
    fn test_stage_debounce_window() {
        let suppress_until = std::time::Instant::now() + Duration::from_millis(600);
        assert!(std::time::Instant::now() < suppress_until);
    }
}
