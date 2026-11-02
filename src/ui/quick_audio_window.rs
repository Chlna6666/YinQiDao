//! Native GPUI child window for realtime EQ and spatial DSP controls.
//! No Windows system dialog or separate audio engine is created: all updates
//! still reach the authoritative MusicApp and its shared audio processing state.

use anyhow::Result;
use gpui::{
    AppContext, Bounds, Context, Entity, Global, IntoElement, Render, Subscription, WeakEntity,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::*, px, size,
};
use lucide_gpui::icon;

use super::{settings, shell::MusicApp, theme};

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
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(760.0), px(550.0))),
            is_resizable: true,
            is_minimizable: true,
            is_movable: true,
            ..Default::default()
        },
        move |_, cx| cx.new(|cx| QuickAudioView::new(parent, cx)),
    )?;
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_4()
                    .border_b_1()
                    .border_color(theme::BORDER_HAIRLINE)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(theme::TEXT_PRIMARY)
                                    .child("实时音效调节"),
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
                            .id("quick-audio-window-close")
                            .p_2()
                            .rounded_full()
                            .cursor_pointer()
                            .hover(|style| style.bg(theme::bg_hover()))
                            .child(theme::themed_icon(
                                icon!(x),
                                18.0,
                                theme::TEXT_SECONDARY.into(),
                            ))
                            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.remove_window();
                            }),
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
