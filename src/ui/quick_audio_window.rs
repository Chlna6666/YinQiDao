//! Native GPUI child window for realtime EQ and spatial DSP controls.
//! No Windows system dialog or separate audio engine is created: all updates
//! still reach the authoritative MusicApp and its shared audio processing state.

use super::{settings, shell::MusicApp, theme};
use anyhow::Result;
use gpui::{
    AppContext, Bounds, Context, Entity, Global, IntoElement, Render, Subscription, WeakEntity,
    Window, WindowBounds, WindowControlArea, WindowHandle, WindowOptions, div, prelude::*, px,
    size,
};

#[derive(Default)]
struct QuickAudioWindowState {
    window: Option<WindowHandle<QuickAudioView>>,
}

impl Global for QuickAudioWindowState {}

pub(super) fn open(parent: Entity<MusicApp>, cx: &mut Context<MusicApp>) -> Result<()> {
    if !cx.has_global::<QuickAudioWindowState>() {
        cx.set_global(QuickAudioWindowState::default());
    }

    if let Some(existing) = cx
        .try_global::<QuickAudioWindowState>()
        .and_then(|state| state.window.clone())
    {
        if existing
            .update(cx, |_view, window, _cx| window.show_window())
            .is_ok()
        {
            return Ok(());
        }
        cx.update_global(|state: &mut QuickAudioWindowState, _| {
            state.window = None;
        });
    }

    let bounds = Bounds::centered(None, size(px(940.0), px(820.0)), cx);
    let mut options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(760.0), px(550.0))),
        is_resizable: true,
        is_minimizable: true,
        is_movable: true,
        ..Default::default()
    };
    #[cfg(windows)]
    {
        options.titlebar = Some(gpui::TitlebarOptions {
            title: Some("实时音效调节".into()),
            appears_transparent: true,
            ..Default::default()
        });
        options.window_background = gpui::WindowBackgroundAppearance::Opaque;
        options.window_corner_preference = gpui::WindowCornerPreference::Rounded;
    }
    #[cfg(target_os = "linux")]
    {
        options.window_decorations = Some(gpui::WindowDecorations::Client);
        options.titlebar = None;
    }
    let window = cx.open_window(options, move |_, cx| {
        cx.new(|cx| QuickAudioView::new(parent, cx))
    })?;
    cx.update_global(|state: &mut QuickAudioWindowState, _| {
        state.window = Some(window);
    });
    Ok(())
}

struct QuickAudioView {
    parent: WeakEntity<MusicApp>,
    _parent_observer: Subscription,
    _release_subscription: Subscription,
}

impl QuickAudioView {
    fn new(parent: Entity<MusicApp>, cx: &mut Context<Self>) -> Self {
        let parent_observer = cx.observe(&parent, |_this, _parent, cx| {
            cx.notify();
        });
        let release_subscription = cx.on_release(|_, cx| {
            if cx.has_global::<QuickAudioWindowState>() {
                cx.update_global(|state: &mut QuickAudioWindowState, _| {
                    state.window = None;
                });
            }
        });
        Self {
            parent: parent.downgrade(),
            _parent_observer: parent_observer,
            _release_subscription: release_subscription,
        }
    }
}

impl Render for QuickAudioView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The EQ popup may be behind the player while a preset animates.
        super::enable_inactive_window_animations(window);

        // Reuse exactly the same EQ and spatial control elements/listeners as
        // the Settings page. Only the native GPUI window shell is independent.
        let content = self
            .parent
            .update(cx, |app, app_cx| {
                settings::render_quick_audio_content(app, app_cx)
            })
            .ok()
            .unwrap_or_else(|| {
                div()
                    .p_5()
                    .text_color(theme::TEXT_SECONDARY)
                    .child("播放器已关闭")
                    .into_any_element()
            });

        div()
            .id("quick-audio-window-root")
            .size_full()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .bg(theme::BG_CANVAS)
            // Dedicated 38px chrome: no duplicate OS caption above this row.
            .child(
                div()
                    .id("quick-audio-custom-titlebar")
                    .relative()
                    .w_full()
                    .h(px(38.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .bg(theme::BG_CANVAS)
                    .border_b_1()
                    .border_color(theme::BORDER_HAIRLINE)
                    .child(
                        div()
                            .id("quick-audio-title-drag")
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .window_control_area(WindowControlArea::Drag)
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::TEXT_SECONDARY)
                                    .child("实时音效调节"),
                            )
                            .on_mouse_down(gpui::MouseButton::Left, |event, window, _cx| {
                                if event.click_count >= 2 {
                                    window.titlebar_double_click();
                                } else if !cfg!(windows) {
                                    window.start_window_move();
                                }
                            }),
                    )
                    .child(
                        div()
                            .relative()
                            .occlude()
                            .window_control_area(WindowControlArea::Client)
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .child(quick_audio_window_button(
                                "quick-audio-close",
                                theme::TRAFFIC_LIGHT_RED,
                                |_, window, cx| {
                                    cx.stop_propagation();
                                    window.remove_window();
                                },
                            ))
                            .child(quick_audio_window_button(
                                "quick-audio-minimize",
                                theme::TRAFFIC_LIGHT_AMBER,
                                |_, window, cx| {
                                    cx.stop_propagation();
                                    window.minimize_window();
                                },
                            ))
                            .child(quick_audio_window_button(
                                "quick-audio-maximize",
                                theme::TRAFFIC_LIGHT_GREEN,
                                |_, window, cx| {
                                    cx.stop_propagation();
                                    if window.is_maximized() {
                                        window.restore_window();
                                    } else {
                                        window.maximize_window();
                                    }
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .id("quick-audio-header")
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_6()
                    .pt_4()
                    .pb_4()
                    .border_b_1()
                    .border_color(theme::BORDER_HAIRLINE)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(theme::TEXT_PRIMARY)
                            .child("音效控制"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::TEXT_SECONDARY)
                            .child("均衡器与空间音效 · 与主播放器实时同步"),
                    ),
            )
            .child(
                div()
                    .id("quick-audio-window-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px_6()
                    .py_5()
                    .child(content),
            )
    }
}

fn quick_audio_window_button(
    id: &'static str,
    color: gpui::Rgba,
    on_press: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(12.0))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(theme::BORDER_HAIRLINE)
        .cursor_pointer()
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .hover(|style| style.opacity(0.80))
        .transition(theme::press_transition())
        .on_mouse_down(gpui::MouseButton::Left, on_press)
}
