use std::{cell::Cell, rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, App, Bounds, Div, ElementId, Empty, Global,
    HorizontalRevealEdge, Hsla, MouseButton, Pixels, SharedString, Stateful, div, hsla, point,
    prelude::*, px, relative, rgb,
};

use crate::ui::theme;
use crate::ui::theme::dark;

type SliderCallback = Rc<dyn Fn(f32, &mut App)>;

#[derive(Clone, Copy, Debug)]
pub struct SliderStyle {
    pub track_height: Pixels,
    pub hover_track_height: Pixels,
    pub thumb_size: Pixels,
    pub hover_thumb_scale: f32,
    pub interaction_height: Pixels,
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
            interaction_height: px(30.0),
            track_bg: dark::white(0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: dark::white(1.0),
            thumb_border: Some(dark::scrim(0.15)),
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
            // 2px rail + 1px tolerance. Expansion occurs outside the player.
            interaction_height: px(3.0),
            track_bg: theme::BORDER_HAIRLINE.into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: dark::white(1.0),
            thumb_border: Some(dark::scrim(0.15)),
            edge_overlay: true,
        }
    }

    pub fn stage_progress() -> Self {
        Self {
            track_height: px(4.0),
            hover_track_height: px(6.0),
            thumb_size: px(10.0),
            hover_thumb_scale: 1.20,
            interaction_height: px(30.0),
            track_bg: dark::white(0.26),
            filled_color: dark::white(0.96),
            thumb_color: dark::white(1.0),
            thumb_border: None,
            edge_overlay: false,
        }
    }

    pub fn stage_volume() -> Self {
        Self {
            track_height: px(4.0),
            hover_track_height: px(5.5),
            thumb_size: px(9.0),
            hover_thumb_scale: 1.20,
            interaction_height: px(22.0),
            track_bg: dark::white(0.24),
            filled_color: dark::white(0.90),
            thumb_color: dark::white(1.0),
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
            interaction_height: px(22.0),
            track_bg: theme::BORDER_HAIRLINE.into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: dark::white(1.0),
            thumb_border: Some(dark::scrim(0.12)),
            edge_overlay: false,
        }
    }

    pub fn settings_control() -> Self {
        Self {
            track_height: px(6.0),
            hover_track_height: px(7.0),
            thumb_size: px(13.0),
            hover_thumb_scale: 1.15,
            interaction_height: px(30.0),
            track_bg: theme::BORDER_HAIRLINE.into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: dark::white(1.0),
            thumb_border: Some(theme::BORDER_HAIRLINE.into()),
            edge_overlay: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SliderGesture {
    last_ratio: f32,
    dragged: bool,
}

#[derive(Debug, Default, PartialEq)]
struct SliderInteractionState {
    active_drag_id: Option<ElementId>,
    gesture: Option<SliderGesture>,
}

impl Global for SliderInteractionState {}

#[derive(Clone, Copy)]
enum SliderAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone)]
struct SliderDrag {
    id: ElementId,
    axis: SliderAxis,
    on_change: SliderCallback,
}

#[derive(Clone)]
pub struct InteractiveSliderState {
    id: ElementId,
    hover_group: SharedString,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    on_click: SliderCallback,
    on_drag: SliderCallback,
    on_drag_end: SliderCallback,
    /// Progress seeks may preview on press and commit once on release.
    defer_commit_until_release: bool,
}

impl InteractiveSliderState {
    pub fn new(
        id: impl Into<ElementId>,
        on_click: impl Fn(f32, &mut App) + 'static,
        on_drag: impl Fn(f32, &mut App) + 'static,
        on_drag_end: impl Fn(f32, &mut App) + 'static,
    ) -> Self {
        let id = id.into();
        let hover_group = SharedString::from(format!("slider-hover-{id}"));
        Self {
            id,
            hover_group,
            bounds: Rc::new(Cell::new(None)),
            on_click: Rc::new(on_click),
            on_drag: Rc::new(on_drag),
            on_drag_end: Rc::new(on_drag_end),
            defer_commit_until_release: false,
        }
    }

    pub fn defer_commit_until_release(&mut self) {
        self.defer_commit_until_release = true;
    }

    pub fn render(&self, ratio: f32, style: SliderStyle) -> Stateful<Div> {
        interactive_slider_state(self, ratio, style, None)
    }

    /// Render a monotonically advancing horizontal progress slider through retained animation.
    ///
    /// `ratio` is the current position. Fill and thumb advance to the end over `duration`.
    /// Change `animation_epoch` after pause/resume, seek, track replacement, or another transport
    /// discontinuity so the retained timeline restarts from the newly sampled ratio.
    pub fn render_animated(
        &self,
        ratio: f32,
        duration: Duration,
        animation_epoch: u64,
        style: SliderStyle,
    ) -> Stateful<Div> {
        interactive_slider_state(
            self,
            ratio,
            style,
            Some(SliderProgressAnimation {
                duration,
                epoch: animation_epoch,
            }),
        )
    }
}

fn is_active_drag(id: &ElementId, cx: &App) -> bool {
    cx.try_global::<SliderInteractionState>()
        .is_some_and(|state| state.active_drag_id.as_ref() == Some(id))
}

fn begin_active_drag(id: &ElementId, cx: &mut App) {
    if !cx.has_global::<SliderInteractionState>() {
        cx.set_global(SliderInteractionState::default());
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = Some(id.clone());
        state.gesture = None;
    });
}

fn begin_slider_gesture(id: &ElementId, ratio: f32, cx: &mut App) {
    begin_active_drag(id, cx);
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.gesture = Some(SliderGesture {
            last_ratio: ratio,
            dragged: false,
        });
    });
}

fn update_slider_gesture(id: &ElementId, ratio: f32, cx: &mut App) -> bool {
    if !is_active_drag(id, cx) {
        return false;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        if let Some(drag) = state.gesture.as_mut() {
            drag.last_ratio = ratio;
            drag.dragged = true;
        }
    });
    true
}

fn take_slider_gesture(id: &ElementId, cx: &mut App) -> Option<SliderGesture> {
    if !is_active_drag(id, cx) {
        return None;
    }
    let mut gesture = None;
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = None;
        gesture = state.gesture.take();
    });
    gesture
}

/// Accept only presses on the actual slider interaction rect. A stale
/// retained hit-test target cannot capture clicks intended for other buttons.
fn pointer_inside_bounds(position: gpui::Point<Pixels>, bounds: Bounds<Pixels>) -> bool {
    position.x >= bounds.left()
        && position.x <= bounds.left() + bounds.size.width
        && position.y >= bounds.top()
        && position.y <= bounds.top() + bounds.size.height
}

#[inline]
fn commit_on_release(dragged: bool, deferred: bool) -> bool {
    dragged || deferred
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
                .w(relative(ratio.clamp(0.0, 1.0)))
                .rounded_full()
                .bg(style.filled_color),
        )
}

/// Unlike normal sliders, the mini edge overlay must not expand the
/// resting hitbox to accommodate an invisible 10px thumb.
fn horizontal_interaction_height(style: SliderStyle) -> Pixels {
    if style.edge_overlay {
        style.interaction_height
    } else {
        px(f32::from(style.interaction_height)
            .max(f32::from(style.thumb_size) * style.hover_thumb_scale)
            .max(f32::from(style.hover_track_height)))
    }
}

/// Extra pixels painted ABOVE the mini player's top edge on hover.
fn edge_reveal_height(style: SliderStyle) -> Pixels {
    px((f32::from(style.hover_track_height) - f32::from(style.track_height)).max(0.0))
}

/// The cached view is positioned `-reveal` above the player's top edge.
/// In the resting state the hitbox starts at +reveal; on hover it expands
/// upward, leaving its bottom edge completely unchanged.
fn edge_overlay_root(root: Div, style: SliderStyle) -> Div {
    let reveal = edge_reveal_height(style);
    let expanded_hit_height = px(f32::from(style.interaction_height) + f32::from(reveal));
    root.absolute()
        .left(px(0.0))
        .right(px(0.0))
        .top(reveal)
        .hover(move |hover| hover.top(px(0.0)).h(expanded_hit_height))
}

fn slider_visual_with_group(
    _id: ElementId,
    ratio: f32,
    style: SliderStyle,
    hover_group: SharedString,
) -> Div {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_height = horizontal_interaction_height(style);
    let thumb_hover_group = hover_group.clone();
    let rail_hover_group = hover_group.clone();
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
            // The 2px line stays anchored at the player boundary, and
            // the increased hover height grows only upward.
            layer.items_end().pb(px(1.0))
        } else {
            layer.items_center()
        };
        layer.child(horizontal_track(clamped_ratio, style.track_height, style))
    };
    let hover_track_layer = if style.edge_overlay {
        // GPUI computes layout before hover hit-testing: changing `.h()`
        // with group_hover never changes the 2px track's layout. Instead,
        // fade in a separate, pre-sized 6px paint layer anchored to the
        // SAME bottom edge. The hover listener stays inside the 3px root.
        Some(
            div()
                .absolute()
                .inset_0()
                .opacity(0.0)
                .group_hover(rail_hover_group, |hover| hover.opacity(1.0))
                .transition(theme::hover_transition())
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .bottom(px(1.0))
                        .h(style.hover_track_height)
                        .child(horizontal_track(
                            clamped_ratio,
                            style.hover_track_height,
                            style,
                        )),
                ),
        )
    } else {
        Some(
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
                )),
        )
    };

    let thumb_layer = if style.edge_overlay {
        div()
            .absolute()
            .left(px(-f32::from(half_thumb)))
            .right(half_thumb)
            .top(px(0.0))
            .h(style.interaction_height)
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
        .relative()
        .cursor_pointer()
        .h(interaction_height)
        .child(base_track_layer)
        .children(hover_track_layer)
        .child(thumb_layer);

    if style.edge_overlay {
        edge_overlay_root(root, style)
    } else {
        root
    }
}

#[derive(Clone, Copy, Debug)]
struct SliderProgressAnimation {
    duration: Duration,
    epoch: u64,
}

fn animated_horizontal_track(
    ratio: f32,
    height: Pixels,
    style: SliderStyle,
    animation_name: &'static str,
    animation: SliderProgressAnimation,
) -> gpui::AnyElement {
    let ratio = ratio.clamp(0.0, 1.0);
    let fill = div()
        .w_full()
        .h_full()
        .rounded_full()
        .bg(style.filled_color);

    let fill =
        if ratio >= 1.0 || animation.duration.is_zero() {
            fill.into_any_element()
        } else {
            fill.with_animation(
                ElementId::NamedInteger(SharedString::new_static(animation_name), animation.epoch),
                Animation::new(animation.duration).with_property(
                    AnimationProperty::horizontal_reveal(HorizontalRevealEdge::Left, ratio, 1.0),
                ),
                |element, _| element,
            )
            .into_any_element()
        };

    div()
        .w_full()
        .h(height)
        .rounded_full()
        .bg(style.track_bg)
        .child(fill)
        .into_any_element()
}

fn slider_visual_with_group_animated(
    id: ElementId,
    ratio: f32,
    style: SliderStyle,
    hover_group: SharedString,
    animation: SliderProgressAnimation,
) -> Div {
    let ratio = ratio.clamp(0.0, 1.0);
    if ratio >= 1.0 || animation.duration.is_zero() {
        return slider_visual_with_group(id, 1.0, style, hover_group);
    }

    let interaction_height = horizontal_interaction_height(style);
    let thumb_hover_group = hover_group.clone();
    let rail_hover_group = hover_group.clone();
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
            // The 2px line stays anchored at the player boundary, and
            // the increased hover height grows only upward.
            layer.items_end().pb(px(1.0))
        } else {
            layer.items_center()
        };
        layer.child(animated_horizontal_track(
            ratio,
            style.track_height,
            style,
            "slider-progress-base-fill",
            animation,
        ))
    };

    let hover_track = animated_horizontal_track(
        ratio,
        style.hover_track_height,
        style,
        "slider-progress-hover-fill",
        animation,
    );
    let hover_track_layer = if style.edge_overlay {
        Some(
            div()
                .absolute()
                .inset_0()
                .opacity(0.0)
                .group_hover(rail_hover_group, |hover| hover.opacity(1.0))
                .transition(theme::hover_transition())
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .bottom(px(1.0))
                        .h(style.hover_track_height)
                        .child(hover_track),
                ),
        )
    } else {
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .opacity(0.0)
                .group_hover(rail_hover_group, |s| s.opacity(1.0))
                .transition(theme::hover_transition())
                .child(hover_track),
        )
    };

    let thumb_layer = if style.edge_overlay {
        div()
            .absolute()
            .left(px(-f32::from(half_thumb)))
            .right(half_thumb)
            .top(px(0.0))
            .h(style.interaction_height)
            .flex()
            .items_center()
            .child(div().flex_none().w_full().h(px(1.0)))
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
            .child(div().flex_none().w_full().h(px(1.0)))
            .child(thumb)
    }
    .with_animation(
        ElementId::NamedInteger(
            SharedString::new_static("slider-progress-thumb"),
            animation.epoch,
        ),
        Animation::new(animation.duration).with_property(AnimationProperty::relative_translation(
            point(ratio - 1.0, 0.0),
            point(0.0, 0.0),
        )),
        |element, _| element,
    );

    let root = div()
        .group(hover_group)
        .relative()
        .cursor_pointer()
        .h(interaction_height)
        .child(base_track_layer)
        .children(hover_track_layer)
        .child(thumb_layer);

    if style.edge_overlay {
        edge_overlay_root(root, style)
    } else {
        root
    }
}

fn slider_visual(id: ElementId, ratio: f32, style: SliderStyle) -> Div {
    let hover_group = SharedString::from(format!("slider-hover-{id}"));
    slider_visual_with_group(id, ratio, style, hover_group)
}

fn vertical_slider_visual(id: ElementId, ratio: f32, height: Pixels, style: SliderStyle) -> Div {
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

pub fn interactive_slider(
    id: impl Into<ElementId>,
    ratio: f32,
    style: SliderStyle,
    on_click: impl Fn(f32, &mut App) + 'static,
    on_drag: impl Fn(f32, &mut App) + 'static,
    on_drag_end: impl Fn(f32, &mut App) + 'static,
) -> Stateful<Div> {
    InteractiveSliderState::new(id, on_click, on_drag, on_drag_end).render(ratio, style)
}

fn interactive_slider_state(
    state: &InteractiveSliderState,
    ratio: f32,
    style: SliderStyle,
    animation: Option<SliderProgressAnimation>,
) -> Stateful<Div> {
    let id = state.id.clone();
    let drag_id = state.id.clone();
    let bounds_for_children = state.bounds.clone();
    let bounds_for_down = state.bounds.clone();
    let bounds_for_up = state.bounds.clone();
    let id_for_down = state.id.clone();
    let id_for_up = state.id.clone();
    let id_for_up_out = state.id.clone();
    let id_for_drag = state.id.clone();
    let click_for_down = if state.defer_commit_until_release {
        state.on_drag.clone()
    } else {
        state.on_click.clone()
    };
    let drag_for_up = state.on_drag_end.clone();
    let drag_for_up_out = state.on_drag_end.clone();
    let deferred_release = state.defer_commit_until_release;
    let on_drag = state.on_drag.clone();

    let visual = animation.map_or_else(
        || slider_visual_with_group(id.clone(), ratio, style, state.hover_group.clone()),
        |animation| {
            slider_visual_with_group_animated(
                id.clone(),
                ratio,
                style,
                state.hover_group.clone(),
                animation,
            )
        },
    );

    visual
        .on_children_prepainted(move |children_bounds, _window, _cx| {
            bounds_for_children.set(children_bounds.first().copied());
        })
        .id(id)
        .on_drag(
            SliderDrag {
                id: drag_id,
                axis: SliderAxis::Horizontal,
                on_change: on_drag,
            },
            |_: &SliderDrag, _, _, cx| cx.new(|_| Empty),
        )
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            let Some(bounds) = bounds_for_down.get() else {
                return;
            };
            // Hit testing can be stale after retained GPUI layer replay.
            // A play/queue/lyrics click must never become SetVolume(0).
            if !pointer_inside_bounds(event.position, bounds) {
                return;
            }
            let ratio = horizontal_ratio(event.position.x, bounds, style.thumb_size);
            cx.stop_propagation();
            begin_slider_gesture(&id_for_down, ratio, cx);
            (click_for_down)(ratio, cx);
        })
        // Leaving the 3px rail during press does not invalidate an active seek.
        // Mouse-up-out commits the sampled release position.
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            let Some(gesture) = take_slider_gesture(&id_for_up, cx) else {
                return;
            };
            cx.stop_propagation();
            // For volume the press already committed. A second release
            // callback is only needed for a real drag or a deferred seek.
            if !commit_on_release(gesture.dragged, deferred_release) {
                return;
            }
            let ratio = bounds_for_up
                .get()
                .filter(|bounds| pointer_inside_bounds(event.position, *bounds))
                .map_or(gesture.last_ratio, |bounds| {
                    horizontal_ratio(event.position.x, bounds, style.thumb_size)
                });
            (drag_for_up)(ratio, cx);
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _window, cx| {
            let Some(gesture) = take_slider_gesture(&id_for_up_out, cx) else {
                return;
            };
            cx.stop_propagation();
            if commit_on_release(gesture.dragged, deferred_release) {
                // Outside coordinates belong to another control: never
                // clamp its X to zero and mute/seek unintentionally.
                (drag_for_up_out)(gesture.last_ratio, cx);
            }
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let (drag_id, axis, on_change) = {
                let drag = event.drag(cx);
                (drag.id.clone(), drag.axis, drag.on_change.clone())
            };
            if drag_id != id_for_drag || !is_active_drag(&drag_id, cx) {
                return;
            }
            let value = match axis {
                SliderAxis::Horizontal => {
                    horizontal_ratio(event.event.position.x, event.bounds, style.thumb_size)
                }
                SliderAxis::Vertical => {
                    vertical_ratio(event.event.position.y, event.bounds, style.thumb_size)
                }
            };
            if update_slider_gesture(&drag_id, value, cx) {
                cx.stop_propagation();
                (on_change)(value, cx);
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
    let drag_id = id.clone();
    let on_change: SliderCallback = Rc::new(on_change);
    let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));

    let bounds_for_down = bounds.clone();
    let bounds_for_up = bounds.clone();
    let id_for_down = id.clone();
    let id_for_up = id.clone();
    let id_for_up_out = id.clone();
    let id_for_drag = id.clone();
    let change_for_down = on_change.clone();
    let change_for_up = on_change.clone();
    let change_for_up_out = on_change.clone();

    vertical_slider_visual(id.clone(), ratio, height, style)
        .on_children_prepainted(move |children_bounds, _window, _cx| {
            bounds.set(children_bounds.first().copied());
        })
        .id(id)
        .on_drag(
            SliderDrag {
                id: drag_id,
                axis: SliderAxis::Vertical,
                on_change: on_change.clone(),
            },
            |_: &SliderDrag, _, _, cx| cx.new(|_| Empty),
        )
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            let Some(bounds) = bounds_for_down.get() else {
                return;
            };
            if !pointer_inside_bounds(event.position, bounds) {
                return;
            }
            let ratio = vertical_ratio(event.position.y, bounds, style.thumb_size);
            cx.stop_propagation();
            begin_slider_gesture(&id_for_down, ratio, cx);
            (change_for_down)(ratio, cx);
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            let Some(gesture) = take_slider_gesture(&id_for_up, cx) else {
                return;
            };
            cx.stop_propagation();
            if !gesture.dragged {
                return;
            }
            let ratio = bounds_for_up
                .get()
                .filter(|bounds| pointer_inside_bounds(event.position, *bounds))
                .map_or(gesture.last_ratio, |bounds| {
                    vertical_ratio(event.position.y, bounds, style.thumb_size)
                });
            (change_for_up)(ratio, cx);
        })
        .on_mouse_up_out(MouseButton::Left, move |_, _window, cx| {
            let Some(gesture) = take_slider_gesture(&id_for_up_out, cx) else {
                return;
            };
            cx.stop_propagation();
            if gesture.dragged {
                (change_for_up_out)(gesture.last_ratio, cx);
            }
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let (drag_id, on_change) = {
                let drag = event.drag(cx);
                (drag.id.clone(), drag.on_change.clone())
            };
            if drag_id != id_for_drag || !is_active_drag(&drag_id, cx) {
                return;
            }
            let ratio = vertical_ratio(event.event.position.y, event.bounds, style.thumb_size);
            if update_slider_gesture(&drag_id, ratio, cx) {
                cx.stop_propagation();
                (on_change)(ratio, cx);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_control_press_cannot_start_or_complete_volume_drag() {
        let volume = Bounds::new(
            gpui::point(px(700.0), px(650.0)),
            gpui::size(px(72.0), px(22.0)),
        );
        assert!(pointer_inside_bounds(
            gpui::point(px(730.0), px(660.0)),
            volume
        ));
        assert!(!pointer_inside_bounds(
            gpui::point(px(390.0), px(660.0)),
            volume
        ));
        assert!(!pointer_inside_bounds(
            gpui::point(px(650.0), px(660.0)),
            volume
        ));
        assert!(!pointer_inside_bounds(
            gpui::point(px(730.0), px(550.0)),
            volume
        ));
        // A click already committed on mouse down; up/out must never write 0.
        assert!(!commit_on_release(false, false));
        assert!(commit_on_release(true, false));
        assert!(commit_on_release(false, true));
    }

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
        let resting_height = horizontal_interaction_height(style);
        let reveal_height = edge_reveal_height(style);
        assert_eq!(f32::from(style.track_height), 2.0);
        assert_eq!(f32::from(resting_height), 3.0);
        assert_eq!(f32::from(reveal_height), 4.0);
        assert_eq!(f32::from(style.hover_track_height), 6.0);
        // On hover expand 4px above the player, never farther below it.
        assert_eq!(f32::from(resting_height) + f32::from(reveal_height), 7.0);
        // Hover is paint-only; the resting hitbox does not resize or
        // extend into the transport controls when the visual grows.
        assert!(style.edge_overlay);
        assert_eq!(f32::from(horizontal_interaction_height(style)), 3.0);
    }

    #[test]
    fn transport_slider_defers_seek_until_release() {
        let mut slider = InteractiveSliderState::new("test-seek", |_, _| {}, |_, _| {}, |_, _| {});
        assert!(!slider.defer_commit_until_release);
        slider.defer_commit_until_release();
        assert!(slider.defer_commit_until_release);
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
