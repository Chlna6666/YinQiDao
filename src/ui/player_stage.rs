use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, Context, Easing,
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, Transition,
    TransitionProperty, StatefulInteractiveElement as _, div, hsla, img, linear_color_stop,
    linear_gradient, point, prelude::*, px, rgb,
};
use lucide_gpui::icons as lucide_icons;

use crate::{
    audio::PlayerCommand,
    gpu::AppleFluidView,
    lyrics::LyricLine,
    model::{PlaybackState, Track},
};

use super::{
    components::{SliderStyle, interactive_slider},
    player_legacy,
    shell::{DragTarget, MusicApp},
    theme::{
        ACCENT_RED, TEXT_WHITE, elegant_gradient_for, format_remaining_time, format_time,
        themed_icon,
    },
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime};

pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    fluid_background: gpui::Entity<AppleFluidView>,
) -> gpui::AnyElement {
    let snapshot = &app.snapshot;
    let track = snapshot.current_track.as_ref();
    let id = track.map(|track| track.id);
    let artwork = id.and_then(|id| app.artworks.get(&id).cloned());
    let lyrics = id
        .and_then(|id| app.lyrics.get(&id))
        .map_or(&[][..], |document| document.timed_lines());
    let (transport_state, transport_position_ms, _) = live_transport(app);
    let fluid_playing = transport_state == PlaybackState::Playing;
    fluid_background.update(cx, |view, cx| view.set_playing(fluid_playing, cx));

    div()
        .id("stage-player-root")
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .text_color(TEXT_WHITE)
        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {
            // Once clean/immersive mode has started, pointer motion must not wake the chrome.
            // This also turns the automatic 20 s idle fade into click-to-wake instead of the old
            // accidental wake caused by tiny mouse jitter or layout-generated move events.
            cx.stop_propagation();
            if this.stage_clean_mode {
                return;
            }
            this.note_stage_pointer_activity(event.position, cx);
        }))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.stage_clean_mode {
                    this.wake_stage_controls(cx);
                } else {
                    this.note_stage_activity(cx);
                }
            }),
        )
        .child(render_fluid_background(app, fluid_background))
        .child(render_stage_overlay(app, cx, track, artwork, lyrics, transport_position_ms))
        .into_any_element()
}

fn render_fluid_background(
    app: &MusicApp,
    fluid_background: gpui::Entity<AppleFluidView>,
) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .child(fluid_background)
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(hsla(230.0, 0.20, 0.06, if app.config.dynamic_blur { 0.26 } else { 0.52 })),
        )
}

fn render_stage_overlay(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    track: Option<&Track>,
    artwork: Option<Arc<[u8]>>,
    lyrics: &[LyricLine],
    position_ms: u64,
) -> impl IntoElement {
    let controls_visible = app.stage_controls_visible();
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .child(render_stage_top_bar(app, cx, track, controls_visible))
        .child(render_stage_center(app, cx, track, artwork, lyrics, position_ms))
        .child(render_stage_bottom_controls(app, cx, controls_visible))
}

fn render_stage_top_bar(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    track: Option<&Track>,
    controls_visible: bool,
) -> impl IntoElement {
    let title = track.map_or("未播放", |track| track.title.as_str());
    let artist = track.map_or("", |track| track.artist.as_str());
    div()
        .h(px(76.0))
        .px_7()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .opacity(if controls_visible { 1.0 } else { 0.0 })
        .transition(Transition::new(Duration::from_millis(220)).with_easing(Easing::EaseOut))
        .child(
            div()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_WHITE)
                        .truncate()
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.58))
                        .truncate()
                        .child(artist.to_owned()),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("stage-clean-mode")
                        .px_3()
                        .py_1p5()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if app.stage_clean_mode {
                            hsla(0.0, 0.0, 1.0, 0.18)
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.08)
                        })
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_WHITE)
                        .child(if app.stage_clean_mode { "退出纯享" } else { "纯享沉浸" })
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_stage_clean_mode(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("stage-close")
                        .size(px(34.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(hsla(0.0, 0.0, 1.0, 0.08))
                        .child(themed_icon(
                            lucide_icons::icon_x(),
                            18.0,
                            hsla(0.0, 0.0, 1.0, 0.88),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_stage(cx);
                            }),
                        ),
                ),
        )
}

fn render_stage_center(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    track: Option<&Track>,
    artwork: Option<Arc<[u8]>>,
    lyrics: &[LyricLine],
    position_ms: u64,
) -> impl IntoElement {
    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(330.0))
            .rounded_2xl()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let id = track.map_or(0, |track| track.id);
        let (c1, c2) = elegant_gradient_for(id);
        div()
            .size(px(330.0))
            .rounded_2xl()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(
                lucide_icons::icon_disc_3(),
                72.0,
                hsla(0.0, 0.0, 1.0, 0.82),
            ))
            .into_any_element()
    };

    div()
        .flex_1()
        .min_h(px(0.0))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(68.0))
        .px(px(82.0))
        .child(
            div()
                .flex_none()
                .child(
                    div()
                        .size(px(330.0))
                        .rounded_2xl()
                        .shadow_2xl()
                        .child(cover),
                )
                .child_if(track.is_some(), || {
                    div()
                        .mt_6()
                        .w(px(330.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_WHITE)
                                .truncate()
                                .child(track.map_or(String::new(), |track| track.title.clone())),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(hsla(0.0, 0.0, 1.0, 0.56))
                                .truncate()
                                .child(track.map_or(String::new(), |track| track.artist.clone())),
                        )
                }),
        )
        .child(render_stage_lyrics(app, cx, lyrics, position_ms))
}

fn render_stage_lyrics(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    lyrics: &[LyricLine],
    position_ms: u64,
) -> impl IntoElement {
    let active = lyrics
        .iter()
        .rposition(|line| line.timestamp_ms <= position_ms)
        .unwrap_or(0);
    let visible_start = active.saturating_sub(3);
    let visible_end = (active + 5).min(lyrics.len());
    let mut list = div()
        .id("stage-lyrics")
        .w(px(560.0))
        .h(px(470.0))
        .flex_none()
        .flex()
        .flex_col()
        .justify_center()
        .gap_3();

    if lyrics.is_empty() {
        return list
            .items_center()
            .child(
                div()
                    .text_lg()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.42))
                    .child("暂无同步歌词"),
            )
            .into_any_element();
    }

    for (index, line) in lyrics[visible_start..visible_end].iter().enumerate() {
        let actual_index = visible_start + index;
        let is_active = actual_index == active;
        let distance = actual_index.abs_diff(active) as f32;
        let opacity = if is_active { 1.0 } else { (0.68 - distance * 0.11).max(0.20) };
        let scale = if is_active { 1.0 } else { 0.94 };
        let line_id = SharedString::from(format!("stage-lyric-{actual_index}"));
        let line_timestamp = line.timestamp_ms;
        list = list.child(
            div()
                .id(line_id)
                .w_full()
                .cursor_pointer()
                .opacity(opacity)
                .scale(scale)
                .transition(
                    Transition::new(Duration::from_millis(300)).with_easing(Easing::EaseOut),
                )
                .child(
                    div()
                        .text_size(px(if is_active { 30.0 } else { 22.0 }))
                        .font_weight(if is_active {
                            gpui::FontWeight::BOLD
                        } else {
                            gpui::FontWeight::MEDIUM
                        })
                        .text_color(if is_active {
                            TEXT_WHITE
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.74)
                        })
                        .child(line.text.clone()),
                )
                .child_if(
                    line.translation
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty()),
                    || {
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(hsla(0.0, 0.0, 1.0, if is_active { 0.68 } else { 0.40 }))
                            .child(line.translation.clone().unwrap_or_default())
                    },
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.seek_to(line_timestamp, cx);
                    }),
                ),
        );
    }

    list.into_any_element()
}

fn render_stage_bottom_controls(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    controls_visible: bool,
) -> impl IntoElement {
    let snapshot = &app.snapshot;
    let duration = snapshot.duration_ms.max(1);
    let progress = app.displayed_seek_ratio();
    let is_playing = snapshot.state == PlaybackState::Playing;
    let app_entity = cx.entity();

    div()
        .h(px(122.0))
        .px(px(78.0))
        .pb_7()
        .flex_none()
        .flex()
        .flex_col()
        .justify_end()
        .gap_3()
        .opacity(if controls_visible { 1.0 } else { 0.0 })
        .transition(Transition::new(Duration::from_millis(220)).with_easing(Easing::EaseOut))
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .w(px(54.0))
                        .text_xs()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.62))
                        .child(format_time(snapshot.position_ms)),
                )
                .child(
                    interactive_slider(
                        "stage-progress",
                        progress,
                        SliderStyle::player_progress(),
                        {
                            let app_entity = app_entity.clone();
                            move |ratio, cx| {
                                let _ = app_entity.update(cx, |app, cx| {
                                    app.pending_seek_ratio = Some(ratio);
                                    cx.notify();
                                });
                            }
                        },
                        {
                            let app_entity = app_entity.clone();
                            move |ratio, cx| {
                                let _ = app_entity.update(cx, |app, cx| {
                                    app.begin_or_update_seek_drag(ratio, cx);
                                });
                            }
                        },
                        {
                            let app_entity = app_entity.clone();
                            move |ratio, cx| {
                                let _ = app_entity.update(cx, |app, cx| {
                                    app.finish_seek_drag(ratio, cx);
                                });
                            }
                        },
                    )
                    .flex_1(),
                )
                .child(
                    div()
                        .w(px(54.0))
                        .text_right()
                        .text_xs()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.62))
                        .child(format_remaining_time(snapshot.position_ms, duration)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap_6()
                .child(
                    div()
                        .id("stage-prev")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .child(themed_icon(
                            lucide_icons::icon_skip_back(),
                            20.0,
                            hsla(0.0, 0.0, 1.0, 0.82),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.previous(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("stage-play")
                        .size(px(46.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(hsla(0.0, 0.0, 1.0, 0.92))
                        .child(themed_icon(
                            if is_playing {
                                lucide_icons::icon_pause()
                            } else {
                                lucide_icons::icon_play()
                            },
                            22.0,
                            hsla(220.0, 0.10, 0.12, 1.0),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_play(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("stage-next")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .child(themed_icon(
                            lucide_icons::icon_skip_forward(),
                            20.0,
                            hsla(0.0, 0.0, 1.0, 0.82),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.next(cx);
                            }),
                        ),
                ),
        )
}

fn live_transport(app: &MusicApp) -> (PlaybackState, u64, u64) {
    app.engine.as_ref().map_or(
        (
            app.snapshot.state,
            app.snapshot.position_ms,
            app.snapshot.duration_ms,
        ),
        |engine| {
            let (_, position_ms, duration_ms) = engine.progress();
            (engine.state(), position_ms, duration_ms)
        },
    )
}
