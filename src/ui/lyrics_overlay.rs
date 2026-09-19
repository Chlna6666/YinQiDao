use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use gpui::{
    AnimationExt as _, AnimationProperty, Context, GpuMesh3d, GpuMesh3dDrawParameters,
    GpuMesh3dDrawRanges, GpuMesh3dRange, GpuMesh3dShader, GpuMesh3dVertex, HorizontalRevealEdge,
    IntoElement, Subscription, Task, Timer, TransformOrigin, WeakEntity, WgslShaderSource, Window,
    WindowControlArea, canvas, div, hsla, point, prelude::*, px, rgb,
};

use crate::{
    desktop_lyrics::LyricsDisplay,
    lyrics::LyricWord,
    model::{PlaybackState, TrackId},
    settings::DesktopLyricsAlignment,
};

use super::shell::MusicApp;

const INTERACTION_BACKGROUND_OPACITY: f32 = 0.36;
const LIQUID_GLASS_CORNER_RADIUS: f32 = 18.0;
const LYRIC_LINE_TRANSITION_DURATION: Duration = Duration::from_millis(360);
const LIQUID_GLASS_SHADER_SOURCE: &str = include_str!("lyrics_liquid_glass.wgsl");

pub(crate) struct DesktopLyricsView {
    parent: WeakEntity<MusicApp>,
    _parent_subscription: Option<Subscription>,
    bounds_subscription: Option<Subscription>,
    clock_task: Option<Task<()>>,
    clock_armed: bool,
    hovered: bool,
    settings_open: bool,
    display_key: Option<(TrackId, usize)>,
    last_display: Option<LyricsDisplay>,
    previous_display: Option<LyricsDisplay>,
    line_transition_started_at: Option<Instant>,
    line_transition_deadline: Option<Instant>,
}

impl DesktopLyricsView {
    pub(crate) fn new(parent: WeakEntity<MusicApp>, cx: &mut Context<Self>) -> Self {
        let parent_subscription = parent.upgrade().map(|parent_entity| {
            cx.observe(&parent_entity, |this, _parent, cx| {
                // GPUI Task cancellation is drop-driven. Any structural player/config/seek update
                // cancels the old lyric deadline so the next render can arm the exact new boundary.
                this.clock_task = None;
                this.clock_armed = false;
                cx.notify();
            })
        });
        Self {
            parent,
            _parent_subscription: parent_subscription,
            bounds_subscription: None,
            clock_task: None,
            clock_armed: false,
            hovered: false,
            settings_open: false,
            display_key: None,
            last_display: None,
            previous_display: None,
            line_transition_started_at: None,
            line_transition_deadline: None,
        }
    }

    fn attach_bounds_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bounds_subscription.is_some() {
            return;
        }
        let parent = self.parent.clone();
        self.bounds_subscription =
            Some(cx.observe_window_bounds(window, move |_view, window, cx| {
                let bounds = window.bounds();
                let _ = parent.update(cx, |app, _cx| {
                    app.persist_desktop_lyrics_bounds(bounds);
                });
            }));
    }

    fn ensure_transport_clock(&mut self, cx: &mut Context<Self>) {
        if self.clock_armed {
            return;
        }
        let Some(parent) = self.parent.upgrade() else {
            self.clock_task = None;
            return;
        };
        let Some(delay) = parent.read(cx).desktop_lyrics_next_boundary_delay() else {
            self.clock_task = None;
            return;
        };

        self.clock_armed = true;
        self.clock_task = Some(cx.spawn(async move |this, cx| {
            Timer::after(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.clock_armed = false;
                cx.notify();
            });
        }));
    }
}

impl gpui::Render for DesktopLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach_bounds_observer(window, cx);
        self.ensure_transport_clock(cx);

        let Some(parent) = self.parent.upgrade() else {
            return div().size_full().into_any_element();
        };
        let (config, display, karaoke_running) = {
            let app = parent.read(cx);
            (
                app.config.desktop_lyrics.clone(),
                app.desktop_lyrics_display(),
                app.snapshot.state == PlaybackState::Playing,
            )
        };

        let now = window.animation_time();
        if self
            .line_transition_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.previous_display = None;
            self.line_transition_started_at = None;
            self.line_transition_deadline = None;
        }

        let next_key = display.as_ref().map(|display| (display.track_id, display.line_index));
        if next_key != self.display_key {
            if self.display_key.is_some() && next_key.is_some() {
                self.previous_display = self.last_display.clone();
                self.line_transition_started_at = Some(now);
                self.line_transition_deadline = Some(now + LYRIC_LINE_TRANSITION_DURATION);
            } else {
                self.previous_display = None;
                self.line_transition_started_at = None;
                self.line_transition_deadline = None;
            }
            self.display_key = next_key;
        }
        self.last_display = display.clone();

        let line_transition_progress = self
            .line_transition_started_at
            .map(|started_at| lyric_line_transition_progress(started_at, now))
            .filter(|progress| *progress < 1.0);

        // WS_EX_NOACTIVATE keeps the widget out of foreground focus, but it also means GPUI treats
        // it as inactive even when HWND_TOPMOST is visible. During playback we deliberately keep
        // this tiny window dirty and let GPUI's platform/VSync request own cadence. refresh() is
        // paired with the presentation request so inactive-frame deferral cannot turn karaoke into
        // one repaint per authored word/line boundary.
        if karaoke_running || line_transition_progress.is_some() {
            request_realtime_lyrics_frame(window);
        }

        let interacting = self.hovered || self.settings_open;
        let background_opacity = if interacting {
            config
                .background_opacity
                .max(INTERACTION_BACKGROUND_OPACITY)
        } else {
            config.background_opacity
        }
        .clamp(0.0, 0.85);

        let lyrics = if let Some(display) = &display {
            desktop_lyrics_stack(display, &config)
        } else {
            div()
                .w_full()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(config.font_size))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(config.active_color & 0x00ff_ffff))
                .child("暂无同步歌词")
                .into_any_element()
        };

        let lyrics = if let Some(previous) = self.previous_display.as_ref()
            && let Some(progress) = line_transition_progress
        {
            let incoming = div()
                .w_full()
                .min_w(px(0.0))
                .child(lyrics)
                .with_sampled_animation(
                    AnimationProperty::translation(
                        point(px(0.0), px(9.0)),
                        point(px(0.0), px(0.0)),
                    ),
                    progress,
                )
                .with_sampled_animation(
                    AnimationProperty::scale_opacity(
                        0.985,
                        1.0,
                        0.0,
                        1.0,
                        TransformOrigin::new(0.5, 0.5),
                    ),
                    progress,
                )
                .into_any_element();

            let mut previous_complete = previous.clone();
            previous_complete.position_ms = u64::MAX;
            let outgoing = div()
                .absolute()
                .inset_0()
                .flex()
                .child(desktop_lyrics_stack(&previous_complete, &config))
                .with_sampled_animation(
                    AnimationProperty::translation(
                        point(px(0.0), px(0.0)),
                        point(px(0.0), px(-7.0)),
                    ),
                    progress,
                )
                .with_sampled_animation(
                    AnimationProperty::scale_opacity(
                        1.0,
                        0.985,
                        1.0,
                        0.0,
                        TransformOrigin::new(0.5, 0.5),
                    ),
                    progress,
                )
                .into_any_element();

            div()
                .relative()
                .flex_1()
                .min_w(px(0.0))
                .child(incoming)
                .child(outgoing)
                .into_any_element()
        } else {
            lyrics
        };

        let mut root = div()
            .id("desktop-lyrics-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .window_control_area(WindowControlArea::Client)
            .when(!config.locked && cfg!(windows), |root| {
                // Use GPUI's native Windows drag hit-test path for the widget surface. The backend
                // arms a drag gesture and only calls start_window_move after the pointer crosses the
                // platform drag threshold; frontmost Client controls automatically override it.
                root.window_control_area(WindowControlArea::Drag)
            })
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if this.hovered != *hovered {
                    this.hovered = *hovered;
                    cx.notify();
                }
            }));

        if !config.locked && !cfg!(windows) {
            // Linux/macOS currently do not consume WindowControlArea::Drag in the same native path,
            // so keep explicit dragging there. Buttons/panels stop propagation themselves.
            root = root.on_mouse_down(
                gpui::MouseButton::Left,
                move |_: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.start_window_move();
                },
            );
        }

        if background_opacity > 0.001 {
            root = root.child(liquid_glass_surface(background_opacity, interacting));
        }

        root = root.child(
            div()
                .absolute()
                .inset_0()
                .px(px(24.0))
                .py(px(12.0))
                .flex()
                .child(lyrics),
        );

        if interacting {
            let lock_parent = self.parent.clone();
            let translation_parent = self.parent.clone();
            let close_parent = self.parent.clone();
            let settings_view = cx.weak_entity();
            let toolbar = div()
                .absolute()
                .top(px(7.0))
                .right(px(8.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .occlude()
                .window_control_area(WindowControlArea::Client)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    |_: &gpui::MouseDownEvent, _, cx| cx.stop_propagation(),
                )
                .child(toolbar_button(
                    "desktop-lyrics-lock",
                    if config.locked { "解" } else { "锁" },
                    move |_, _window, cx| {
                        let _ = lock_parent.update(cx, |app, app_cx| {
                            app.toggle_desktop_lyrics_lock(app_cx);
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-translation",
                    "译",
                    move |_, _window, cx| {
                        let _ = translation_parent.update(cx, |app, app_cx| {
                            app.toggle_desktop_lyrics_translation(app_cx);
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-settings",
                    "⋯",
                    move |_, _window, cx| {
                        let _ = settings_view.update(cx, |view, view_cx| {
                            view.settings_open = !view.settings_open;
                            view_cx.notify();
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-close",
                    "×",
                    move |_, _window, cx| {
                        // The owner serializes hide -> native close -> optional reopen. Do not call
                        // remove_window a second time here or clear the tracked handle prematurely.
                        let _ = close_parent.update(cx, |app, app_cx| {
                            app.desktop_lyrics_window_closed(app_cx);
                        });
                    },
                ));
            root = root.child(toolbar);
        }

        if self.settings_open {
            root = root.child(settings_panel(&self.parent, &config));
        }

        root.into_any_element()
    }
}

fn desktop_lyrics_stack(
    display: &LyricsDisplay,
    config: &crate::settings::DesktopLyricsConfig,
) -> gpui::AnyElement {
    let mut lyrics = div()
        .w_full()
        .min_w(px(0.0))
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(3.0));

    lyrics = match config.alignment {
        DesktopLyricsAlignment::Left => lyrics.items_start(),
        DesktopLyricsAlignment::Center => lyrics.items_center(),
        DesktopLyricsAlignment::Right => lyrics.items_end(),
    };

    lyrics = lyrics.child(animated_current_line(
        display,
        config.alignment,
        config.font_size,
        config.active_color,
    ));

    if config.show_translation
        && let Some(translation) = display
            .translation
            .as_ref()
            .filter(|text| !text.trim().is_empty())
    {
        lyrics = lyrics.child(aligned_line(
            translation.clone(),
            config.alignment,
            (config.font_size * 0.52).max(13.0),
            config.translation_color,
            gpui::FontWeight::MEDIUM,
            0.90,
        ));
    }
    if config.two_line
        && let Some(next) = display.next.as_ref().filter(|text| !text.trim().is_empty())
    {
        lyrics = lyrics.child(aligned_line(
            next.clone(),
            config.alignment,
            (config.font_size * 0.66).max(15.0),
            config.inactive_color,
            gpui::FontWeight::MEDIUM,
            0.70,
        ));
    }
    if config.two_line
        && config.show_translation
        && let Some(next_translation) = display
            .next_translation
            .as_ref()
            .filter(|text| !text.trim().is_empty())
    {
        lyrics = lyrics.child(aligned_line(
            next_translation.clone(),
            config.alignment,
            (config.font_size * 0.44).max(12.0),
            config.translation_color,
            gpui::FontWeight::NORMAL,
            0.58,
        ));
    }

    lyrics.into_any_element()
}

fn animated_current_line(
    display: &LyricsDisplay,
    alignment: DesktopLyricsAlignment,
    font_size: f32,
    color: u32,
) -> gpui::AnyElement {
    if display.current_words.is_empty()
        || !words_cover_primary_text(&display.current, &display.current_words)
    {
        return aligned_line(
            display.current.clone(),
            alignment,
            font_size,
            color,
            gpui::FontWeight::SEMIBOLD,
            1.0,
        );
    }

    let current_word = display
        .current_words
        .partition_point(|word| word.timestamp_ms <= display.position_ms)
        .checked_sub(1);
    let mut content = div()
        .max_w(px(1_420.0))
        .min_w(px(0.0))
        .flex()
        .items_center()
        .overflow_hidden()
        .text_size(px(font_size))
        .font_weight(gpui::FontWeight::SEMIBOLD);

    for (index, word) in display.current_words.iter().enumerate() {
        content = content.child(desktop_karaoke_word(
            &display.current_words,
            word,
            index,
            current_word,
            display.position_ms,
            color,
        ));
    }

    let row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .child(content);
    match alignment {
        DesktopLyricsAlignment::Left => row.justify_start(),
        DesktopLyricsAlignment::Center => row.justify_center(),
        DesktopLyricsAlignment::Right => row.justify_end(),
    }
    .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn desktop_karaoke_word(
    words: &[LyricWord],
    word: &LyricWord,
    index: usize,
    current_word: Option<usize>,
    position_ms: u64,
    color: u32,
) -> gpui::AnyElement {
    const DIM_ALPHA: f32 = 0.34;
    let color = color & 0x00ff_ffff;

    let Some(current_word) = current_word else {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .opacity(DIM_ALPHA)
            .text_color(rgb(color))
            .child(word.text.clone())
            .into_any_element();
    };
    if index < current_word {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(rgb(color))
            .child(word.text.clone())
            .into_any_element();
    }
    if index > current_word {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .opacity(DIM_ALPHA)
            .text_color(rgb(color))
            .child(word.text.clone())
            .into_any_element();
    }

    let duration_ms = authored_or_inferred_word_duration(words, index);
    let progress = word_reveal_progress(word, duration_ms, position_ms);
    let base = div()
        .flex_none()
        .whitespace_nowrap()
        .opacity(DIM_ALPHA)
        .text_color(rgb(color))
        .child(word.text.clone());
    let overlay = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w_full()
        .h_full()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(rgb(color))
        .child(word.text.clone());

    let overlay = if progress < 1.0 {
        overlay
            .with_sampled_animation(
                AnimationProperty::horizontal_reveal(HorizontalRevealEdge::Left, 0.0, 1.0),
                progress,
            )
            .into_any_element()
    } else {
        overlay.into_any_element()
    };

    div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .child(base)
        .child(overlay)
        .into_any_element()
}

fn authored_or_inferred_word_duration(words: &[LyricWord], index: usize) -> Option<u64> {
    let word = words.get(index)?;
    word.duration_ms
        .filter(|duration| *duration > 0)
        .or_else(|| {
            words.get(index + 1).and_then(|next| {
                let duration = next.timestamp_ms.saturating_sub(word.timestamp_ms);
                (duration > 0).then_some(duration)
            })
        })
}

fn word_reveal_progress(word: &LyricWord, duration_ms: Option<u64>, position_ms: u64) -> f32 {
    let Some(duration_ms) = duration_ms.filter(|duration| *duration > 0) else {
        return if position_ms >= word.timestamp_ms { 1.0 } else { 0.0 };
    };
    let elapsed = position_ms
        .saturating_sub(word.timestamp_ms)
        .min(duration_ms);
    (elapsed as f32 / duration_ms as f32).clamp(0.0, 1.0)
}

fn words_cover_primary_text(text: &str, words: &[LyricWord]) -> bool {
    if words.is_empty() || text.is_empty() {
        return false;
    }
    let mut remaining = text;
    for word in words {
        let Some(rest) = remaining.strip_prefix(word.text.as_str()) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}

fn lyric_line_transition_progress(started_at: Instant, now: Instant) -> f32 {
    let duration = LYRIC_LINE_TRANSITION_DURATION.as_secs_f32().max(f32::EPSILON);
    let raw = (now.saturating_duration_since(started_at).as_secs_f32() / duration)
        .clamp(0.0, 1.0);
    // Symmetric smootherstep makes the line hand-off visibly continuous instead of consuming most
    // opacity/translation in the first few frames.
    raw * raw * raw * (raw * (raw * 6.0 - 15.0) + 10.0)
}

fn request_realtime_lyrics_frame(window: &mut Window) {
    if window.is_minimized() {
        return;
    }
    // Request cadence first so refresh sees a pending frame callback and is not classified as
    // ordinary inactive/background dirtiness. The platform VSync scheduler still owns timing.
    window.request_animation_frame();
    window.refresh();
}

fn aligned_line(
    text: String,
    alignment: DesktopLyricsAlignment,
    font_size: f32,
    color: u32,
    weight: gpui::FontWeight,
    opacity: f32,
) -> gpui::AnyElement {
    let content = div()
        .max_w(px(1_420.0))
        .min_w(px(0.0))
        .text_size(px(font_size))
        .font_weight(weight)
        .text_color(rgb(color & 0x00ff_ffff))
        .truncate()
        .child(text);

    let row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .opacity(opacity)
        .child(content);
    match alignment {
        DesktopLyricsAlignment::Left => row.justify_start(),
        DesktopLyricsAlignment::Center => row.justify_center(),
        DesktopLyricsAlignment::Right => row.justify_end(),
    }
    .into_any_element()
}

fn toolbar_button(
    id: &'static str,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .min_w(px(28.0))
        .h(px(24.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(hsla(0.0, 0.0, 0.10, 0.80))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0xff_ff_ff))
        .hover(|style| style.bg(hsla(0.0, 0.0, 0.04, 0.92)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
}

fn settings_panel(
    parent: &WeakEntity<MusicApp>,
    config: &crate::settings::DesktopLyricsConfig,
) -> impl IntoElement {
    let topmost_parent = parent.clone();
    let two_line_parent = parent.clone();
    let translation_parent = parent.clone();
    let alignment_parent = parent.clone();
    let background_parent = parent.clone();
    let alignment_label = match config.alignment {
        DesktopLyricsAlignment::Left => "左",
        DesktopLyricsAlignment::Center => "中",
        DesktopLyricsAlignment::Right => "右",
    };

    div()
        .absolute()
        .top(px(35.0))
        .right(px(8.0))
        .w(px(322.0))
        .p(px(6.0))
        .rounded(px(12.0))
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .bg(hsla(0.0, 0.0, 0.16, 0.92))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.16))
        .shadow_md()
        .flex()
        .flex_wrap()
        .gap(px(4.0))
        .on_mouse_down(
            gpui::MouseButton::Left,
            |_: &gpui::MouseDownEvent, _, cx| cx.stop_propagation(),
        )
        .child(settings_row(
            "desktop-lyrics-menu-topmost",
            "总在最前",
            if config.always_on_top { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = topmost_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_topmost(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-two-line",
            "切换双行模式",
            if config.two_line { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = two_line_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_two_line(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-translation",
            "外文歌词显示",
            if config.show_translation { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = translation_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_translation(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-alignment",
            "对齐方式",
            alignment_label,
            move |_, _window, cx| {
                let _ = alignment_parent.update(cx, |app, app_cx| {
                    let next = match app.config.desktop_lyrics.alignment {
                        DesktopLyricsAlignment::Left => DesktopLyricsAlignment::Center,
                        DesktopLyricsAlignment::Center => DesktopLyricsAlignment::Right,
                        DesktopLyricsAlignment::Right => DesktopLyricsAlignment::Left,
                    };
                    app.set_desktop_lyrics_alignment(next, app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-background",
            "显示透明背景",
            if config.background_opacity > 0.01 {
                "✓"
            } else {
                ""
            },
            move |_, _window, cx| {
                let _ = background_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_background(app_cx);
                });
            },
        ))
}

fn settings_row(
    id: &'static str,
    label: &'static str,
    value: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(98.0))
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_between()
        .cursor_pointer()
        .window_control_area(WindowControlArea::Client)
        .text_xs()
        .text_color(rgb(0xf2_f2_f7))
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.10)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
        .child(
            div()
                .min_w(px(18.0))
                .text_right()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(0xff_ff_ff))
                .child(value),
        )
}

fn liquid_glass_surface(opacity: f32, interacting: bool) -> gpui::AnyElement {
    match lyrics_liquid_glass_mesh() {
        Ok(mesh) => {
            let mut metadata = [[0.0_f32; 4]; 4];
            metadata[0][0] = opacity;
            metadata[0][1] = if interacting { 1.0 } else { 0.0 };
            metadata[0][2] = LIQUID_GLASS_CORNER_RADIUS;
            metadata[3][3] = 1.0;
            let parameters = GpuMesh3dDrawParameters {
                view_projection_model: metadata,
            };
            canvas(
                move |bounds, _window, _cx| bounds,
                move |bounds, _prepaint, window, _cx| {
                    window.paint_gpu_mesh_3d(bounds, mesh.clone(), parameters);
                },
            )
            .absolute()
            .inset_0()
            .into_any_element()
        }
        Err(_error) => div()
            .absolute()
            .inset_0()
            .rounded(px(LIQUID_GLASS_CORNER_RADIUS))
            .bg(hsla(0.0, 0.0, 0.18, opacity))
            .into_any_element(),
    }
}

fn lyrics_liquid_glass_mesh() -> Result<Arc<GpuMesh3d>, String> {
    static MESH: OnceLock<Result<Arc<GpuMesh3d>, String>> = OnceLock::new();
    MESH.get_or_init(|| {
        let result = build_lyrics_liquid_glass_mesh();
        if let Err(error) = &result {
            tracing::warn!(error = %error, "桌面歌词 Liquid Glass shader 不可用，回退透明灰背景");
        }
        result
    })
    .clone()
}

fn build_lyrics_liquid_glass_mesh() -> Result<Arc<GpuMesh3d>, String> {
    let source = WgslShaderSource::from_source(
        "src/ui/lyrics_liquid_glass.wgsl",
        LIQUID_GLASS_SHADER_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let shader = Arc::new(GpuMesh3dShader::new(
        Arc::new(source),
        "vs_lyrics_liquid_glass",
        "fs_lyrics_liquid_glass",
    ));
    let vertices = vec![
        GpuMesh3dVertex {
            position: [-1.0, -1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [1.0, -1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [1.0, 1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [-1.0, 1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ];
    let indices = vec![0_u32, 1, 2, 0, 2, 3];
    let mesh = GpuMesh3d::new(
        Arc::from(vertices.into_boxed_slice()),
        Arc::from(indices.into_boxed_slice()),
        GpuMesh3dDrawRanges {
            opaque: GpuMesh3dRange::default(),
            glass: GpuMesh3dRange { start: 0, count: 6 },
            water: GpuMesh3dRange::default(),
        },
        [0.0, 0.0, 0.0],
        1.0,
        1.0,
        shader,
    );
    Ok(Arc::new(mesh))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(timestamp_ms: u64, duration_ms: Option<u64>, text: &str) -> LyricWord {
        LyricWord {
            timestamp_ms,
            duration_ms,
            text: text.to_owned(),
        }
    }

    #[test]
    fn desktop_karaoke_requires_exact_authored_text_coverage() {
        let words = [
            word(1_000, Some(300), "Hello "),
            word(1_300, Some(400), "world"),
        ];
        assert!(words_cover_primary_text("Hello world", &words));
        assert!(!words_cover_primary_text("Hello, world", &words));
        assert!(!words_cover_primary_text("Hello world!", &words));
    }

    #[test]
    fn desktop_karaoke_infers_enhanced_lrc_duration_from_next_word() {
        let words = [
            word(1_000, None, "A"),
            word(1_450, None, "B"),
        ];
        assert_eq!(authored_or_inferred_word_duration(&words, 0), Some(450));
        assert_eq!(authored_or_inferred_word_duration(&words, 1), None);
    }

    #[test]
    fn desktop_karaoke_progress_is_continuous_for_authored_duration() {
        let word = word(2_000, Some(400), "AB");
        assert_eq!(word_reveal_progress(&word, Some(400), 1_999), 0.0);
        assert_eq!(word_reveal_progress(&word, Some(400), 2_000), 0.0);
        assert!((word_reveal_progress(&word, Some(400), 2_200) - 0.5).abs() < 0.001);
        assert_eq!(word_reveal_progress(&word, Some(400), 2_400), 1.0);
    }
}
