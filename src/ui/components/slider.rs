use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Bounds, Div, ElementId, Empty, Global, Hsla, MouseButton, Pixels, Stateful, canvas, div,
    hsla, prelude::*, px, relative, rgb,
};

use crate::ui::theme;

type SliderCallback = Rc<dyn Fn(f32, &mut App)>;

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

#[derive(Clone)]
struct SliderDrag {
    id: String,
    thumb_size: Pixels,
    on_change: SliderCallback,
}

#[derive(Default)]
struct SliderInteractionState {
    active_drag_id: Option<String>,
}

impl Global for SliderInteractionState {}

fn ratio_from_position(position_x: Pixels, bounds: Bounds<Pixels>, thumb_size: Pixels) -> f32 {
    let thumb = f32::from(thumb_size);
    let usable_width = (f32::from(bounds.size.width) - thumb).max(1.0);
    let local = f32::from(position_x - bounds.left()) - thumb * 0.5;
    (local / usable_width).clamp(0.0, 1.0)
}

fn is_active_drag(id: &str, cx: &App) -> bool {
    cx.try_global::<SliderInteractionState>()
        .is_some_and(|state| state.active_drag_id.as_deref() == Some(id))
}

fn begin_active_drag(id: &str, cx: &mut App) {
    if !cx.has_global::<SliderInteractionState>() {
        cx.set_global(SliderInteractionState::default());
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = Some(id.to_owned());
    });
}

fn end_active_drag(id: &str, cx: &mut App) -> bool {
    if !is_active_drag(id, cx) {
        return false;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = None;
    });
    true
}

fn slider_visual(id: ElementId, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_height = px((f32::from(style.thumb_size) * style.hover_thumb_scale)
        .max(f32::from(style.hover_track_height)));

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
        .id(id)
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
                .child(div().flex_none().w(relative(clamped_ratio)).h(px(1.0)))
                .child(thumb),
        )
}

/// Pure visual slider kept for places that do not need pointer interaction.
pub fn smooth_slider(id: impl Into<ElementId>, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    slider_visual(id.into(), ratio, style)
}

/// Slider with retained pointer drag semantics.
///
/// Mapping is derived from the element's actual painted bounds and exactly matches the thumb's
/// `(width - thumb_size)` travel range. GPUI's drag session continues dispatching `on_drag_move`
/// after the pointer leaves the slider, while `on_mouse_up_out` commits only when the button is
/// actually released outside the element.
pub fn interactive_slider(
    id: impl Into<ElementId>,
    ratio: f32,
    style: SliderStyle,
    on_change: impl Fn(f32, &mut App) + 'static,
    on_commit: impl Fn(f32, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let id_string = id.to_string();
    let on_change: SliderCallback = Rc::new(on_change);
    let on_commit: SliderCallback = Rc::new(on_commit);
    let bounds: Rc<RefCell<Option<Bounds<Pixels>>>> = Rc::default();

    let bounds_for_prepaint = bounds.clone();
    let bounds_for_down = bounds.clone();
    let bounds_for_up = bounds.clone();
    let bounds_for_up_out = bounds.clone();
    let id_for_down = id_string.clone();
    let id_for_down_out = id_string.clone();
    let id_for_drag = id_string.clone();
    let id_for_up = id_string.clone();
    let id_for_up_out = id_string.clone();
    let change_for_down = on_change.clone();
    let commit_for_up = on_commit.clone();
    let commit_for_up_out = on_commit.clone();
    let drag = SliderDrag {
        id: id_string,
        thumb_size: style.thumb_size,
        on_change,
    };

    slider_visual(id, ratio, style)
        // GPUI 557f9950 does not yet expose `on_children_prepainted`. A zero-paint absolute canvas
        // participates in the same retained layout and receives the slider's exact inner bounds in
        // prepaint, so click mapping remains geometry-driven instead of reverting to window-space
        // constants. Canvas itself creates no hitbox and emits no primitive.
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    *bounds_for_prepaint.borrow_mut() = Some(bounds);
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        )
        .on_drag(drag, |_: &SliderDrag, _, _, cx| cx.new(|_| Empty))
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            begin_active_drag(&id_for_down, cx);
            if let Some(bounds) = *bounds_for_down.borrow() {
                (change_for_down)(
                    ratio_from_position(event.position.x, bounds, style.thumb_size),
                    cx,
                );
            }
        })
        .on_mouse_down_out(move |_event, _window, cx| {
            let _ = end_active_drag(&id_for_down_out, cx);
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let (thumb_size, on_change) = {
                let drag = event.drag(cx);
                if drag.id != id_for_drag {
                    return;
                }
                (drag.thumb_size, drag.on_change.clone())
            };
            let ratio = ratio_from_position(event.event.position.x, event.bounds, thumb_size);
            (on_change)(ratio, cx);
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            if !end_active_drag(&id_for_up, cx) {
                return;
            }
            if let Some(bounds) = *bounds_for_up.borrow() {
                (commit_for_up)(
                    ratio_from_position(event.position.x, bounds, style.thumb_size),
                    cx,
                );
            }
        })
        .on_mouse_up_out(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            if !end_active_drag(&id_for_up_out, cx) {
                return;
            }
            if let Some(bounds) = *bounds_for_up_out.borrow() {
                (commit_for_up_out)(
                    ratio_from_position(event.position.x, bounds, style.thumb_size),
                    cx,
                );
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_mapping_matches_thumb_travel() {
        let bounds = Bounds::new(
            gpui::point(px(100.0), px(0.0)),
            gpui::size(px(210.0), px(12.0)),
        );
        let thumb = px(10.0);
        assert_eq!(ratio_from_position(px(105.0), bounds, thumb), 0.0);
        assert!((ratio_from_position(px(205.0), bounds, thumb) - 0.5).abs() < 0.0001);
        assert_eq!(ratio_from_position(px(305.0), bounds, thumb), 1.0);
        assert_eq!(ratio_from_position(px(-500.0), bounds, thumb), 0.0);
        assert_eq!(ratio_from_position(px(900.0), bounds, thumb), 1.0);
    }
}
