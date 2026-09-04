use gpui::{Div, ElementId, IntoElement, Stateful, div, prelude::*, px};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImmersionDockDirection {
    Top,
    Bottom,
}

/// 沉浸浮岛容器：支持透明度渐变与天地微位移滑入滑出
pub fn immersion_dock(
    id: impl Into<ElementId>,
    visibility: f32,
    direction: ImmersionDockDirection,
    max_offset: f32,
    child: impl IntoElement,
) -> Stateful<Div> {
    let v = visibility.clamp(0.0, 1.0);
    let offset = (1.0 - v) * max_offset;

    let y_offset = match direction {
        ImmersionDockDirection::Top => -offset,
        ImmersionDockDirection::Bottom => offset,
    };

    div()
        .id(id.into())
        .opacity(v)
        .top(px(y_offset))
        .child(child)
}
