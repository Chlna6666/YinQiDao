use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::Arc;

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, Context, ElementId, Entity,
    HorizontalRevealEdge, IntoElement, SharedString, Subscription, WeakEntity, Window,
    WindowControlArea, div, hsla, point, prelude::*, px, rgb,
};

use crate::gpu::{ShaderKind, ShaderParams16, next_shader_effect_id, shader_effect_canvas};
use crate::ui::theme::{RADIUS_LG, RADIUS_MD, dark};
use crate::{
    desktop_lyrics::LyricsDisplay, lyrics::LyricWord, model::TrackId,
    settings::DesktopLyricsAlignment,
};

use super::{
    app_ui_events::{AppUiEvent, AppUiEventBridge},
    shell::MusicApp,
};

const INTERACTION_BACKGROUND_OPACITY: f32 = 0.36;
const LIQUID_GLASS_CORNER_RADIUS: f32 = 18.0;
const LYRIC_LINE_TRANSITION_DURATION: Duration = Duration::from_millis(260);

pub(crate) struct DesktopLyricsView {
    parent: WeakEntity<MusicApp>,
    _parent_subscription: Option<Subscription>,
    _ui_subscription: Subscription,
    bounds_subscription: Option<Subscription>,
    hovered: bool,
    settings_open: bool,
    display_key: Option<(TrackId, usize)>,
    last_display: Option<LyricsDisplay>,
    previous_display: Option<LyricsDisplay>,
    line_transition_epoch: u64,
    line_transition_deadline: Option<Instant>,
    effect_id: u64,
}

impl DesktopLyricsView {
    pub(crate) fn new(
        parent: WeakEntity<MusicApp>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let parent_subscription = parent.upgrade().map(|parent_entity| {
            cx.observe(&parent_entity, |_this, _parent, cx| {
                // Structural transport/config changes re-render this View immediately. The next
                // render computes a new GPUI deadline; stale deadlines are coalesced per entity.
                cx.notify();
            })
        });
        let ui_subscription = cx.subscribe(&ui_events, |_this, _bridge, event, cx| {
            if matches!(
                *event,
                AppUiEvent::PlaybackStateChanged(_) | AppUiEvent::ProgressChanged { .. }
            ) {
                cx.notify();
            }
        });
        Self {
            parent,
            _parent_subscription: parent_subscription,
            _ui_subscription: ui_subscription,
            bounds_subscription: None,
            hovered: false,
            settings_open: false,
            display_key: None,
            last_display: None,
            previous_display: None,
            line_transition_epoch: 0,
            line_transition_deadline: None,
            effect_id: next_shader_effect_id(),
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

    fn schedule_transport_deadline(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.parent.upgrade() else {
            return;
        };
        let Some(delay) = parent.read(cx).desktop_lyrics_next_boundary_delay() else {
            return;
        };

        window.request_invalidation_at(Instant::now() + delay, cx);
    }
}

impl gpui::Render for DesktopLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // This popup intentionally never activates on Windows. Its retained
        // lyric transitions still need the visible inactive-window cadence.
        super::enable_inactive_window_animations(window);
        self.attach_bounds_observer(window, cx);
        self.schedule_transport_deadline(window, cx);

        let Some(parent) = self.parent.upgrade() else {
            return div().size_full().into_any_element();
        };
        let (config, display) = {
            let app = parent.read(cx);
            (
                app.config.desktop_lyrics.clone(),
                app.desktop_lyrics_display(),
            )
        };

        let now = window.animation_time();
        if self
            .line_transition_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.previous_display = None;
            self.line_transition_deadline = None;
        }

        let next_key = desktop_lyrics_display_key(display.as_ref());
        if next_key != self.display_key {
            if self.display_key.is_some() && next_key.is_some() {
                self.previous_display = self.last_display.clone();
                self.line_transition_epoch = self.line_transition_epoch.wrapping_add(1);
                self.line_transition_deadline = Some(now + LYRIC_LINE_TRANSITION_DURATION);
            } else {
                self.previous_display = None;
                self.line_transition_deadline = None;
            }
            self.display_key = next_key;
        }
        self.last_display = display.clone();

        let line_transition_active = self.previous_display.is_some()
            && self
                .line_transition_deadline
                .is_some_and(|deadline| deadline > now);
        if line_transition_active && let Some(deadline) = self.line_transition_deadline {
            // One semantic cleanup wake only; retained animation owns all intermediate frames.
            window.request_invalidation_at(deadline, cx);
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

        // Both animated and steady states share exactly the same full-size
        // layout box. An intrinsic-height incoming wrapper previously
        // re-centered the lyrics as soon as the 260ms animation finished,
        // producing a visible vertical snap (most noticeable on short lines).
        let mut lyric_viewport = div()
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_hidden();
        let incoming = div().absolute().inset_0().flex().child(lyrics);

        if let Some(previous) = self.previous_display.as_ref()
            && line_transition_active
        {
            let travel = (config.font_size * 0.30).clamp(7.0, 17.0);
            let epoch = self.line_transition_epoch;

            // One retained translation+opacity animation, rather than
            // nested translate + scale timelines that fight the active
            // per-word karaoke highlight and change the text's raster bounds.
            let incoming = incoming
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("desktop-lyrics-line-in"),
                        epoch,
                    ),
                    Animation::new(LYRIC_LINE_TRANSITION_DURATION)
                        .with_easing(desktop_smootherstep)
                        .with_property(AnimationProperty::translation_opacity(
                            point(px(0.0), px(travel)),
                            point(px(0.0), px(0.0)),
                            0.74,
                            1.0,
                        )),
                    |element, _| element,
                )
                .into_any_element();

            let mut previous_complete = previous.clone();
            previous_complete.position_ms = u64::MAX;
            let outgoing = div()
                .absolute()
                .inset_0()
                .flex()
                // Preserve the same current/next/translation geometry while
                // fading the previous line. A current-only outgoing layer
                // was vertically centered differently from a two-line stack.
                .child(desktop_lyrics_stack(&previous_complete, &config))
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("desktop-lyrics-line-out"),
                        epoch,
                    ),
                    Animation::new(LYRIC_LINE_TRANSITION_DURATION)
                        .with_easing(desktop_smootherstep)
                        .with_property(AnimationProperty::translation_opacity(
                            point(px(0.0), px(0.0)),
                            point(px(0.0), px(-travel)),
                            1.0,
                            0.0,
                        )),
                    |element, _| element,
                )
                .into_any_element();

            lyric_viewport = lyric_viewport.child(incoming).child(outgoing);
        } else {
            lyric_viewport = lyric_viewport.child(incoming);
        }
        let lyrics = lyric_viewport.into_any_element();

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
            root = root.child(liquid_glass_surface(
                self.effect_id,
                background_opacity,
                interacting,
            ));
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

/// Word timing changes must not restart the whole line transition.
#[inline]
fn desktop_lyrics_display_key(display: Option<&LyricsDisplay>) -> Option<(TrackId, usize)> {
    display.map(|display| (display.track_id, display.line_index))
}

fn desktop_lyrics_stack(
    display: &LyricsDisplay,
    config: &crate::settings::DesktopLyricsConfig,
) -> gpui::AnyElement {
    let mut lyrics = desktop_stack_shell(config).child(desktop_current_group(display, config));

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

fn desktop_stack_shell(config: &crate::settings::DesktopLyricsConfig) -> gpui::Div {
    let lyrics = div()
        .w_full()
        .min_w(px(0.0))
        .flex_1()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(3.0));
    match config.alignment {
        DesktopLyricsAlignment::Left => lyrics.items_start(),
        DesktopLyricsAlignment::Center => lyrics.items_center(),
        DesktopLyricsAlignment::Right => lyrics.items_end(),
    }
}

fn desktop_current_group(
    display: &LyricsDisplay,
    config: &crate::settings::DesktopLyricsConfig,
) -> gpui::AnyElement {
    let mut group = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .child(animated_current_line(
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
        group = group.child(aligned_line(
            translation.clone(),
            config.alignment,
            (config.font_size * 0.52).max(13.0),
            config.translation_color,
            gpui::FontWeight::MEDIUM,
            0.90,
        ));
    }

    group.into_any_element()
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
            desktop_word_animation_id(
                display.track_id,
                display.line_index,
                index,
                word.timestamp_ms,
            ),
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
    animation_id: u64,
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
        let end_ms = duration_ms
            .map(|duration| word.timestamp_ms.saturating_add(duration))
            .unwrap_or(position_ms);
        let remaining = Duration::from_millis(end_ms.saturating_sub(position_ms).max(1));
        overlay
            .with_animation(
                ElementId::NamedInteger(
                    SharedString::new_static("desktop-lyrics-word-reveal"),
                    animation_id,
                ),
                Animation::new(remaining).with_property(AnimationProperty::horizontal_reveal(
                    HorizontalRevealEdge::Left,
                    progress,
                    1.0,
                )),
                |element, _| element,
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
        return if position_ms >= word.timestamp_ms {
            1.0
        } else {
            0.0
        };
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

#[inline]
fn desktop_word_animation_id(
    track_id: TrackId,
    line_index: usize,
    word_index: usize,
    timestamp_ms: u64,
) -> u64 {
    (track_id as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add((line_index as u64).rotate_left(17))
        .wrapping_add((word_index as u64).rotate_left(33))
        .wrapping_add(timestamp_ms.rotate_left(7))
}

#[inline]
fn desktop_smootherstep(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
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
        .text_color(dark::white(1.0))
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
        .rounded(px(RADIUS_LG))
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .bg(hsla(0.0, 0.0, 0.16, 0.92))
        .border_1()
        .border_color(dark::hover_fill_strong())
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
        .rounded(px(RADIUS_MD))
        .flex()
        .items_center()
        .justify_between()
        .cursor_pointer()
        .window_control_area(WindowControlArea::Client)
        .text_xs()
        .text_color(dark::text_primary())
        .hover(|style| style.bg(dark::hover_fill()))
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
                .text_color(dark::white(1.0))
                .child(value),
        )
}

/// Native translucent shader, compiled at build time for Nova DX/Vulkan/Metal.
fn liquid_glass_surface(id: u64, opacity: f32, interacting: bool) -> gpui::AnyElement {
    let mut metadata = [[0.0_f32; 4]; 4];
    metadata[0][0] = opacity;
    metadata[0][1] = if interacting { 1.0 } else { 0.0 };
    metadata[0][2] = LIQUID_GLASS_CORNER_RADIUS;
    metadata[3][3] = 1.0;
    shader_effect_canvas(
        ShaderKind::LiquidGlass,
        id,
        ShaderParams16::from_columns(metadata),
        false,
    )
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
    fn desktop_line_transition_key_ignores_word_position() {
        let mut line = LyricsDisplay {
            track_id: 7,
            line_index: 3,
            position_ms: 1_000,
            current: "line A".into(),
            current_words: Arc::from(Vec::<LyricWord>::new()),
            translation: Some("译文".into()),
            next: Some("line B".into()),
            next_translation: None,
        };
        let initial_key = desktop_lyrics_display_key(Some(&line));
        line.position_ms = 2_500;
        assert_eq!(desktop_lyrics_display_key(Some(&line)), initial_key);
        line.line_index += 1;
        assert_ne!(desktop_lyrics_display_key(Some(&line)), initial_key);
        assert_eq!(desktop_lyrics_display_key(None), None);
    }

    #[test]
    fn desktop_line_transition_easing_keeps_endpoints_and_no_overshoot() {
        assert_eq!(desktop_smootherstep(0.0), 0.0);
        assert_eq!(desktop_smootherstep(1.0), 1.0);
        for step in 0..=100 {
            let t = step as f32 / 100.0;
            let opacity = desktop_smootherstep(t);
            assert!((0.0..=1.0).contains(&opacity));
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
        let words = [word(1_000, None, "A"), word(1_450, None, "B")];
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
