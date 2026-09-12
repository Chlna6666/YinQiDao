use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, BorrowAppContext as _, Context,
    Easing, EncodedImageBytes, Entity, Global, ImageFormat, IntoElement, ObjectFit, Render,
    SharedString, StatefulInteractiveElement as _, WeakEntity, Window, div, hsla, img,
    linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    gpu::AppleFluidView,
    model::{PlaybackState, Track, TrackId},
};

use super::{
    player_legacy, stage_controls, stage_lyrics,
    shell::MusicApp,
    theme::{TEXT_WHITE, elegant_gradient_for, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime, mini_player};

#[derive(Default)]
struct StagePlayerViewCache {
    view: Option<Entity<StagePlayerView>>,
}

impl Global for StagePlayerViewCache {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StagePlayerRenderKey {
    track_id: Option<TrackId>,
    artwork_ptr: usize,
    artwork_len: usize,
}

pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    fluid_background: Entity<AppleFluidView>,
) -> gpui::AnyElement {
    let lyrics = stage_lyrics::view(app, cx);
    let controls = stage_controls::view(app, cx);
    let parent = cx.entity().downgrade();
    let track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let key = StagePlayerRenderKey {
        track_id,
        artwork_ptr: artwork.as_ref().map_or(0, |bytes| bytes.as_ptr() as usize),
        artwork_len: artwork.as_ref().map_or(0, |bytes| bytes.len()),
    };

    fluid_background.update(cx, |view, cx| {
        view.set_playing(app.snapshot.state == PlaybackState::Playing, cx)
    });

    let initial_fluid = fluid_background.clone();
    let initial_lyrics = lyrics.clone();
    let initial_controls = controls.clone();
    let initial_track = app.snapshot.current_track.clone();
    let initial_artwork = artwork.clone();
    let stage = cx.update_default_global(move |cache: &mut StagePlayerViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StagePlayerView {
            parent,
            fluid_background: initial_fluid,
            lyrics: initial_lyrics,
            controls: initial_controls,
            track: initial_track,
            artwork: initial_artwork,
            key,
        });
        cache.view = Some(view.clone());
        view
    });

    stage.update(cx, |stage, cx| {
        let mut changed = false;
        if stage.key != key {
            stage.key = key;
            stage.track = app.snapshot.current_track.clone();
            stage.artwork = artwork.clone();
            changed = true;
        }
        if stage.fluid_background != fluid_background {
            stage.fluid_background = fluid_background.clone();
            changed = true;
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
    parent: WeakEntity<MusicApp>,
    fluid_background: Entity<AppleFluidView>,
    lyrics: Entity<stage_lyrics::StageLyricsView>,
    controls: Entity<stage_controls::StageControlsView>,
    track: Option<Track>,
    artwork: Option<Arc<[u8]>>,
    key: StagePlayerRenderKey,
}

impl Render for StagePlayerView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let parent_move = self.parent.clone();
        let parent_down = self.parent.clone();
        div()
            .id("stage-player-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x0e0f16))
            .text_color(TEXT_WHITE)
            .on_mouse_move(move |event: &gpui::MouseMoveEvent, _, cx| {
                cx.stop_propagation();
                let _ = parent_move.update(cx, |app, _cx| {
                    app.stage_last_mouse_pos = Some(event.position);
                    if app.stage_suppress_wake_until.is_some()
                        || app.stage_controls_visibility < 0.995
                    {
                        return;
                    }
                    app.stage_last_user_activity = Instant::now();
                });
            })
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                let _ = parent_down.update(cx, |app, app_cx| {
                    if app.stage_suppress_wake_until.is_some()
                        || app.stage_controls_visibility < 0.995
                    {
                        app.wake_stage_controls_immediately(app_cx);
                    } else {
                        app.stage_last_user_activity = Instant::now();
                    }
                });
            })
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
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .gap_12()
                            .items_center()
                            .child(stage_cover(self.track.as_ref(), self.artwork.clone()))
                            .child(self.lyrics.clone()),
                    )
                    .child(self.controls.clone()),
            )
    }
}

fn stage_cover(track: Option<&Track>, artwork: Option<Arc<[u8]>>) -> impl IntoElement {
    let title = track.map_or("未在播放音乐", |track| track.title.as_str());
    let artist = track.map_or("请选择音乐", |track| track.artist.as_str());
    let album = track.map_or("未知专辑", |track| track.album.as_str());
    let track_key = track.map_or(i64::MIN, |track| track.id);
    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track.map_or(0, |track| track.id));
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

    let cover_enter = Animation::from_spec(
        AnimationSpec::new(Duration::from_millis(220)).ease(Easing::OutCubic),
    )
    .with_property(AnimationProperty::scale_opacity(
        0.975,
        1.0,
        0.0,
        1.0,
        gpui::TransformOrigin::CENTER,
    ));
    let cover_card = div()
        .size(px(280.0))
        .rounded_2xl()
        .overflow_hidden()
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.15))
        .shadow_lg()
        .child(cover)
        .with_animation(
            SharedString::from(format!("stage-cover-enter-{track_key}")),
            cover_enter,
            |element, _| element,
        );

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
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_base()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.72))
                        .truncate()
                        .child(artist.to_owned()),
                )
                .child(
                    div()
                        .max_w(px(360.0))
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.42))
                        .truncate()
                        .child(album.to_owned()),
                ),
        )
}

fn ambient_background(fluid_background: Entity<AppleFluidView>) -> gpui::AnyElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .child(fluid_background)
        .into_any_element()
}
