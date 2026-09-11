use std::{cell::Cell, rc::Rc};

use gpui::{
    App, Bounds, Div, ElementId, Global, Hsla, MouseButton, Pixels, Stateful, canvas, div, hsla,
    prelude::*, px, relative, rgb,
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
    /// Overlay the interaction strip across the leading layout edge without reserving vertical
    /// space. Used by the mini-player so the progress rail is the player's top boundary rather
    /// than a separate row above its controls.
    pub edge_overlay: bool,
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
            edge_overlay: false,
        }
    }
}

impl SliderStyle {
    pub fn mini_progress() -> Self {
        Self {
            track_height: px(2.0),
            hover_track_height: px(6.0),
            thumb_size: px(10.0),
            hover_thumb_scale: 1.20,
            track_bg: rgb(0xe1_e4_e9).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.15)),
            edge_overlay: true,
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
            edge_overlay: false,
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
            edge_overlay: false,
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
            edge_overlay: false,
        }
    }

    pub fn settings_control() -> Self {
        Self {
            track_height: px(6.0),
            hover_track_height: px(7.0),
            thumb_size: px(13.0),
            hover_thumb_scale: 1.15,
            track_bg: rgb(0xe1_e4_ea).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(220.0, 0.08, 0.68, 0.55)),
            edge_overlay: false,
        }
    }
}

#[derive(Default)]
struct SliderInteractionState {
    pressed_id: Option<String>,
    dragging: bool,
}

impl Global for SliderInteractionState {}

#[derive(Clone)]
struct SliderDrag {
    id: String,
}

fn begin_pointer_press(id: &str, cx: &mut App) {
    if !cx.has_global::<SliderInteractionState>() {
        cx.set_global(SliderInteractionState::default());
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.pressed_id = Some(id.to_owned());
        state.dragging = false;
    });
}

fn mark_pointer_dragging(id: &str, cx: &mut App) -> bool {
    if !cx.has_global::<SliderInteractionState>() {
        return false;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        if state.pressed_id.as_deref() != Some(id) {
            return false;
        }
        state.dragging = true;
        true
    })
}

fn end_pointer_press(id: &str, cx: &mut App) -> Option<bool> {
    if !cx.has_global::<SliderInteractionState>() {
        return None;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        if state.pressed_id.as_deref() != Some(id) {
            return None;
        }
        let dragging = state.dragging;
        state.pressed_id = None;
        state.dragging = false;
        Some(dragging)
    })
}

fn horizontal_ratio(position_x: Pixels, bounds: Bounds<Pixels>, _thumb_size: Pixels) -> f32 {
    let width = f32::from(bounds.size.width).max(1.0);
    let local = f32::from(position_x - bounds.left());
    (local / width).clamp(0.0, 1.0)
}

fn vertical_ratio(position_y: Pixels, bounds: Bounds<Pixels>, thumb_size: Pixels) -> f32 {
    let thumb = f32::from(thumb_size);
    let usable_height = (f32::from(bounds.size.height) - thumb).max(1.0);
    let local = f32::from(position_y - bounds.top()) - thumb * 0.5;
    (1.0 - local / usable_height).clamp(0.0, 1.0)
}

fn horizontal_track(ratio: f32, height: Pixels, style: SliderStyle) -> Div {
    div()
        .w_full()
        .h(height)
        .rounded_full()
        .bg(style.track_bg)
        .child(
            div()
                .h_full()
                .w(relative(ratio))
                .rounded_full()
                .bg(style.filled_color),
        )
}

fn slider_visual(id: ElementId, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_height = px((f32::from(style.thumb_size) * style.hover_thumb_scale)
        .max(f32::from(style.hover_track_height)));
    let hover_group = format!("slider-hover-{id}");
    let thumb_hover_group = hover_group.clone();
    let rail_hover_group = hover_group.clone();
    let edge_hover_top = px(f32::from(style.track_height) - f32::from(style.hover_track_height));
    let half_thumb = px(f32::from(style.thumb_size) * 0.5);

    let mut thumb = div()
        .flex_none()
        .size(style.thumb_size)
        .rounded_full()
        .bg(style.thumb_color)
        .shadow_md()
        .opacity(0.0)
        .group_hover(thumb_hover_group, move |s| {
            s.opacity(1.0).scale(style.hover_thumb_scale)
        })
        .transition(theme::hover_transition());

    if let Some(border) = style.thumb_border {
        thumb = thumb.border_1().border_color(border);
    }

    let base_track_layer = {
        let layer = div().absolute().inset_0().flex();
        let layer = if style.edge_overlay {
            layer.items_start()
        } else {
            layer.items_center()
        };
        layer.child(horizontal_track(clamped_ratio, style.track_height, style))
    };
    let hover_track_layer = if style.edge_overlay {
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(edge_hover_top)
            .h(style.hover_track_height)
            .flex()
            .items_start()
            .opacity(0.0)
            .group_hover(rail_hover_group, |s| s.opacity(1.0))
            .transition(theme::hover_transition())
            .child(horizontal_track(
                clamped_ratio,
                style.hover_track_height,
                style,
            ))
    } else {
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .opacity(0.0)
            .group_hover(rail_hover_group, |s| s.opacity(1.0))
            .transition(theme::hover_transition())
            .child(horizontal_track(
                clamped_ratio,
                style.hover_track_height,
                style,
            ))
    };

    let thumb_layer = if style.edge_overlay {
        div()
            .absolute()
            .left(px(-f32::from(half_thumb)))
            .right(half_thumb)
            .top(edge_hover_top)
            .h(style.hover_track_height)
            .flex()
            .items_center()
            .child(div().flex_none().w(relative(clamped_ratio)).h(px(1.0)))
            .child(thumb)
    } else {
        div()
            .absolute()
            .left(px(-f32::from(half_thumb)))
            .right(half_thumb)
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .child(div().flex_none().w(relative(clamped_ratio)).h(px(1.0)))
            .child(thumb)
    };

    let root = div()
        .group(hover_group)
        .id(id)
        .relative()
        .cursor_pointer()
        .h(interaction_height)
        .child(base_track_layer)
        .child(hover_track_layer)
        .child(thumb_layer);

    if style.edge_overlay {
        root.absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
    } else {
        root
    }
}

fn vertical_slider_visual(
    id: ElementId,
    ratio: f32,
    height: Pixels,
    style: SliderStyle,
) -> Stateful<Div> {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_width = px((f32::from(style.thumb_size) * style.hover_thumb_scale)
        .max(f32::from(style.hover_track_height)));
    let hover_group = format!("slider-hover-{id}");
    let thumb_hover_group = hover_group.clone();
    let rail_hover_group = hover_group.clone();

    let mut thumb = div()
        .flex_none()
        .size(style.thumb_size)
        .rounded_full()
        .bg(style.thumb_color)
        .shadow_md()
        .opacity(0.0)
        .group_hover(thumb_hover_group, move |s| {
            s.opacity(1.0).scale(style.hover_thumb_scale)
        })
        .transition(theme::hover_transition());
    if let Some(border) = style.thumb_border {
        thumb = thumb.border_1().border_color(border);
    }

    let track = |width: Pixels| {
        div()
            .h_full()
            .w(width)
            .rounded_full()
            .bg(style.track_bg)
            .flex()
            .flex_col()
            .justify_end()
            .child(
                div()
                    .w_full()
                    .h(relative(clamped_ratio))
                    .rounded_full()
                    .bg(style.filled_color),
            )
    };

    div()
        .group(hover_group)
        .id(id)
        .relative()
        .cursor_pointer()
        .w(interaction_width)
        .h(height)
        .flex()
        .justify_center()
        .child(track(style.track_height))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .opacity(0.0)
                .group_hover(rail_hover_group, |s| s.opacity(1.0))
                .transition(theme::hover_transition())
                .child(track(style.hover_track_height)),
        )
        .child(
            div()
                .absolute()
                .top(px(0.0))
                .bottom(style.thumb_size)
                .left(px(0.0))
                .right(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .h(relative(1.0 - clamped_ratio))
                        .w(px(1.0)),
                )
                .child(thumb),
        )
}

pub fn smooth_slider(id: impl Into<ElementId>, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    slider_visual(id.into(), ratio, style)
}

pub fn interactive_slider(
    id: impl Into<ElementId>,
    ratio: f32,
    style: SliderStyle,
    on_click: impl Fn(f32, &mut App) + 'static,
    on_drag: impl Fn(f32, &mut App) + 'static,
    on_drag_end: impl Fn(f32, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let id_string = id.to_string();
    let on_click: SliderCallback = Rc::new(on_click);
    let on_drag: SliderCallback = Rc::new(on_drag);
    let on_drag_end: SliderCallback = Rc::new(on_drag_end);
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));

    let bounds_for_prepaint = bounds.clone();
    let bounds_for_down = bounds.clone();
    let bounds_for_up = bounds.clone();
    let bounds_for_up_out = bounds.clone();
    let id_for_down = id_string.clone();
    let id_for_drag = id_string.clone();
    let id_for_up = id_string.clone();
    let id_for_up_out = id_string.clone();
    let click_for_up = on_click.clone();
    let click_for_up_out = on_click;
    let drag_for_move = on_drag;
    let drag_end_for_up = on_drag_end.clone();
    let drag_end_for_up_out = on_drag_end;
    let drag_payload = SliderDrag { id: id_string };

    slider_visual(id, ratio, style)
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    bounds_for_prepaint.set(Some(bounds));
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        )
        .on_drag(drag_payload, |_: &SliderDrag, _, _, cx| cx.new(|_| gpui::Empty))
        .on_mouse_down(MouseButton::Left, move |_event, _window, cx| {
            cx.stop_propagation();
            if bounds_for_down.get().is_some() {
                begin_pointer_press(&id_for_down, cx);
            }
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let drag = event.drag(cx);
            if drag.id != id_for_drag || !mark_pointer_dragging(&id_for_drag, cx) {
                return;
            }
            let ratio = horizontal_ratio(event.event.position.x, event.bounds, style.thumb_size);
            (drag_for_move)(ratio, cx);
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            let Some(was_dragging) = end_pointer_press(&id_for_up, cx) else {
                return;
            };
            let Some(bounds) = bounds_for_up.get() else {
                return;
            };
            let ratio = horizontal_ratio(event.position.x, bounds, style.thumb_size);
            if was_dragging {
                (drag_end_for_up)(ratio, cx);
            } else {
                (click_for_up)(ratio, cx);
            }
        })
        .on_mouse_up_out(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            let Some(was_dragging) = end_pointer_press(&id_for_up_out, cx) else {
                return;
            };
            let Some(bounds) = bounds_for_up_out.get() else {
                return;
            };
            let ratio = horizontal_ratio(event.position.x, bounds, style.thumb_size);
            if was_dragging {
                (drag_end_for_up_out)(ratio, cx);
            } else {
                (click_for_up_out)(ratio, cx);
            }
        })
}

pub fn interactive_vertical_slider(
    id: impl Into<ElementId>,
    ratio: f32,
    height: Pixels,
    style: SliderStyle,
    on_change: impl Fn(f32, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let id_string = id.to_string();
    let on_change: SliderCallback = Rc::new(on_change);
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));

    let bounds_for_prepaint = bounds.clone();
    let bounds_for_down = bounds.clone();
    let bounds_for_up = bounds.clone();
    let bounds_for_up_out = bounds.clone();
    let id_for_down = id_string.clone();
    let id_for_drag = id_string.clone();
    let id_for_up = id_string.clone();
    let id_for_up_out = id_string.clone();
    let change_for_down = on_change.clone();
    let change_for_move = on_change.clone();
    let change_for_up = on_change.clone();
    let change_for_up_out = on_change;
    let drag_payload = SliderDrag { id: id_string };

    vertical_slider_visual(id, ratio, height, style)
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    bounds_for_prepaint.set(Some(bounds));
                },
                |_bounds, (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        )
        .on_drag(drag_payload, |_: &SliderDrag, _, _, cx| cx.new(|_| gpui::Empty))
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            let Some(bounds) = bounds_for_down.get() else {
                return;
            };
            begin_pointer_press(&id_for_down, cx);
            (change_for_down)(
                vertical_ratio(event.position.y, bounds, style.thumb_size),
                cx,
            );
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let drag = event.drag(cx);
            if drag.id != id_for_drag || !mark_pointer_dragging(&id_for_drag, cx) {
                return;
            }
            let ratio = vertical_ratio(event.event.position.y, event.bounds, style.thumb_size);
            (change_for_move)(ratio, cx);
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            let Some(was_dragging) = end_pointer_press(&id_for_up, cx) else {
                return;
            };
            if !was_dragging {
                return;
            }
            if let Some(bounds) = bounds_for_up.get() {
                (change_for_up)(
                    vertical_ratio(event.position.y, bounds, style.thumb_size),
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
            if let Some(bounds) = bounds_for_up_out.get() {
                (change_for_up_out)(
                    vertical_ratio(event.position.y, bounds, style.thumb_size),
                    cx,
                );
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_mapping_matches_visible_track() {
        let bounds = Bounds::new(
            gpui::point(px(100.0), px(0.0)),
            gpui::size(px(210.0), px(12.0)),
        );
        let thumb = px(10.0);
        assert_eq!(horizontal_ratio(px(100.0), bounds, thumb), 0.0);
        assert!((horizontal_ratio(px(205.0), bounds, thumb) - 0.5).abs() < 0.0001);
        assert_eq!(horizontal_ratio(px(310.0), bounds, thumb), 1.0);
    }

    #[test]
    fn vertical_pointer_mapping_is_bottom_to_top() {
        let bounds = Bounds::new(
            gpui::point(px(0.0), px(100.0)),
            gpui::size(px(16.0), px(210.0)),
        );
        let thumb = px(10.0);
        assert_eq!(vertical_ratio(px(105.0), bounds, thumb), 1.0);
        assert!((vertical_ratio(px(205.0), bounds, thumb) - 0.5).abs() < 0.0001);
        assert_eq!(vertical_ratio(px(305.0), bounds, thumb), 0.0);
    }

    #[test]
    fn mini_progress_hover_expands_outward() {
        let style = SliderStyle::mini_progress();
        assert!(f32::from(style.hover_track_height) > f32::from(style.track_height));
        assert!(f32::from(style.track_height) - f32::from(style.hover_track_height) < 0.0);
    }

    #[test]
    fn bounds_cell_can_be_updated_without_runtime_borrowing() {
        let cell = Rc::new(Cell::new(Some(Bounds::new(
            gpui::point(px(0.0), px(0.0)),
            gpui::size(px(100.0), px(12.0)),
        ))));
        assert!(cell.get().is_some());
        cell.set(None);
        assert!(cell.get().is_none());
    }
}
