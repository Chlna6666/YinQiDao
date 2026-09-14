use anyhow::{Result, anyhow, bail};

use super::schema::{
    UiNode, UiPageModel, UiSchemaLimits, UiSelectOption, UiSpacerSize, validate_page_model,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireUiPage {
    pub root: u32,
    pub nodes: Vec<WireUiNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireUiSelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireUiSpacerSize {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireUiNode {
    Text(String),
    Heading {
        level: u8,
        text: String,
    },
    Column(Vec<u32>),
    Row(Vec<u32>),
    Section {
        title: Option<String>,
        children: Vec<u32>,
    },
    Card(Vec<u32>),
    List(Vec<u32>),
    Image {
        asset: String,
        alt: Option<String>,
    },
    Button {
        label: String,
        action_id: String,
        disabled: bool,
    },
    Input {
        field_id: String,
        value: String,
        placeholder: Option<String>,
        secret: bool,
    },
    Select {
        field_id: String,
        selected: Option<String>,
        options: Vec<WireUiSelectOption>,
    },
    Toggle {
        field_id: String,
        label: String,
        value: bool,
    },
    Progress {
        value_basis_points: u16,
        label: Option<String>,
    },
    Badge(String),
    Divider,
    Spacer(WireUiSpacerSize),
}

impl WireUiPage {
    /// Convert the flat Component transport into the recursive Host semantic model.
    ///
    /// The wire graph must be a real tree: every non-root node has exactly one parent, child indices
    /// stay in bounds, every node is reachable from root, cycles are rejected, and Host depth/node
    /// budgets are enforced before recursive materialization. This prevents a small guest arena from
    /// expanding exponentially through shared subtrees.
    pub fn into_page_model(self, limits: &UiSchemaLimits) -> Result<UiPageModel> {
        validate_wire_graph(&self, limits)?;
        let root = usize::try_from(self.root).map_err(|_| anyhow!("插件 UI root index 无效"))?;
        let page = UiPageModel {
            root: build_node(root, &self.nodes)?,
        };
        validate_page_model(&page, limits)?;
        Ok(page)
    }
}

fn validate_wire_graph(page: &WireUiPage, limits: &UiSchemaLimits) -> Result<()> {
    if page.nodes.is_empty() {
        bail!("插件 UI wire page 不能为空");
    }
    if page.nodes.len() > limits.max_nodes {
        bail!("插件 UI wire node 数量超过 {}", limits.max_nodes);
    }
    let root = usize::try_from(page.root).map_err(|_| anyhow!("插件 UI root index 无效"))?;
    if root >= page.nodes.len() {
        bail!("插件 UI root index 越界: {}", page.root);
    }

    let mut indegree = vec![0_u16; page.nodes.len()];
    for (index, node) in page.nodes.iter().enumerate() {
        let children = child_indices(node);
        if children.len() > limits.max_children_per_node {
            bail!(
                "插件 UI wire node {index} children 超过 {}",
                limits.max_children_per_node
            );
        }
        for child in children {
            let child = usize::try_from(*child)
                .map_err(|_| anyhow!("插件 UI child index 无效: {child}"))?;
            if child >= page.nodes.len() {
                bail!("插件 UI child index 越界: node={index}, child={child}");
            }
            indegree[child] = indegree[child].saturating_add(1);
            if indegree[child] > 1 {
                bail!("插件 UI wire 禁止共享子树: node {child} 有多个父节点");
            }
        }
    }
    if indegree[root] != 0 {
        bail!("插件 UI root 不能被其他节点引用");
    }

    // 0 = unvisited, 1 = visiting, 2 = done. Use an explicit stack so malformed guest input cannot
    // consume the native stack before the Host depth budget rejects it.
    let mut state = vec![0_u8; page.nodes.len()];
    let mut stack = vec![(root, 1_usize, false)];
    let mut reached = 0_usize;
    while let Some((index, depth, exiting)) = stack.pop() {
        if exiting {
            if state[index] == 1 {
                state[index] = 2;
                reached += 1;
            }
            continue;
        }
        if depth > limits.max_depth {
            bail!("插件 UI wire tree 深度超过 {}", limits.max_depth);
        }
        match state[index] {
            1 => bail!("插件 UI wire 检测到环: node {index}"),
            2 => continue,
            _ => {}
        }
        state[index] = 1;
        stack.push((index, depth, true));
        for child in child_indices(&page.nodes[index]).iter().rev() {
            let child = usize::try_from(*child)
                .map_err(|_| anyhow!("插件 UI child index 无效: {child}"))?;
            stack.push((child, depth + 1, false));
        }
    }
    if reached != page.nodes.len() {
        bail!(
            "插件 UI wire 包含不可达节点: reached={reached}, total={}",
            page.nodes.len()
        );
    }
    Ok(())
}

fn child_indices(node: &WireUiNode) -> &[u32] {
    match node {
        WireUiNode::Column(children)
        | WireUiNode::Row(children)
        | WireUiNode::Card(children)
        | WireUiNode::List(children) => children,
        WireUiNode::Section { children, .. } => children,
        _ => &[],
    }
}

fn build_node(index: usize, nodes: &[WireUiNode]) -> Result<UiNode> {
    let node = nodes
        .get(index)
        .ok_or_else(|| anyhow!("插件 UI node index 越界: {index}"))?;
    Ok(match node {
        WireUiNode::Text(text) => UiNode::Text { text: text.clone() },
        WireUiNode::Heading { level, text } => UiNode::Heading {
            level: *level,
            text: text.clone(),
        },
        WireUiNode::Column(children) => UiNode::Column {
            children: build_children(children, nodes)?,
        },
        WireUiNode::Row(children) => UiNode::Row {
            children: build_children(children, nodes)?,
        },
        WireUiNode::Section { title, children } => UiNode::Section {
            title: title.clone(),
            children: build_children(children, nodes)?,
        },
        WireUiNode::Card(children) => UiNode::Card {
            children: build_children(children, nodes)?,
        },
        WireUiNode::List(children) => UiNode::List {
            children: build_children(children, nodes)?,
        },
        WireUiNode::Image { asset, alt } => UiNode::Image {
            asset: asset.clone(),
            alt: alt.clone(),
        },
        WireUiNode::Button {
            label,
            action_id,
            disabled,
        } => UiNode::Button {
            label: label.clone(),
            action_id: action_id.clone(),
            disabled: *disabled,
        },
        WireUiNode::Input {
            field_id,
            value,
            placeholder,
            secret,
        } => UiNode::Input {
            field_id: field_id.clone(),
            value: value.clone(),
            placeholder: placeholder.clone(),
            secret: *secret,
        },
        WireUiNode::Select {
            field_id,
            selected,
            options,
        } => UiNode::Select {
            field_id: field_id.clone(),
            selected: selected.clone(),
            options: options
                .iter()
                .map(|option| UiSelectOption {
                    value: option.value.clone(),
                    label: option.label.clone(),
                })
                .collect(),
        },
        WireUiNode::Toggle {
            field_id,
            label,
            value,
        } => UiNode::Toggle {
            field_id: field_id.clone(),
            label: label.clone(),
            value: *value,
        },
        WireUiNode::Progress {
            value_basis_points,
            label,
        } => UiNode::Progress {
            value_basis_points: *value_basis_points,
            label: label.clone(),
        },
        WireUiNode::Badge(text) => UiNode::Badge { text: text.clone() },
        WireUiNode::Divider => UiNode::Divider,
        WireUiNode::Spacer(size) => UiNode::Spacer {
            size: match size {
                WireUiSpacerSize::Small => UiSpacerSize::Small,
                WireUiSpacerSize::Medium => UiSpacerSize::Medium,
                WireUiSpacerSize::Large => UiSpacerSize::Large,
            },
        },
    })
}

fn build_children(children: &[u32], nodes: &[WireUiNode]) -> Result<Vec<UiNode>> {
    children
        .iter()
        .map(|child| {
            let index = usize::try_from(*child)
                .map_err(|_| anyhow!("插件 UI child index 无效: {child}"))?;
            build_node(index, nodes)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_tree_converts_to_semantic_page() {
        let page = WireUiPage {
            root: 0,
            nodes: vec![
                WireUiNode::Column(vec![1, 2]),
                WireUiNode::Heading {
                    level: 2,
                    text: "Demo".into(),
                },
                WireUiNode::Toggle {
                    field_id: "enabled".into(),
                    label: "Enabled".into(),
                    value: true,
                },
            ],
        };
        let page = page
            .into_page_model(&UiSchemaLimits::default())
            .expect("convert");
        let UiNode::Column { children } = page.root else {
            panic!("expected column");
        };
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn shared_subtree_is_rejected_before_expansion() {
        let page = WireUiPage {
            root: 0,
            nodes: vec![
                WireUiNode::Column(vec![1, 2]),
                WireUiNode::Row(vec![3]),
                WireUiNode::Row(vec![3]),
                WireUiNode::Text("shared".into()),
            ],
        };
        assert!(page.into_page_model(&UiSchemaLimits::default()).is_err());
    }

    #[test]
    fn unreachable_cycle_is_rejected() {
        let page = WireUiPage {
            root: 0,
            nodes: vec![
                WireUiNode::Text("root".into()),
                WireUiNode::Column(vec![2]),
                WireUiNode::Column(vec![1]),
            ],
        };
        assert!(page.into_page_model(&UiSchemaLimits::default()).is_err());
    }

    #[test]
    fn out_of_bounds_child_is_rejected() {
        let page = WireUiPage {
            root: 0,
            nodes: vec![WireUiNode::Column(vec![99])],
        };
        assert!(page.into_page_model(&UiSchemaLimits::default()).is_err());
    }
}
