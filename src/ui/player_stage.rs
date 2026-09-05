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
    model::{PlaybackState, PlayerSnapshot, Track},
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
    let lyrics = id
        .and_then(|id| app.lyrics.get(&id))
        .map_or(&[][..], |document| document.timed_lines());
    let fluid_playing = snapshot.state == PlaybackState::Playing;

    div()
        .id("stage-player-root")
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .text_color(TEXT_WHITE)
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| this.wake_stage_controls_immediately(cx)),
        )
        .child(ambient_background(fluid_background, fluid_playing))
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
                        .child(stage_lyrics(app, lyrics, snapshot.position_ms, cx)),
                )
                .child(stage_controls(app, snapshot, cx)),
        )
        .into_any_element()
}

fn stage_cover(track: Option<&Track>, artwork: Option<Arc<[u8]>>) -> impl IntoElement {
    let title = track.map_or("未在播放音乐", |track| track.title.as_str());
    let artist = track.map_or("请选择音乐", |track| track.artist.as_str());
    let album = track.map_or("未知专辑", |track| track.album.as_str());
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
                lucide_icons::icon_disc_3(),
                96.0,
                hsla(0.0, 0.0, 1.0, 0.7),
            ))
            .into_any_element()
    };

    div()
        .w(px(380.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        .child(
            div()
                .size(px(280.0))
                .rounded_2xl()
                .overflow_hidden()
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                .shadow_lg()
                .child(cover),
        )
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

fn stage_lyrics(
    app: &MusicApp,
    lyrics: &[LyricLine],
    position_ms: u64,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    if lyrics.is_empty() {
        return div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(themed_icon(
                lucide_icons::icon_music(),
                36.0,
                hsla(0.0, 0.0, 1.0, 0.25),
            ))
            .child(
                div()
                    .text_lg()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.50))
                    .child("暂无同步滚动歌词"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.30))
                    .child("支持内嵌 LRC 或联网自动检索"),
            )
            .into_any_element();
    }

    let active = lyrics
        .iter()
        .rposition(|line| line.timestamp_ms <= position_ms)
        .unwrap_or(0);

    let mut viewport = div()
        .id("stage-lyrics-viewport")
        .relative()
        .flex_1()
        .h_full()
        .min_w(px(0.0))
        .min_h(px(0.0))
        .overflow_y_scroll()
        .scrollbar_width(px(0.0))
        .track_scroll(&app.lyrics_scroll_handle)
        .pt(px(96.0))
        .pb(px(112.0))
        .pr(px(8.0))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| this.wake_stage_controls_immediately(cx)),
        )
        .on_scroll_wheel(cx.listener(
            |this, _: &gpui::ScrollWheelEvent, _, cx| {
                this.lyrics_user_scrolling_until =
                    Some(Instant::now() + Duration::from_secs(3));
                this.lyrics_scroll_target_y = None;
                this.wake_stage_controls(cx);
            },
        ));

    for (index, line) in lyrics.iter().enumerate() {
        let distance = index.abs_diff(active);
        let alpha = match distance {
            0 => 1.0,
            1 => 0.48,
            2 => 0.28,
            _ => 0.18,
        };
        let scale = match distance {
            0 => 1.0,
            1 => 0.955,
            _ => 0.925,
        };
        let timestamp = line.timestamp_ms;
        let weight = if index == active {
            gpui::FontWeight::BOLD
        } else if distance == 1 {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        };

        let mut text = div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap_1()
            .font_weight(weight)
            .child(
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .text_size(px(28.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                    .child(line.text.clone()),
            );

        if let Some(translation) = line
            .translation
            .as_deref()
            .filter(|translation| !translation.trim().is_empty())
        {
            text = text.child(
                div()
                    .w_full()
                    .min_w(px(0.0))
                    .text_size(px(17.0))
                    .text_color(hsla(0.0, 0.0, 1.0, 0.72))
                    .child(translation.to_owned()),
            );
        }

        let line_element = div()
            .id(SharedString::from(format!("lyric-line-{index}")))
            .w_full()
            .min_w(px(0.0))
            .flex_none()
            .pl(px(16.0))
            .pr(px(12.0))
            .py(px(11.0))
            .mb(px(10.0))
            .scale(scale)
            .opacity(alpha)
            .transition(lyric_focus_transition())
            .cursor_pointer()
            .hover(|style| style.opacity(1.0))
            .child(text)
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.seek_to_ms(timestamp, cx);
                    this.lyrics_user_scrolling_until = None;
                    if this.last_lyric_index != Some(index) {
                        this.last_lyric_index = Some(index);
                        this.lyric_motion_epoch = this.lyric_motion_epoch.wrapping_add(1);
                    }
                    this.lyrics_scroll_target_y =
                        Some(f32::from(this.lyrics_scroll_handle.offset().y));
                    this.wake_stage_controls_immediately(cx);
                }),
            );

        let line_element = if index == active {
            let enter = Animation::from_spec(
                AnimationSpec::new(Duration::from_millis(440)).ease(Easing::OutCubic),
            )
            .with_property(AnimationProperty::translation(
                point(px(0.0), px(20.0)),
                point(px(0.0), px(0.0)),
            ));
            line_element
                .with_animation(
                    SharedString::from(format!(
                        "lyric-enter-{}-{index}",
                        app.lyric_motion_epoch
                    )),
                    enter,
                    |element, _| element,
                )
                .into_any_element()
        } else {
            line_element.into_any_element()
        };

        viewport = viewport.child(line_element);
    }

    viewport.into_any_element()
}

fn lyric_focus_transition() -> Transition {
    Transition::new(Duration::from_millis(420))
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity, TransitionProperty::Scale])
}
fn stage_controls(
    app: &MusicApp,
    snapshot: &PlayerSnapshot,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let position = app.displayed_position_ms();
    let volume = app.displayed_volume_ratio();
    let playing = snapshot.state == PlaybackState::Playing;
    let visibility = app.stage_controls_visibility;

    div()
        .id("stage-bottom-dock")
        .top(px((1.0 - visibility) * 56.0))
        .opacity(visibility)
        .flex()
        .items_center()
        .gap_5()
        .px_6()
        .py_3()
        .rounded_2xl()
        .bg(hsla(0.0, 0.0, 0.0, 0.40))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.stage_controls_hovered = *hovered;
            if *hovered {
                this.wake_stage_controls(cx);
            }
        }))
        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {
            this.handle_stage_mouse_move(event.position, cx);
        }))
        .child(
            div()
                .text_xs()
                .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                .child(format_time(position)),
        )
        .child(
            interactive_slider(
                "stage-progress-track",
                app.displayed_progress_ratio(),
                SliderStyle::stage_progress(),
                {
                    let view = cx.entity().downgrade();
                    move |ratio, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.wake_stage_controls_immediately(cx);
                            if this.drag_target == Some(DragTarget::Progress) {
                                this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                            } else {
                                this.begin_drag(DragTarget::Progress, ratio, cx);
                            }
                        });
                    }
                },
                {
                    let view = cx.entity().downgrade();
                    move |ratio, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.wake_stage_controls_immediately(cx);
                            if this.drag_target == Some(DragTarget::Progress) {
                                this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                            } else {
                                this.begin_drag(DragTarget::Progress, ratio, cx);
                            }
                            this.commit_drag(cx);
                        });
                    }
                },
            )
            .flex_1(),
        )
        .child(
            div()
                .text_xs()
                .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                .child(format_remaining_time(position, snapshot.duration_ms)),
        )
        .child(control_button(
            "stage-prev-btn",
            lucide_icons::icon_skip_back(),
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.wake_stage_controls_immediately(cx);
                this.previous(cx);
            }),
        ))
        .child(
            div()
                .id("stage-play-btn")
                .size(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(ACCENT_RED)
                .active(|style| style.scale(0.95))
                .child(themed_icon(
                    if playing {
                        lucide_icons::icon_pause()
                    } else {
                        lucide_icons::icon_play()
                    },
                    22.0,
                    hsla(0.0, 0.0, 1.0, 1.0),
                ))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.wake_stage_controls_immediately(cx);
                        this.toggle_play(cx);
                    }),
                ),
        )
        .child(control_button(
            "stage-next-btn",
            lucide_icons::icon_skip_forward(),
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.wake_stage_controls_immediately(cx);
                this.next(cx);
            }),
        ))
        .child(
            div()
                .id("stage-volume-group")
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_full()
                .bg(hsla(0.0, 0.0, 1.0, 0.08))
                .child(
                    div()
                        .id("stage-volume-mute")
                        .cursor_pointer()
                        .child(themed_icon(
                            if volume <= 0.001 {
                                lucide_icons::icon_volume_x()
                            } else if volume < 0.5 {
                                lucide_icons::icon_volume_1()
                            } else {
                                lucide_icons::icon_volume_2()
                            },
                            16.0,
                            hsla(0.0, 0.0, 1.0, 0.82),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.wake_stage_controls_immediately(cx);
                                this.pending_volume_ratio = None;
                                this.toggle_mute(cx);
                            }),
                        ),
                )
                .child(
                    interactive_slider(
                        "stage-volume-track",
                        volume,
                        SliderStyle::stage_volume(),
                        {
                            let view = cx.entity().downgrade();
                            move |ratio, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.wake_stage_controls_immediately(cx);
                                    if this.drag_target == Some(DragTarget::Volume) {
                                        this.update_drag_ratio(DragTarget::Volume, ratio, cx);
                                    } else {
                                        this.begin_drag(DragTarget::Volume, ratio, cx);
                                    }
                                    this.send(PlayerCommand::SetVolume(ratio));
                                });
                            }
                        },
                        {
                            let view = cx.entity().downgrade();
                            move |ratio, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.wake_stage_controls_immediately(cx);
                                    if this.drag_target == Some(DragTarget::Volume) {
                                        this.update_drag_ratio(DragTarget::Volume, ratio, cx);
                                    } else {
                                        this.begin_drag(DragTarget::Volume, ratio, cx);
                                    }
                                    this.commit_drag(cx);
                                });
                            }
                        },
                    )
                    .w(px(72.0))
                    .on_scroll_wheel(cx.listener(
                        |this, event: &gpui::ScrollWheelEvent, _window, cx| {
                            cx.stop_propagation();
                            let delta = event.delta.pixel_delta(px(48.0)).y;
                            if delta < px(0.0) {
                                this.adjust_volume(0.04, cx);
                            } else if delta > px(0.0) {
                                this.adjust_volume(-0.04, cx);
                            }
                            this.wake_stage_controls_immediately(cx);
                        },
                    )),
                ),
        )
}

fn control_button(
    id: &'static str,
    icon: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(36.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
        .active(|style| style.scale(0.92))
        .child(themed_icon(
            icon,
            20.0,
            hsla(0.0, 0.0, 1.0, 0.85),
        ))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn ambient_background(
    fluid_background: gpui::Entity<AppleFluidView>,
    playing: bool,
) -> gpui::AnyElement {
    let background = div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16));

    if playing {
        background.child(fluid_background).into_any_element()
    } else {
        background.into_any_element()
    }
}
