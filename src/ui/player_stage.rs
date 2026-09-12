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
    audio::PlayerCommand,
    gpu::AppleFluidView,
    model::{PlaybackState, Track},
};

use super::{
    components::{SliderStyle, interactive_slider},
    player_legacy, stage_lyrics,
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
    // Position/duration come from the hot atomic transport clock, but play/pause presentation must
    // follow MusicApp's optimistic UI snapshot. `toggle_play()` updates that snapshot immediately;
    // reading the engine state again here can briefly resurrect the pre-fade state and render the
    // opposite action icon after the user has already paused/resumed.
    let (_, live_position_ms, duration_ms) = live_transport(app);
    let transport_state = snapshot.state;
    let (_, displayed_position_ms, _, progress_ratio) = displayed_transport_from_live(
        app,
        transport_state,
        live_position_ms,
        duration_ms,
    );
    let fluid_playing = transport_state == PlaybackState::Playing;
    fluid_background.update(cx, |view, cx| view.set_playing(fluid_playing, cx));
    let lyrics_view = stage_lyrics::view(app, cx);

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
                .child(stage_controls(
                    app,
                    cx,
                    transport_state,
                    displayed_position_ms,
                    duration_ms,
                    progress_ratio,
                )),
        )
        .into_any_element()
}

fn live_transport(app: &MusicApp) -> (PlaybackState, u64, u64) {
    app.engine.as_ref().map_or(
        (
            app.snapshot.state,
            app.snapshot.position_ms,
            app.snapshot.duration_ms,
        ),
        |engine| engine.progress(),
    )
}

fn displayed_transport_from_live(
    app: &MusicApp,
    state: PlaybackState,
    live_position_ms: u64,
    duration_ms: u64,
) -> (PlaybackState, u64, u64, f32) {
    let override_ratio = app.drag_progress_ratio;

    let position_ms = override_ratio.map_or(live_position_ms, |ratio| {
        (duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
    });
    let ratio = override_ratio.unwrap_or_else(|| {
        if duration_ms == 0 {
            0.0
        } else {
            (live_position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
        }
    });

    (state, position_ms, duration_ms, ratio)
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

fn stage_controls(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    transport_state: PlaybackState,
    position: u64,
    duration_ms: u64,
    progress_ratio: f32,
) -> impl IntoElement {
    let volume = app.displayed_volume_ratio();
    let playing = transport_state == PlaybackState::Playing;
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
        .on_hover(cx.listener(|this, hovered: &bool, _, _cx| {
            this.stage_controls_hovered = false;
            if *hovered
                && this.stage_suppress_wake_until.is_none()
                && this.stage_controls_visibility >= 0.995
            {
                this.stage_last_user_activity = Instant::now();
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
                progress_ratio,
                SliderStyle::stage_progress(),
                {
                    let view = cx.entity().downgrade();
                    move |ratio, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.wake_stage_controls_immediately(cx);
                            // `seek_to_ratio` records a track-bound optimistic seek. Keep it until
                            // the audio engine confirms the new position instead of immediately
                            // reverting the UI to the pre-seek transport clock.
                            this.seek_to_ratio(ratio, cx);
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
                            // `commit_drag` stores the same track-bound pending ratio before sending
                            // the seek. Polling clears it only after the engine reaches the target.
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
                .child(format_remaining_time(position, duration_ms)),
        )
        .child(control_button(
            "stage-prev-btn",
            icon!(skip_back),
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
                        icon!(pause)
                    } else {
                        icon!(play)
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
            icon!(skip_forward),
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
                                icon!(volume_x)
                            } else if volume < 0.5 {
                                icon!(volume_1)
                            } else {
                                icon!(volume_2)
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
                                    this.pending_volume_ratio = None;
                                    this.set_app_volume(ratio, cx);
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
                                    this.pending_volume_ratio = None;
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

fn ambient_background(fluid_background: gpui::Entity<AppleFluidView>) -> gpui::AnyElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .child(fluid_background)
        .into_any_element()
}
