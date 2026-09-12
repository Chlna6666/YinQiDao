use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, Context, Easing,
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString,
    StatefulInteractiveElement as _, div, hsla, img, linear_color_stop, linear_gradient,
    prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    gpu::AppleFluidView,
    model::{PlaybackState, Track},
};

use super::{
    player_legacy, stage_controls, stage_lyrics,
    shell::MusicApp,
    theme::{TEXT_WHITE, elegant_gradient_for, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime, mini_player};

pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    fluid_background: gpui::Entity<AppleFluidView>,
) -> gpui::AnyElement {
    let snapshot = &app.snapshot;
    let track = snapshot.current_track.as_ref();
    let id = track.map(|track| track.id);
    let artwork = id.and_then(|id| app.artworks.get(&id).cloned());
    let transport_state = snapshot.state;
    let fluid_playing = transport_state == PlaybackState::Playing;
    fluid_background.update(cx, |view, cx| view.set_playing(fluid_playing, cx));
    let lyrics_view = stage_lyrics::view(app, cx);
    let controls_view = stage_controls::view(app, cx);

    div()
        .id("stage-player-root")
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .text_color(TEXT_WHITE)
        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {
            cx.stop_propagation();
            this.stage_last_mouse_pos = Some(event.position);
            if this.stage_suppress_wake_until.is_some()
                || this.stage_controls_visibility < 0.995
            {
                return;
            }
            this.stage_last_user_activity = Instant::now();
        }))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.stage_suppress_wake_until.is_some()
                    || this.stage_controls_visibility < 0.995
                {
                    this.wake_stage_controls_immediately(cx);
                } else {
                    this.stage_last_user_activity = Instant::now();
                }
            }),
        )
        .child(ambient_background(fluid_background))
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
                        .child(stage_cover(track, artwork))
                        .child(lyrics_view),
                )
                .child(controls_view),
        )
        .into_any_element()
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

    // Keep the transition identity tied only to the track. Async artwork replacing the fallback
    // image must not create a second animation identity, otherwise opening the stage can replay the
    // same scale/fade several times while cover metadata arrives.
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

fn ambient_background(fluid_background: gpui::Entity<AppleFluidView>) -> gpui::AnyElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .child(fluid_background)
        .into_any_element()
}
