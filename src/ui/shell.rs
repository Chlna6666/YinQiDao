use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{Receiver, Sender},
    },
    time::Duration,
};

use anyhow::{Result, anyhow};
use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, App, AppContext, Bounds,
    CompositeLayerExt as _, Context, Easing, ElementId, Entity, Focusable, IntoElement,
    KeyDownEvent, Render, SharedString, Subscription, Timer, TransformOrigin, WeakEntity, Window,
    WindowBounds,
    WindowOptions, div, hsla, prelude::*, px, rgb, size,
};
use gpui_tokio::Tokio;
use lucide_gpui::icon;

use crate::lyrics::LyricsDocument;
use crate::{
    artwork::ArtworkCache,
    audio::{AudioEngine, EqPreset, PlayerCommand},
    library::{Library, ScanReport},
    model::{AppPage, LibraryTab, PlaybackState, PlayerSnapshot, RepeatMode, Track, TrackId},
    playback_cache::PlaybackAssetCache,
    settings::{AppConfig, ConfigStore},
};

use super::{
    app_runtime_events, home, library as library_page, mini_player_view, player,
    player::NowPlaying,
    route::{self, AppRoute},
    settings as settings_page, stage_chrome, stage_controls, stage_lyrics, theme,
};

const MAX_LYRICS_MEMORY_ENTRIES: usize = 64;
const ONLINE_ASSET_LOOKAHEAD: usize = 2;
const ONLINE_AUDIO_PRELOAD_DELAY: Duration = Duration::from_secs(8);
const STAGE_TRANSITION_DURATION: Duration = Duration::from_millis(220);
// Hidden Stage preparation must never compete with the transport's startup window. The audible
// playback timing is intentionally unchanged; only the offscreen immersive UI work is deferred.
const STAGE_BACKGROUND_PREWARM_DELAY: Duration = Duration::from_millis(2_000);
const STAGE_MANUAL_WAKE_THRESHOLD_PX: f32 = 8.0;

fn online_source_key(
    route: &crate::plugin::abi::PluginRoute,
    source: &crate::plugin::abi::SourceTrackRef,
) -> (String, String, String, String) {
    (
        route.plugin_id.clone(),
        route.provider_id.clone(),
        route.account_id.clone(),
        source.source_id.clone(),
    )
}

async fn load_online_lyrics_cached_or_remote(
    playback_cache: Option<PlaybackAssetCache>,
    route: crate::plugin::abi::PluginRoute,
    source: crate::plugin::abi::SourceTrackRef,
) -> Result<Option<LyricsDocument>> {
    if let Some(cache) = playback_cache.clone() {
        let source_for_cache = source.clone();
        match tokio::task::spawn_blocking(move || cache.load_lyrics(&source_for_cache)).await {
            Ok(Ok(Some(document))) => return Ok(Some(document)),
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                tracing::debug!(error = %error, "读取在线歌词缓存失败，回退插件请求");
            }
            Err(error) => {
                tracing::debug!(error = %error, "在线歌词缓存读取任务异常，回退插件请求");
            }
        }
    }

    let frontend =
        crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
    let result = frontend.lyrics_for_route(&route, &source).await?;
    let Some(document) = result.value else {
        return Ok(None);
    };
    let Some(document) =
        crate::plugin::frontend::plugin_lyrics_to_player(document, &route.provider_id)?
    else {
        return Ok(None);
    };

    if let Some(cache) = playback_cache {
        let source_for_store = source.clone();
        let document_for_store = document.clone();
        match tokio::task::spawn_blocking(move || {
            cache.store_lyrics(&source_for_store, &document_for_store)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(error = %error, "写入在线歌词缓存失败");
            }
            Err(error) => {
                tracing::debug!(error = %error, "在线歌词缓存写入任务异常");
            }
        }
    }

    Ok(Some(document))
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DragTarget {
    Progress,
    Volume,
}

#[derive(Clone, Debug)]
pub struct OnlinePlaylistViewData {
    pub title: String,
    pub subtitle: String,
    pub cover_url: Option<String>,
    pub tracks: Vec<crate::plugin::abi::RemoteTrack>,
    pub loading: bool,
    pub route: crate::plugin::abi::PluginRoute,
}

#[derive(Clone, Debug)]
pub(crate) struct OnlinePlaylistQueue {
    pub route: crate::plugin::abi::PluginRoute,
    pub playlist_title: String,
    pub tracks: Arc<[crate::plugin::abi::RemoteTrack]>,
    pub current_index: usize,
}

pub struct MusicApp {
    pub(crate) config_store: ConfigStore,
    pub(crate) config: AppConfig,
    pub(crate) library: Option<Library>,
    pub(crate) engine: Option<Arc<AudioEngine>>,
    pub(crate) tracks: Vec<Track>,
    library_track_ids: Arc<Vec<TrackId>>,
    track_index_by_id: HashMap<TrackId, usize>,
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
    last_system_media_metadata_fingerprint: Option<u64>,
    last_system_media_state: Option<PlaybackState>,
    last_system_media_position_sec: u64,
    pub(crate) artwork_cache: Option<ArtworkCache>,
    playback_cache: Option<PlaybackAssetCache>,
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
    pub(crate) stage_open: bool,
    pub(crate) stage_progress: f32,
    pub(crate) stage_animating: bool,
    stage_transition_epoch: u64,
    stage_transition_from: f32,
    stage_transition_to: f32,
    stage_transition_started_at: Option<std::time::Instant>,
    stage_transition_duration: Duration,
    stage_transition_start_armed: bool,
    stage_prepared: bool,
    stage_prewarm_after: Option<std::time::Instant>,
    pub(crate) stage_last_user_activity: std::time::Instant,
    pub(crate) stage_last_mouse_pos: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) stage_controls_hovered: bool,
    pub(crate) stage_suppress_wake_until: Option<std::time::Instant>,
    // Transport buttons update their retained controls optimistically. Defer the heavy MusicApp
    // root repaint until AudioEngine ACK arrives so mouse-down never competes with a full-window
    // layout generation frame.
    transport_root_notify_pending: bool,
    pub(crate) fluid_background: Option<Entity<crate::gpu::AppleFluidView>>,
    pub(crate) artwork_online_fallback_requested: HashSet<TrackId>,
    pub(crate) library_scroll_handle: gpui::UniformListScrollHandle,
    previous_page: AppPage,
    background_started: bool,
    library_refresh_request: u64,
    queue_matches_tracks: bool,
    runtime_events_started: bool,
    last_saved_position_ms: u64,
    last_saved_at: std::time::Instant,
    config_save_dirty: bool,
    last_config_save_at: std::time::Instant,
    ui_content_revision: u64,
    sessions_restored: bool,
    pub(crate) has_online_plugins: bool,
    pub(crate) online_authenticated: bool,
    pub(crate) online_daily_tracks: Vec<crate::plugin::abi::RemoteTrack>,
    pub(crate) online_playlists: Vec<crate::plugin::abi::CollectionRecommendationItem>,
    pub(crate) online_user_playlists: Vec<(
        crate::plugin::abi::PluginRoute,
        crate::plugin::abi::PlaylistDescriptor,
    )>,
    pub(crate) online_new_tracks: Vec<crate::plugin::abi::RemoteTrack>,
    pub(crate) online_route: Option<crate::plugin::abi::PluginRoute>,
    pub(crate) online_recommendations_loading: bool,
    pub(crate) online_search_results: Vec<crate::plugin::abi::RemoteTrack>,
    pub(crate) online_search_loading: bool,
    pub(crate) online_search_route: Option<crate::plugin::abi::PluginRoute>,
    pub(crate) recent_plays: Vec<TrackId>,
    pub(crate) online_playback_meta: HashMap<
        TrackId,
        (
            crate::plugin::abi::PluginRoute,
            crate::plugin::abi::SourceTrackRef,
            Option<String>,
        ),
    >,
    pub(crate) online_track_buffering: Option<String>,
    online_play_request_generation: u64,
    online_play_task: Option<gpui::Task<()>>,
    online_preload_generation: u64,
    online_audio_preload_task: Option<gpui::Task<()>>,
    online_asset_preload_task: Option<gpui::Task<()>>,
    online_preloaded_tracks: HashMap<(String, String, String, String), Track>,
    pub(crate) online_playlist_cache: HashMap<String, OnlinePlaylistViewData>,
    pub(crate) online_track_cache: HashMap<TrackId, Track>,
    pub(crate) online_remote_tracks: HashMap<
        TrackId,
        (
            crate::plugin::abi::PluginRoute,
            crate::plugin::abi::RemoteTrack,
        ),
    >,
    home_page: Option<Entity<HomePage>>,
    library_page: Option<Entity<LibraryPage>>,
    online_playlist_page: Option<Entity<OnlinePlaylistPage>>,
    pub(crate) active_modal: Option<super::components::modal::GlobalModal>,
    pub(crate) active_online_playlist: Option<OnlinePlaylistViewData>,
    pub(crate) online_playlist_queue: Option<OnlinePlaylistQueue>,
    pub(crate) sidebar_created_playlists_collapsed: bool,
    pub(crate) sidebar_collected_playlists_collapsed: bool,
    pub(crate) search_input: Option<Entity<crate::ui::components::input::HostTextInput>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HomePageRenderKey {
    active: bool,
    content_revision: u64,
    plugin_ui_revision: u64,
    scan_in_progress: bool,
    has_online_plugins: bool,
    online_authenticated: bool,
    current_track: Option<TrackId>,
}

fn home_page_render_key(app: &MusicApp) -> HomePageRenderKey {
    HomePageRenderKey {
        active: app.page == AppPage::Home,
        content_revision: app.ui_content_revision,
        plugin_ui_revision: crate::plugin::management::ui_observable_revision(),
        scan_in_progress: app.scan_in_progress,
        has_online_plugins: app.has_online_plugins,
        online_authenticated: app.online_authenticated,
        current_track: app.snapshot.current_track.as_ref().map(|track| track.id),
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
    }
}

struct HomePage {
    parent: WeakEntity<MusicApp>,
    last_key: HomePageRenderKey,
    refresh_pending: bool,
    plugin_home_loading: HashMap<String, u64>,
    plugin_home_failed: HashMap<String, u64>,
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
            plugin_home_loading: HashMap::new(),
            plugin_home_failed: HashMap::new(),
            _subscription: subscription,
        }
    }

    fn ensure_plugin_home_sections(&mut self, cx: &mut Context<Self>) {
        if !self.last_key.active || !crate::plugin::management::ui_client_ready() {
            return;
        }
        let Ok(sections) = crate::plugin::extensions::home_sections() else {
            return;
        };
        let generation = self.last_key.plugin_ui_revision;
        for section in sections {
            let qualified_id = section.qualified_id;
            match crate::plugin::extensions::home_section_snapshot(&qualified_id) {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => {
                    self.plugin_home_failed
                        .insert(qualified_id.clone(), generation);
                    tracing::warn!(section = %qualified_id, %error, "读取插件 Home Section 快照失败");
                    continue;
                }
            }
            if self.plugin_home_loading.get(&qualified_id).copied() == Some(generation)
                || self.plugin_home_failed.get(&qualified_id).copied() == Some(generation)
            {
                continue;
            }

            self.plugin_home_loading
                .insert(qualified_id.clone(), generation);
            let task_id = qualified_id.clone();
            let parent = self.parent.clone();
            let task = Tokio::spawn_result(cx, async move {
                crate::plugin::extensions::load_home_section(&task_id).await
            });
            cx.spawn(async move |this, cx| -> Result<()> {
                let result = task.await;
                let succeeded = result.is_ok();
                let status = match &result {
                    Ok(snapshot) => format!(
                        "首页插件内容已加载：{}/{} · rev {}",
                        snapshot.plugin_id, snapshot.page_id, snapshot.revision
                    ),
                    Err(error) => format!("首页插件内容加载失败：{error:#}"),
                };
                this.update(cx, |this, cx| {
                    if this.plugin_home_loading.get(&qualified_id).copied() == Some(generation) {
                        this.plugin_home_loading.remove(&qualified_id);
                    }
                    if !succeeded && this.last_key.plugin_ui_revision == generation {
                        this.plugin_home_failed
                            .insert(qualified_id.clone(), generation);
                    }
                    cx.notify();
                })?;
                let _ = parent.update(cx, |app, _cx| {
                    app.status = status;
                });
                Ok(())
            })
            .detach();
        }
    }
}

impl Render for HomePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_plugin_home_sections(cx);
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct OnlinePlaylistRenderKey {
    active: bool,
    playlist_title: Option<String>,
    tracks_count: usize,
    loading: bool,
    buffering_track: Option<String>,
    current_track: Option<TrackId>,
}

fn online_playlist_render_key(app: &MusicApp) -> OnlinePlaylistRenderKey {
    OnlinePlaylistRenderKey {
        active: app.page == AppPage::OnlinePlaylist,
        playlist_title: app.active_online_playlist.as_ref().map(|p| p.title.clone()),
        tracks_count: app
            .active_online_playlist
            .as_ref()
            .map_or(0, |p| p.tracks.len()),
        loading: app
            .active_online_playlist
            .as_ref()
            .is_some_and(|p| p.loading),
        buffering_track: app.online_track_buffering.clone(),
        current_track: app.snapshot.current_track.as_ref().map(|track| track.id),
    }
}

struct OnlinePlaylistPage {
    parent: WeakEntity<MusicApp>,
    last_key: OnlinePlaylistRenderKey,
    refresh_pending: bool,
    _subscription: Subscription,
}

impl OnlinePlaylistPage {
    fn new(
        parent: Entity<MusicApp>,
        initial_key: OnlinePlaylistRenderKey,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.observe(&parent, |this, parent, cx| {
            if this.refresh_pending {
                return;
            }
            this.refresh_pending = true;
            let this_entity = cx.weak_entity();
            cx.defer(move |cx| {
                let next_key = online_playlist_render_key(parent.read(cx));
                let _ = this_entity.update(cx, |this, cx| {
                    this.refresh_pending = false;
                    if next_key != this.last_key {
                        let is_active = next_key.active;
                        this.last_key = next_key;
                        if is_active {
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

impl Render for OnlinePlaylistPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let app = parent.read(cx);
        super::online_playlist::render(app, &self.parent)
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
        let playback_cache = config_store
            .path()
            .parent()
            .and_then(|parent| PlaybackAssetCache::new(parent.join("playback-cache")).ok());
        let library = config_store
            .path()
            .parent()
            .map(|parent| parent.join("library.sqlite3"))
            .and_then(|path| Library::new(path).ok());
        let tracks = library
            .as_ref()
            .and_then(|library| library.tracks(None).ok())
            .unwrap_or_default();
        let library_track_ids = Arc::new(tracks.iter().map(|track| track.id).collect::<Vec<_>>());
        let track_index_by_id = tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (track.id, index))
            .collect::<HashMap<_, _>>();
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

        let mut initial_track = config
            .current_track
            .and_then(|id| tracks.iter().find(|t| t.id == id).cloned());
        if initial_track.is_none() {
            if let Some(saved) = &config.last_played_track {
                initial_track = Some(Track::from(saved.clone()));
            }
        }
        let mut initial_online_cache = HashMap::new();
        let mut initial_online_playback_meta = HashMap::new();
        let mut initial_online_remote_tracks = HashMap::new();
        let mut initial_lyrics = HashMap::new();
        let mut initial_lyrics_order = VecDeque::new();
        let mut initial_artworks = HashMap::new();
        let mut initial_blurred_artworks = HashMap::new();
        let mut initial_artwork_palettes = HashMap::new();

        if let Some(track) = &initial_track
            && track.id < 0
        {
            initial_online_cache.insert(track.id, track.clone());

            // Restore the last plugin track's semantic provenance and presentation assets before the
            // first window frame. The cache contains no stream URL or auth secret; it only reconnects
            // the saved TrackId to provider/source plus already-persisted lyrics/artwork.
            if let Some(cached) = playback_cache
                .as_ref()
                .and_then(|cache| cache.load_last(track.id).ok().flatten())
            {
                let cover_url = cached
                    .cover_url
                    .clone()
                    .or_else(|| cached.remote.cover_url.clone());
                initial_online_playback_meta.insert(
                    track.id,
                    (
                        cached.route.clone(),
                        cached.source.clone(),
                        cover_url.clone(),
                    ),
                );
                initial_online_remote_tracks
                    .insert(track.id, (cached.route.clone(), cached.remote.clone()));

                if let Some(lyrics) = cached.lyrics {
                    initial_lyrics.insert(track.id, lyrics);
                    initial_lyrics_order.push_back(track.id);
                }

                if let Some(url) = cover_url
                    && let Some(cache) = artwork_cache.as_ref()
                    && let Ok(Some(artwork)) = cache.load_key(&url)
                {
                    initial_artworks.insert(track.id, artwork.png.into());
                    initial_blurred_artworks.insert(track.id, artwork.blurred_png.into());
                    initial_artwork_palettes.insert(track.id, artwork.palette);
                }
            }
        }

        let initial_duration = initial_track.as_ref().map_or(0, |t| t.duration_ms);
        let initial_position = if initial_duration > 0 {
            config.position_ms.min(initial_duration)
        } else {
            config.position_ms
        };
        let queue_matches_tracks = config.queue.len() == library_track_ids.len()
            && config
                .queue
                .iter()
                .copied()
                .eq(library_track_ids.iter().copied());

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
                if track.id < 0 {
                    engine.try_play_transient_track(track.clone());
                    engine.try_send(PlayerCommand::Pause);
                    engine.try_send(PlayerCommand::Seek(Duration::from_millis(initial_position)));
                } else {
                    engine.try_send(PlayerCommand::RestoreTrack {
                        track_id: track.id,
                        position: Duration::from_millis(initial_position),
                        play: false,
                    });
                }
            }
        }
        Self {
            config_store,
            config,
            library,
            engine,
            tracks,
            library_track_ids,
            track_index_by_id,
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
            artworks: initial_artworks,
            blurred_artworks: initial_blurred_artworks,
            artwork_palettes: initial_artwork_palettes,
            lyrics: initial_lyrics,
            lyrics_order: initial_lyrics_order,
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
            last_system_media_metadata_fingerprint: None,
            last_system_media_state: None,
            last_system_media_position_sec: 0,
            artwork_cache,
            playback_cache,
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
            stage_open: false,
            stage_progress: 0.0,
            stage_animating: false,
            stage_transition_epoch: 0,
            stage_transition_from: 0.0,
            stage_transition_to: 0.0,
            stage_transition_started_at: None,
            stage_transition_duration: STAGE_TRANSITION_DURATION,
            stage_transition_start_armed: false,
            stage_prepared: false,
            stage_prewarm_after: Some(
                std::time::Instant::now() + STAGE_BACKGROUND_PREWARM_DELAY,
            ),
            stage_last_user_activity: std::time::Instant::now(),
            stage_last_mouse_pos: None,
            stage_controls_hovered: false,
            stage_suppress_wake_until: None,
            transport_root_notify_pending: false,
            fluid_background: None,
            artwork_online_fallback_requested: HashSet::new(),
            library_scroll_handle: gpui::UniformListScrollHandle::new(),
            previous_page: AppPage::Home,
            background_started: false,
            library_refresh_request: 0,
            queue_matches_tracks,
            runtime_events_started: false,
            last_saved_position_ms: initial_position,
            last_saved_at: std::time::Instant::now(),
            config_save_dirty: false,
            last_config_save_at: std::time::Instant::now() - Duration::from_secs(1),
            ui_content_revision: 0,
            sessions_restored: false,
            has_online_plugins: false,
            online_authenticated: false,
            online_daily_tracks: Vec::new(),
            online_playlists: Vec::new(),
            online_user_playlists: Vec::new(),
            online_new_tracks: Vec::new(),
            online_route: None,
            online_recommendations_loading: false,
            online_search_results: Vec::new(),
            online_search_loading: false,
            online_search_route: None,
            recent_plays: Vec::new(),
            online_playback_meta: initial_online_playback_meta,
            online_track_buffering: None,
            online_play_request_generation: 0,
            online_play_task: None,
            online_preload_generation: 0,
            online_audio_preload_task: None,
            online_asset_preload_task: None,
            online_preloaded_tracks: HashMap::new(),
            online_playlist_cache: HashMap::new(),
            online_track_cache: initial_online_cache,
            online_remote_tracks: initial_online_remote_tracks,
            home_page: None,
            library_page: None,
            online_playlist_page: None,
            active_modal: None,
            active_online_playlist: None,
            online_playlist_queue: None,
            sidebar_created_playlists_collapsed: false,
            sidebar_collected_playlists_collapsed: false,
            search_input: None,
        }
    }

    pub(crate) fn has_enabled_online_plugins(&self) -> bool {
        if let Ok(services) = crate::plugin::accounts::service_summaries() {
            services
                .iter()
                .any(|s| s.enabled && !s.providers.is_empty())
        } else {
            false
        }
    }

    pub(crate) fn check_online_authenticated(&self) -> bool {
        if let Ok(services) = crate::plugin::accounts::service_summaries() {
            services.iter().any(|s| {
                s.enabled
                    && s.providers.iter().any(|p| {
                        p.accounts
                            .iter()
                            .any(|a| a.state == crate::plugin::accounts::PluginAccountSessionStatus::Authenticated)
                    })
            })
        } else {
            false
        }
    }

    pub(crate) fn sync_online_service_state(&mut self, cx: &mut Context<Self>) {
        let has_plugins = self.has_enabled_online_plugins();
        let is_auth = if has_plugins {
            self.check_online_authenticated()
        } else {
            false
        };
        self.has_online_plugins = has_plugins;
        self.online_authenticated = is_auth;

        if !has_plugins {
            self.cancel_online_play_request();
            self.cancel_online_preload_tasks();
            self.online_preloaded_tracks.clear();
            self.online_daily_tracks.clear();
            self.online_playlists.clear();
            self.online_user_playlists.clear();
            self.online_new_tracks.clear();
            self.online_route = None;
            self.online_playlist_queue = None;
            if self.page == AppPage::OnlinePlaylist {
                self.show_page(AppPage::Library, cx);
            }
        }
        self.bump_ui_content_revision();
        cx.notify();
    }

    pub fn open_modal(
        &mut self,
        modal: super::components::modal::GlobalModal,
        cx: &mut Context<Self>,
    ) {
        self.active_modal = Some(modal);
        cx.notify();
    }

    pub fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.active_modal = None;
        cx.notify();
    }

    #[inline]
    pub(crate) fn bump_ui_content_revision(&mut self) {
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

    fn ensure_online_playlist_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<OnlinePlaylistPage> {
        if let Some(page) = &self.online_playlist_page {
            return page.clone();
        }
        let initial_key = online_playlist_render_key(self);
        let parent = cx.entity();
        let page = cx.new(move |cx| OnlinePlaylistPage::new(parent, initial_key, cx));
        self.online_playlist_page = Some(page.clone());
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
        let linear = now.saturating_duration_since(started_at).as_secs_f32()
            / self.stage_transition_duration.as_secs_f32();
        let eased = stage_ease_in_out_cubic(linear);
        self.stage_transition_from + (self.stage_transition_to - self.stage_transition_from) * eased
    }

    fn finalize_stage_close_route(&mut self, cx: &mut Context<Self>) {
        let return_page = if self.previous_page == AppPage::Player {
            AppPage::Home
        } else {
            self.previous_page
        };
        self.page = return_page;
        route::navigate_to(cx, return_page);
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
            if !open {
                self.finalize_stage_close_route(cx);
            }
            return;
        }

        self.stage_transition_epoch = self.stage_transition_epoch.wrapping_add(1);
        self.stage_open = open;
        self.stage_progress = current;
        self.stage_transition_from = current;
        self.stage_transition_to = target;
        self.stage_transition_started_at = None;
        self.stage_transition_start_armed = false;

        let distance = (target - current).abs();
        if distance <= 0.001 {
            self.stage_progress = target;
            self.stage_animating = false;
            if !open {
                self.finalize_stage_close_route(cx);
            }
            return;
        }
        self.stage_transition_duration = Duration::from_secs_f32(
            (STAGE_TRANSITION_DURATION.as_secs_f32() * distance).max(0.001),
        );
        self.stage_animating = true;

        // The renderer-owned animation is armed from `render` after the sampled start frame has
        // actually been presented. Do not begin a wall-clock timer here: on a cold first open the
        // stage may still be compiling its shader pipeline, uploading artwork, shaping text or
        // materializing retained layers. Starting the clock before those operations finish makes
        // the visible animation jump directly into its middle.
        cx.notify();
    }

    fn arm_stage_transition_after_present(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.stage_animating
            || self.stage_transition_started_at.is_some()
            || self.stage_transition_start_armed
        {
            return;
        }

        self.stage_transition_start_armed = true;
        let epoch = self.stage_transition_epoch;
        let entity = cx.entity();
        window.on_next_frame(move |window, cx| {
            let started = entity.update(cx, |this, cx| {
                if this.stage_transition_epoch != epoch
                    || !this.stage_animating
                    || this.stage_transition_started_at.is_some()
                {
                    if this.stage_transition_epoch == epoch {
                        this.stage_transition_start_armed = false;
                    }
                    return None;
                }

                this.stage_transition_start_armed = false;
                this.stage_transition_started_at = Some(std::time::Instant::now());
                let duration = this.stage_transition_duration;
                cx.notify();
                Some(duration)
            });

            let Some(duration) = started else {
                return;
            };

            // `with_animation` creates the scene animation during paint. Wait for that first
            // animated frame to finish as well before starting the completion timer. This can make
            // the logical state live for at most one extra presented frame, but can never cut a
            // renderer animation short because a cold frame took longer than expected.
            let finish_entity = entity.clone();
            window.on_next_frame(move |_window, cx| {
                finish_entity.update(cx, |this, cx| {
                    if this.stage_transition_epoch != epoch || !this.stage_animating {
                        return;
                    }
                    cx.spawn(async move |this, cx| -> Result<()> {
                        Timer::after(duration).await;
                        this.update(cx, |this, cx| {
                            if this.stage_transition_epoch != epoch || !this.stage_animating {
                                return;
                            }
                            this.stage_progress = this.stage_transition_to;
                            this.stage_animating = false;
                            this.stage_transition_started_at = None;
                            this.stage_transition_start_armed = false;
                            if this.stage_progress <= 0.001 {
                                this.finalize_stage_close_route(cx);
                            }
                            cx.notify();
                        })?;
                        Ok(())
                    })
                    .detach();
                });
            });
        });
    }

    pub(crate) fn show_page(&mut self, page: AppPage, cx: &mut Context<Self>) {
        if page == AppPage::Player {
            self.open_stage(cx);
            return;
        }
        if self.stage_open || self.stage_animating {
            self.previous_page = page;
            self.close_stage(cx);
            return;
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
        // A visible Stage render supersedes any delayed hidden prewarm.
        self.stage_prewarm_after = None;
        self.stage_prepared = true;
        self.begin_stage_transition(true, cx);
        self.stage_last_user_activity = std::time::Instant::now();
        self.stage_last_mouse_pos = None;
        self.stage_suppress_wake_until = None;
        self.page = AppPage::Player;
        route::navigate_to(cx, AppPage::Player);
        cx.notify();
    }

    pub(crate) fn close_stage(&mut self, cx: &mut Context<Self>) {
        self.begin_stage_transition(false, cx);
        self.stage_last_mouse_pos = None;
        self.stage_suppress_wake_until = None;
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
        let needs_notify = stage_chrome::needs_wake_surface(self);
        self.stage_last_user_activity = std::time::Instant::now();
        if needs_notify {
            cx.notify();
        }
    }

    pub(crate) fn wake_stage_controls_immediately(&mut self, cx: &mut Context<Self>) {
        let needs_notify = stage_chrome::needs_wake_surface(self);
        self.stage_suppress_wake_until = None;
        self.stage_last_user_activity = std::time::Instant::now();
        if needs_notify {
            cx.notify();
        }
    }

    pub(crate) fn hide_stage_controls_immediately(
        &mut self,
        pointer_pos: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        let needs_notify = stage_chrome::target_visible(self);
        let now = std::time::Instant::now();
        self.stage_last_user_activity = now
            .checked_sub(stage_chrome::IDLE_TIMEOUT + Duration::from_secs(1))
            .unwrap_or(now);
        self.stage_controls_hovered = false;
        self.stage_last_mouse_pos = Some(pointer_pos);
        self.stage_suppress_wake_until = Some(now);
        if needs_notify {
            cx.notify();
        }
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

    pub(crate) fn show_library_tab(&mut self, tab: LibraryTab, cx: &mut Context<Self>) {
        self.page = AppPage::Library;
        self.library_tab = tab;
        route::navigate_to(cx, AppPage::Library);
        cx.notify();
    }

    pub(crate) fn ensure_queue(&mut self) {
        if self.config.queue.is_empty() && !self.library_track_ids.is_empty() {
            // The projection is prepared when the library is loaded/refreshed. Playback controls
            // only clone the Arc, so a click never walks the full library.
            let queue = self.library_track_ids.clone();
            self.config.queue = queue.clone();
            self.bump_ui_content_revision();
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
        self.bump_ui_content_revision();
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

    pub(crate) fn sync_lyrics_surfaces(
        &mut self,
        track_id: TrackId,
        cx: &mut Context<Self>,
    ) {
        let is_current = self
            .snapshot
            .current_track
            .as_ref()
            .is_some_and(|track| track.id == track_id);
        if !is_current {
            return;
        }

        stage_lyrics::sync_if_created(self, cx);
        if self.config.desktop_lyrics.visible {
            self.sync_desktop_lyrics_window(cx);
        }
    }

    fn cancel_online_play_request(&mut self) {
        self.online_play_request_generation =
            self.online_play_request_generation.wrapping_add(1);
        self.online_track_buffering = None;
        // gpui_tokio::Tokio::spawn_result is cancellation-safe: dropping the owning GPUI task
        // cancels the Tokio request as well, so superseded stream materializations do not keep
        // consuming bandwidth in the background.
        self.online_play_task.take();
    }

    fn cancel_online_preload_tasks(&mut self) {
        self.online_preload_generation = self.online_preload_generation.wrapping_add(1);
        self.online_audio_preload_task.take();
        self.online_asset_preload_task.take();
    }

    fn online_playlist_lookahead(
        &self,
        max_items: usize,
    ) -> Option<(
        crate::plugin::abi::PluginRoute,
        Vec<crate::plugin::abi::RemoteTrack>,
    )> {
        let queue = self.online_playlist_queue.as_ref()?;
        if queue.tracks.len() <= 1 || self.config.repeat == RepeatMode::One {
            return None;
        }

        let mut tracks = Vec::with_capacity(max_items.min(queue.tracks.len().saturating_sub(1)));
        for step in 1..=max_items {
            let raw_index = queue.current_index.saturating_add(step);
            let index = if raw_index < queue.tracks.len() {
                raw_index
            } else if self.config.repeat == RepeatMode::All {
                raw_index % queue.tracks.len()
            } else {
                break;
            };
            if index == queue.current_index {
                break;
            }
            tracks.push(queue.tracks[index].clone());
        }
        (!tracks.is_empty()).then(|| (queue.route.clone(), tracks))
    }

    fn schedule_online_playlist_prefetch(&mut self, cx: &mut Context<Self>) {
        self.cancel_online_preload_tasks();

        let Some((route, candidates)) =
            self.online_playlist_lookahead(ONLINE_ASSET_LOOKAHEAD)
        else {
            self.online_preloaded_tracks.clear();
            return;
        };
        let generation = self.online_preload_generation;

        // Artwork is tiny after image_cache normalization and already deduplicated by URL. Warm at
        // most two covers so the next rows/stage never wait for network.
        for remote in &candidates {
            if let Some(url) = remote.cover_url.as_deref() {
                crate::ui::image_cache::fetch_detached(url);
            }
        }

        // Lyrics are also bounded to the next two tracks. Run them concurrently, persist only the
        // canonical Host document, and never allocate future TrackIds merely to cache lyrics.
        let asset_route = route.clone();
        let asset_candidates = candidates.clone();
        let playback_cache = self.playback_cache.clone();
        let asset_task = Tokio::spawn_result(cx, async move {
            let mut joins = tokio::task::JoinSet::new();

            for remote in asset_candidates {
                let route = asset_route.clone();
                let cache = playback_cache.clone();
                joins.spawn(async move {
                    let _ = load_online_lyrics_cached_or_remote(
                        cache,
                        route,
                        remote.source,
                    )
                    .await?;
                    Ok::<(), anyhow::Error>(())
                });
            }

            while let Some(result) = joins.join_next().await {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        tracing::debug!(error = %error, "在线歌词预加载失败");
                    }
                    Err(error) => {
                        tracing::debug!(error = %error, "在线歌词预加载任务异常退出");
                    }
                }
            }
            Ok(())
        });
        self.online_asset_preload_task = Some(cx.spawn(async move |_this, _cx| {
            let _ = asset_task.await;
        }));

        // Full stream materialization is deliberately limited to exactly one track ahead. It starts
        // after a short dwell so rapid browsing does not download every skipped song. Once that next
        // track becomes current the pipeline advances and warms the following track.
        let next_remote = candidates[0].clone();
        let next_key = online_source_key(&route, &next_remote.source);
        self.online_preloaded_tracks
            .retain(|key, _| key == &next_key);
        if self.online_preloaded_tracks.contains_key(&next_key) {
            return;
        }

        let audio_route = route;
        self.online_audio_preload_task = Some(cx.spawn(async move |this, cx| {
            Timer::after(ONLINE_AUDIO_PRELOAD_DELAY).await;
            let should_start = this
                .update(cx, |app, _| {
                    app.online_preload_generation == generation
                        && matches!(
                            app.snapshot.state,
                            PlaybackState::Loading | PlaybackState::Playing
                        )
                })
                .unwrap_or(false);
            if !should_start {
                return;
            }

            let remote_for_prepare = next_remote.clone();
            let route_for_prepare = audio_route.clone();
            let task = Tokio::spawn_result(cx, async move {
                let frontend = crate::plugin::frontend::global()
                    .ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
                let result = frontend
                    .prepare_remote_track_for_route(
                        &route_for_prepare,
                        &remote_for_prepare,
                        None,
                    )
                    .await?;
                result
                    .value
                    .map(|prepared| prepared.into_track())
                    .ok_or_else(|| anyhow!("预加载下一首在线音频失败"))
            });
            let result = task.await;

            let _ = this.update(cx, |app, _| {
                if app.online_preload_generation != generation {
                    return;
                }
                if let Ok(track) = result {
                    app.online_preloaded_tracks.clear();
                    app.online_preloaded_tracks.insert(next_key, track);
                }
            });
        }));
    }

    pub(crate) fn toggle_play(&mut self, cx: &mut Context<Self>) {
        // Pause/resume is an O(1) UI command path. A transport click also supersedes any unresolved
        // online switch immediately instead of waiting for its network/materialization task.
        let cancelled_pending_online = self.online_track_buffering.is_some();
        if cancelled_pending_online {
            self.cancel_online_play_request();
            if self.snapshot.current_track.is_none() {
                self.cancel_online_preload_tasks();
                self.online_preloaded_tracks.clear();
                self.snapshot.state = PlaybackState::Stopped;
                cx.notify();
                return;
            }
        }
        if self.snapshot.current_track.is_none() {
            self.ensure_queue();
            if let Some(first_id) = self
                .config
                .queue
                .first()
                .copied()
                .or_else(|| self.library_track_ids.first().copied())
            {
                self.play_track(first_id, cx);
                return;
            }
        }

        let previous_state = self.snapshot.state;
        let (next_state, command) = match previous_state {
            PlaybackState::Playing | PlaybackState::Loading | PlaybackState::Buffering => {
                (PlaybackState::Paused, PlayerCommand::Pause)
            }
            PlaybackState::Paused | PlaybackState::Stopped | PlaybackState::Error => {
                (PlaybackState::Playing, PlayerCommand::Play)
            }
        };

        // React on the foreground turn. AudioEngine mirrors the intent through its atomic
        // optimistic state; the existing audible startup/delay path remains unchanged.
        self.snapshot.state = next_state;
        if matches!(
            previous_state,
            PlaybackState::Playing | PlaybackState::Loading | PlaybackState::Buffering
        ) {
            self.cancel_online_preload_tasks();
        }

        // Mini-player and Stage controls both provide local optimistic feedback. Do not rebuild
        // the root shell in the same input turn; the audio worker's SnapshotChanged ACK performs
        // the authoritative synchronization shortly after the non-blocking command enqueue.
        self.transport_root_notify_pending = true;

        if self.send(command) {
            self.save_config();
            if next_state == PlaybackState::Playing {
                self.schedule_online_playlist_prefetch(cx);
            }
        } else {
            self.transport_root_notify_pending = false;
            self.snapshot.state = previous_state;
            self.status = "音频输出不可用，请检查默认音频设备".into();
            cx.notify();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancel_online_play_request();
        self.cancel_online_preload_tasks();
        self.online_preloaded_tracks.clear();
        self.snapshot.state = PlaybackState::Stopped;
        self.snapshot.position_ms = 0;
        self.position_ms = 0;
        self.config.position_ms = 0;
        cx.notify();

        if self.send(PlayerCommand::Stop) {
            self.save_config();
        } else {
            self.status = "音频输出不可用，请检查默认音频设备".into();
            cx.notify();
        }
    }

    fn reset_progress_for_track_switch(&mut self) {
        if self.drag_target == Some(DragTarget::Progress) {
            self.drag_target = None;
            self.drag_progress_ratio = None;
            self.seeking = false;
        }
        self.snapshot.position_ms = 0;
        self.position_ms = 0;
        self.config.position_ms = 0;
    }

    pub(crate) fn record_recent_play(&mut self, track: &Track) {
        let track_id = track.id;
        let title_lower = track.title.trim().to_lowercase();
        let artist_lower = track.artist.trim().to_lowercase();
        let tracks = &self.tracks;
        let track_index_by_id = &self.track_index_by_id;
        let online_track_cache = &self.online_track_cache;
        self.recent_plays.retain(|id| {
            if *id == track_id {
                return false;
            }
            let local = track_index_by_id
                .get(id)
                .and_then(|index| tracks.get(*index));
            if let Some(t) = local.or_else(|| online_track_cache.get(id))
                && !title_lower.is_empty()
                && t.title.trim().to_lowercase() == title_lower
                && t.artist.trim().to_lowercase() == artist_lower
            {
                return false;
            }
            true
        });
        self.recent_plays.insert(0, track_id);
        self.recent_plays.truncate(100);
    }

    pub(crate) fn handle_track_ended(&mut self, cx: &mut Context<Self>) {
        if let Some(queue) = &mut self.online_playlist_queue {
            if !queue.tracks.is_empty() {
                let next_idx = match self.config.repeat {
                    RepeatMode::One => Some(queue.current_index),
                    RepeatMode::All => Some((queue.current_index + 1) % queue.tracks.len()),
                    RepeatMode::Off => {
                        if queue.current_index + 1 < queue.tracks.len() {
                            Some(queue.current_index + 1)
                        } else {
                            None
                        }
                    }
                };
                if let Some(idx) = next_idx {
                    queue.current_index = idx;
                    let track = queue.tracks[idx].clone();
                    let route = queue.route.clone();
                    self.play_online_remote_track(route, track, cx);
                    return;
                }
            }
        }

        self.next(cx);
    }

    pub(crate) fn next(&mut self, cx: &mut Context<Self>) {
        if self
            .online_playlist_queue
            .as_ref()
            .is_some_and(|queue| !queue.tracks.is_empty())
        {
            let next = {
                let queue = self
                    .online_playlist_queue
                    .as_mut()
                    .expect("online playlist queue checked above");
                let next_idx = match self.config.repeat {
                    RepeatMode::One => Some(queue.current_index),
                    RepeatMode::All => Some((queue.current_index + 1) % queue.tracks.len()),
                    RepeatMode::Off => (queue.current_index + 1 < queue.tracks.len())
                        .then_some(queue.current_index + 1),
                };
                next_idx.map(|next_idx| {
                    queue.current_index = next_idx;
                    (queue.route.clone(), queue.tracks[next_idx].clone())
                })
            };

            if let Some((route, next_track)) = next {
                self.play_online_remote_track(route, next_track, cx);
            } else {
                self.stop(cx);
            }
            return;
        }

        self.ensure_queue();
        let queue = self.config.queue.clone();
        if queue.is_empty() {
            return;
        }

        let curr_idx = self
            .config
            .current_track
            .and_then(|curr| queue.iter().position(|id| *id == curr));

        let next_idx = match (curr_idx, self.config.repeat) {
            (Some(pos), RepeatMode::One) => Some(pos),
            (Some(pos), RepeatMode::All) => Some((pos + 1) % queue.len()),
            (Some(pos), RepeatMode::Off) => {
                if pos + 1 < queue.len() {
                    Some(pos + 1)
                } else {
                    None
                }
            }
            (None, _) => Some(0),
        };

        let Some(target_idx) = next_idx else {
            self.stop(cx);
            return;
        };

        let target_id = queue[target_idx];
        if target_id < 0 {
            if let Some((route, remote)) = self.online_remote_tracks.get(&target_id).cloned() {
                self.play_online_remote_track(route, remote, cx);
                return;
            } else if let Some(track) = self.online_track_cache.get(&target_id).cloned() {
                self.play_prepared_remote_track(track, cx);
                return;
            }
        }

        self.play_track(target_id, cx);
    }

    pub(crate) fn previous(&mut self, cx: &mut Context<Self>) {
        if let Some(queue) = &mut self.online_playlist_queue {
            if !queue.tracks.is_empty() {
                if self.snapshot.position_ms >= 3_000 {
                    self.send(PlayerCommand::Seek(Duration::ZERO));
                    return;
                }
                let prev_idx = match self.config.repeat {
                    RepeatMode::One => queue.current_index,
                    RepeatMode::All => {
                        if queue.current_index > 0 {
                            queue.current_index - 1
                        } else {
                            queue.tracks.len().saturating_sub(1)
                        }
                    }
                    RepeatMode::Off => {
                        if queue.current_index > 0 {
                            queue.current_index - 1
                        } else {
                            0
                        }
                    }
                };
                queue.current_index = prev_idx;
                let prev_track = queue.tracks[prev_idx].clone();
                let route = queue.route.clone();
                self.play_online_remote_track(route, prev_track, cx);
                return;
            }
        }

        self.ensure_queue();
        let queue = self.config.queue.clone();
        if queue.is_empty() {
            return;
        }

        if self.snapshot.position_ms >= 3_000 {
            self.send(PlayerCommand::Seek(Duration::ZERO));
            return;
        }

        let curr_idx = self
            .config
            .current_track
            .and_then(|curr| queue.iter().position(|id| *id == curr));

        let prev_idx = match (curr_idx, self.config.repeat) {
            (Some(pos), RepeatMode::One) => Some(pos),
            (Some(pos), RepeatMode::All) => {
                if pos > 0 {
                    Some(pos - 1)
                } else {
                    Some(queue.len().saturating_sub(1))
                }
            }
            (Some(pos), RepeatMode::Off) => {
                if pos > 0 {
                    Some(pos - 1)
                } else {
                    Some(0)
                }
            }
            (None, _) => Some(0),
        };

        let Some(target_idx) = prev_idx else {
            return;
        };

        let target_id = queue[target_idx];
        if target_id < 0 {
            if let Some((route, remote)) = self.online_remote_tracks.get(&target_id).cloned() {
                self.play_online_remote_track(route, remote, cx);
                return;
            } else if let Some(track) = self.online_track_cache.get(&target_id).cloned() {
                self.play_prepared_remote_track(track, cx);
                return;
            }
        }

        self.play_track(target_id, cx);
    }

    pub(crate) fn play_track(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        self.cancel_online_play_request();
        self.cancel_online_preload_tasks();
        self.online_preloaded_tracks.clear();
        self.online_playlist_queue = None;
        let Some(engine) = self.engine.clone() else {
            self.status = "音频输出不可用，请检查默认音频设备".into();
            cx.notify();
            return;
        };

        if !self.queue_matches_tracks {
            let queue = self.library_track_ids.clone();
            if !engine.try_send(PlayerCommand::SetQueue(queue.clone())) {
                self.status = "音频命令队列繁忙，请稍后重试".into();
                cx.notify();
                return;
            }
            self.config.queue = queue;
            self.bump_ui_content_revision();
            self.queue_matches_tracks = true;
        }

        if engine.try_send(PlayerCommand::PlayTrack(track_id)) {
            self.reset_progress_for_track_switch();
            self.config.current_track = Some(track_id);
            if let Some(track) = self
                .track_index_by_id
                .get(&track_id)
                .and_then(|index| self.tracks.get(*index))
                .cloned()
            {
                // Present selected metadata immediately. Decoder/startup timing is deliberately
                // untouched; only the UI no longer waits for a structural engine snapshot.
                self.snapshot.current_track = Some(track.clone());
                self.snapshot.duration_ms = track.duration_ms;
                self.snapshot.state = PlaybackState::Loading;
                self.record_recent_play(&track);
            } else {
                self.recent_plays.retain(|id| *id != track_id);
                self.recent_plays.insert(0, track_id);
                self.recent_plays.truncate(100);
            }
            self.bump_ui_content_revision();
            self.status = "正在准备播放".into();
            self.save_config();
        } else {
            self.status = "音频命令队列繁忙，请稍后重试".into();
        }
        cx.notify();
    }

    pub(crate) fn add_to_queue(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        if !self.config.queue.contains(&track_id) {
            Arc::make_mut(&mut self.config.queue).push(track_id);
            self.bump_ui_content_revision();
            self.queue_matches_tracks = false;
            self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
            self.save_config();
            self.status = "已加入播放队列".into();
        }
        cx.notify();
    }

    pub(crate) fn insert_next_in_queue(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        let queue = Arc::make_mut(&mut self.config.queue);
        queue.retain(|id| *id != track_id);
        let insert_pos = if let Some(curr) = self.config.current_track {
            queue
                .iter()
                .position(|id| *id == curr)
                .map(|idx| idx + 1)
                .unwrap_or(0)
        } else {
            0
        };
        if insert_pos <= queue.len() {
            queue.insert(insert_pos, track_id);
        } else {
            queue.push(track_id);
        }
        self.bump_ui_content_revision();
        self.queue_matches_tracks = false;
        self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn ensure_track_for_remote(
        &mut self,
        route: &crate::plugin::abi::PluginRoute,
        remote: &crate::plugin::abi::RemoteTrack,
    ) -> Track {
        for (id, (r, s, _)) in &self.online_playback_meta {
            if r.provider_id == route.provider_id && s.source_id == remote.source.source_id {
                if let Some(track) = self.online_track_cache.get(id).cloned() {
                    self.online_remote_tracks
                        .insert(*id, (route.clone(), remote.clone()));
                    return track;
                }
            }
        }

        let track_id = crate::plugin::streaming::allocate_remote_track_id().unwrap_or(-100_000);
        let track = Track::new(crate::model::TrackData {
            id: track_id,
            path: std::path::PathBuf::from(format!(
                "remote://{}/{}",
                route.provider_id, remote.source.source_id
            )),
            title: remote.title.clone(),
            artist: if remote.artists.is_empty() {
                "未知艺术家".into()
            } else {
                remote.artists.join("/")
            },
            album: if remote.album.is_empty() {
                "未知专辑".into()
            } else {
                remote.album.clone()
            },
            year: None,
            genre: None,
            duration_ms: remote.duration_ms.unwrap_or(0),
            codec: "remote".into(),
            sample_rate: 44_100,
            channels: 2,
            artwork_key: None,
        });

        self.online_playback_meta.insert(
            track_id,
            (
                route.clone(),
                remote.source.clone(),
                remote.cover_url.clone(),
            ),
        );
        self.online_track_cache.insert(track_id, track.clone());
        self.online_remote_tracks
            .insert(track_id, (route.clone(), remote.clone()));

        if let Some(url) = &remote.cover_url {
            crate::ui::image_cache::fetch_detached(url);
        }

        track
    }

    pub(crate) fn remove_from_queue(&mut self, track_id: TrackId, cx: &mut Context<Self>) {
        let old_len = self.config.queue.len();
        Arc::make_mut(&mut self.config.queue).retain(|id| *id != track_id);
        if self.config.queue.len() != old_len {
            self.bump_ui_content_revision();
        }
        self.queue_matches_tracks = false;
        self.send(PlayerCommand::SetQueue(self.config.queue.clone()));
        self.save_config();
        cx.notify();
    }

    pub(crate) fn clear_queue(&mut self, cx: &mut Context<Self>) {
        if !self.config.queue.is_empty() {
            Arc::make_mut(&mut self.config.queue).clear();
            self.bump_ui_content_revision();
        }
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
                    Err(error) => this.status = format!("音频设备切换失败：{error:#}"),
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
        let query = self.search.clone();
        self.trigger_online_search(query, cx);
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
        let task = Tokio::spawn_result(cx, async move {
            let (tracks, engine_tracks, library_track_ids, track_index_by_id) =
                tokio::task::spawn_blocking(move || {
                    let tracks = library.tracks(None)?;
                    let library_track_ids =
                        Arc::new(tracks.iter().map(|track| track.id).collect::<Vec<_>>());
                    let track_index_by_id = tracks
                        .iter()
                        .enumerate()
                        .map(|(index, track)| (track.id, index))
                        .collect::<HashMap<_, _>>();
                    let engine_tracks = tracks.clone();
                    Ok::<_, anyhow::Error>((
                        tracks,
                        engine_tracks,
                        library_track_ids,
                        track_index_by_id,
                    ))
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))??;
            Ok::<_, anyhow::Error>((
                tracks,
                engine_tracks,
                library_track_ids,
                track_index_by_id,
            ))
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let query_result = task.await;
            this.update(cx, |this, cx| {
                if request != this.library_refresh_request {
                    return;
                }
                match query_result {
                    Ok((tracks, engine_tracks, library_track_ids, track_index_by_id)) => {
                        this.tracks = tracks;
                        this.library_track_ids = library_track_ids;
                        this.track_index_by_id = track_index_by_id;
                        this.bump_ui_content_revision();
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
        // Keep UI interactions allocation-light. The 2 s maintenance loop coalesces changes and
        // performs the snapshot clone/disk handoff later; Drop still flushes a dirty configuration.
        self.config_save_dirty = true;
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
            config.last_played_track = Some(crate::settings::SavedTrackInfo::from(track));
        }
        self.config_save_dirty = false;
        self.last_config_save_at = std::time::Instant::now();
        let _ = crate::runtime::spawn_blocking(move || {
            let _ = store.save(&config);
        });
    }

    fn start_runtime_events(&mut self, cx: &mut Context<Self>) {
        if self.runtime_events_started {
            return;
        }
        self.runtime_events_started = true;

        let (_discard_library_tx, discard_library_rx) = std::sync::mpsc::channel();
        let library_events = std::mem::replace(&mut self.library_update_rx, discard_library_rx);
        let (_discard_media_tx, discard_media_rx) = std::sync::mpsc::channel();
        let media_events = std::mem::replace(&mut self.media_event_rx, discard_media_rx);
        app_runtime_events::attach_root_sources(self, library_events, media_events, cx);
        self.ensure_system_media_async(cx);
        self.update_system_media_async(cx);
    }

    fn ensure_plugin_sessions_restored(&mut self, cx: &mut Context<Self>) {
        if self.sessions_restored {
            return;
        }
        self.sessions_restored = true;
        let task = Tokio::spawn_result(cx, async move {
            if let Some(frontend) = crate::plugin::frontend::global() {
                let results = frontend.restore_all_pending_accounts().await;
                let restored = results
                    .iter()
                    .filter(|(_, res)| matches!(res, Ok(true)))
                    .count();
                Ok(restored)
            } else {
                Ok(0)
            }
        });
        cx.spawn(async move |this, cx| {
            let _ = task.await;
            let _ = this.update(cx, |app, cx| {
                app.sync_online_service_state(cx);
                if app.online_authenticated {
                    app.refresh_online_recommendations(cx);
                }
            });
        })
        .detach();
    }

    fn persist_online_playback_assets(
        &self,
        track: &Track,
        route: &crate::plugin::abi::PluginRoute,
        source: &crate::plugin::abi::SourceTrackRef,
        remote: &crate::plugin::abi::RemoteTrack,
        cover_url: Option<&str>,
    ) {
        let Some(cache) = self.playback_cache.clone() else {
            return;
        };
        let track = track.clone();
        let route = route.clone();
        let source = source.clone();
        let remote = remote.clone();
        let cover_url = cover_url.map(str::to_owned);
        let _ = crate::runtime::spawn_blocking(move || {
            if let Err(error) = cache.store_playback(
                &track,
                &route,
                &source,
                &remote,
                cover_url.as_deref(),
            ) {
                tracing::warn!(error = %error, "持久化在线播放资源元数据失败");
            }
        });
    }

    pub(crate) fn play_prepared_remote_track(
        &mut self,
        track: Track,
        cx: &mut Context<Self>,
    ) -> bool {
        let track_id = track.id;
        let accepted = self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.try_play_transient_track(track.clone()));
        if !accepted {
            self.status = "音频命令队列繁忙，请稍后重试".into();
            cx.notify();
            return false;
        }

        self.online_track_cache.insert(track_id, track.clone());
        self.reset_progress_for_track_switch();
        // Keep persistence/UI identity coherent before the audio bridge posts its structural
        // SnapshotChanged event. This is metadata-only and performs no decoder/file/network work.
        self.snapshot.current_track = Some(track.clone());
        self.snapshot.duration_ms = track.duration_ms;
        self.snapshot.state = PlaybackState::Loading;
        self.config.current_track = Some(track_id);
        self.record_recent_play(&track);
        self.bump_ui_content_revision();
        self.status = "正在准备播放".into();
        self.save_config();
        cx.notify();
        true
    }

    pub(crate) fn play_online_remote_track(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        remote: crate::plugin::abi::RemoteTrack,
        cx: &mut Context<Self>,
    ) {
        let title = remote.title.clone();
        let artist = remote.artists.join("/");
        let cover_url = remote.cover_url.clone();
        let source = remote.source.clone();
        let source_key = online_source_key(&route, &source);

        // Reuse the single bounded look-ahead materialization before starting any new network work.
        let preloaded = self.online_preloaded_tracks.remove(&source_key);
        self.online_preloaded_tracks.clear();
        self.cancel_online_play_request();
        self.cancel_online_preload_tasks();
        self.online_play_request_generation =
            self.online_play_request_generation.wrapping_add(1);
        let generation = self.online_play_request_generation;

        self.online_track_buffering = Some(source.source_id.clone());
        self.status = format!("正在切换在线音频：{title} · {artist}");
        // Keep PlayerSnapshot authoritative to the currently audible AudioEngine state while the
        // next remote source is still being resolved/materialized. Buffering is a separate request
        // state, not a transport state for audio that has not been accepted yet.
        self.notify_online_playlist_surface(cx);

        if let Some(cover_url) = &cover_url {
            crate::ui::image_cache::fetch_detached(cover_url);
        }

        if let Some(track) = preloaded {
            let track_id = track.id;
            let track_for_cache = track.clone();
            self.online_track_buffering = None;
            if self.play_prepared_remote_track(track, cx) {
                self.persist_online_playback_assets(
                    &track_for_cache,
                    &route,
                    &source,
                    &remote,
                    cover_url.as_deref(),
                );
                self.online_playback_meta.insert(
                    track_id,
                    (route.clone(), source.clone(), cover_url.clone()),
                );
                self.online_remote_tracks
                    .insert(track_id, (route.clone(), remote.clone()));
                self.status = format!("正在播放在线音频：{title} · {artist}");
                if let Some(url) = cover_url {
                    self.fetch_online_artwork_for_track(track_id, url, cx);
                }
                self.fetch_online_lyrics_for_track(track_id, route, source, cx);
                self.schedule_online_playlist_prefetch(cx);
            }
            return;
        }

        let route_for_prep = route.clone();
        let remote_for_prep = remote.clone();
        let remote_for_cache = remote.clone();
        let audio_task = Tokio::spawn_result(cx, async move {
            let frontend =
                crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
            let result = frontend
                .prepare_remote_track_for_route(&route_for_prep, &remote_for_prep, None)
                .await?;
            result
                .value
                .map(|prepared| prepared.into_track())
                .ok_or_else(|| anyhow!("物化在线音频流失败"))
        });

        // Lyrics are independent of stream materialization. Start them at the same time instead of
        // serially waiting for a complete audio download. The task is owned by online_play_task, so
        // replacing/stopping playback cancels both requests together.
        let lyrics_route = route.clone();
        let lyrics_source = source.clone();
        let lyrics_cache = self.playback_cache.clone();
        let lyrics_task = Tokio::spawn_result(cx, async move {
            load_online_lyrics_cached_or_remote(lyrics_cache, lyrics_route, lyrics_source).await
        });

        self.online_play_task = Some(cx.spawn(async move |this, cx| {
            let played_track_id = match audio_task.await {
                Ok(track) => this
                    .update(cx, |app, cx| {
                        if app.online_play_request_generation != generation {
                            return None;
                        }
                        app.online_track_buffering = None;
                        let track_id = track.id;
                        let track_for_cache = track.clone();
                        if !app.play_prepared_remote_track(track, cx) {
                            return None;
                        }

                        app.persist_online_playback_assets(
                            &track_for_cache,
                            &route,
                            &source,
                            &remote_for_cache,
                            cover_url.as_deref(),
                        );
                        app.online_playback_meta.insert(
                            track_id,
                            (route.clone(), source.clone(), cover_url.clone()),
                        );
                        app.online_remote_tracks
                            .insert(track_id, (route.clone(), remote_for_cache.clone()));
                        app.status = format!("正在播放在线音频：{title} · {artist}");

                        if let Some(url) = cover_url.clone() {
                            app.fetch_online_artwork_for_track(track_id, url, cx);
                        }
                        app.schedule_online_playlist_prefetch(cx);
                        Some(track_id)
                    })
                    .ok()
                    .flatten(),
                Err(error) => {
                    let _ = this.update(cx, |app, cx| {
                        if app.online_play_request_generation != generation {
                            return;
                        }
                        app.online_track_buffering = None;
                        app.status = format!("播放失败：{error:#}");
                        cx.notify();
                    });
                    None
                }
            };

            let Some(track_id) = played_track_id else {
                // Dropping an unfinished gpui_tokio task cancels its Tokio future.
                drop(lyrics_task);
                return;
            };

            if let Ok(Some(document)) = lyrics_task.await {
                let _ = this.update(cx, |app, cx| {
                    if app.online_play_request_generation != generation {
                        return;
                    }
                    let same_source = app
                        .online_playback_meta
                        .get(&track_id)
                        .is_some_and(|(_, current_source, _)| current_source == &source);
                    if same_source {
                        app.cache_lyrics(track_id, document);
                        app.sync_lyrics_surfaces(track_id, cx);
                    }
                });
            }
        }));
    }

    pub(crate) fn fetch_online_artwork_for_track(
        &mut self,
        track_id: TrackId,
        url: String,
        cx: &mut Context<Self>,
    ) {
        if self.artworks.contains_key(&track_id) {
            return;
        }
        let artwork_cache = self.artwork_cache.clone();
        let target_url = url.clone();
        let task = Tokio::spawn_result(cx, async move {
            if let Some(cache) = artwork_cache.clone() {
                let key = target_url.clone();
                let cached = tokio::task::spawn_blocking(move || cache.load_key(&key))
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))??;
                if let Some(art) = cached {
                    return Ok((art.png, art.blurred_png, art.palette));
                }
            }

            let cached_url = target_url.clone();
            let cached_bytes = tokio::task::spawn_blocking(move || {
                crate::ui::image_cache::load_disk_cached(&cached_url)
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            if let Some(bytes) = cached_bytes {
                if let Some(cache) = artwork_cache.clone() {
                    let key = target_url.clone();
                    let cache_bytes = bytes.clone();
                    let art =
                        tokio::task::spawn_blocking(move || cache.store(&key, cache_bytes.as_ref()))
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))??;
                    return Ok((art.png, art.blurred_png, art.palette));
                }

                return tokio::task::spawn_blocking(move || {
                    let img = image::load_from_memory(bytes.as_ref())?;
                    let small = img.thumbnail(img.width().min(768), img.height().min(768));
                    let mut png_buf = Vec::new();
                    small.write_to(
                        &mut std::io::Cursor::new(&mut png_buf),
                        image::ImageFormat::Png,
                    )?;
                    let blurred = crate::artwork::generate_blurred_artwork(&small)
                        .unwrap_or_else(|_| png_buf.clone());
                    let pal = crate::artwork::extract_palette(&small);
                    Ok::<_, anyhow::Error>((png_buf, blurred, pal))
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            }

            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()?;
            let bytes = client
                .get(&target_url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?
                .to_vec();

            if let Some(cache) = artwork_cache {
                let key = target_url;
                let art = tokio::task::spawn_blocking(move || cache.store(&key, &bytes))
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))??;
                Ok((art.png, art.blurred_png, art.palette))
            } else {
                tokio::task::spawn_blocking(move || {
                    let img = image::load_from_memory(&bytes)?;
                    let small = img.thumbnail(img.width().min(768), img.height().min(768));
                    let mut png_buf = Vec::new();
                    small.write_to(
                        &mut std::io::Cursor::new(&mut png_buf),
                        image::ImageFormat::Png,
                    )?;
                    let blurred = crate::artwork::generate_blurred_artwork(&small)
                        .unwrap_or_else(|_| png_buf.clone());
                    let pal = crate::artwork::extract_palette(&small);
                    Ok::<_, anyhow::Error>((png_buf, blurred, pal))
                })
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?
            }
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            if let Ok((png, blurred, pal)) = task.await {
                let _ = this.update(cx, |app, cx| {
                    app.set_artwork_parts(track_id, png, Some(blurred), Some(pal));
                    cx.notify();
                });
            }
            Ok(())
        })
        .detach();
    }

    pub(crate) fn fetch_online_lyrics_for_track(
        &mut self,
        track_id: TrackId,
        route: crate::plugin::abi::PluginRoute,
        source: crate::plugin::abi::SourceTrackRef,
        cx: &mut Context<Self>,
    ) {
        if self.lyrics.contains_key(&track_id) {
            return;
        }
        let playback_cache = self.playback_cache.clone();
        let task = Tokio::spawn_result(cx, async move {
            load_online_lyrics_cached_or_remote(playback_cache, route, source).await
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            if let Ok(Some(document)) = task.await {
                let _ = this.update(cx, |app, cx| {
                    app.cache_lyrics(track_id, document);
                    app.sync_lyrics_surfaces(track_id, cx);
                });
            }
            Ok(())
        })
        .detach();
    }

    pub(crate) fn load_and_play_online_playlist(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        collection: crate::plugin::abi::MediaCollectionRef,
        playlist_title: String,
        cx: &mut Context<Self>,
    ) {
        self.status = "正在加载歌单歌曲...".into();
        cx.notify();

        let task = Tokio::spawn_result(cx, async move {
            let frontend =
                crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
            let tracks = frontend
                .collection_tracks(&route, &collection, 0, 50)
                .await?;
            Ok((route, tracks))
        });

        cx.spawn(async move |this, cx| match task.await {
            Ok((route, tracks)) => {
                if let Some(first) = tracks.first() {
                    let first_title = first.title.clone();
                    let _ = this.update(cx, |app, cx| {
                        app.status = format!("正在播放歌单，首曲：{first_title}");
                        app.play_online_playlist_track(
                            route,
                            playlist_title,
                            tracks,
                            0,
                            cx,
                        );
                    });
                }
            }
            Err(err) => {
                let _ = this.update(cx, |app, cx| {
                    app.status = format!("加载歌单失败：{err:#}");
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn prefetch_online_playlist_images(
        data: &OnlinePlaylistViewData,
        cx: &mut Context<Self>,
    ) {
        let mut urls = Vec::with_capacity(data.tracks.len().saturating_add(1));
        if let Some(url) = data.cover_url.as_ref().filter(|url| !url.trim().is_empty()) {
            urls.push(url.clone());
        }
        urls.extend(
            data.tracks
                .iter()
                .filter_map(|track| track.cover_url.as_ref())
                .filter(|url| !url.trim().is_empty())
                .cloned(),
        );
        crate::ui::image_cache::prefetch_urls(urls, cx);
    }

    pub(crate) fn notify_home_surface(&mut self, cx: &mut Context<Self>) {
        if self.page == AppPage::Home
            && let Some(page) = self.home_page.clone()
        {
            page.update(cx, |_, page_cx| page_cx.notify());
        }
    }

    pub(crate) fn notify_library_surface(&mut self, cx: &mut Context<Self>) {
        if self.page == AppPage::Library
            && let Some(page) = self.library_page.clone()
        {
            page.update(cx, |_, page_cx| page_cx.notify());
        }
    }

    fn notify_current_content_surface(&mut self, cx: &mut Context<Self>) {
        match self.page {
            AppPage::Home => self.notify_home_surface(cx),
            AppPage::Library => self.notify_library_surface(cx),
            AppPage::OnlinePlaylist => self.notify_online_playlist_surface(cx),
            _ => {}
        }
    }

    fn notify_online_playlist_surface(&mut self, cx: &mut Context<Self>) {
        if self.page == AppPage::OnlinePlaylist
            && let Some(page) = self.online_playlist_page.clone()
        {
            page.update(cx, |_, page_cx| page_cx.notify());
            return;
        }
        // The page entity is materialized by the root. Fall back only during the first navigation
        // frame; steady-state playlist interactions stay isolated to OnlinePlaylistPage.
        cx.notify();
    }

    pub(crate) fn show_online_playlist_detail(
        &mut self,
        data: OnlinePlaylistViewData,
        cx: &mut Context<Self>,
    ) {
        Self::prefetch_online_playlist_images(&data, cx);
        self.active_online_playlist = Some(data);
        self.page = AppPage::OnlinePlaylist;
        cx.notify();
    }

    pub(crate) fn load_and_show_online_playlist(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        collection: crate::plugin::abi::MediaCollectionRef,
        title: String,
        subtitle: String,
        cover_url: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let cache_key = collection.source_id.clone();
        if let Some(cached) = self.online_playlist_cache.get(&cache_key).cloned() {
            self.show_online_playlist_detail(cached, cx);
            return;
        }

        self.show_online_playlist_detail(
            OnlinePlaylistViewData {
                title: title.clone(),
                subtitle: subtitle.clone(),
                cover_url: cover_url.clone(),
                tracks: Vec::new(),
                loading: true,
                route: route.clone(),
            },
            cx,
        );

        let c_key = cache_key.clone();
        let c_title = title.clone();
        let c_subtitle = subtitle.clone();
        let c_cover = cover_url.clone();
        let task = Tokio::spawn_result(cx, async move {
            let frontend =
                crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
            let tracks = frontend
                .collection_tracks(&route, &collection, 0, 100)
                .await?;
            Ok((route, tracks))
        });

        cx.spawn(async move |this, cx| match task.await {
            Ok((route, tracks)) => {
                let _ = this.update(cx, |app, cx| {
                    let view_data = OnlinePlaylistViewData {
                        title: c_title,
                        subtitle: c_subtitle,
                        cover_url: c_cover,
                        tracks,
                        loading: false,
                        route,
                    };
                    Self::prefetch_online_playlist_images(&view_data, cx);
                    app.online_playlist_cache.insert(c_key, view_data.clone());
                    if let Some(data) = &mut app.active_online_playlist {
                        if data.title == view_data.title {
                            *data = view_data;
                        }
                    }
                    app.notify_online_playlist_surface(cx);
                });
            }
            Err(err) => {
                let _ = this.update(cx, |app, cx| {
                    if let Some(data) = &mut app.active_online_playlist {
                        data.loading = false;
                    }
                    app.status = format!("加载歌单失败：{err:#}");
                    app.notify_online_playlist_surface(cx);
                });
            }
        })
        .detach();
    }

    pub(crate) fn show_daily_recommendations_playlist(&mut self, cx: &mut Context<Self>) {
        let route = self
            .online_route
            .clone()
            .unwrap_or_else(crate::ui::home::default_netease_route);
        let tracks = self.online_daily_tracks.clone();
        let cover_url = tracks.first().and_then(|t| t.cover_url.clone());
        let loading =
            self.online_recommendations_loading || (tracks.is_empty() && self.online_authenticated);
        self.show_online_playlist_detail(
            OnlinePlaylistViewData {
                title: "每日歌曲推荐".into(),
                subtitle: "根据你的音乐口味生成，每日 6:00 更新".into(),
                cover_url,
                tracks,
                loading,
                route,
            },
            cx,
        );
        if self.online_daily_tracks.is_empty() && self.online_authenticated {
            self.refresh_online_recommendations(cx);
        }
    }

    pub(crate) fn load_and_show_user_favorite_playlist(&mut self, cx: &mut Context<Self>) {
        if !self.online_authenticated {
            self.status = "请先登录网易云音乐以查看我喜欢的音乐".into();
            self.open_modal(super::components::modal::GlobalModal::ServiceAuth, cx);
            return;
        }

        if let Some(cached) = self.online_playlist_cache.get("user_favorite").cloned() {
            self.show_online_playlist_detail(cached, cx);
            return;
        }

        let route = self
            .online_route
            .clone()
            .unwrap_or_else(crate::ui::home::default_netease_route);
        self.show_online_playlist_detail(
            OnlinePlaylistViewData {
                title: "我喜欢的音乐".into(),
                subtitle: "云端红心收藏歌曲".into(),
                cover_url: None,
                tracks: Vec::new(),
                loading: true,
                route: route.clone(),
            },
            cx,
        );

        let task = Tokio::spawn_result(cx, async move {
            let frontend =
                crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
            let fanout = frontend
                .playlists(&crate::plugin::abi::RoutingPolicy::default())
                .await?;
            for batch in fanout.batches {
                if let Some(first_pl) = batch.playlists.first() {
                    let col = crate::plugin::abi::MediaCollectionRef {
                        provider_id: batch.route.provider_id.clone(),
                        kind: crate::plugin::abi::MediaCollectionKind::Playlist,
                        source_id: first_pl.source_id.clone(),
                    };
                    let tracks = frontend
                        .collection_tracks(&batch.route, &col, 0, 100)
                        .await?;
                    return Ok((
                        batch.route,
                        first_pl.name.clone(),
                        first_pl.cover_url.clone(),
                        tracks,
                    ));
                }
            }
            Err(anyhow!("未找到用户我喜欢的音乐歌单"))
        });

        cx.spawn(async move |this, cx| match task.await {
            Ok((route, name, cover_url, tracks)) => {
                let _ = this.update(cx, |app, cx| {
                    let view_data = OnlinePlaylistViewData {
                        title: name,
                        subtitle: format!("共 {} 首歌曲", tracks.len()),
                        cover_url,
                        tracks,
                        loading: false,
                        route,
                    };
                    Self::prefetch_online_playlist_images(&view_data, cx);
                    app.online_playlist_cache
                        .insert("user_favorite".to_string(), view_data.clone());
                    if let Some(data) = &mut app.active_online_playlist {
                        if data.title == "我喜欢的音乐" || data.title == view_data.title {
                            *data = view_data;
                        }
                    }
                    cx.notify();
                });
            }
            Err(err) => {
                let _ = this.update(cx, |app, cx| {
                    if let Some(data) = &mut app.active_online_playlist {
                        data.loading = false;
                    }
                    app.status = format!("获取我喜欢的音乐失败：{err:#}");
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn load_and_show_cloud_library(&mut self, cx: &mut Context<Self>) {
        if !self.online_authenticated {
            self.status = "请先登录网易云音乐以查看云盘".into();
            self.open_modal(super::components::modal::GlobalModal::ServiceAuth, cx);
            return;
        }

        let cache_key = "netease_cloud_library".to_string();
        if let Some(cached) = self.online_playlist_cache.get(&cache_key).cloned() {
            self.show_online_playlist_detail(cached, cx);
            return;
        }

        let route = self
            .online_route
            .clone()
            .unwrap_or_else(crate::ui::home::default_netease_route);

        self.show_online_playlist_detail(
            OnlinePlaylistViewData {
                title: "我的音乐网盘".into(),
                subtitle: "网易云音乐云盘曲目".into(),
                cover_url: None,
                tracks: Vec::new(),
                loading: true,
                route: route.clone(),
            },
            cx,
        );

        let task = Tokio::spawn_result(cx, async move {
            let frontend =
                crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
            let fanout = frontend
                .cloud_library(0, 100, &crate::plugin::abi::RoutingPolicy::default())
                .await?;
            let mut all_tracks = Vec::new();
            let mut final_route = route;
            for batch in fanout.batches {
                final_route = batch.route;
                all_tracks.extend(batch.tracks);
            }
            Ok((final_route, all_tracks))
        });

        cx.spawn(async move |this, cx| match task.await {
            Ok((route, tracks)) => {
                let _ = this.update(cx, |app, cx| {
                    let view_data = OnlinePlaylistViewData {
                        title: "我的音乐网盘".into(),
                        subtitle: format!("共收录 {} 首云盘曲目", tracks.len()),
                        cover_url: None,
                        tracks,
                        loading: false,
                        route,
                    };
                    Self::prefetch_online_playlist_images(&view_data, cx);
                    app.online_playlist_cache
                        .insert("netease_cloud_library".to_string(), view_data.clone());
                    if let Some(data) = &mut app.active_online_playlist {
                        if data.title == "我的音乐网盘" {
                            *data = view_data;
                        }
                    }
                    cx.notify();
                });
            }
            Err(err) => {
                let _ = this.update(cx, |app, cx| {
                    if let Some(data) = &mut app.active_online_playlist {
                        data.loading = false;
                    }
                    app.status = format!("加载云盘失败：{err:#}");
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn play_online_playlist_track(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        playlist_title: String,
        tracks: Vec<crate::plugin::abi::RemoteTrack>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        self.play_online_playlist_track_shared(route, playlist_title, tracks.into(), index, cx);
    }

    pub(crate) fn play_online_playlist_track_shared(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        playlist_title: String,
        tracks: Arc<[crate::plugin::abi::RemoteTrack]>,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        if let Some(track) = tracks.get(index).cloned() {
            self.online_playlist_queue = Some(OnlinePlaylistQueue {
                route: route.clone(),
                playlist_title,
                tracks,
                current_index: index,
            });
            self.play_online_remote_track(route, track, cx);
        }
    }

    pub(crate) fn play_online_playlist_all(
        &mut self,
        route: crate::plugin::abi::PluginRoute,
        playlist_title: String,
        tracks: Vec<crate::plugin::abi::RemoteTrack>,
        first: crate::plugin::abi::RemoteTrack,
        cx: &mut Context<Self>,
    ) {
        let tracks: Arc<[crate::plugin::abi::RemoteTrack]> = tracks.into();
        let index = tracks
            .iter()
            .position(|t| t.source.source_id == first.source.source_id)
            .unwrap_or(0);
        self.play_online_playlist_track_shared(route, playlist_title, tracks, index, cx);
    }

    pub(crate) fn open_context_menu(
        &mut self,
        pos: gpui::Point<gpui::Pixels>,
        title: String,
        target: super::components::modal::ContextMenuTarget,
        cx: &mut Context<Self>,
    ) {
        use super::components::modal::{
            ContextMenuAction, ContextMenuData, ContextMenuItem, ContextMenuTarget,
        };
        let items = match &target {
            ContextMenuTarget::DailyRecommendations | ContextMenuTarget::OnlinePlaylist { .. } => {
                vec![
                    ContextMenuItem {
                        label: "查看列表".into(),
                        icon: icon!(eye),
                        action: ContextMenuAction::ViewList,
                    },
                    ContextMenuItem {
                        label: "播放".into(),
                        icon: icon!(play),
                        action: ContextMenuAction::Play,
                    },
                    ContextMenuItem {
                        label: "下一首播放".into(),
                        icon: icon!(list_plus),
                        action: ContextMenuAction::PlayNext,
                    },
                    ContextMenuItem {
                        label: "添加到播放清单".into(),
                        icon: icon!(list_music),
                        action: ContextMenuAction::AddToQueue,
                    },
                    ContextMenuItem {
                        label: "下载全部".into(),
                        icon: icon!(download),
                        action: ContextMenuAction::DownloadAll,
                    },
                ]
            }
            ContextMenuTarget::OnlineTrack { .. } => vec![
                ContextMenuItem {
                    label: "播放".into(),
                    icon: icon!(play),
                    action: ContextMenuAction::Play,
                },
                ContextMenuItem {
                    label: "下一首播放".into(),
                    icon: icon!(list_plus),
                    action: ContextMenuAction::PlayNext,
                },
                ContextMenuItem {
                    label: "添加到播放清单".into(),
                    icon: icon!(list_music),
                    action: ContextMenuAction::AddToQueue,
                },
                ContextMenuItem {
                    label: "下载".into(),
                    icon: icon!(download),
                    action: ContextMenuAction::DownloadAll,
                },
            ],
        };

        self.open_modal(
            super::components::modal::GlobalModal::ContextMenu(Box::new(ContextMenuData {
                position: pos,
                title,
                items,
                target,
            })),
            cx,
        );
    }

    pub(crate) fn execute_context_menu_action(
        &mut self,
        action: &super::components::modal::ContextMenuAction,
        target: &super::components::modal::ContextMenuTarget,
        cx: &mut Context<Self>,
    ) {
        use super::components::modal::{ContextMenuAction, ContextMenuTarget};
        match (action, target) {
            (ContextMenuAction::ViewList, ContextMenuTarget::DailyRecommendations) => {
                self.show_daily_recommendations_playlist(cx);
            }
            (
                ContextMenuAction::ViewList,
                ContextMenuTarget::OnlinePlaylist {
                    route,
                    collection,
                    title,
                    subtitle,
                    cover_url,
                },
            ) => {
                self.load_and_show_online_playlist(
                    route.clone(),
                    collection.clone(),
                    title.clone(),
                    subtitle.clone(),
                    cover_url.clone(),
                    cx,
                );
            }
            (ContextMenuAction::Play, ContextMenuTarget::DailyRecommendations) => {
                if !self.online_daily_tracks.is_empty() {
                    let route = self
                        .online_route
                        .clone()
                        .unwrap_or_else(crate::ui::home::default_netease_route);
                    self.play_online_playlist_track(
                        route,
                        "每日歌曲推荐".into(),
                        self.online_daily_tracks.clone(),
                        0,
                        cx,
                    );
                } else {
                    self.refresh_online_recommendations(cx);
                }
            }
            (
                ContextMenuAction::Play,
                ContextMenuTarget::OnlinePlaylist {
                    route,
                    collection,
                    title,
                    ..
                },
            ) => {
                self.load_and_play_online_playlist(
                    route.clone(),
                    collection.clone(),
                    title.clone(),
                    cx,
                );
            }
            (ContextMenuAction::Play, ContextMenuTarget::OnlineTrack { route, track }) => {
                self.play_online_remote_track(route.clone(), track.clone(), cx);
            }
            (ContextMenuAction::PlayNext, ContextMenuTarget::DailyRecommendations) => {
                let route = self
                    .online_route
                    .clone()
                    .unwrap_or_else(crate::ui::home::default_netease_route);
                let daily_tracks = self.online_daily_tracks.clone();
                let mut added_ids = Vec::new();
                for t in &daily_tracks {
                    let prepared = self.ensure_track_for_remote(&route, t);
                    added_ids.push(prepared.id);
                }
                for id in added_ids.into_iter().rev() {
                    self.insert_next_in_queue(id, cx);
                }
                self.status = "已将每日推荐添加到下一首播放".into();
                cx.notify();
            }
            (ContextMenuAction::AddToQueue, ContextMenuTarget::DailyRecommendations) => {
                let route = self
                    .online_route
                    .clone()
                    .unwrap_or_else(crate::ui::home::default_netease_route);
                let daily_tracks = self.online_daily_tracks.clone();
                for t in &daily_tracks {
                    let prepared = self.ensure_track_for_remote(&route, t);
                    self.add_to_queue(prepared.id, cx);
                }
                self.status = "已将每日推荐添加到播放清单".into();
                cx.notify();
            }
            (
                ContextMenuAction::PlayNext,
                ContextMenuTarget::OnlinePlaylist {
                    route,
                    collection,
                    title,
                    ..
                },
            ) => {
                let c_title = title.clone();
                let c_route = route.clone();
                let c_col = collection.clone();
                let task = Tokio::spawn_result(cx, async move {
                    let frontend = crate::plugin::frontend::global()
                        .ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
                    let tracks = frontend.collection_tracks(&c_route, &c_col, 0, 100).await?;
                    Ok((c_route, tracks))
                });
                cx.spawn(async move |this, cx| {
                    if let Ok((r, tracks)) = task.await {
                        let _ = this.update(cx, |app, cx| {
                            let mut added_ids = Vec::new();
                            for t in &tracks {
                                let prep = app.ensure_track_for_remote(&r, t);
                                added_ids.push(prep.id);
                            }
                            for id in added_ids.into_iter().rev() {
                                app.insert_next_in_queue(id, cx);
                            }
                            app.status = format!("已将歌单《{c_title}》添加到下一首播放");
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            (
                ContextMenuAction::AddToQueue,
                ContextMenuTarget::OnlinePlaylist {
                    route,
                    collection,
                    title,
                    ..
                },
            ) => {
                let c_title = title.clone();
                let c_route = route.clone();
                let c_col = collection.clone();
                let task = Tokio::spawn_result(cx, async move {
                    let frontend = crate::plugin::frontend::global()
                        .ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
                    let tracks = frontend.collection_tracks(&c_route, &c_col, 0, 100).await?;
                    Ok((c_route, tracks))
                });
                cx.spawn(async move |this, cx| {
                    if let Ok((r, tracks)) = task.await {
                        let _ = this.update(cx, |app, cx| {
                            for t in &tracks {
                                let prep = app.ensure_track_for_remote(&r, t);
                                app.add_to_queue(prep.id, cx);
                            }
                            app.status = format!("已将歌单《{c_title}》添加到播放清单");
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            (ContextMenuAction::PlayNext, ContextMenuTarget::OnlineTrack { route, track }) => {
                let prepared = self.ensure_track_for_remote(route, track);
                self.insert_next_in_queue(prepared.id, cx);
                self.status = format!("已将《{}》添加到下一首播放", track.title);
                cx.notify();
            }
            (ContextMenuAction::AddToQueue, ContextMenuTarget::OnlineTrack { route, track }) => {
                let prepared = self.ensure_track_for_remote(route, track);
                self.add_to_queue(prepared.id, cx);
                self.status = format!("已将《{}》添加到播放清单", track.title);
                cx.notify();
            }
            (ContextMenuAction::DownloadAll, ContextMenuTarget::DailyRecommendations) => {
                self.status = "每日推荐已加入离线下载队列".into();
                cx.notify();
            }
            (ContextMenuAction::DownloadAll, ContextMenuTarget::OnlinePlaylist { title, .. }) => {
                self.status = format!("歌单《{title}》已加入离线下载队列");
                cx.notify();
            }
            (ContextMenuAction::DownloadAll, ContextMenuTarget::OnlineTrack { track, .. }) => {
                self.status = format!("《{}》已加入离线下载队列", track.title);
                cx.notify();
            }
            _ => {}
        }
    }

    pub(crate) fn ensure_search_input(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<crate::ui::components::input::HostTextInput> {
        if let Some(input) = &self.search_input {
            return input.clone();
        }
        crate::ui::components::input::ensure_initialized(cx);
        let parent = cx.entity().downgrade();
        let parent_commit = parent.clone();
        let commit: crate::ui::components::input::HostTextInputCommitHandler =
            std::rc::Rc::new(move |val: String, _window, cx| {
                if let Some(parent) = parent_commit.upgrade() {
                    parent.update(cx, |this, cx| {
                        this.search = val.clone();
                        this.trigger_online_search(val, cx);
                        if this.page != AppPage::Library {
                            this.page = AppPage::Library;
                        }
                        cx.notify();
                    });
                }
            });
        let on_change: crate::ui::components::input::HostTextInputCommitHandler =
            std::rc::Rc::new(move |val: String, _window, cx| {
                if let Some(parent) = parent.upgrade() {
                    parent.update(cx, |this, cx| {
                        this.search = val.clone();
                        this.trigger_online_search(val, cx);
                        if !this.search.trim().is_empty() && this.page != AppPage::Library {
                            this.page = AppPage::Library;
                        }
                        cx.notify();
                    });
                }
            });
        let initial_val = self.search.clone();
        let input = cx.new(move |cx| {
            crate::ui::components::input::HostTextInput::new(
                cx,
                initial_val,
                "搜索音乐、艺术家、专辑...",
                false,
                commit,
            )
            .with_on_change(on_change)
        });
        self.search_input = Some(input.clone());
        input
    }

    pub(crate) fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search.clear();
        self.search_active = false;
        self.online_search_results.clear();
        self.online_search_loading = false;
        if let Some(input) = &self.search_input {
            input.update(cx, |this, cx| {
                this.set_value("", cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn refresh_online_recommendations(&mut self, cx: &mut Context<Self>) {
        if !self.online_authenticated {
            self.online_daily_tracks.clear();
            self.online_playlists.clear();
            self.online_user_playlists.clear();
            self.online_new_tracks.clear();
            self.online_route = None;
            self.online_recommendations_loading = false;
            cx.notify();
            return;
        }
        if self.online_recommendations_loading {
            return;
        }
        self.online_recommendations_loading = true;
        cx.notify();

        let task = Tokio::spawn_result(cx, async move {
            let Some(frontend) = crate::plugin::frontend::global() else {
                return Ok(None);
            };
            let plan = frontend.plan(
                crate::plugin::abi::ServiceKind::Recommendations,
                &crate::plugin::abi::RoutingPolicy::default(),
            )?;
            if plan.eligible_routes.is_empty() {
                return Ok(None);
            }
            let route = plan.eligible_routes[0].clone();

            // 1. 每日推荐歌曲
            let daily_mix_res = frontend
                .recommendations(
                    &crate::plugin::abi::RecommendationRequest {
                        surface: crate::plugin::abi::RecommendationSurface::DailyMix,
                        limit: 30,
                        seed: None,
                        exclude: vec![],
                    },
                    &crate::plugin::abi::RoutingPolicy::default(),
                )
                .await;
            let daily_tracks = daily_mix_res
                .ok()
                .and_then(|f| f.batches.into_iter().next())
                .map(|b| b.items.into_iter().map(|i| i.track).collect::<Vec<_>>())
                .unwrap_or_default();

            // 2. 每日推荐歌单
            let playlists_res = frontend
                .collection_recommendations(
                    &route,
                    &crate::plugin::abi::CollectionRecommendationRequest {
                        kind: crate::plugin::abi::MediaCollectionKind::Playlist,
                        surface: crate::plugin::abi::CollectionRecommendationSurface::Daily,
                        limit: 8,
                        seed: None,
                        exclude: vec![],
                    },
                )
                .await;
            let playlists = playlists_res.unwrap_or_default();

            // 3. 个性推荐新歌
            let home_rec_res = frontend
                .recommendations(
                    &crate::plugin::abi::RecommendationRequest {
                        surface: crate::plugin::abi::RecommendationSurface::Home,
                        limit: 12,
                        seed: None,
                        exclude: vec![],
                    },
                    &crate::plugin::abi::RoutingPolicy::default(),
                )
                .await;
            let new_tracks = home_rec_res
                .ok()
                .and_then(|f| f.batches.into_iter().next())
                .map(|b| b.items.into_iter().map(|i| i.track).collect::<Vec<_>>())
                .unwrap_or_default();

            // 4. 用户自建与收藏歌单（用于侧边栏渲染）
            let user_pl_res = frontend
                .playlists(&crate::plugin::abi::RoutingPolicy::default())
                .await;
            let mut user_playlists = Vec::new();
            if let Ok(fanout) = user_pl_res {
                for batch in fanout.batches {
                    for pl in batch.playlists {
                        user_playlists.push((batch.route.clone(), pl));
                    }
                }
            }

            Ok(Some((
                route,
                daily_tracks,
                playlists,
                new_tracks,
                user_playlists,
            )))
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Some((route, daily_tracks, playlists, new_tracks, user_playlists))) =
                task.await
            {
                let _ = this.update(cx, |app, cx| {
                    let mut urls = Vec::new();
                    for t in &daily_tracks {
                        if let Some(u) = &t.cover_url {
                            urls.push(u.clone());
                        }
                    }
                    for p in &playlists {
                        if let Some(u) = &p.collection.artwork_url {
                            urls.push(u.clone());
                        }
                    }
                    for t in &new_tracks {
                        if let Some(u) = &t.cover_url {
                            urls.push(u.clone());
                        }
                    }
                    for (_, pl) in &user_playlists {
                        if let Some(u) = &pl.cover_url {
                            urls.push(u.clone());
                        }
                    }
                    crate::ui::image_cache::prefetch_urls(urls, cx);

                    app.online_route = Some(route);
                    app.online_daily_tracks = daily_tracks;
                    app.online_playlists = playlists;
                    app.online_new_tracks = new_tracks;
                    app.online_user_playlists = user_playlists;
                    app.online_recommendations_loading = false;
                    if let Some(active_pl) = &mut app.active_online_playlist {
                        if active_pl.title == "每日歌曲推荐" {
                            active_pl.tracks = app.online_daily_tracks.clone();
                            active_pl.cover_url =
                                active_pl.tracks.first().and_then(|t| t.cover_url.clone());
                            active_pl.loading = false;
                        }
                    }
                    app.bump_ui_content_revision();
                    cx.notify();
                });
            } else {
                let _ = this.update(cx, |app, cx| {
                    app.online_recommendations_loading = false;
                    if let Some(active_pl) = &mut app.active_online_playlist {
                        active_pl.loading = false;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn trigger_online_search(&mut self, query: String, cx: &mut Context<Self>) {
        if !self.has_online_plugins {
            self.online_search_results.clear();
            self.online_search_loading = false;
            cx.notify();
            return;
        }
        let trimmed = query.trim().to_string();
        if trimmed.is_empty() {
            self.online_search_results.clear();
            self.online_search_loading = false;
            cx.notify();
            return;
        }
        self.online_search_loading = true;
        cx.notify();

        let task = Tokio::spawn_result(cx, async move {
            let Some(frontend) = crate::plugin::frontend::global() else {
                return Ok(None);
            };
            let plan = frontend.plan(
                crate::plugin::abi::ServiceKind::Search,
                &crate::plugin::abi::RoutingPolicy::default(),
            )?;
            if plan.eligible_routes.is_empty() {
                return Ok(None);
            }
            let route = plan.eligible_routes[0].clone();
            let search_res = frontend
                .search(&trimmed, 30, &crate::plugin::abi::RoutingPolicy::default())
                .await?;
            let tracks = search_res
                .batches
                .into_iter()
                .flat_map(|b| b.tracks)
                .collect::<Vec<_>>();
            Ok(Some((route, tracks)))
        });

        cx.spawn(async move |this, cx| {
            if let Ok(Some((route, tracks))) = task.await {
                let _ = this.update(cx, |app, cx| {
                    let urls = tracks.iter().filter_map(|t| t.cover_url.clone()).collect();
                    crate::ui::image_cache::prefetch_urls(urls, cx);

                    app.online_search_route = Some(route);
                    app.online_search_results = tracks;
                    app.online_search_loading = false;
                    app.bump_ui_content_revision();
                    cx.notify();
                });
            } else {
                let _ = this.update(cx, |app, cx| {
                    app.online_search_loading = false;
                    cx.notify();
                });
            }
        })
        .detach();
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
                library_scan_all(&scan_library, &roots)
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
            tokio::task::spawn_blocking(move || library_scan_all(&scan_library, &roots)).await?
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

    fn sync_transport_surfaces(&mut self, cx: &mut Context<MusicApp>) {
        let playing = self.snapshot.state == PlaybackState::Playing;

        if let (Some(progress), Some(time)) =
            (self.playback_progress.clone(), self.playback_time.clone())
        {
            // MiniPlayerView::view is a cached entity synchronizer. Calling it here updates only the
            // retained transport surface; it does not rebuild the MusicApp shell.
            let _ = mini_player_view::view(self, cx, progress, time.clone());
            time.update(cx, |_, cx| cx.notify());
        }

        if self.stage_prepared || self.stage_open || self.stage_animating {
            let _ = stage_controls::view(self, cx);
        }

        if let Some(fluid) = self.fluid_background.clone() {
            fluid.update(cx, |view, cx| view.set_playing(playing, cx));
        }
    }

    pub(crate) fn sync_audio_snapshot_event(&mut self, cx: &mut Context<MusicApp>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };

        // Transport commands already update the foreground snapshot optimistically. Preserve enough
        // structural identity to recognize a later bridge ACK and avoid repainting the whole root
        // tree when the engine merely confirms exactly what the button already showed.
        let previous_state = self.snapshot.state;
        let previous_track = self.snapshot.current_track.clone();
        let previous_queue = self.snapshot.queue.clone();
        let previous_duration_ms = self.snapshot.duration_ms;
        let previous_volume = self.snapshot.volume;
        let previous_repeat = self.snapshot.repeat;
        let previous_shuffle = self.snapshot.shuffle;
        let previous_error = self.snapshot.error.clone();
        let transport_ack_pending = self.transport_root_notify_pending;
        self.transport_root_notify_pending = false;

        self.snapshot = engine.snapshot();
        let track_changed = match (
            previous_track.as_ref(),
            self.snapshot.current_track.as_ref(),
        ) {
            (None, None) => false,
            (Some(previous), Some(current)) => !previous.ptr_eq(current),
            _ => true,
        };
        let playback_state_changed = previous_state != self.snapshot.state;
        let root_visible_change = track_changed
            || !Arc::ptr_eq(&previous_queue, &self.snapshot.queue)
            || previous_duration_ms != self.snapshot.duration_ms
            || (previous_volume - self.snapshot.volume).abs() > 0.0005
            || previous_repeat != self.snapshot.repeat
            || previous_shuffle != self.snapshot.shuffle
            || previous_error.as_deref() != self.snapshot.error.as_deref();

        if self.drag_target.is_none() {
            self.position_ms = self.snapshot.position_ms;
            self.config.position_ms = self.position_ms;
        }

        if let Some(ratio) = self.pending_volume_ratio
            && (self.config.volume - ratio).abs() < 0.02
        {
            self.pending_volume_ratio = None;
        }

        if self.snapshot.current_track.is_some() {
            self.config.current_track = self.snapshot.current_track.as_ref().map(|track| track.id);
        }

        if let Some(track) = &self.snapshot.current_track {
            let track_id = track.id;
            let track_path = track.path.clone();
            if !self.lyrics.contains_key(&track_id) && self.lyrics_checked.insert(track_id) {
                if let Some((route, source, _)) = self.online_playback_meta.get(&track_id).cloned()
                {
                    self.fetch_online_lyrics_for_track(track_id, route, source, cx);
                } else {
                    let task = Tokio::spawn_result(cx, async move {
                        tokio::task::spawn_blocking(move || crate::lyrics::read_local(&track_path))
                            .await
                            .map_err(|e| anyhow::anyhow!("{e}"))
                    });
                    cx.spawn(async move |this, cx| -> Result<()> {
                        if let Ok(Some(lrc)) = task.await {
                            this.update(cx, |this, cx| {
                                this.cache_lyrics(track_id, lrc);
                                this.sync_lyrics_surfaces(track_id, cx);
                            })?;
                        }
                        Ok(())
                    })
                    .detach();
                }
            }

            if !self.artworks.contains_key(&track_id) {
                if let Some((_, _, Some(cover_url))) =
                    self.online_playback_meta.get(&track_id).cloned()
                {
                    self.fetch_online_artwork_for_track(track_id, cover_url, cx);
                }
            }
        }

        let curr_track_id = self.snapshot.current_track.as_ref().map(|track| track.id);
        if curr_track_id != self.last_polled_track_id {
            self.last_polled_track_id = curr_track_id;
            self.request_current_artwork(cx);
            self.request_current_enrichment(cx);
        }

        self.update_system_media_async(cx);

        if playback_state_changed || transport_ack_pending {
            self.sync_transport_surfaces(cx);
        }
        if root_visible_change {
            cx.notify();
        }
    }

    pub(crate) fn runtime_maintenance_tick(&mut self, cx: &mut Context<MusicApp>) {
        if let Some(engine) = &self.engine {
            let (state, position_ms, duration_ms) = engine.progress();
            self.snapshot.position_ms = position_ms;
            self.snapshot.duration_ms = duration_ms;
            if self.drag_target.is_none() {
                self.position_ms = position_ms;
                self.config.position_ms = position_ms;
            }

            if state == PlaybackState::Playing
                && self.last_saved_at.elapsed() >= Duration::from_secs(3)
            {
                let diff = (self.position_ms as i64 - self.last_saved_position_ms as i64).abs();
                if diff >= 1_000 {
                    self.last_saved_position_ms = self.position_ms;
                    self.last_saved_at = std::time::Instant::now();
                    self.save_config();
                }
            }
        }

        self.flush_config_save_if_due();
        self.update_system_media_async(cx);
        self.ensure_system_media_async(cx);
    }

    fn ensure_system_media_async(&mut self, cx: &mut Context<MusicApp>) {
        if self.system_media.is_some()
            || self.system_media_init_in_flight
            || self.system_media_update_in_flight
            || self.system_media_init_attempts >= 10
        {
            return;
        }

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
        let metadata_fingerprint = crate::media_controls::metadata_fingerprint(track.as_ref());
        let position_sec = position_ms / 1000;
        let needs_update = self.system_media_sync_dirty
            || self.last_system_media_track_id != track_id
            || self.last_system_media_metadata_fingerprint != Some(metadata_fingerprint)
            || self.last_system_media_state != Some(state)
            || position_sec.abs_diff(self.last_system_media_position_sec) >= 2;
        if !needs_update {
            self.system_media = Some(bridge);
            return;
        }

        self.last_system_media_track_id = track_id;
        self.last_system_media_metadata_fingerprint = Some(metadata_fingerprint);
        self.last_system_media_state = Some(state);
        self.last_system_media_position_sec = position_sec;
        self.system_media_sync_dirty = false;
        self.system_media_update_in_flight = true;

        let task = Tokio::spawn_result(cx, async move {
            tokio::task::spawn_blocking(move || {
                let metadata_synced = bridge.update_metadata(track.as_ref());
                let playback_synced = bridge.update_playback(state, position_ms);
                (bridge, metadata_synced && playback_synced)
            })
            .await
            .map_err(|error| anyhow::anyhow!("系统媒体状态更新任务异常退出: {error}"))
        });
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await;
            this.update(cx, |this, _cx| {
                this.system_media_update_in_flight = false;
                match result {
                    Ok((bridge, sync_ok)) => {
                        let current_track_id =
                            this.snapshot.current_track.as_ref().map(|track| track.id);
                        let current_metadata_fingerprint =
                            crate::media_controls::metadata_fingerprint(
                                this.snapshot.current_track.as_ref(),
                            );
                        let current_position_sec = this.snapshot.position_ms / 1000;
                        this.system_media_sync_dirty = this.system_media_sync_dirty
                            || !sync_ok
                            || current_track_id != track_id
                            || current_metadata_fingerprint != metadata_fingerprint
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
        if track.id < 0
            && let Some((_, _, Some(url))) = self.online_playback_meta.get(&track.id).cloned()
        {
            self.fetch_online_artwork_for_track(track.id, url, cx);
            return;
        }
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
                        if this.config.online_metadata
                            && this.artwork_online_fallback_requested.insert(track_id)
                        {
                            this.enrichment_done.remove(&track_id);
                            this.status = "本地封面不可用，正在尝试联网封面…".into();
                            this.request_current_enrichment(cx);
                        }
                    }
                    Err(error) => {
                        this.artwork_missing.insert(track_id);
                        if this.config.online_metadata
                            && this.artwork_online_fallback_requested.insert(track_id)
                        {
                            this.enrichment_done.remove(&track_id);
                            this.status = format!("本地封面解析失败，正在尝试联网封面：{error:#}");
                            this.request_current_enrichment(cx);
                        } else {
                            this.status = format!("封面读取失败：{error:#}");
                        }
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
                this.notify_current_content_surface(cx);
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn displayed_progress_ratio(&self) -> f32 {
        self.drag_progress_ratio.unwrap_or_else(|| {
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
        if let Some(ratio) = self.drag_progress_ratio {
            (duration_ms as f32 * ratio).round() as u64
        } else {
            self.snapshot.position_ms
        }
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
                    .is_none_or(|c| (c - ratio).abs() >= 0.0005);
                if changed {
                    self.drag_progress_ratio = Some(ratio);
                    cx.notify();
                    return true;
                }
            }
            DragTarget::Volume => {
                let changed = self
                    .drag_volume_ratio
                    .is_none_or(|c| (c - ratio).abs() >= 0.001);
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
        self.sync_transport_surfaces(cx);
    }

    pub(crate) fn seek_to_ms(&mut self, position_ms: u64, cx: &mut Context<Self>) {
        let duration_ms = self.snapshot.duration_ms;
        let clamped = if duration_ms > 0 {
            position_ms.min(duration_ms)
        } else {
            position_ms
        };
        if self.send(PlayerCommand::Seek(Duration::from_millis(clamped))) {
            self.snapshot.position_ms = clamped;
            self.position_ms = clamped;
            self.config.position_ms = clamped;
            self.sync_transport_surfaces(cx);
            if let Some(track_id) = self.snapshot.current_track.as_ref().map(|track| track.id) {
                self.sync_lyrics_surfaces(track_id, cx);
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn seek_to_ratio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        let duration_ms = self.snapshot.duration_ms;
        if duration_ms == 0 {
            return;
        }
        let ratio = ratio.clamp(0.0, 1.0);
        let position_ms = (duration_ms as f32 * ratio).round() as u64;
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
        self.system_media_sync_dirty = true;
        self.update_system_media_async(cx);
        self.sync_transport_surfaces(cx);
    }

    pub(crate) fn set_app_volume(&mut self, vol: f32, cx: &mut Context<Self>) {
        self.config.volume = vol.clamp(0.0, 1.0);
        self.send(PlayerCommand::SetVolume(self.config.volume));
        self.save_config();
        self.system_media_sync_dirty = true;
        self.update_system_media_async(cx);
        self.sync_transport_surfaces(cx);
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
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|_, event: &gpui::MouseDownEvent, window, _| {
                            if event.click_count >= 2 {
                                window.titlebar_double_click();
                            }
                        }),
                    ),
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
        self.start_runtime_events(cx);
        self.ensure_plugin_sessions_restored(cx);

        let now = std::time::Instant::now();

        // Route restoration may request the Stage before this frame's surface decision. Resolve it
        // first so the very same frame constructs the drawer and only then arms its presentation
        // fence; otherwise the animation clock could start one frame before the Stage exists.
        let current_route = route::current_route(cx);
        if current_route == AppRoute::Player && !self.stage_open && self.stage_progress < 0.001 {
            self.open_stage(cx);
        }

        let stage_prewarm = !self.stage_prepared
            && !self.stage_open
            && !self.stage_animating
            && self.stage_prewarm_after.is_some_and(|deadline| deadline <= now);
        if !self.stage_prepared
            && !self.stage_open
            && !self.stage_animating
            && let Some(deadline) = self.stage_prewarm_after
            && deadline > now
        {
            // Exactly one deferred invalidation; the transport interaction frame never builds the
            // hidden immersive tree.
            window.request_invalidation_at(deadline, cx);
        }
        if stage_prewarm {
            self.stage_prepared = true;
            self.stage_prewarm_after = None;
        }

        let is_idle = self.stage_open
            && self.stage_last_user_activity.elapsed() >= stage_chrome::IDLE_TIMEOUT
            && !self.seeking
            && !self.volume_dragging
            && !self.stage_controls_hovered;

        if self.stage_open
            && self.stage_suppress_wake_until.is_none()
            && !is_idle
            && !self.seeking
            && !self.volume_dragging
            && !self.stage_controls_hovered
        {
            let deadline = self.stage_last_user_activity + stage_chrome::IDLE_TIMEOUT;
            if deadline > now {
                window.request_invalidation_at(deadline, cx);
            }
        }

        let stage_surface_needed =
            self.stage_progress > 0.001 || self.stage_animating || stage_prewarm;
        let fluid_background = stage_surface_needed.then(|| {
            let fluid_background = self.ensure_fluid_background(cx);
            let fluid_track_id = self
                .snapshot
                .current_track
                .as_ref()
                .map_or(0, |track| track.id);
            let fluid_palette = self.artwork_palettes.get(&fluid_track_id).cloned();
            // Do not run a full-screen shader RAF while the whole stage is itself moving. The
            // retained drawer animation replays the already painted stage; fluid resumes once the
            // drawer settles.
            let fluid_active = !stage_prewarm && self.stage_open && !self.stage_animating;
            let fluid_dynamic = self.config.dynamic_blur;
            fluid_background.update(cx, |view, cx| {
                view.sync(
                    fluid_track_id,
                    fluid_palette,
                    fluid_dynamic,
                    fluid_active,
                    cx,
                );
            });
            fluid_background
        });

        self.arm_stage_transition_after_present(window, cx);

        let stage_fully_covering =
            self.stage_open && self.stage_progress >= 0.999 && !self.stage_animating;

        // Once the immersive Stage fully covers the window, do not even synchronize/build the
        // hidden application shell. This removes sidebar/online-playlist/mini-player work from
        // transport and drag frames.
        let base_shell = if stage_fully_covering {
            div()
                .size_full()
                .bg(theme::BG_CANVAS)
                .into_any_element()
        } else {
            let (playback_progress, playback_time) = self.ensure_playback_progress(cx);
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
            let online_playlist_page = self.ensure_online_playlist_page(cx);
            let search_input = self.ensure_search_input(cx);
            let content = match main_page {
                AppPage::Home => home_page.clone().into_any_element(),
                AppPage::Library => library_page.into_any_element(),
                AppPage::Player => home_page.into_any_element(),
                AppPage::Settings => settings_page::render(self, cx),
                AppPage::OnlinePlaylist => online_playlist_page.into_any_element(),
            };

            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(theme::BG_CANVAS)
                .text_color(theme::TEXT_PRIMARY)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    let modifiers = event.keystroke.modifiers;
                    if this.acoustid_key_active {
                        this.update_acoustid_key(key, cx);
                    } else if modifiers.control && key.eq_ignore_ascii_case("f") {
                        this.search_active = true;
                        if let Some(input) = &this.search_input {
                            let handle = input.read(cx).focus_handle(cx);
                            window.focus(&handle);
                        }
                        cx.notify();
                    } else if key == "space" {
                        this.toggle_play(cx);
                    } else if key == "left" {
                        this.seek_relative(-10_000, cx);
                    } else if key == "right" {
                        this.seek_relative(10_000, cx);
                    }
                }))
                .child(self.custom_titlebar(window, cx))
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .child(sidebar(self, search_input, cx))
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
                ))
                .into_any_element()
        };

        let stage_drawer = if stage_surface_needed {
            let fluid_background = fluid_background
                .clone()
                .expect("Stage surface requires a prepared fluid background");
            let stage_visual = |progress: f32| {
                let progress = progress.clamp(0.0, 1.0);
                (0.992 + 0.008 * progress, progress)
            };
            let (from_scale, from_opacity) = stage_visual(self.stage_transition_from);
            let (to_scale, to_opacity) = stage_visual(self.stage_transition_to);
            let motion = AnimationProperty::scale_opacity(
                from_scale,
                to_scale,
                from_opacity,
                to_opacity,
                TransformOrigin::new(0.5, 0.5),
            );
            let hidden_stage = AnimationProperty::scale_opacity(
                0.992,
                1.0,
                0.0,
                1.0,
                TransformOrigin::new(0.5, 0.5),
            );
            let stage_titlebar = stage_controls::titlebar_view(self, cx);

            let stage_layer = div()
                .id("stage-drawer-root")
                .absolute()
                .inset_0()
                .overflow_hidden()
                .bg(rgb(0x0e0f16))
                .text_color(theme::TEXT_WHITE)
                .on_mouse_move(
                    cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {
                        this.handle_stage_mouse_move(event.position, cx);
                    }),
                )
                // Do not register blanket mouse-down/up handlers on the full-screen Stage.
                // Interactive descendants own their pointer gestures and stop propagation where
                // appropriate. A parent hit handler here can win the hit-test path across Entity
                // boundaries and is especially harmful for the bottom retained controls.
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    this.wake_stage_controls_immediately(cx);
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
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .child(player::render(self, cx, fluid_background.clone())),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .right(px(0.0))
                        .child(stage_titlebar),
                )
                .composite_layer();

            let stage_surface = if stage_prewarm {
                stage_layer
                    .with_sampled_animation(hidden_stage, 0.0)
                    .into_any_element()
            } else if self.stage_animating {
                if self.stage_transition_started_at.is_some() {
                    let animation = Animation::from_spec(
                        AnimationSpec::new(self.stage_transition_duration).ease(Easing::InOutCubic),
                    )
                    .with_property(motion);
                    stage_layer
                        .with_animation(
                            ElementId::NamedInteger(
                                SharedString::new_static("stage-drawer-transition"),
                                self.stage_transition_epoch,
                            ),
                            animation,
                            |element, _| element,
                        )
                        .into_any_element()
                } else {
                    stage_layer
                        .with_sampled_animation(motion, 0.0)
                        .into_any_element()
                }
            } else {
                stage_layer.into_any_element()
            };
            Some(stage_surface)
        } else {
            None
        };

        let global_modal = super::components::modal::render(self, cx);

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
            .child(base_shell)
            .children(stage_drawer)
            .children(global_modal)
    }
}

fn sidebar(
    app: &MusicApp,
    search_input: Entity<crate::ui::components::input::HostTextInput>,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let track_count = app.tracks.len();
    let active_plugin_route = super::plugin_navigation::current(cx).ok().flatten();
    let sidebar_routes = super::plugin_navigation::sidebar_routes().unwrap_or_default();
    let has_sidebar_routes = !sidebar_routes.is_empty();
    let mut plugin_section = div()
        .flex()
        .flex_col()
        .gap_1()
        .child(sidebar_section_header("插件"));
    for target in sidebar_routes {
        let active = active_plugin_route
            .as_ref()
            .is_some_and(|current| current.pathname == target.pathname);
        plugin_section =
            plugin_section.child(super::plugin_navigation::sidebar_entry(target, active, cx));
    }

    let (my_section, created_section, collected_section, music_library_section) = if app
        .has_online_plugins
        && app.online_authenticated
    {
        let mut created_playlists = Vec::new();
        let mut collected_playlists = Vec::new();
        for (route, pl) in &app.online_user_playlists {
            if pl.editable {
                created_playlists.push((route.clone(), pl.clone()));
            } else {
                collected_playlists.push((route.clone(), pl.clone()));
            }
        }

        let created_section = if created_playlists.is_empty() {
            None
        } else {
            let count = created_playlists.len();
            let collapsed = app.sidebar_created_playlists_collapsed;
            let mut group = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(render_playlist_group_header(
                    "创建的歌单",
                    count,
                    collapsed,
                    cx.listener(|this, _, _, cx| {
                        this.sidebar_created_playlists_collapsed =
                            !this.sidebar_created_playlists_collapsed;
                        cx.notify();
                    }),
                ));
            if !collapsed {
                for (route, pl) in created_playlists {
                    let is_active = app.page == AppPage::OnlinePlaylist
                        && app
                            .active_online_playlist
                            .as_ref()
                            .is_some_and(|p| p.title == pl.name);
                    group = group.child(render_playlist_sidebar_item(route, pl, is_active, cx));
                }
            }
            Some(group)
        };

        let collected_section = if collected_playlists.is_empty() {
            None
        } else {
            let count = collected_playlists.len();
            let collapsed = app.sidebar_collected_playlists_collapsed;
            let mut group = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(render_playlist_group_header(
                    "收藏的歌单",
                    count,
                    collapsed,
                    cx.listener(|this, _, _, cx| {
                        this.sidebar_collected_playlists_collapsed =
                            !this.sidebar_collected_playlists_collapsed;
                        cx.notify();
                    }),
                ));
            if !collapsed {
                for (route, pl) in collected_playlists {
                    let is_active = app.page == AppPage::OnlinePlaylist
                        && app
                            .active_online_playlist
                            .as_ref()
                            .is_some_and(|p| p.title == pl.name);
                    group = group.child(render_playlist_sidebar_item(route, pl, is_active, cx));
                }
            }
            Some(group)
        };

        let my_sec = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(sidebar_section_header("我的"))
            .child(sidebar_item(
                "我喜欢的音乐",
                icon!(heart),
                app.page == AppPage::OnlinePlaylist
                    && app
                        .active_online_playlist
                        .as_ref()
                        .is_some_and(|p| p.title == "我喜欢的音乐"),
                cx.listener(|this, _, _, cx| {
                    this.load_and_show_user_favorite_playlist(cx);
                }),
            ))
            .child(sidebar_item(
                "最近播放",
                icon!(history),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Recent,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Recent, cx);
                }),
            ))
            .child(sidebar_item(
                "本地音乐",
                icon!(music),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Songs,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Songs, cx);
                }),
            ))
            .child(sidebar_item(
                "我的音乐网盘",
                icon!(cloud),
                app.page == AppPage::OnlinePlaylist
                    && app
                        .active_online_playlist
                        .as_ref()
                        .is_some_and(|p| p.title == "我的音乐网盘"),
                cx.listener(|this, _, _, cx| {
                    this.load_and_show_cloud_library(cx);
                }),
            ))
            .child(sidebar_item(
                "我的收藏",
                icon!(bookmark),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Playlists,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Playlists, cx);
                }),
            ));

        let lib_sec = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(sidebar_section_header("音乐库"))
            .child(sidebar_item(
                "专辑",
                icon!(disc_3),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Albums,
                cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Albums, cx)),
            ))
            .child(sidebar_item(
                "艺术家",
                icon!(users_round),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Artists,
                cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Artists, cx)),
            ))
            .child(sidebar_item(
                "播放队列",
                icon!(list_music),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Playlists,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Playlists, cx);
                }),
            ));

        (
            Some(my_sec),
            created_section,
            collected_section,
            Some(lib_sec),
        )
    } else {
        // Pure local music mode or unauthenticated: clean, focused navigation
        let local_sec = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(sidebar_section_header("音乐库"))
            .child(sidebar_item(
                "本地音乐",
                icon!(music),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Songs,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Songs, cx);
                }),
            ))
            .child(sidebar_item(
                "最近播放",
                icon!(history),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Recent,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Recent, cx);
                }),
            ))
            .child(sidebar_item(
                "专辑",
                icon!(disc_3),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Albums,
                cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Albums, cx)),
            ))
            .child(sidebar_item(
                "艺术家",
                icon!(users_round),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Artists,
                cx.listener(|this, _, _, cx| this.show_library_tab(LibraryTab::Artists, cx)),
            ))
            .child(sidebar_item(
                "播放队列",
                icon!(list_music),
                app.page == AppPage::Library && app.library_tab == LibraryTab::Playlists,
                cx.listener(|this, _, _, cx| {
                    this.show_library_tab(LibraryTab::Playlists, cx);
                }),
            ));

        (None, None, None, Some(local_sec))
    };

    div()
        .w(px(236.0))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .bg(theme::BG_SIDEBAR)
        .border_r_1()
        .border_color(theme::BORDER_HAIRLINE)
        .child(
            div()
                .flex_none()
                .px_4()
                .pt_5()
                .pb_3()
                .flex()
                .flex_col()
                .gap_4()
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
                                    icon!(audio_waveform),
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
                        .id("sidebar-search-container")
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(div().flex_1().min_w(px(0.0)).child(search_input))
                        .children(if !app.search.is_empty() {
                            Some(
                                div()
                                    .id("sidebar-search-clear-btn")
                                    .size(px(24.0))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::bg_hover()))
                                    .transition(theme::press_transition())
                                    .child(theme::themed_icon(
                                        icon!(x),
                                        12.0,
                                        theme::TEXT_SECONDARY.into(),
                                    ))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.clear_search(cx);
                                        }),
                                    ),
                            )
                        } else {
                            None
                        }),
                ),
        )
        .child(
            div()
                .id("sidebar-nav-scroll")
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .px_4()
                .py_2()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(sidebar_section_header("探索"))
                        .child(sidebar_item(
                            "发现",
                            icon!(compass),
                            app.page == AppPage::Home,
                            cx.listener(|this, _, _, cx| this.show_page(AppPage::Home, cx)),
                        )),
                )
                .children(my_section)
                .children(created_section)
                .children(collected_section)
                .children(music_library_section)
                .children(has_sidebar_routes.then_some(plugin_section))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(sidebar_section_header("服务与系统"))
                        .child(sidebar_item(
                            "插件中心",
                            icon!(plug),
                            active_plugin_route.is_none()
                                && app.page == AppPage::Settings
                                && settings_page::workspace()
                                    == settings_page::SettingsWorkspace::Plugins,
                            cx.listener(|this, _, _, cx| {
                                settings_page::select_workspace(
                                    settings_page::SettingsWorkspace::Plugins,
                                    cx,
                                );
                                this.show_page(AppPage::Settings, cx);
                            }),
                        ))
                        .children(app.has_online_plugins.then(|| {
                            sidebar_item(
                                "音乐服务",
                                icon!(cloud),
                                active_plugin_route.is_none()
                                    && app.page == AppPage::Settings
                                    && (settings_page::workspace()
                                        == settings_page::SettingsWorkspace::Authentication
                                        || settings_page::workspace()
                                            == settings_page::SettingsWorkspace::Services),
                                cx.listener(|this, _, _, cx| {
                                    settings_page::select_workspace(
                                        settings_page::SettingsWorkspace::Authentication,
                                        cx,
                                    );
                                    this.show_page(AppPage::Settings, cx);
                                }),
                            )
                        }))
                        .child(sidebar_item(
                            "偏好设置",
                            icon!(settings),
                            active_plugin_route.is_none()
                                && app.page == AppPage::Settings
                                && settings_page::workspace()
                                    == settings_page::SettingsWorkspace::Preferences,
                            cx.listener(|this, _, _, cx| {
                                settings_page::select_workspace(
                                    settings_page::SettingsWorkspace::Preferences,
                                    cx,
                                );
                                this.show_page(AppPage::Settings, cx);
                            }),
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
                ),
        )
}

fn render_playlist_group_header<F>(
    title: &str,
    count: usize,
    collapsed: bool,
    on_toggle: F,
) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(theme::hover_transition())
        .on_mouse_down(gpui::MouseButton::Left, on_toggle)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::TEXT_SECONDARY)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!("{count}")),
                ),
        )
        .child(theme::themed_icon(
            if collapsed {
                icon!(chevron_down)
            } else {
                icon!(chevron_up)
            },
            13.0,
            theme::TEXT_TERTIARY.into(),
        ))
        .into_any_element()
}

fn render_playlist_sidebar_item(
    route: crate::plugin::abi::PluginRoute,
    pl: crate::plugin::abi::PlaylistDescriptor,
    active: bool,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let pl_name = pl.name.clone();
    let pl_source_id = pl.source_id.clone();
    let pl_track_count = pl.track_count.unwrap_or(0);
    let pl_cover = pl.cover_url.clone();
    let click_route = route.clone();
    let click_source_id = pl_source_id.clone();
    let click_name = pl_name.clone();
    let click_cover = pl_cover.clone();

    let bg_color = if active {
        theme::accent_red_muted()
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };

    div()
        .id(SharedString::from(format!("side-pl-{}", pl_source_id)))
        .flex()
        .items_center()
        .gap_2p5()
        .px_3()
        .py_1p5()
        .rounded_md()
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
                .size(px(32.0))
                .flex_none()
                .rounded(px(4.0))
                .overflow_hidden()
                .border_1()
                .border_color(theme::BORDER_CARD)
                .child(crate::ui::image_cache::render_remote_cover(
                    pl_cover.as_deref(),
                    32.0,
                    32.0,
                    4.0,
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .text_xs()
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active {
                    theme::ACCENT_RED
                } else {
                    theme::TEXT_PRIMARY
                })
                .truncate()
                .child(pl_name),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                this.load_and_show_online_playlist(
                    click_route.clone(),
                    crate::plugin::abi::MediaCollectionRef {
                        provider_id: click_route.provider_id.clone(),
                        kind: crate::plugin::abi::MediaCollectionKind::Playlist,
                        source_id: click_source_id.clone(),
                    },
                    click_name.clone(),
                    format!("共 {pl_track_count} 首歌曲"),
                    click_cover.clone(),
                    cx,
                );
            }),
        )
        .into_any_element()
}

fn sidebar_sub_item<F>(
    id_str: &str,
    title: &str,
    subtitle: Option<&str>,
    active: bool,
    on_press: F,
) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
{
    let text_color = if active {
        theme::ACCENT_RED
    } else {
        theme::TEXT_SECONDARY
    };
    let bg_color = if active {
        theme::accent_red_muted()
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };

    div()
        .id(SharedString::from(format!("side-sub-{id_str}")))
        .flex()
        .items_center()
        .justify_between()
        .pl_6()
        .pr_2()
        .py_1p5()
        .rounded_md()
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
                .gap_2()
                .min_w(px(0.0))
                .flex_1()
                .child(
                    div()
                        .size(px(4.0))
                        .rounded_full()
                        .flex_none()
                        .bg(if active {
                            theme::ACCENT_RED.into()
                        } else {
                            hsla(220.0, 0.08, 0.60, 1.0)
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(text_color)
                        .font_weight(if active {
                            gpui::FontWeight::SEMIBOLD
                        } else {
                            gpui::FontWeight::NORMAL
                        })
                        .truncate()
                        .child(title.to_string()),
                ),
        )
        .children(subtitle.map(|sub| {
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .flex_none()
                .child(sub.to_string())
        }))
        .on_mouse_down(gpui::MouseButton::Left, on_press)
        .into_any_element()
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
    on_press: F,
) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
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
        .on_mouse_down(gpui::MouseButton::Left, on_press)
}

fn traffic_light_button<F>(id: &'static str, color: gpui::Rgba, on_press: F) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
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
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            on_press(event, window, cx);
        })
}

fn library_scan_all(library: &Library, roots: &[PathBuf]) -> Result<Vec<ScanReport>> {
    library.scan_all(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_render_keys_ignore_transport_only_updates() {
        let mut app = MusicApp::new(false);
        app.page = AppPage::Home;
        let home_key = home_page_render_key(&app);
        app.snapshot.position_ms = app.snapshot.position_ms.saturating_add(1_000);
        app.config.volume = 0.23;
        app.drag_progress_ratio = Some(0.42);
        app.drag_volume_ratio = Some(0.61);
        assert_eq!(home_key, home_page_render_key(&app));

        app.page = AppPage::Library;
        let library_key = library_page_render_key(&app);
        app.snapshot.position_ms = app.snapshot.position_ms.saturating_add(1_000);
        app.config.volume = 0.77;
        app.drag_progress_ratio = Some(0.11);
        app.drag_volume_ratio = Some(0.88);
        assert_eq!(library_key, library_page_render_key(&app));
    }

    #[test]
    fn page_render_keys_track_visible_content_changes() {
        let mut app = MusicApp::new(false);
        app.page = AppPage::Home;
        let home_key = home_page_render_key(&app);
        app.bump_ui_content_revision();
        assert_ne!(home_key, home_page_render_key(&app));

        app.page = AppPage::Library;
        let library_key = library_page_render_key(&app);
        app.library_tab = LibraryTab::Albums;
        assert_ne!(library_key, library_page_render_key(&app));

        let status_key = library_page_render_key(&app);
        app.status.push_str(" · updated");
        assert_ne!(status_key, library_page_render_key(&app));

        let search_key = library_page_render_key(&app);
        app.search.push_str("ambient");
        assert_ne!(search_key, library_page_render_key(&app));
    }

    #[test]
    fn test_drag_progress_ratio_precedence() {
        let mut app = MusicApp::new(false);
        app.snapshot.duration_ms = 100_000;
        app.snapshot.position_ms = 20_000;

        assert!((app.displayed_progress_ratio() - 0.20).abs() < 0.001);
        assert_eq!(app.displayed_position_ms(), 20_000);

        app.drag_progress_ratio = Some(0.85);
        assert!((app.displayed_progress_ratio() - 0.85).abs() < 0.001);
        assert_eq!(app.displayed_position_ms(), 85_000);

        app.drag_progress_ratio = None;
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
        assert_eq!(stage_chrome::IDLE_TIMEOUT, Duration::from_secs(20));
    }

    #[test]
    fn stage_transition_is_short_and_bounded() {
        assert_eq!(STAGE_TRANSITION_DURATION, Duration::from_millis(220));
        assert_eq!(stage_ease_in_out_cubic(0.0), 0.0);
        assert!((stage_ease_in_out_cubic(0.5) - 0.5).abs() < f32::EPSILON);
        assert_eq!(stage_ease_in_out_cubic(1.0), 1.0);
    }

    #[test]
    fn manual_immersive_requires_meaningful_pointer_motion() {
        assert!(STAGE_MANUAL_WAKE_THRESHOLD_PX > 2.0);
        assert_eq!(STAGE_MANUAL_WAKE_THRESHOLD_PX, 8.0);
    }

    #[test]
    fn recent_plays_deduplicates_by_title_and_artist() {
        let mut app = MusicApp::new(false);
        let make_track = |id: TrackId, title: &str, artist: &str, path: &str| {
            Track::new(crate::model::TrackData {
                id,
                path: PathBuf::from(path),
                title: title.into(),
                artist: artist.into(),
                album: "Album".into(),
                year: None,
                genre: None,
                duration_ms: 10_000,
                codec: "flac".into(),
                sample_rate: 48_000,
                channels: 2,
                artwork_key: None,
            })
        };

        let track1 = make_track(-1, "Senbonzakura", "Lindsey Stirling", "stream://1");
        let track2 = make_track(-2, "Celestial", "Ed Sheeran", "stream://2");
        let track1_replayed = make_track(-3, "Senbonzakura", "Lindsey Stirling", "stream://3");

        app.online_track_cache.insert(-1, track1.clone());
        app.online_track_cache.insert(-2, track2.clone());
        app.online_track_cache.insert(-3, track1_replayed.clone());

        app.record_recent_play(&track1);
        assert_eq!(app.recent_plays, vec![-1]);

        app.record_recent_play(&track2);
        assert_eq!(app.recent_plays, vec![-2, -1]);

        // When replaying track 1 with new ID -3, -1 must be removed and -3 moved to head
        app.record_recent_play(&track1_replayed);
        assert_eq!(app.recent_plays, vec![-3, -2]);
        assert_eq!(app.recent_plays.len(), 2);
    }

    #[test]
    fn sidebar_playlist_collapse_state_defaults_expanded() {
        let mut app = MusicApp::new(false);
        assert!(!app.sidebar_created_playlists_collapsed);
        assert!(!app.sidebar_collected_playlists_collapsed);

        app.sidebar_created_playlists_collapsed = true;
        assert!(app.sidebar_created_playlists_collapsed);
    }

    #[test]
    fn home_render_key_tracks_plugin_and_auth_state() {
        let mut app = MusicApp::new(false);
        app.page = AppPage::Home;
        let key_pure_local = home_page_render_key(&app);
        assert!(!key_pure_local.has_online_plugins);
        assert!(!key_pure_local.online_authenticated);

        app.has_online_plugins = true;
        let key_with_plugin = home_page_render_key(&app);
        assert_ne!(key_pure_local, key_with_plugin);
        assert!(key_with_plugin.has_online_plugins);
        assert!(!key_with_plugin.online_authenticated);

        app.online_authenticated = true;
        let key_authenticated = home_page_render_key(&app);
        assert_ne!(key_with_plugin, key_authenticated);
        assert!(key_authenticated.online_authenticated);
    }
}
