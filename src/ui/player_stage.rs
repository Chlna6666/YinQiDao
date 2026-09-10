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
use lucide_gpui::icon;

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
    // Sample the engine transport exactly once for this stage frame. Lyrics, the elapsed/remaining
    // clocks and the progress rail must derive from the same position; independently reading the
    // live atomics in different children makes fast seeks visibly disagree for a frame.
    let (transport_state, live_position_ms, duration_ms) = live_transport(app);
    let (_, displayed_position_ms, _, progress_ratio) = displayed_transport_from_live(
        app,
        transport_state,
        live_position_ms,
        duration_ms,
    );
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
                        .child(stage_lyrics(
                            app,
                            lyrics,
                            displayed_position_ms,
                            cx,
                        )),
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
    let override_ratio = app
        .drag_progress_ratio
        .or_else(|| app.pending_progress_ratio.map(|(_, ratio)| ratio));

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
                icon!(music),
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
    let reading_mode = app
        .lyrics_user_scrolling_until
        .is_some_and(|until| until > Instant::now());
    // Pausing freezes transport time; it must not implicitly enter manual-reading mode. Keep the
    // active-line focus hierarchy and enhanced-LRC word highlight frozen at the exact pause point.
    // Only explicit lyric scrolling temporarily flattens the focus hierarchy for reading.

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
        // GPUI's current offscreen element blur path produces dark tile/rectangle artifacts when
        // translucent glyph surfaces are composited over the animated fluid background. Keep the
        // Apple Music focus hierarchy through contrast and font weight until that renderer path is
        // fixed; never run the stage lyrics through an element blur filter.
        let alpha = if reading_mode {
            1.0
        } else {
            match distance {
                0 => 1.0,
                1 => 0.78,
                2 => 0.60,
                _ => 0.46,
            }
        };
        let timestamp = line.timestamp_ms;
        let weight = if index == active {
            gpui::FontWeight::BOLD
        } else if distance == 1 {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        };
        let karaoke_active = index == active && !reading_mode;
        let hover_group = format!("lyric-hover-{index}");
        let hover_group_for_time = hover_group.clone();

        let mut text = div()
            .w_full()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap_1()
            .font_weight(weight)
            .child(stage_primary_lyric(line, position_ms, karaoke_active));

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
            .group(hover_group)
            .id(SharedString::from(format!("lyric-line-{index}")))
            .relative()
            .w_full()
            .min_w(px(0.0))
            .flex_none()
            .pl(px(16.0))
            .pr(px(92.0))
            .py(px(11.0))
            .mb(px(10.0))
            .opacity(alpha)
            .transition(lyric_focus_transition())
            .cursor_pointer()
            .hover(|style| style.opacity(1.0))
            .child(text)
            .child(
                div()
                    .absolute()
                    .right(px(10.0))
                    .top(px(13.0))
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 0.0, 0.28))
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hsla(0.0, 0.0, 1.0, 0.82))
                    .opacity(0.0)
                    .group_hover(hover_group_for_time, |style| style.opacity(1.0))
                    .transition(lyric_focus_transition())
                    .child(format_lyric_time(timestamp)),
            )
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.seek_to_ms(timestamp, cx);
                    this.pending_progress_ratio = None;
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

fn stage_primary_lyric(
    line: &LyricLine,
    position_ms: u64,
    karaoke_active: bool,
) -> gpui::AnyElement {
    // Enhanced-LRC is only safe to render segment-by-segment when those segments reconstruct the
    // complete primary line. Some providers leave an untimed prefix/suffix around inline stamps;
    // rendering only `words` made that text disappear while the line was active.
    if !karaoke_active || !enhanced_words_cover_primary_text(line) {
        return div()
            .w_full()
            .min_w(px(0.0))
            .text_size(px(28.0))
            .text_color(hsla(0.0, 0.0, 1.0, 1.0))
            .child(line.text.clone())
            .into_any_element();
    }

    let current_word = line
        .words
        .iter()
        .rposition(|word| word.timestamp_ms <= position_ms);
    let mut row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_wrap()
        .items_baseline()
        .text_size(px(28.0));

    for (index, word) in line.words.iter().enumerate() {
        let alpha = match current_word {
            Some(current) if index < current => 0.88,
            Some(current) if index == current => 1.0,
            Some(_) => 0.42,
            None if index == 0 => 0.90,
            None => 0.42,
        };
        row = row.child(
            div()
                .flex_none()
                .font_weight(if current_word == Some(index) || (current_word.is_none() && index == 0)
                {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::SEMIBOLD
                })
                .text_color(hsla(0.0, 0.0, 1.0, alpha))
                .child(word.text.clone()),
        );
    }

    row.into_any_element()
}

fn enhanced_words_cover_primary_text(line: &LyricLine) -> bool {
    if line.words.is_empty() || line.text.is_empty() {
        return false;
    }
    let mut remaining = line.text.as_str();
    for word in line.words.iter() {
        let Some(rest) = remaining.strip_prefix(word.text.as_str()) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}

fn format_lyric_time(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let millis = ms % 1_000;
    let hours = total_secs / 3_600;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

fn lyric_focus_transition() -> Transition {
    Transition::new(Duration::from_millis(180))
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity])
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
                            this.seek_to_ratio(ratio, cx);
                            this.pending_progress_ratio = None;
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
                            this.commit_drag(cx);
                            this.pending_progress_ratio = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::LyricWord;

    #[test]
    fn precise_lyric_time_keeps_subsecond_timing() {
        assert_eq!(format_lyric_time(62_345), "01:02.345");
        assert_eq!(format_lyric_time(3_662_007), "01:01:02.007");
    }

    #[test]
    fn incomplete_enhanced_lrc_falls_back_to_full_line() {
        let complete = LyricLine {
            timestamp_ms: 1_000,
            text: "你好 世界".into(),
            translation: None,
            words: Arc::from([
                LyricWord {
                    timestamp_ms: 1_000,
                    text: "你好 ".into(),
                },
                LyricWord {
                    timestamp_ms: 1_500,
                    text: "世界".into(),
                },
            ]),
        };
        assert!(enhanced_words_cover_primary_text(&complete));

        let incomplete = LyricLine {
            timestamp_ms: 1_000,
            text: "前缀你好".into(),
            translation: None,
            words: Arc::from([LyricWord {
                timestamp_ms: 1_200,
                text: "你好".into(),
            }]),
        };
        assert!(!enhanced_words_cover_primary_text(&incomplete));
    }
}
