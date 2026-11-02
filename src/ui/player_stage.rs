use std::sync::Arc;

use gpui::{
    AnimationExt as _, AnyView, BorrowAppContext as _, Context, EncodedImageBytes, Entity, Global, ImageFormat,
    IntoElement, ObjectFit, Render, SharedString, StyleRefinement, Subscription, Window, div, img,
    linear_color_stop, linear_gradient, prelude::*, px,
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
    theme::{dark, DURATION_LAYOUT, SampledMotion, STAGE_BG, TEXT_WHITE, ease_out_cubic, elegant_gradient_for, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime};

// Desktop split follows the golden ratio: 38.2% artwork, 61.8% lyrics.
// The ratio applies to usable column space, not to the square cover image.
const STAGE_LYRICS_GOLDEN_SHARE: f32 = 0.618_034;
const STAGE_STACKED_BREAKPOINT_PX: f32 = 800.0;
const STAGE_CONTROLS_SLOT_HEIGHT_PX: f32 = 158.0;
const STAGE_METADATA_SLOT_HEIGHT_PX: f32 = 72.0;
const STAGE_CONTENT_VISIBLE_TOP_PX: f32 = 54.0;
const STAGE_CONTENT_HIDDEN_TOP_PX: f32 = 24.0;
const STAGE_CONTENT_VISIBLE_BOTTOM_PX: f32 = 32.0;
const STAGE_CONTENT_HIDDEN_BOTTOM_PX: f32 = 24.0;
/// 舞台 chrome 布局位移：统一走 theme::SampledMotion（300ms ease-out cubic）。
type StageChromeLayoutMotion = SampledMotion;

fn stage_chrome_layout_motion(visible: bool) -> StageChromeLayoutMotion {
    SampledMotion::new(DURATION_LAYOUT, ease_out_cubic, visible)
}

#[inline]
fn stage_lerp(hidden: f32, visible: f32, visible_progress: f32) -> f32 {
    hidden + (visible - hidden) * visible_progress.clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug)]
struct StageLayoutMetrics {
    /// The content column split never changes when the chrome fades away.
    horizontal: bool,
    gutter: f32,
    column_gap: f32,
    artwork_column_width: f32,
    lyrics_column_width: f32,
    content_height: f32,
    cover_size: f32,
    controls_width: f32,
    content_top: f32,
    content_bottom: f32,
    metadata_height: f32,
    controls_height: f32,
    cluster_gap: f32,
    metadata_offset_y: f32,
}

impl StageLayoutMetrics {
    fn compute(viewport_width: f32, viewport_height: f32, visible_progress: f32) -> Self {
        let width = viewport_width.max(320.0);
        let height = viewport_height.max(320.0);
        let progress = visible_progress.clamp(0.0, 1.0);
        let horizontal = width >= STAGE_STACKED_BREAKPOINT_PX;
        let gutter = (width * 0.035).clamp(20.0, 48.0);
        let content_width = (width - 2.0 * gutter).max(1.0);
        let column_gap = (content_width * 0.04).clamp(24.0, 48.0);
        let column_space = (content_width - column_gap).max(1.0);
        let artwork_column_width = if horizontal {
            column_space * (1.0 - STAGE_LYRICS_GOLDEN_SHARE)
        } else {
            content_width
        };
        let lyrics_column_width = if horizontal {
            column_space - artwork_column_width
        } else {
            content_width
        };
        let content_top = stage_lerp(
            STAGE_CONTENT_HIDDEN_TOP_PX,
            STAGE_CONTENT_VISIBLE_TOP_PX,
            progress,
        );
        let content_bottom = stage_lerp(
            STAGE_CONTENT_HIDDEN_BOTTOM_PX,
            STAGE_CONTENT_VISIBLE_BOTTOM_PX,
            progress,
        );
        let metadata_height = STAGE_METADATA_SLOT_HEIGHT_PX * progress;
        let controls_height = STAGE_CONTROLS_SLOT_HEIGHT_PX * progress;
        let cluster_gap = 16.0 * progress;
        let available_height = (height - content_top - content_bottom).max(1.0);
        let cover_height_limit = if horizontal {
            (available_height - metadata_height - controls_height - cluster_gap * 2.0)
                .max(1.0)
                * 0.90
        } else {
            // Keep part of the viewport available for lyrics on narrow windows.
            (available_height
                - metadata_height
                - controls_height
                - cluster_gap * 2.0
                - 180.0)
                .max(1.0)
        };
        let cover_width_limit = if horizontal {
            artwork_column_width * 0.88
        } else {
            content_width * 0.60
        };
        let cover_size = cover_width_limit
            .min(cover_height_limit)
            .min(if horizontal { 400.0 } else { 280.0 })
            .max(1.0);
        let controls_width = (artwork_column_width - 12.0).min(320.0).max(1.0);

        Self {
            horizontal,
            gutter,
            column_gap,
            artwork_column_width,
            lyrics_column_width,
            content_height: available_height,
            cover_size,
            controls_width,
            content_top,
            content_bottom,
            metadata_height,
            controls_height,
            cluster_gap,
            metadata_offset_y: -18.0 * (1.0 - progress),
        }
    }

    fn artwork_cluster_height(self) -> f32 {
        self.cover_size + self.metadata_height + self.controls_height + 2.0 * self.cluster_gap
    }
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
                chrome_layout: stage_chrome_layout_motion(chrome_visible),
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

    // StageLyrics owns its own retained/cached boundary. Keep the stage shell cached as well so
    // a lyric-only notify makes StagePlayerView a TraversalAncestor instead of rerendering cover,
    // metadata, controls, and the whole stage layout. Direct StagePlayer updates (track/chrome
    // layout/etc.) still invalidate this view and take the normal render path.
    AnyView::from(stage)
        .cached(StyleRefinement::default().size_full())
        .into_any_element()
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
        let viewport = window.viewport_size();
        let layout = StageLayoutMetrics::compute(
            f32::from(viewport.width),
            f32::from(viewport.height),
            visible_progress,
        );

        // The exact same golden split is passed to both renderer branches.
        // No independent window-width estimates are used in the lyric column.
        let lyrics = AnyView::from(self.lyrics.clone())
            .cached(if layout.horizontal {
                StyleRefinement::default()
                    .w(px(layout.lyrics_column_width))
                    .h_full()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
            } else {
                StyleRefinement::default()
                    .w_full()
                    .flex_1()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
            })
            .reuse_on_window_refresh();
        let controls = AnyView::from(self.controls.clone())
            .cached(
                StyleRefinement::default()
                    .w(px(layout.controls_width))
                    .h(px(STAGE_CONTROLS_SLOT_HEIGHT_PX)),
            )
            .reuse_on_window_refresh();

        let metadata = stage_metadata(
            &self.cover,
            visible_progress,
            layout.metadata_offset_y,
            layout.cover_size,
        );
        let control_slot = div()
            .w_full()
            .h(px(layout.controls_height))
            .flex_none()
            .overflow_hidden()
            .flex()
            .justify_center()
            .child(controls);

        let artwork_column = div()
            .w(px(layout.artwork_column_width))
            .h(if layout.horizontal {
                px(layout.content_height)
            } else {
                px(layout.artwork_cluster_height())
            })
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(layout.cluster_gap))
            .child(stage_artwork(&self.cover, layout.cover_size))
            .child(
                div()
                    .w_full()
                    .h(px(layout.metadata_height))
                    .flex_none()
                    .overflow_hidden()
                    .child(metadata),
            )
            .child(control_slot);

        let columns = if layout.horizontal {
            div()
                .relative()
                .flex()
                .flex_1()
                .min_h(px(0.0))
                .gap(px(layout.column_gap))
                .items_center()
                .child(artwork_column)
                .child(lyrics)
        } else {
            div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .gap(px(12.0))
                .items_center()
                .child(artwork_column)
                .child(lyrics)
        };

        div()
            .id("stage-player-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(STAGE_BG)
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
                    .px(px(layout.gutter))
                    .pt(px(layout.content_top))
                    .pb(px(layout.content_bottom))
                    .child(columns),
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
                dark::white(0.7),
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
        .border_color(dark::white(0.15))
        .shadow_lg()
        .child(cover)
        .into_any_element()
}

fn stage_metadata(
    data: &StageCoverRenderData,
    visibility: f32,
    offset_y: f32,
    artwork_width: f32,
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
                // Text aligns with the artwork's leading edge instead
                // of retaining a fixed 280px width when the cover grows.
                .w(px(artwork_width))
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
                        .text_color(dark::white(0.70))
                        .truncate()
                        .child(data.artist.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(dark::white(0.34))
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
        .bg(STAGE_BG)
        .child(fluid)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_ratio_allocates_desktop_columns_without_hidden_chrome_drift() {
        let hidden = StageLayoutMetrics::compute(1116.0, 713.0, 0.0);
        let visible = StageLayoutMetrics::compute(1116.0, 713.0, 1.0);
        assert!(hidden.horizontal);
        let usable = 1116.0 - hidden.gutter * 2.0 - hidden.column_gap;
        assert!((hidden.artwork_column_width / usable - 0.381_966).abs() < 0.0001);
        assert!((hidden.lyrics_column_width / usable - 0.618_034).abs() < 0.0001);
        assert!((hidden.artwork_column_width - visible.artwork_column_width).abs() < 0.01);
        assert!((hidden.lyrics_column_width - visible.lyrics_column_width).abs() < 0.01);
        assert!(hidden.cover_size > 280.0);
        assert!(hidden.cover_size <= hidden.artwork_column_width);
    }

    #[test]
    fn artwork_and_controls_fit_at_visible_and_hidden_extremes() {
        for width in [640.0, 800.0, 1116.0, 1920.0] {
            for height in [540.0, 713.0, 1080.0] {
                for progress in [0.0, 0.5, 1.0] {
                    let metrics = StageLayoutMetrics::compute(width, height, progress);
                    let available = height - metrics.content_top - metrics.content_bottom;
                    assert!(metrics.cover_size <= metrics.artwork_column_width);
                    assert!(metrics.controls_width <= metrics.artwork_column_width);
                    assert!(metrics.artwork_cluster_height() <= available + 0.01);
                    assert!(metrics.lyrics_column_width > 0.0);
                }
            }
        }
    }

    #[test]
    fn narrow_windows_stack_without_horizontal_overflow() {
        let metrics = StageLayoutMetrics::compute(700.0, 720.0, 0.0);
        assert!(!metrics.horizontal);
        assert!((metrics.artwork_column_width - metrics.lyrics_column_width).abs() < 0.01);
        assert!(metrics.artwork_cluster_height() < 400.0);
    }
}
