use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Bounds, Div, ElementId, Global, Hsla, MouseButton, Pixels, Stateful, canvas, div, hsla,
    prelude::*, px, relative, rgb,
};

use crate::ui::theme;

type SliderCallback = Rc<dyn Fn(f32, &mut App)>;
const DRAG_THRESHOLD_PX: f32 = 3.0;

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

#[derive(Default)]
struct SliderInteractionState {
    pressed_id: Option<String>,
    press_x: f32,
    dragging: bool,
}

impl Global for SliderInteractionState {}

fn ratio_from_position(position_x: Pixels, bounds: Bounds<Pixels>, thumb_size: Pixels) -> f32 {
    let thumb = f32::from(thumb_size);
    let usable_width = (f32::from(bounds.size.width) - thumb).max(1.0);
    let local = f32::from(position_x - bounds.left()) - thumb * 0.5;
    (local / usable_width).clamp(0.0, 1.0)
}

fn begin_pointer_press(id: &str, position_x: Pixels, cx: &mut App) {
    if !cx.has_global::<SliderInteractionState>() {
        cx.set_global(SliderInteractionState::default());
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.pressed_id = Some(id.to_owned());
        state.press_x = f32::from(position_x);
        state.dragging = false;
    });
}

fn pointer_state(id: &str, cx: &App) -> Option<(f32, bool)> {
    let state = cx.try_global::<SliderInteractionState>()?;
    (state.pressed_id.as_deref() == Some(id)).then_some((state.press_x, state.dragging))
}

fn mark_pointer_dragging(id: &str, cx: &mut App) {
    if cx
        .try_global::<SliderInteractionState>()
        .is_none_or(|state| state.pressed_id.as_deref() != Some(id))
    {
        return;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.dragging = true;
    });
}

fn end_pointer_press(id: &str, cx: &mut App) -> Option<bool> {
    let dragging = pointer_state(id, cx)?.1;
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.pressed_id = None;
        state.dragging = false;
    });
    Some(dragging)
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

/// Slider with native pointer semantics: click commits once, movement turns the press into scrubbing.
///
/// This intentionally does not use GPUI's drag-and-drop API. Registering `.on_drag(...)` creates a
/// framework drag session for the press itself, which makes a normal track click inherit drag
/// lifecycle/state. Here a press is just a press until the pointer moves at least 3 px while the
/// left button remains held.
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
    let bounds_for_move = bounds.clone();
    let bounds_for_up = bounds.clone();
    let bounds_for_up_out = bounds.clone();
    let id_for_down = id_string.clone();
    let id_for_move = id_string.clone();
    let id_for_up = id_string.clone();
    let id_for_up_out = id_string;
    let change_for_move = on_change;
    let commit_for_up = on_commit.clone();
    let commit_for_up_out = on_commit;

    slider_visual(id, ratio, style)
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
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            begin_pointer_press(&id_for_down, event.position.x, cx);
        })
        .on_mouse_move(move |event: &gpui::MouseMoveEvent, _window, cx| {
            if !event.dragging() {
                return;
            }
            let Some((press_x, dragging)) = pointer_state(&id_for_move, cx) else {
                return;
            };
            let distance = (f32::from(event.position.x) - press_x).abs();
            if !dragging && distance < DRAG_THRESHOLD_PX {
                return;
            }
            if !dragging {
                mark_pointer_dragging(&id_for_move, cx);
            }
            if let Some(bounds) = *bounds_for_move.borrow() {
                (change_for_move)(
                    ratio_from_position(event.position.x, bounds, style.thumb_size),
                    cx,
                );
            }
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            if end_pointer_press(&id_for_up, cx).is_none() {
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
            let Some(was_dragging) = end_pointer_press(&id_for_up_out, cx) else {
                return;
            };
            if !was_dragging {
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
