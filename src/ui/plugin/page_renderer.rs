use std::rc::Rc;

use gpui::{
    App, AnyElement, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, Window,
    div, img, prelude::*, px,
};

use crate::plugin::{
    assets,
    ui::schema::{UiNode, UiPageModel, UiSpacerSize},
};

use super::{plugin_input, plugin_theme};

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
    let palette = plugin_theme::PluginPageTheme::host();
    render_node(None, &model.root, handler, &palette).into_any_element()
}

/// Render one plugin page with the current explicitly selected scoped Theme, or the Host palette
/// when no valid selection exists. `page_id + revision` comes from the Host page-cache snapshot and
/// gives input editors a stable identity independent of the `UiPageModel` allocation address.
pub fn render_plugin_page(
    plugin_id: &str,
    page_id: &str,
    revision: u64,
    model: &UiPageModel,
    handler: Option<PluginUiInteractionHandler>,
) -> AnyElement {
    let active_theme = plugin_theme::active_palette();
    render_plugin_page_with_theme(
        plugin_id,
        page_id,
        revision,
        model,
        handler,
        active_theme.as_ref(),
    )
}

/// Render one plugin page with an optional pre-parsed scoped Theme.
///
/// This path performs no filesystem I/O, Theme parsing, registry locking, image decoding or guest
/// execution. The palette is copied once at the render root and passed through the declarative tree.
pub fn render_plugin_page_with_theme(
    plugin_id: &str,
    page_id: &str,
    revision: u64,
    model: &UiPageModel,
    handler: Option<PluginUiInteractionHandler>,
    scoped_theme: Option<&plugin_theme::PluginPageTheme>,
) -> AnyElement {
    let _input_surface = plugin_input::begin_surface(plugin_id, page_id, revision);
    let themed = scoped_theme.is_some();
    let palette = scoped_theme
        .copied()
        .unwrap_or_else(plugin_theme::PluginPageTheme::host);
    let body = render_node(Some(plugin_id), &model.root, handler, &palette);

    if themed {
        div()
            .w_full()
            .p_4()
            .rounded(px(palette.radius_large))
            .bg(palette.background)
            .child(body)
            .into_any_element()
    } else {
        body.into_any_element()
    }
}

fn render_node(
    plugin_id: Option<&str>,
    node: &UiNode,
    handler: Option<PluginUiInteractionHandler>,
    palette: &plugin_theme::PluginPageTheme,
) -> AnyElement {
    match node {
        UiNode::Text { text } => div()
            .text_sm()
            .text_color(palette.text_secondary)
            .child(text.clone())
            .into_any_element(),
        UiNode::Heading { level, text } => {
            let heading = div()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(palette.text_primary)
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
        UiNode::Column { children } => {
            render_children(plugin_id, children, handler, false, palette)
        }
        UiNode::Row { children } => render_children(plugin_id, children, handler, true, palette),
        UiNode::Section { title, children } => {
            let mut section = div().w_full().flex().flex_col().gap_3();
            if let Some(title) = title {
                section = section.child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(palette.text_primary)
                        .child(title.clone()),
                );
            }
            for child in children {
                section = section.child(render_node(plugin_id, child, handler.clone(), palette));
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
                .rounded(px(palette.radius_large))
                .bg(palette.surface_elevated)
                .border_1()
                .border_color(palette.border);
            for child in children {
                card = card.child(render_node(plugin_id, child, handler.clone(), palette));
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
                                .text_color(palette.text_tertiary)
                                .child(format!("{}.", index + 1)),
                        )
                        .child(div().flex_1().min_w(px(0.0)).child(render_node(
                            plugin_id,
                            child,
                            handler.clone(),
                            palette,
                        ))),
                );
            }
            list.into_any_element()
        }
        UiNode::Image { asset, alt } => {
            render_image(plugin_id, asset, alt.as_deref(), palette)
        }
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
                .rounded(px(palette.radius_medium))
                .bg(if enabled {
                    palette.accent
                } else {
                    palette.surface
                })
                .border_1()
                .border_color(if enabled {
                    palette.accent
                } else {
                    palette.border
                })
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if enabled {
                    palette.accent_foreground()
                } else {
                    palette.text_tertiary
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
                .rounded(px(palette.radius_medium))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .text_sm()
                .text_color(if value.is_empty() {
                    palette.text_tertiary
                } else {
                    palette.text_primary
                })
                .child(display);
            if enabled {
                let key = input_key.expect("enabled input requires Host surface key");
                let field_id = field_id.clone();
                let current_value = value.clone();
                let placeholder = placeholder.clone().unwrap_or_default();
                let secret = *secret;
                let handler = handler.expect("enabled requires interaction handler");
                let accent = palette.accent;
                input = input
                    .cursor_text()
                    .hover(move |style| style.border_color(accent))
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
                .rounded(px(palette.radius_medium))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .text_sm()
                .text_color(palette.text_primary)
                .child(current_label);
            if enabled {
                let field_id = field_id.clone();
                let value = next_value.expect("enabled select has next value");
                let handler = handler.expect("enabled requires interaction handler");
                let accent = palette.accent;
                select = select
                    .cursor_pointer()
                    .hover(move |style| style.border_color(accent))
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
                .rounded(px(palette.radius_medium))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .child(
                    div()
                        .text_sm()
                        .text_color(palette.text_primary)
                        .child(label.clone()),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(px(palette.radius_small))
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .bg(if *value {
                            palette.accent_muted()
                        } else {
                            palette.background
                        })
                        .text_color(if *value {
                            palette.accent
                        } else {
                            palette.text_tertiary
                        })
                        .child(if *value { "开启" } else { "关闭" }),
                );
            if enabled {
                let field_id = field_id.clone();
                let next_value = !*value;
                let handler = handler.expect("enabled requires interaction handler");
                let accent = palette.accent;
                toggle = toggle
                    .cursor_pointer()
                    .hover(move |style| style.border_color(accent))
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
                    .text_color(palette.text_secondary)
                    .child(label.clone().unwrap_or_else(|| "进度".into())),
            )
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(palette.radius_small))
                    .bg(palette.surface)
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(palette.text_primary)
                    .child(format!("{:.1}%", f32::from(*value_basis_points) / 100.0)),
            )
            .into_any_element(),
        UiNode::Badge { text } => div()
            .px_2()
            .py_1()
            .rounded(px(palette.radius_small))
            .bg(palette.accent_muted())
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(palette.accent)
            .child(text.clone())
            .into_any_element(),
        UiNode::Divider => div()
            .w_full()
            .h(px(1.0))
            .bg(palette.border)
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

fn render_image(
    plugin_id: Option<&str>,
    asset: &str,
    alt: Option<&str>,
    palette: &plugin_theme::PluginPageTheme,
) -> AnyElement {
    let image = plugin_id.and_then(|plugin_id| assets::cached_image(plugin_id, asset).ok().flatten());
    if let Some(image) = image {
        let mut container = div().w_full().flex().flex_col().gap_1p5();
        container = container.child(
            div()
                .w_full()
                .h(px(220.0))
                .rounded(px(palette.radius_large))
                .overflow_hidden()
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
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
                    .text_color(palette.text_tertiary)
                    .child(alt.to_owned()),
            );
        }
        return container.into_any_element();
    }

    div()
        .w_full()
        .min_h(px(92.0))
        .rounded(px(palette.radius_large))
        .bg(palette.surface)
        .border_1()
        .border_color(palette.border)
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
                .text_color(palette.text_secondary)
                .child(alt.unwrap_or("插件图片资源").to_owned()),
        )
        .child(
            div()
                .text_xs()
                .text_color(palette.text_tertiary)
                .child(asset.to_owned()),
        )
        .into_any_element()
}

fn render_children(
    plugin_id: Option<&str>,
    children: &[UiNode],
    handler: Option<PluginUiInteractionHandler>,
    row: bool,
    palette: &plugin_theme::PluginPageTheme,
) -> AnyElement {
    let mut container = div().w_full().flex().gap_3();
    if row {
        container = container.flex_row().flex_wrap().items_start();
    } else {
        container = container.flex_col();
    }
    for child in children {
        container = container.child(render_node(plugin_id, child, handler.clone(), palette));
    }
    container.into_any_element()
}
