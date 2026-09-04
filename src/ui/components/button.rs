use gpui::{Div, ElementId, Hsla, Pixels, SharedString, Stateful, div, hsla, prelude::*};

use crate::ui::theme;

/// 优雅的胶囊/毛玻璃按钮组件
pub fn glass_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
) -> Stateful<Div> {
    let bg_color = if active {
        theme::ACCENT_RED.into()
    } else {
        hsla(0.0, 0.0, 1.0, 0.12)
    };

    let hover_bg = if active {
        theme::ACCENT_RED.into()
    } else {
        hsla(0.0, 0.0, 1.0, 0.22)
    };

    div()
        .id(id.into())
        .px_3()
        .py_1()
        .rounded_full()
        .cursor_pointer()
        .bg(bg_color)
        .hover(move |s| s.bg(hover_bg))
        .transition(theme::press_transition())
        .active(|s| s.scale(0.95))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(hsla(0.0, 0.0, 1.0, 0.90))
        .child(label.into())
}

/// 统一的图标按钮组件（支持自定义尺寸、悬停背景与按压弹性缩放）
pub fn icon_button(
    id: impl Into<ElementId>,
    icon_svg: &'static str,
    button_size: Pixels,
    icon_size: f32,
    icon_color: Hsla,
    active: bool,
) -> Stateful<Div> {
    let bg_color = if active {
        theme::accent_red_muted()
    } else {
        hsla(0.0, 0.0, 0.0, 0.0)
    };

    div()
        .id(id.into())
        .size(button_size)
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(bg_color)
        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.16)))
        .transition(theme::press_transition())
        .active(|s| s.scale(0.92))
        .child(theme::themed_icon(icon_svg, icon_size, icon_color))
}
