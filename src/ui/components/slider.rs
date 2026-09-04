use gpui::{Div, ElementId, Hsla, Pixels, Stateful, div, hsla, prelude::*, px, relative, rgb};

use crate::ui::theme;

#[derive(Clone, Copy, Debug)]
pub struct SliderStyle {
    pub track_height: Pixels,
    pub hover_track_height: Pixels,
    pub thumb_size: Pixels,
    pub hover_thumb_scale: f32,
    pub track_bg: Hsla,
    pub filled_color: Hsla,
    pub thumb_color: Hsla,
    pub thumb_border: Option<Hsla>,
}

impl Default for SliderStyle {
    fn default() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.0),
            thumb_size: px(12.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.15)),
        }
    }
}

impl SliderStyle {
    pub fn mini_progress() -> Self {
        Self {
            track_height: px(3.5),
            hover_track_height: px(5.5),
            thumb_size: px(11.0),
            hover_thumb_scale: 1.25,
            track_bg: rgb(0xe8_ea_ee).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.15)),
        }
    }

    pub fn stage_progress() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.5),
            thumb_size: px(13.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: None,
        }
    }

    pub fn stage_volume() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.0),
            thumb_size: px(11.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: None,
        }
    }

    pub fn mini_volume() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(6.5),
            thumb_size: px(10.0),
            hover_thumb_scale: 1.20,
            track_bg: rgb(0xe0_e2_e8).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.12)),
        }
    }
}

/// 统一的高精度交互式滑块组件。
///
/// Thumb 使用独立的 `(width - thumb_size)` 定位轨道，而不是作为 filled track 的子元素。
/// 因此 0% 时左边缘恰好落在控件左侧，100% 时右边缘恰好落在控件右侧，任何比例都不会
/// 因圆点半径而越过滑块边界。
pub fn smooth_slider(id: impl Into<ElementId>, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_height = px(
        (f32::from(style.thumb_size) * style.hover_thumb_scale)
            .max(f32::from(style.hover_track_height)),
    );

    let mut thumb = div()
        .flex_none()
        .size(style.thumb_size)
        .rounded_full()
        .bg(style.thumb_color)
        .shadow_md()
        .hover(move |s| s.scale(style.hover_thumb_scale))
        .transition(theme::hover_transition());

    if let Some(border) = style.thumb_border {
        thumb = thumb.border_1().border_color(border);
    }

    let track = div()
        .w_full()
        .h(style.track_height)
        .rounded_full()
        .bg(style.track_bg)
        .hover(move |s| s.h(style.hover_track_height))
        .transition(theme::hover_transition())
        .child(
            div()
                .h_full()
                .w(relative(clamped_ratio))
                .rounded_full()
                .bg(style.filled_color),
        );

    div()
        .id(id.into())
        .relative()
        .cursor_pointer()
        .h(interaction_height)
        .flex()
        .items_center()
        .child(track)
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .right(style.thumb_size)
                .top(px(0.0))
                .bottom(px(0.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .w(relative(clamped_ratio))
                        .h(px(1.0)),
                )
                .child(thumb),
        )
}
