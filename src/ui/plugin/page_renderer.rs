use std::rc::Rc;

use gpui::{
    App, AnyElement, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, Window,
    div, img, prelude::*, px,
};

use crate::plugin::{
    assets,
    ui::schema::{UiNode, UiPageModel, UiSpacerSize},
};

use super::{plugin_input, theme};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginUiInteraction {
    Action { action_id: String },
    BeginInput {
        field_id: String,
        current_value: String,
        secret: bool,
    },
    SelectChanged { field_id: String, value: String },
    ToggleChanged { field_id: String, value: bool },
}

pub type PluginUiInteractionHandler =
    Rc<dyn Fn(PluginUiInteraction, &mut Window, &mut App) + 'static>;

/// Render a page without package asset context. Kept for non-plugin callers/tests; image nodes fall
/// back to their alt placeholder because paint is never allowed to resolve plugin files directly.
pub fn render_page(
    model: &UiPageModel,
    handler: Option<PluginUiInteractionHandler>,
) -> AnyElement {
    render_node(None, &model.root, handler).into_any_element()
}

/// Render one plugin page from Host-validated immutable models and already-normalized image bytes.
/// This function performs no filesystem I/O, image decoding or guest execution.
pub fn render_plugin_page(
    plugin_id: &str,
    model: &UiPageModel,
    handler: Option<PluginUiInteractionHandler>,
) -> AnyElement {
    let _input_surface = plugin_input::begin_surface(plugin_id, model);
    render_node(Some(plugin_id), &model.root, handler).into_any_element()
}

fn render_node(
    plugin_id: Option<&str>,
    node: &UiNode,
    handler: Option<PluginUiInteractionHandler>,
) -> AnyElement {
    match node {
        UiNode::Text { text } => div()
            .text_sm()
            .text_color(theme::TEXT_SECONDARY)
            .child(text.clone())
            .into_any_element(),
        UiNode::Heading { level, text } => {
            let heading = div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::TEXT_PRIMARY)
                .child(text.clone());
            match level {
                1 => heading.text_3xl().into_any_element(),
                2 => heading.text_2xl().into_any_element(),
                3 => heading.text_xl().into_any_element(),
                4 => heading.text_lg().into_any_element(),
                5 => heading.text_base().into_any_element(),
                _ => heading.text_sm().into_any_element(),
            }
        }
        UiNode::Column { children } => render_children(plugin_id, children, handler, false),
        UiNode::Row { children } => render_children(plugin_id, children, handler, true),
        UiNode::Section { title, children } => {
            let mut section = div().w_full().flex().flex_col().gap_3();
            if let Some(title) = title {
                section = section.child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::TEXT_PRIMARY)
                        .child(title.clone()),
                );
            }
            for child in children {
                section = section.child(render_node(plugin_id, child, handler.clone()));
            }
            section.into_any_element()
        }
        UiNode::Card { children } => {
            let mut card = div()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .p_4()
                .rounded_xl()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD);
            for child in children {
                card = card.child(render_node(plugin_id, child, handler.clone()));
            }
            card.into_any_element()
        }
        UiNode::List { children } => {
            let mut list = div().w_full().flex().flex_col().gap_2();
            for (index, child) in children.iter().enumerate() {
                list = list.child(
                    div()
                        .w_full()
                        .flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .w(px(22.0))
                                .flex_none()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!("{}.", index + 1)),
                        )
                        .child(div().flex_1().min_w(px(0.0)).child(render_node(
                            plugin_id,
                            child,
                            handler.clone(),
                        ))),
                );
            }
            list.into_any_element()
        }
        UiNode::Image { asset, alt } => render_image(plugin_id, asset, alt.as_deref()),
        UiNode::Button {
            label,
            action_id,
            disabled,
        } => {
            let enabled = !*disabled && handler.is_some();
            let mut button = div()
                .id(SharedString::from(format!("plugin-ui-action-{action_id}")))
                .px_4()
                .py_2()
                .rounded_lg()
                .bg(if enabled {
                    theme::ACCENT_RED.into()
                } else {
                    theme::BG_CARD.into()
                })
                .border_1()
                .border_color(if enabled {
                    theme::ACCENT_RED
                } else {
                    theme::BORDER_CARD
                })
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if enabled {
                    theme::TEXT_WHITE
                } else {
                    theme::TEXT_TERTIARY
                })
                .child(label.clone());
            if enabled {
                let action_id = action_id.clone();
                let handler = handler.expect("enabled requires interaction handler");
                button = button
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.88))
                    .active(|style| style.scale(0.98))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        handler(
                            PluginUiInteraction::Action {
                                action_id: action_id.clone(),
                            },
                            window,
                            cx,
                        );
                    });
            }
            button.into_any_element()
        }
        UiNode::Input {
            field_id,
            value,
            placeholder,
            secret,
        } => {
            let input_key = handler
                .as_ref()
                .and_then(|_| plugin_input::key_for_field(field_id));
            if let Some(active) = input_key.as_ref().and_then(plugin_input::active) {
                return div().w_full().child(active).into_any_element();
            }

            let display = if value.is_empty() {
                placeholder.clone().unwrap_or_default()
            } else if *secret {
                "•".repeat(value.chars().count().min(32))
            } else {
                value.clone()
            };
            let enabled = handler.is_some() && input_key.is_some();
            let mut input = div()
                .id(SharedString::from(format!("plugin-ui-input-{field_id}")))
                .w_full()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .text_sm()
                .text_color(if value.is_empty() {
                    theme::TEXT_TERTIARY
                } else {
                    theme::TEXT_PRIMARY
                })
                .child(display);
            if enabled {
                let key = input_key.expect("enabled input requires Host surface key");
                let field_id = field_id.clone();
                let current_value = value.clone();
                let placeholder = placeholder.clone().unwrap_or_default();
                let secret = *secret;
                let handler = handler.expect("enabled requires interaction handler");
                input = input
                    .cursor_text()
                    .hover(|style| style.border_color(theme::ACCENT_RED))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        let commit_handler = handler.clone();
                        let commit_field_id = field_id.clone();
                        plugin_input::activate(
                            key.clone(),
                            current_value.clone(),
                            placeholder.clone(),
                            secret,
                            Rc::new(move |value, window, cx| {
                                // Input and Select are both validated text-valued fields at the
                                // application boundary. Reuse the existing text field dispatch so
                                // no guest call occurs until the Host editor commits with Enter.
                                commit_handler(
                                    PluginUiInteraction::SelectChanged {
                                        field_id: commit_field_id.clone(),
                                        value,
                                    },
                                    window,
                                    cx,
                                );
                            }),
                            window,
                            cx,
                        );
                    });
            }
            input.into_any_element()
        }
        UiNode::Select {
            field_id,
            selected,
            options,
        } => {
            let current_label = selected
                .as_ref()
                .and_then(|selected| {
                    options
                        .iter()
                        .find(|option| option.value == *selected)
                        .map(|option| option.label.clone())
                })
                .unwrap_or_else(|| "请选择".into());
            let next_value = if options.is_empty() {
                None
            } else {
                let index = selected
                    .as_ref()
                    .and_then(|selected| options.iter().position(|option| option.value == *selected))
                    .map_or(0, |index| (index + 1) % options.len());
                Some(options[index].value.clone())
            };
            let enabled = handler.is_some() && next_value.is_some();
            let mut select = div()
                .id(SharedString::from(format!("plugin-ui-select-{field_id}")))
                .w_full()
                .px_3()
                .py_2p5()
                .rounded_lg()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .text_sm()
                .text_color(theme::TEXT_PRIMARY)
                .child(current_label);
            if enabled {
                let field_id = field_id.clone();
                let value = next_value.expect("enabled select has next value");
                let handler = handler.expect("enabled requires interaction handler");
                select = select
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::ACCENT_RED))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        handler(
                            PluginUiInteraction::SelectChanged {
                                field_id: field_id.clone(),
                                value: value.clone(),
                            },
                            window,
                            cx,
                        );
                    });
            }
            select.into_any_element()
        }
        UiNode::Toggle {
            field_id,
            label,
            value,
        } => {
            let enabled = handler.is_some();
            let mut toggle = div()
                .id(SharedString::from(format!("plugin-ui-toggle-{field_id}")))
                .w_full()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::TEXT_PRIMARY)
                        .child(label.clone()),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_full()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .bg(if *value {
                            theme::accent_red_muted()
                        } else {
                            theme::BG_CANVAS.into()
                        })
                        .text_color(if *value {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_TERTIARY
                        })
                        .child(if *value { "开启" } else { "关闭" }),
                );
            if enabled {
                let field_id = field_id.clone();
                let next_value = !*value;
                let handler = handler.expect("enabled requires interaction handler");
                toggle = toggle
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::ACCENT_RED))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                        handler(
                            PluginUiInteraction::ToggleChanged {
                                field_id: field_id.clone(),
                                value: next_value,
                            },
                            window,
                            cx,
                        );
                    });
            }
            toggle.into_any_element()
        }
        UiNode::Progress {
            value_basis_points,
            label,
        } => div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme::TEXT_SECONDARY)
                    .child(label.clone().unwrap_or_else(|| "进度".into())),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(theme::BG_CARD)
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT_PRIMARY)
                    .child(format!("{:.1}%", f32::from(*value_basis_points) / 100.0)),
            )
            .into_any_element(),
        UiNode::Badge { text } => div()
            .px_2()
            .py_1()
            .rounded_full()
            .bg(theme::accent_red_muted())
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::ACCENT_RED)
            .child(text.clone())
            .into_any_element(),
        UiNode::Divider => div()
            .w_full()
            .h(px(1.0))
            .bg(theme::BORDER_HAIRLINE)
            .into_any_element(),
        UiNode::Spacer { size } => div()
            .h(match size {
                UiSpacerSize::Small => px(8.0),
                UiSpacerSize::Medium => px(16.0),
                UiSpacerSize::Large => px(28.0),
            })
            .into_any_element(),
    }
}

fn render_image(plugin_id: Option<&str>, asset: &str, alt: Option<&str>) -> AnyElement {
    let image = plugin_id.and_then(|plugin_id| assets::cached_image(plugin_id, asset).ok().flatten());
    if let Some(image) = image {
        let mut container = div().w_full().flex().flex_col().gap_1p5();
        container = container.child(
            div()
                .w_full()
                .h(px(220.0))
                .rounded_xl()
                .overflow_hidden()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .child(
                    img(EncodedImageBytes::new(ImageFormat::Png, image.png))
                        .size_full()
                        .object_fit(ObjectFit::Contain),
                ),
        );
        if let Some(alt) = alt.filter(|alt| !alt.is_empty()) {
            container = container.child(
                div()
                    .text_xs()
                    .text_color(theme::TEXT_TERTIARY)
                    .child(alt.to_owned()),
            );
        }
        return container.into_any_element();
    }

    div()
        .w_full()
        .min_h(px(92.0))
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .px_4()
        .py_3()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_SECONDARY)
                .child(alt.unwrap_or("插件图片资源").to_owned()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child(asset.to_owned()),
        )
        .into_any_element()
}

fn render_children(
    plugin_id: Option<&str>,
    children: &[UiNode],
    handler: Option<PluginUiInteractionHandler>,
    row: bool,
) -> AnyElement {
    let mut container = div().w_full().flex().gap_3();
    if row {
        container = container.flex_row().flex_wrap().items_start();
    } else {
        container = container.flex_col();
    }
    for child in children {
        container = container.child(render_node(plugin_id, child, handler.clone()));
    }
    container.into_any_element()
}
