use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::manifest::{validate_local_id, validate_relative_asset_path};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPageModel {
    pub root: UiNode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiSelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSpacerSize {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiNode {
    Text {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    Column {
        children: Vec<UiNode>,
    },
    Row {
        children: Vec<UiNode>,
    },
    Section {
        title: Option<String>,
        children: Vec<UiNode>,
    },
    Card {
        children: Vec<UiNode>,
    },
    List {
        children: Vec<UiNode>,
    },
    Image {
        asset: String,
        alt: Option<String>,
    },
    Button {
        label: String,
        action_id: String,
        #[serde(default)]
        disabled: bool,
    },
    Input {
        field_id: String,
        value: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        secret: bool,
    },
    Select {
        field_id: String,
        selected: Option<String>,
        options: Vec<UiSelectOption>,
    },
    Toggle {
        field_id: String,
        label: String,
        value: bool,
    },
    Progress {
        /// 0..=10_000 maps to 0..=100% without introducing floating-point ABI ambiguity.
        value_basis_points: u16,
        label: Option<String>,
    },
    Badge {
        text: String,
    },
    Divider,
    Spacer {
        size: UiSpacerSize,
    },
}

#[derive(Clone, Debug)]
pub struct UiSchemaLimits {
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_children_per_node: usize,
    pub max_total_text_bytes: usize,
    pub max_single_text_bytes: usize,
    pub max_select_options: usize,
}

impl Default for UiSchemaLimits {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_nodes: 2_048,
            max_children_per_node: 512,
            max_total_text_bytes: 1024 * 1024,
            max_single_text_bytes: 64 * 1024,
            max_select_options: 256,
        }
    }
}

#[derive(Default)]
struct ValidationState {
    nodes: usize,
    text_bytes: usize,
}

pub fn validate_page_model(page: &UiPageModel, limits: &UiSchemaLimits) -> Result<()> {
    if limits.max_depth == 0 || limits.max_nodes == 0 || limits.max_children_per_node == 0 {
        bail!("插件 UI schema limits 不能为 0");
    }
    let mut state = ValidationState::default();
    validate_node(&page.root, 1, limits, &mut state)
}

fn validate_node(
    node: &UiNode,
    depth: usize,
    limits: &UiSchemaLimits,
    state: &mut ValidationState,
) -> Result<()> {
    if depth > limits.max_depth {
        bail!("插件 UI page tree 深度超过 {}", limits.max_depth);
    }
    state.nodes = state.nodes.saturating_add(1);
    if state.nodes > limits.max_nodes {
        bail!("插件 UI page node 数量超过 {}", limits.max_nodes);
    }

    match node {
        UiNode::Text { text } | UiNode::Badge { text } => record_text(text, limits, state)?,
        UiNode::Heading { level, text } => {
            if !(1..=6).contains(level) {
                bail!("插件 UI heading level 必须在 1..=6");
            }
            record_text(text, limits, state)?;
        }
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children } => {
            validate_children(children, depth, limits, state)?;
        }
        UiNode::Section { title, children } => {
            if let Some(title) = title {
                record_text(title, limits, state)?;
            }
            validate_children(children, depth, limits, state)?;
        }
        UiNode::Image { asset, alt } => {
            validate_relative_asset_path(asset)?;
            if let Some(alt) = alt {
                record_text(alt, limits, state)?;
            }
        }
        UiNode::Button {
            label,
            action_id,
            ..
        } => {
            record_text(label, limits, state)?;
            validate_local_id(action_id, "action id")?;
        }
        UiNode::Input {
            field_id,
            value,
            placeholder,
            ..
        } => {
            validate_local_id(field_id, "field id")?;
            record_text(value, limits, state)?;
            if let Some(placeholder) = placeholder {
                record_text(placeholder, limits, state)?;
            }
        }
        UiNode::Select {
            field_id,
            selected,
            options,
        } => {
            validate_local_id(field_id, "field id")?;
            if options.len() > limits.max_select_options {
                bail!("插件 UI select options 超过 {}", limits.max_select_options);
            }
            if let Some(selected) = selected {
                record_text(selected, limits, state)?;
            }
            for option in options {
                record_text(&option.value, limits, state)?;
                record_text(&option.label, limits, state)?;
            }
        }
        UiNode::Toggle {
            field_id, label, ..
        } => {
            validate_local_id(field_id, "field id")?;
            record_text(label, limits, state)?;
        }
        UiNode::Progress {
            value_basis_points,
            label,
        } => {
            if *value_basis_points > 10_000 {
                bail!("插件 UI progress value 超过 100%");
            }
            if let Some(label) = label {
                record_text(label, limits, state)?;
            }
        }
        UiNode::Divider | UiNode::Spacer { .. } => {}
    }
    Ok(())
}

fn validate_children(
    children: &[UiNode],
    depth: usize,
    limits: &UiSchemaLimits,
    state: &mut ValidationState,
) -> Result<()> {
    if children.len() > limits.max_children_per_node {
        bail!(
            "插件 UI 单节点 children 超过 {}",
            limits.max_children_per_node
        );
    }
    for child in children {
        validate_node(child, depth + 1, limits, state)?;
    }
    Ok(())
}

fn record_text(value: &str, limits: &UiSchemaLimits, state: &mut ValidationState) -> Result<()> {
    if value.len() > limits.max_single_text_bytes || value.contains('\0') {
        bail!("插件 UI 单字段文本超过限制或包含 NUL");
    }
    state.text_bytes = state.text_bytes.saturating_add(value.len());
    if state.text_bytes > limits.max_total_text_bytes {
        bail!("插件 UI page 文本总量超过限制");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_tree_is_rejected() {
        let mut node = UiNode::Text { text: "ok".into() };
        for _ in 0..4 {
            node = UiNode::Column {
                children: vec![node],
            };
        }
        let limits = UiSchemaLimits {
            max_depth: 3,
            ..UiSchemaLimits::default()
        };
        assert!(validate_page_model(&UiPageModel { root: node }, &limits).is_err());
    }

    #[test]
    fn action_ids_are_validated() {
        let page = UiPageModel {
            root: UiNode::Button {
                label: "Run".into(),
                action_id: "../escape".into(),
                disabled: false,
            },
        };
        assert!(validate_page_model(&page, &UiSchemaLimits::default()).is_err());
    }
}
