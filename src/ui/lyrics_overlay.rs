use gpui::{
    Context, IntoElement, Subscription, WeakEntity, Window, WindowControlArea, div, hsla,
    prelude::*, px, rgb,
};

use crate::settings::DesktopLyricsAlignment;

use super::shell::MusicApp;

pub(crate) struct DesktopLyricsView {
    parent: WeakEntity<MusicApp>,
    bounds_subscription: Option<Subscription>,
}

impl DesktopLyricsView {
    pub(crate) fn new(parent: WeakEntity<MusicApp>) -> Self {
        Self {
            parent,
            bounds_subscription: None,
        }
    }

    fn attach_bounds_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bounds_subscription.is_some() {
            return;
        }
        let parent = self.parent.clone();
        self.bounds_subscription = Some(cx.observe_window_bounds(window, move |_view, window, cx| {
            let bounds = window.bounds();
            let _ = parent.update(cx, |app, _cx| {
                app.persist_desktop_lyrics_bounds(bounds);
            });
        }));
    }
}

impl gpui::Render for DesktopLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach_bounds_observer(window, cx);

        let Some(parent) = self.parent.upgrade() else {
            return div().size_full().into_any_element();
        };
        let app = parent.read(cx);
        let config = app.config.desktop_lyrics.clone();
        let display = app.desktop_lyrics_display();
        let current = display
            .as_ref()
            .map_or_else(|| "暂无同步歌词".to_string(), |lyrics| lyrics.current.clone());
        let translation = display
            .as_ref()
            .and_then(|lyrics| lyrics.translation.clone())
            .filter(|text| config.show_translation && !text.trim().is_empty());
        let next = display
            .as_ref()
            .and_then(|lyrics| lyrics.next.clone())
            .filter(|text| config.two_line && !text.trim().is_empty());
        let next_translation = display
            .as_ref()
            .and_then(|lyrics| lyrics.next_translation.clone())
            .filter(|text| config.two_line && config.show_translation && !text.trim().is_empty());

        let parent_weak = self.parent.clone();
        let close_parent = self.parent.clone();
        let translation_parent = self.parent.clone();

        let mut lyrics = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(3.0));

        lyrics = match config.alignment {
            DesktopLyricsAlignment::Left => lyrics.items_start(),
            DesktopLyricsAlignment::Center => lyrics.items_center(),
            DesktopLyricsAlignment::Right => lyrics.items_end(),
        };

        lyrics = lyrics.child(aligned_line(
            current,
            config.alignment,
            config.font_size,
            config.active_color,
            gpui::FontWeight::SEMIBOLD,
            1.0,
        ));

        if let Some(translation) = translation {
            lyrics = lyrics.child(aligned_line(
                translation,
                config.alignment,
                (config.font_size * 0.52).max(13.0),
                config.translation_color,
                gpui::FontWeight::MEDIUM,
                0.90,
            ));
        }
        if let Some(next) = next {
            lyrics = lyrics.child(aligned_line(
                next,
                config.alignment,
                (config.font_size * 0.66).max(15.0),
                config.inactive_color,
                gpui::FontWeight::MEDIUM,
                0.70,
            ));
        }
        if let Some(next_translation) = next_translation {
            lyrics = lyrics.child(aligned_line(
                next_translation,
                config.alignment,
                (config.font_size * 0.44).max(12.0),
                config.translation_color,
                gpui::FontWeight::NORMAL,
                0.58,
            ));
        }

        let toolbar = div()
            .absolute()
            .top(px(7.0))
            .right(px(8.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .window_control_area(WindowControlArea::Client)
            .child(toolbar_button(
                "desktop-lyrics-lock",
                if config.locked { "解锁" } else { "锁定" },
                move |_, _window, cx| {
                    let _ = parent_weak.update(cx, |app, app_cx| {
                        app.toggle_desktop_lyrics_lock(app_cx);
                    });
                },
            ))
            .child(toolbar_button(
                "desktop-lyrics-translation",
                if config.show_translation { "译" } else { "原" },
                move |_, _window, cx| {
                    let _ = translation_parent.update(cx, |app, app_cx| {
                        app.toggle_desktop_lyrics_translation(app_cx);
                    });
                },
            ))
            .child(toolbar_button(
                "desktop-lyrics-close",
                "×",
                move |_, window, cx| {
                    window.remove_window();
                    let _ = close_parent.update(cx, |app, app_cx| {
                        app.desktop_lyrics_window_closed(app_cx);
                    });
                },
            ));

        let mut root = div()
            .id("desktop-lyrics-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(hsla(0.0, 0.0, 0.0, 0.0))
            .px(px(24.0))
            .py(px(12.0))
            .child(lyrics)
            .child(toolbar);

        if !config.locked {
            root = root
                .window_control_area(WindowControlArea::Drag)
                .cursor_move();
        }

        root.into_any_element()
    }
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
        .bg(rgb(0x2c_2c_2e))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0xff_ff_ff))
        .hover(|style| style.bg(rgb(0x1c_1c_1e)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
}
