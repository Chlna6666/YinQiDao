use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnimationExt as _, AnyView, BorrowAppContext as _, Context, EncodedImageBytes, Entity, Global, ImageFormat,
    IntoElement, ObjectFit, Render, SharedString, StyleRefinement, Subscription, Window, div, hsla, img,
    linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    gpu::AppleFluidView,
    model::{PlaybackState, Track, TrackId},
};

use super::{
    app_ui_events::{self, AppUiEvent},
    player_legacy, stage_chrome, stage_controls, stage_lyrics,
    shell::MusicApp,
    theme::{TEXT_WHITE, elegant_gradient_for, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime};

const STAGE_CHROME_LAYOUT_DURATION: Duration = Duration::from_millis(300);
const STAGE_LEFT_VISIBLE_WIDTH_PX: f32 = 340.0;
const STAGE_LEFT_HIDDEN_WIDTH_PX: f32 = 250.0;
const STAGE_CONTROLS_SLOT_HEIGHT_PX: f32 = 158.0;
const STAGE_METADATA_SLOT_HEIGHT_PX: f32 = 72.0;
const STAGE_CONTENT_VISIBLE_TOP_PX: f32 = 54.0;
const STAGE_CONTENT_HIDDEN_TOP_PX: f32 = 24.0;
const STAGE_CONTENT_VISIBLE_BOTTOM_PX: f32 = 32.0;
const STAGE_CONTENT_HIDDEN_BOTTOM_PX: f32 = 24.0;
const STAGE_COLUMN_VISIBLE_GAP_PX: f32 = 48.0;
const STAGE_COLUMN_HIDDEN_GAP_PX: f32 = 28.0;

#[derive(Clone, Copy, Debug)]
struct StageChromeLayoutMotion {
    value: f32,
    from: f32,
    to: f32,
    started_at: Option<Instant>,
    duration: Duration,
}

impl StageChromeLayoutMotion {
    fn new(visible: bool) -> Self {
        let value = if visible { 1.0 } else { 0.0 };
        Self {
            value,
            from: value,
            to: value,
            started_at: None,
            duration: STAGE_CHROME_LAYOUT_DURATION,
        }
    }

    #[inline]
    fn ease(progress: f32) -> f32 {
        let t = progress.clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }

    fn sample(&self, now: Instant) -> f32 {
        let Some(started_at) = self.started_at else {
            return self.value;
        };
        if self.duration.is_zero() {
            return self.to;
        }
        let linear =
            now.saturating_duration_since(started_at).as_secs_f32() / self.duration.as_secs_f32();
        let eased = Self::ease(linear);
        self.from + (self.to - self.from) * eased
    }

    fn set_target(&mut self, visible: bool) -> bool {
        let target = if visible { 1.0 } else { 0.0 };
        if (self.to - target).abs() <= 0.001 {
            return false;
        }

        let now = Instant::now();
        let current = self.sample(now).clamp(0.0, 1.0);
        self.value = current;
        self.from = current;
        self.to = target;

        let distance = (target - current).abs();
        if distance <= 0.001 {
            self.value = target;
            self.from = target;
            self.started_at = None;
            return true;
        }

        self.duration = Duration::from_secs_f32(
            (STAGE_CHROME_LAYOUT_DURATION.as_secs_f32() * distance).max(0.001),
        );
        self.started_at = Some(now);
        true
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        self.value = self.sample(now).clamp(0.0, 1.0);
        if now.saturating_duration_since(started_at) >= self.duration {
            self.value = self.to;
            self.from = self.to;
            self.started_at = None;
            false
        } else {
            true
        }
    }

    #[inline]
    fn value(&self) -> f32 {
        self.value.clamp(0.0, 1.0)
    }
}

#[inline]
fn stage_lerp(hidden: f32, visible: f32, visible_progress: f32) -> f32 {
    hidden + (visible - hidden) * visible_progress.clamp(0.0, 1.0)
}

#[derive(Default)]
struct StagePlayerViewCache {
    view: Option<Entity<StagePlayerView>>,
}

impl Global for StagePlayerViewCache {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StagePlayerRenderKey {
    track_id: Option<TrackId>,
    artwork_ptr: usize,
    artwork_len: usize,
}

#[derive(Clone)]
struct StageCoverRenderData {
    track_id: Option<TrackId>,
    title: SharedString,
    artist: SharedString,
    album: SharedString,
    artwork: Option<Arc<[u8]>>,
}

impl Default for StageCoverRenderData {
    fn default() -> Self {
        Self {
            track_id: None,
            title: SharedString::new_static("未在播放音乐"),
            artist: SharedString::new_static("请选择音乐"),
            album: SharedString::new_static("未知专辑"),
            artwork: None,
        }
    }
}

impl StageCoverRenderData {
    fn from_track(track: Option<&Track>, artwork: Option<Arc<[u8]>>) -> Self {
        let Some(track) = track else {
            return Self {
                artwork,
                ..Self::default()
            };
        };
        Self {
            track_id: Some(track.id),
            title: SharedString::from(track.title.clone()),
            artist: SharedString::from(track.artist.clone()),
            album: SharedString::from(track.album.clone()),
            artwork,
        }
    }
}

pub(super) fn sync_chrome_if_created(app: &MusicApp, cx: &mut Context<MusicApp>) {
    let existing = cx
        .try_global::<StagePlayerViewCache>()
        .and_then(|cache| cache.view.clone());
    let Some(stage) = existing else {
        return;
    };

    let chrome_visible = stage_chrome::target_visible(app);
    stage.update(cx, |stage, cx| {
        if stage.chrome_layout.set_target(chrome_visible) {
            cx.notify();
        }
    });
}

pub(super) fn sync_if_created(app: &MusicApp, cx: &mut Context<MusicApp>) {
    let existing = cx
        .try_global::<StagePlayerViewCache>()
        .and_then(|cache| cache.view.clone());
    let Some(stage) = existing else {
        return;
    };

    let track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let key = StagePlayerRenderKey {
        track_id,
        artwork_ptr: artwork.as_ref().map_or(0, |bytes| bytes.as_ptr() as usize),
        artwork_len: artwork.as_ref().map_or(0, |bytes| bytes.len()),
    };

    stage.update(cx, |stage, cx| {
        let mut changed = false;
        if stage.key != key {
            stage.key = key;
            stage.cover = StageCoverRenderData::from_track(
                app.snapshot.current_track.as_ref(),
                artwork,
            );
            changed = true;
        }
        if changed {
            cx.notify();
        }
    });
}

pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    fluid_background: Entity<AppleFluidView>,
) -> gpui::AnyElement {
    let lyrics = stage_lyrics::view(app, cx);
    let controls = stage_controls::view(app, cx);
    let track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let key = StagePlayerRenderKey {
        track_id,
        artwork_ptr: artwork.as_ref().map_or(0, |bytes| bytes.as_ptr() as usize),
        artwork_len: artwork.as_ref().map_or(0, |bytes| bytes.len()),
    };
    let playing = app.snapshot.state == PlaybackState::Playing;
    let chrome_visible = stage_chrome::target_visible(app);

    let ui_events = app_ui_events::bridge(cx);
    let initial_fluid = fluid_background.clone();
    let initial_lyrics = lyrics.clone();
    let initial_controls = controls.clone();
    let stage = cx.update_default_global(move |cache: &mut StagePlayerViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view_events = ui_events.clone();
        let view = cx.new(move |cx| {
            let ui_subscription = cx.subscribe(&view_events, |stage: &mut StagePlayerView, _bridge, event, cx| {
                if let AppUiEvent::PlaybackStateChanged(state) = *event {
                    let playing = state == PlaybackState::Playing;
                    if stage.playing != playing {
                        stage.playing = playing;
                        let fluid = stage.fluid_background.clone();
                        fluid.update(cx, |view, cx| view.set_playing(playing, cx));
                    }
                }
            });
            StagePlayerView {
                fluid_background: initial_fluid,
                lyrics: initial_lyrics,
                controls: initial_controls,
                cover: StageCoverRenderData::default(),
                key: StagePlayerRenderKey::default(),
                playing: false,
                chrome_layout: StageChromeLayoutMotion::new(chrome_visible),
                _ui_subscription: ui_subscription,
            }
        });
        cache.view = Some(view.clone());
        view
    });

    stage.update(cx, |stage, cx| {
        let mut changed = false;
        let fluid_changed = stage.fluid_background != fluid_background;
        if stage.key != key {
            stage.key = key;
            stage.cover = StageCoverRenderData::from_track(
                app.snapshot.current_track.as_ref(),
                artwork.clone(),
            );
            changed = true;
        }
        if fluid_changed {
            stage.fluid_background = fluid_background.clone();
            changed = true;
        }
        if stage.playing != playing || fluid_changed {
            stage.playing = playing;
            let fluid = stage.fluid_background.clone();
            fluid.update(cx, |view, cx| view.set_playing(playing, cx));
        }
        if stage.lyrics != lyrics {
            stage.lyrics = lyrics.clone();
            changed = true;
        }
        if stage.controls != controls {
            stage.controls = controls.clone();
            changed = true;
        }
        if stage.chrome_layout.set_target(chrome_visible) {
            changed = true;
        }
        if changed {
            cx.notify();
        }
    });

    stage.into_any_element()
}

struct StagePlayerView {
    fluid_background: Entity<AppleFluidView>,
    lyrics: Entity<stage_lyrics::StageLyricsView>,
    controls: Entity<stage_controls::StageControlsView>,
    cover: StageCoverRenderData,
    key: StagePlayerRenderKey,
    playing: bool,
    chrome_layout: StageChromeLayoutMotion,
    _ui_subscription: Subscription,
}

impl Render for StagePlayerView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // with_layout_animation_target already schedules the next platform presentation frame.
        // Sample the shared frame clock instead of running a second fixed 60 Hz timer.
        let layout_animating = self.chrome_layout.advance(window.animation_time());

        let visible_progress = self.chrome_layout.value();
        let viewport_height = f32::from(window.viewport_size().height).max(1.0);
        let visible_cover_size = (viewport_height * 0.34).clamp(220.0, 260.0);
        let hidden_cover_size = (visible_cover_size * 0.88).clamp(194.0, 228.0);
        let cover_size = stage_lerp(hidden_cover_size, visible_cover_size, visible_progress);
        let left_width = stage_lerp(
            STAGE_LEFT_HIDDEN_WIDTH_PX,
            STAGE_LEFT_VISIBLE_WIDTH_PX,
            visible_progress,
        );
        let column_gap = stage_lerp(
            STAGE_COLUMN_HIDDEN_GAP_PX,
            STAGE_COLUMN_VISIBLE_GAP_PX,
            visible_progress,
        );
        let content_top = stage_lerp(
            STAGE_CONTENT_HIDDEN_TOP_PX,
            STAGE_CONTENT_VISIBLE_TOP_PX,
            visible_progress,
        );
        let content_bottom = stage_lerp(
            STAGE_CONTENT_HIDDEN_BOTTOM_PX,
            STAGE_CONTENT_VISIBLE_BOTTOM_PX,
            visible_progress,
        );
        let metadata_height = STAGE_METADATA_SLOT_HEIGHT_PX * visible_progress;
        let controls_height = STAGE_CONTROLS_SLOT_HEIGHT_PX * visible_progress;
        let cluster_gap = 16.0 * visible_progress;
        let metadata_offset_y = -18.0 * (1.0 - visible_progress);

        let lyrics = AnyView::from(self.lyrics.clone())
            .cached(
                StyleRefinement::default()
                    .flex_1()
                    .h_full()
                    .min_w(px(0.0))
                    .min_h(px(0.0)),
            )
            .reuse_on_window_refresh();
        let controls = AnyView::from(self.controls.clone())
            .cached(
                StyleRefinement::default()
                    .w(px(320.0))
                    .h(px(STAGE_CONTROLS_SLOT_HEIGHT_PX)),
            )
            .reuse_on_window_refresh();

        let metadata = stage_metadata(&self.cover, visible_progress, metadata_offset_y);
        let control_slot = div()
            .w_full()
            .h(px(controls_height))
            .flex_none()
            .overflow_hidden()
            .flex()
            .justify_center()
            .child(controls);

        div()
            .id("stage-player-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x0e0f16))
            .text_color(TEXT_WHITE)
            // Pointer activity is handled once by shell's stage-drawer-root. Duplicating the same
            // move/down handlers here caused nested Entity::update calls for every pointer event.
            .child(ambient_background(self.fluid_background.clone()))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .px_8()
                    .pt(px(content_top))
                    .pb(px(content_bottom))
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .gap(px(column_gap))
                            .items_center()
                            .child(
                                div()
                                    .w(px(left_width))
                                    .h_full()
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap(px(cluster_gap))
                                    .child(stage_artwork(&self.cover, cover_size))
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(metadata_height))
                                            .flex_none()
                                            .overflow_hidden()
                                            .child(metadata),
                                    )
                                    .child(control_slot),
                            )
                            .child(lyrics),
                    ),
            )
            .with_layout_animation_target(layout_animating)
    }
}

fn stage_artwork(data: &StageCoverRenderData, size_px: f32) -> gpui::AnyElement {
    let cover = if let Some(bytes) = data.artwork.clone() {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(data.track_id.unwrap_or(0));
        div()
            .size_full()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(
                icon!(disc_3),
                (size_px * 0.32).clamp(64.0, 88.0),
                hsla(0.0, 0.0, 1.0, 0.7),
            ))
            .into_any_element()
    };

    div()
        .id("stage-cover-card")
        .size(px(size_px))
        .flex_none()
        .rounded_2xl()
        .overflow_hidden()
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.15))
        .shadow_lg()
        .child(cover)
        .into_any_element()
}

fn stage_metadata(
    data: &StageCoverRenderData,
    visibility: f32,
    offset_y: f32,
) -> gpui::AnyElement {
    div()
        .relative()
        .top(px(offset_y))
        .w_full()
        .flex()
        .justify_center()
        .opacity(visibility)
        .child(
            div()
                .w(px(280.0))
                .flex()
                .flex_col()
                .items_start()
                .gap_1()
                .child(
                    div()
                        .w_full()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .truncate()
                        .child(data.title.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.70))
                        .truncate()
                        .child(data.artist.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.34))
                        .truncate()
                        .child(data.album.clone()),
                ),
        )
        .into_any_element()
}

fn ambient_background(fluid_background: Entity<AppleFluidView>) -> gpui::AnyElement {
    let fluid = AnyView::from(fluid_background)
        .cached(StyleRefinement::default().size_full())
        .reuse_on_window_refresh();
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .child(fluid)
        .into_any_element()
}
