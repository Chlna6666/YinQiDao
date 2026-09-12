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
    AnimationExt as _, AnimationProperty, App, AppContext, Bounds, CompositeLayerExt as _, Context,
    Entity, IntoElement, KeyDownEvent, Render, SharedString, Subscription, Timer, WeakEntity, Window,
    WindowBounds, WindowOptions, div, hsla, point, prelude::*, px, rgb, size,
};
use gpui_tokio::Tokio;
use lucide_gpui::icon;

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
const STAGE_TRANSITION_DURATION: Duration = Duration::from_millis(190);
const STAGE_MANUAL_WAKE_THRESHOLD_PX: f32 = 8.0;

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
    pub(crate) pending_volume_ratio: Option<f32>,
    lyrics_checked: HashSet<TrackId>,
    last_polled_track_id: Option<TrackId>,
    pub(crate) lyrics_scroll_handle: gpui::ScrollHandle,
    pub(crate) last_lyric_index: Option<usize>,
    pub(crate) lyric_motion_epoch: u64,
    pub(crate) hovered_lyric_index: Option<usize>,
    pub(crate) stage_open: bool,
    pub(crate) stage_progress: f32,
    pub(crate) stage_animating: bool,
    stage_transition_epoch: u64,
    stage_transition_from: f32,
    stage_transition_to: f32,
    stage_transition_started_at: Option<std::time::Instant>,
    stage_transition_duration: Duration,
    stage_prepared: bool,
    pub(crate) last_frame_instant: Option<std::time::Instant>,
    pub(crate) stage_controls_visibility: f32,
    pub(crate) stage_last_user_activity: std::time::Instant,
    pub(crate) stage_last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) stage_controls_hovered: bool,
    pub(crate) stage_suppress_wake_until: Option<std::time::Instant>,
    pub(crate) lyrics_user_scrolling_until: Option<std::time::Instant>,
    pub(crate) lyrics_scroll_target_y: Option<f32>,
    pub(crate) fluid_background: Option<Entity<crate::gpu::AppleFluidView>>,
    pub(crate) artwork_online_fallback_requested: HashSet<TrackId>,
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
    ui_content_revision: u64,
    home_page: Option<Entity<HomePage>>,
    library_page: Option<Entity<LibraryPage>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HomePageRenderKey {
    active: bool,
    content_revision: u64,
    scan_in_progress: bool,
    current_track: Option<TrackId>,
    playback_state: PlaybackState,
}

fn home_page_render_key(app: &MusicApp) -> HomePageRenderKey {
    HomePageRenderKey {
        active: app.page == AppPage::Home,
        content_revision: app.ui_content_revision,
        scan_in_progress: app.scan_in_progress,
        current_track: app.snapshot.current_track.as_ref().map(|track| track.id),
        playback_state: app.snapshot.state,
    }
}

#[inline]
fn text_fingerprint(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[inline]
fn stage_ease_in_out_cubic(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress * progress * progress
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LibraryPageRenderKey {
    active: bool,
    content_revision: u64,
    status_fingerprint: u64,
    search_fingerprint: u64,
    tab: LibraryTab,
    search_active: bool,
    scan_in_progress: bool,
    current_track: Option<TrackId>,
    playback_state: PlaybackState,
}

fn library_page_render_key(app: &MusicApp) -> LibraryPageRenderKey {
    LibraryPageRenderKey {
        active: app.page == AppPage::Library,
        content_revision: app.ui_content_revision,
        status_fingerprint: text_fingerprint(&app.status),
        search_fingerprint: text_fingerprint(&app.search),
        tab: app.library_tab,
        search_active: app.search_active,
        scan_in_progress: app.scan_in_progress,
        current_track: app.snapshot.current_track.as_ref().map(|track| track.id),
        playback_state: app.snapshot.state,
    }
}

struct HomePage {
    parent: WeakEntity<MusicApp>,
    last_key: HomePageRenderKey,
    refresh_pending: bool,
    _subscription: Subscription,
}

impl HomePage {
    fn new(
        parent: Entity<MusicApp>,
        initial_key: HomePageRenderKey,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&parent, |this, parent, cx| {
            if this.refresh_pending {
                return;
            }
            this.refresh_pending = true;
            let this_entity = cx.weak_entity();
            cx.defer(move |cx| {
                let next_key = home_page_render_key(parent.read(cx));
                let _ = this_entity.update(cx, |this, cx| {
                    this.refresh_pending = false;
                    if next_key != this.last_key {
                        this.last_key = next_key;
                        if next_key.active {
                            cx.notify();
                        }
                    }
                });
            });
        });
        Self {
            parent: parent.downgrade(),
            last_key: initial_key,
            refresh_pending: false,
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
    last_key: LibraryPageRenderKey,
    refresh_pending: bool,
    _subscription: Subscription,
}

impl LibraryPage {
    fn new(
        parent: Entity<MusicApp>,
        initial_key: LibraryPageRenderKey,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&parent, |this, parent, cx| {
            if this.refresh_pending {
                return;
            }
            this.refresh_pending = true;
            let this_entity = cx.weak_entity();
            cx.defer(move |cx| {
                let next_key = library_page_render_key(parent.read(cx));
                let _ = this_entity.update(cx, |this, cx| {
                    this.refresh_pending = false;
                    if next_key != this.last_key {
                        this.last_key = next_key;
                        if next_key.active {
                            cx.notify();
                        }
                    }
                });
            });
        });
        Self {
            parent: parent.downgrade(),
            last_key: initial_key,
            refresh_pending: false,
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
            pending_volume_ratio: None,
            lyrics_checked: HashSet::new(),
            last_polled_track_id: None,
            lyrics_scroll_handle: gpui::ScrollHandle::new(),
            last_lyric_index: None,
            lyric_motion_epoch: 0,
            hovered_lyric_index: None,
            stage_open: false,
            stage_progress: 0.0,
            stage_animating: false,
            stage_transition_epoch: 0,
            stage_transition_from: 0.0,
            stage_transition_to: 0.0,
            stage_transition_started_at: None,
            stage_transition_duration: STAGE_TRANSITION_DURATION,
            stage_prepared: false,
            last_frame_instant: None,
            stage_controls_visibility: 1.0,
            stage_last_user_activity: std::time::Instant::now(),
            stage_last_mouse_pos: None,
            stage_controls_hovered: false,
            stage_suppress_wake_until: None,
            lyrics_user_scrolling_until: None,
            lyrics_scroll_target_y: None,
            fluid_background: None,
            artwork_online_fallback_requested: HashSet::new(),
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
            ui_content_revision: 0,
            home_page: None,
            library_page: None,
        }
    }

    #[inline]
    fn bump_ui_content_revision(&mut self) {
        self.ui_content_revision = self.ui_content_revision.wrapping_add(1);
    }

    fn ensure_home_page(&mut self, cx: &mut Context<Self>) -> Entity<HomePage> {
        if let Some(page) = &self.home_page {
            return page.clone();
        }
        let initial_key = home_page_render_key(self);
        let parent = cx.entity();
        let page = cx.new(move |cx| HomePage::new(parent, initial_key, cx));
        self.home_page = Some(page.clone());
        page
    }

    fn ensure_library_page(&mut self, cx: &mut Context<Self>) -> Entity<LibraryPage> {
        if let Some(page) = &self.library_page {
            return page.clone();
        }
        let initial_key = library_page_render_key(self);
        let parent = cx.entity();
        let page = cx.new(move |cx| LibraryPage::new(parent, initial_key, cx));
        self.library_page = Some(page.clone());
        page
    }

    fn ensure_fluid_background(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::gpu::AppleFluidView> {
        if let Some(view) = &self.fluid_background {
            return view.clone();
        }
        let view = cx.new(|_| crate::gpu::AppleFluidView::new());
        self.fluid_background = Some(view.clone());
        view
    }

    fn sample_stage_progress_at(&self, now: std::time::Instant) -> f32 {
        if !self.stage_animating {
            return self.stage_progress;
        }
        let Some(started_at) = self.stage_transition_started_at else {
            return self.stage_progress;
        };
        if self.stage_transition_duration.is_zero() {
            return self.stage_transition_to;
        }
        let linear = now
            .saturating_duration_since(started_at)
            .as_secs_f32()
            / self.stage_transition_duration.as_secs_f32();
        let eased = stage_ease_in_out_cubic(linear);
        self.stage_transition_from
            + (self.stage_transition_to - self.stage_transition_from) * eased
    }

    fn advance_stage_transition(&mut self, now: std::time::Instant) {
        if !self.stage_animating || self.stage_transition_started_at.is_none() {
            return;
        }
        self.stage_progress = self.sample_stage_progress_at(now).clamp(0.0, 1.0);
        let started_at = self
            .stage_transition_started_at
            .expect("started transition must have a timestamp");
        if now.saturating_duration_since(started_at) >= self.stage_transition_duration {
            self.stage_progress = self.stage_transition_to;
            self.stage_animating = false;
            self.stage_transition_started_at = None;
            if self.stage_progress <= 0.001 {
                self.hovered_lyric_index = None;
            }
        }
    }

    fn begin_stage_transition(&mut self, open: bool, cx: &mut Context<Self>) {
        let target = if open { 1.0 } else { 0.0 };
        if self.stage_animating && (self.stage_transition_to - target).abs() <= 0.001 {
            return;
        }

        let now = std::time::Instant::now();
        let current = self.sample_stage_progress_at(now).clamp(0.0, 1.0);
        if !self.stage_animating && (current - target).abs() <= 0.001 {
            self.stage_open = open;
            self.stage_progress = target;
            return;
        }

        self.stage_transition_epoch = self.stage_transition_epoch.wrapping_add(1);
        let epoch = self.stage_transition_epoch;
        self.stage_open = open;
        self.hovered_lyric_index = None;
        self.stage_progress = current;
        self.stage_transition_from = current;
        self.stage_transition_to = target;
        self.stage_transition_started_at = None;

        let distance = (target - current).abs();
        if distance <= 0.001 {
            self.stage_progress = target;
            self.stage_animating = false;
            return;
        }
        self.stage_transition_duration = Duration::from_secs_f32(
            (STAGE_TRANSITION_DURATION.as_secs_f32() * distance).max(0.001),
        );
        self.stage_animating = true;

        // Do not start the clock until the first transition frame has fully materialized. On the
        // first open this frame may compile the fluid shader, shape lyrics and build the composite;
        // starting earlier would let that work consume the entire 190 ms and visually skip motion.
        cx.spawn(async move |this, cx| -> Result<()> {
            Timer::after(Duration::from_millis(1)).await;
            this.update(cx, |this, cx| {
                if this.stage_transition_epoch != epoch
                    || !this.stage_animating
                    || this.stage_transition_started_at.is_some()
                {
                    return;
                }
                this.stage_transition_started_at = Some(std::time::Instant::now());
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
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
        self.begin_stage_transition(true, cx);
        self.last_frame_instant = None;
        self.stage_last_user_activity = std::time::Instant::now();
        self.stage_last_mouse_pos = None;
        self.stage_suppress_wake_until = None;
        self.stage_controls_visibility = 1.0;
        self.page = AppPage::Player;
        route::navigate_to(cx, AppPage::Player);
        cx.notify();
    }

    pub(crate) fn close_stage(&mut self, cx: &mut Context<Self>) {
        self.begin_stage_transition(false, cx);
        self.last_frame_instant = None;
        self.stage_last_mouse_pos = None;
        self.stage_suppress_wake_until = None;
        self.hovered_lyric_index = None;
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
        if self.stage_suppress_wake_until.is_some() {
            return;
        }
        self.stage_last_user_activity = std::time::Instant::now();
        if self.stage_controls_visibility < 0.995 {
            self.stage_controls_visibility = 1.0;
            cx.notify();
        }
    }

    pub(crate) fn wake_stage_controls_immediately(&mut self, cx: &mut Context<Self>) {
        self.stage_suppress_wake_until = None;
        self.stage_last_user_activity = std::time::Instant::now();
        self.stage_controls_visibility = 1.0;
        cx.notify();
    }

    pub(crate) fn hide_stage_controls_immediately(
        &mut self,
        pointer_pos: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let now = std::time::Instant::now();
        self.stage_controls_visibility = 0.0;
        self.stage_last_user_activity = now
            .checked_sub(STAGE_CONTROLS_IDLE_TIMEOUT + Duration::from_secs(1))
            .unwrap_or(now);
        self.stage_controls_hovered = false;
        self.stage_last_mouse_pos = Some(pointer_pos);
        // Presence of this marker means the user explicitly entered clean/immersive mode. It is
        // released by a meaningful pointer movement or an explicit action, not by a short timer.
        self.stage_suppress_wake_until = Some(now);
        cx.notify();
    }

    pub(crate) fn handle_stage_mouse_move(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let manual_immersive = self.stage_suppress_wake_until.is_some();
        let threshold = if manual_immersive {
            STAGE_MANUAL_WAKE_THRESHOLD_PX
        } else {
            2.0
        };
        let moved = self.stage_last_mouse_pos.is_none_or(|last| {
            let dx = f32::from(pos.x - last.x).abs();
            let dy = f32::from(pos.y - last.y).abs();
            dx >= threshold || dy >= threshold
        });
        if moved {
            self.stage_last_mouse_pos = Some(pos);
            if manual_immersive {
                self.stage_suppress_wake_until = None;
            }
            self.wake_stage_controls(cx);
        }
    }

    pub(crate) fn has_active_animations(&self) -> bool {
        let stage_transition = self.stage_animating && self.stage_transition_started_at.is_some();
        stage_transition
            || (self.stage_open
                && self.stage_controls_visibility > 0.005
                && self.stage_controls_visibility < 0.995)
    }

    pub(crate) fn show_library_tab(&mut self, tab: LibraryTab, cx: &mut Context<Self>) {
        self.page = AppPage::Library;
        self.library_tab = tab;
        route::navigate_to(cx, tab);
        cx.notify();
    }
