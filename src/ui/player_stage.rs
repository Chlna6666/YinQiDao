use std::sync::Arc;

use gpui::{
    AnyView, BorrowAppContext as _, Context, EncodedImageBytes, Entity, Global, ImageFormat,
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
    player_legacy, stage_controls, stage_lyrics,
    shell::MusicApp,
    theme::{TEXT_WHITE, elegant_gradient_for, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime};

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
            let ui_subscription = cx.subscribe(&view_events, |stage, _bridge, event, cx| {
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
    _ui_subscription: Subscription,
}

impl Render for StagePlayerView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
                    .w_full()
                    .h(px(72.0)),
            )
            .reuse_on_window_refresh();
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
                    .pt(px(54.0))
                    .pb_8()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .gap_12()
                            .items_center()
                            // The transport dock is an overlay, not a layout sibling. Lyrics keep
                            // painting to the bottom edge and remain visible underneath the
                            // translucent dock like Apple Music, while the dock's occlude() hitbox
                            // still owns pointer input above the lyric surface.
                            .child(stage_cover(&self.cover))
                            .child(lyrics)
                            .child(
                                div()
                                    .absolute()
                                    .left(px(0.0))
                                    .right(px(0.0))
                                    .bottom(px(0.0))
                                    .child(controls),
                            ),
                    ),
            )
    }
}

fn stage_cover(data: &StageCoverRenderData) -> impl IntoElement {
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
                96.0,
                hsla(0.0, 0.0, 1.0, 0.7),
            ))
            .into_any_element()
    };

    // The whole immersive stage already owns the enter/exit translation animation. Keeping a
    // second 0→1 opacity animation on the cover made stage prewarm/rematerialization temporarily
    // hide an otherwise ready texture and showed up as a one-frame flash on every drawer open.
    // Keep the card identity stable and fully opaque; track/artwork changes only replace its child.
    let cover_card = div()
        .id("stage-cover-card")
        .size(px(280.0))
        .rounded_2xl()
        .overflow_hidden()
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.15))
        .shadow_lg()
        .child(cover);

    div()
        .w(px(380.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        .child(cover_card)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_center()
                        .truncate()
                        .child(data.title.clone()),
                )
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_base()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.72))
                        .truncate()
                        .child(data.artist.clone()),
                )
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.42))
                        .truncate()
                        .child(data.album.clone()),
                ),
        )
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
