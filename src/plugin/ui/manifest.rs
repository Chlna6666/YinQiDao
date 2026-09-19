use std::{
    collections::HashSet,
    path::{Component, Path},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const UI_CONTRIBUTION_SCHEMA_VERSION: u32 = 1;

const MAX_ROUTES: usize = 64;
const MAX_PAGES: usize = 64;
const MAX_COMMANDS: usize = 128;
const MAX_HOME_SECTIONS: usize = 32;
const MAX_THEMES: usize = 32;
const MAX_LABEL_BYTES: usize = 256;
const MAX_ICON_BYTES: usize = 128;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginUiContributions {
    #[serde(default)]
    pub routes: Vec<UiRouteContribution>,
    #[serde(default)]
    pub pages: Vec<UiPageContribution>,
    #[serde(default)]
    pub commands: Vec<UiCommandContribution>,
    #[serde(default)]
    pub home_sections: Vec<UiHomeSectionContribution>,
    #[serde(default)]
    pub themes: Vec<UiThemeContribution>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiRoutePlacement {
    Sidebar,
    Settings,
    Home,
    #[default]
    Hidden,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiRouteContribution {
    pub id: String,
    pub title: String,
    pub page_id: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub placement: UiRoutePlacement,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub required_provider_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPageContribution {
    pub id: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCommandPlacement {
    CommandPalette,
    TrackContext,
    PlaylistContext,
    PageLocal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiCommandContribution {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub placements: Vec<UiCommandPlacement>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiHomeSectionContribution {
    pub id: String,
    pub title: String,
    /// Declarative plugin page rendered inside this Home section. Home content therefore reuses the
    /// same bounded `UiPageModel` runtime and does not create a second guest rendering protocol.
    pub page_id: String,
    #[serde(default)]
    pub order: i32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiThemeContribution {
    pub id: String,
    pub display_name: String,
    /// Relative package path to a static TOML/JSON semantic-token file.
    pub tokens_asset: String,
}

pub fn validate_contributions(
    plugin_id: &str,
    contributions: &PluginUiContributions,
) -> Result<()> {
    validate_namespace_id(plugin_id, "plugin id")?;
    if contributions.routes.len() > MAX_ROUTES {
        bail!("插件 UI route 数量超过 {MAX_ROUTES}");
    }
    if contributions.pages.len() > MAX_PAGES {
        bail!("插件 UI page 数量超过 {MAX_PAGES}");
    }
    if contributions.commands.len() > MAX_COMMANDS {
        bail!("插件 UI command 数量超过 {MAX_COMMANDS}");
    }
    if contributions.home_sections.len() > MAX_HOME_SECTIONS {
        bail!("插件 UI home section 数量超过 {MAX_HOME_SECTIONS}");
    }
    if contributions.themes.len() > MAX_THEMES {
        bail!("插件 UI theme 数量超过 {MAX_THEMES}");
    }

    let mut page_ids = HashSet::new();
    for page in &contributions.pages {
        validate_local_id(&page.id, "page id")?;
        validate_label(&page.title, "page title")?;
        if !page_ids.insert(page.id.as_str()) {
            bail!("插件 UI page id 重复: {}", page.id);
        }
    }

    let mut route_ids = HashSet::new();
    for route in &contributions.routes {
        validate_local_id(&route.id, "route id")?;
        validate_local_id(&route.page_id, "route page id")?;
        validate_label(&route.title, "route title")?;
        if let Some(icon) = route.icon.as_deref()
            && (icon.is_empty() || icon.len() > MAX_ICON_BYTES)
        {
            bail!("插件 UI route icon 非法: {}", route.id);
        }
        if let Some(provider_id) = route.required_provider_id.as_deref() {
            validate_namespace_id(provider_id, "required provider id")?;
        }
        if !page_ids.contains(route.page_id.as_str()) {
            bail!(
                "插件 UI route {} 引用了不存在的 page {}",
                route.id,
                route.page_id
            );
        }
        if !route_ids.insert(route.id.as_str()) {
            bail!("插件 UI route id 重复: {}", route.id);
        }
    }

    let mut command_ids = HashSet::new();
    for command in &contributions.commands {
        validate_local_id(&command.id, "command id")?;
        validate_label(&command.title, "command title")?;
        let unique = command.placements.iter().copied().collect::<HashSet<_>>();
        if unique.len() != command.placements.len() {
            bail!("插件 UI command placement 重复: {}", command.id);
        }
        if !command_ids.insert(command.id.as_str()) {
            bail!("插件 UI command id 重复: {}", command.id);
        }
    }

    let mut section_ids = HashSet::new();
    for section in &contributions.home_sections {
        validate_local_id(&section.id, "home section id")?;
        validate_local_id(&section.page_id, "home section page id")?;
        validate_label(&section.title, "home section title")?;
        if !page_ids.contains(section.page_id.as_str()) {
            bail!(
                "插件 UI home section {} 引用了不存在的 page {}",
                section.id,
                section.page_id
            );
        }
        if !section_ids.insert(section.id.as_str()) {
            bail!("插件 UI home section id 重复: {}", section.id);
        }
    }

    let mut theme_ids = HashSet::new();
    for theme in &contributions.themes {
        validate_local_id(&theme.id, "theme id")?;
        validate_label(&theme.display_name, "theme display name")?;
        validate_relative_asset_path(&theme.tokens_asset)?;
        let extension = Path::new(&theme.tokens_asset)
            .extension()
            .and_then(|value| value.to_str());
        if !matches!(extension, Some("toml" | "json")) {
            bail!(
                "插件 Theme tokens 仅支持 .toml/.json: {}",
                theme.tokens_asset
            );
        }
        if !theme_ids.insert(theme.id.as_str()) {
            bail!("插件 UI theme id 重复: {}", theme.id);
        }
    }

    Ok(())
}

pub fn qualified_ui_id(plugin_id: &str, local_id: &str) -> Result<String> {
    validate_namespace_id(plugin_id, "plugin id")?;
    validate_local_id(local_id, "UI local id")?;
    Ok(format!("plugin:{plugin_id}/{local_id}"))
}

pub(super) fn validate_local_id(value: &str, label: &str) -> Result<()> {
    validate_namespace_id(value, label)
}

pub(crate) fn validate_relative_asset_path(value: &str) -> Result<()> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > 512
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("插件 UI asset 必须是 package 内相对路径: {value:?}");
    }
    Ok(())
}

fn validate_namespace_id(value: &str, label: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('.')
        || value.ends_with('.')
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'_')
        })
    {
        bail!("插件 UI {label} 非法: {value:?}");
    }
    Ok(())
}

fn validate_label(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > MAX_LABEL_BYTES || value.contains('\0') {
        bail!("插件 UI {label} 非法");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PluginUiContributions {
        PluginUiContributions {
            pages: vec![UiPageContribution {
                id: "library".into(),
                title: "Library".into(),
            }],
            routes: vec![UiRouteContribution {
                id: "library".into(),
                title: "Library".into(),
                page_id: "library".into(),
                icon: None,
                placement: UiRoutePlacement::Sidebar,
                order: 0,
                required_provider_id: None,
            }],
            ..PluginUiContributions::default()
        }
    }

    #[test]
    fn route_must_reference_declared_page() {
        let mut contributions = sample();
        contributions.routes[0].page_id = "missing".into();
        assert!(validate_contributions("plugin.test", &contributions).is_err());
    }

    #[test]
    fn home_section_must_reference_declared_page() {
        let mut contributions = sample();
        contributions.home_sections.push(UiHomeSectionContribution {
            id: "daily".into(),
            title: "Daily".into(),
            page_id: "missing".into(),
            order: 0,
        });
        assert!(validate_contributions("plugin.test", &contributions).is_err());
        contributions.home_sections[0].page_id = "library".into();
        assert!(validate_contributions("plugin.test", &contributions).is_ok());
    }

    #[test]
    fn ids_are_plugin_namespaced() {
        assert_eq!(
            qualified_ui_id("plugin.test", "library").expect("qualified"),
            "plugin:plugin.test/library"
        );
    }

    #[test]
    fn theme_asset_cannot_escape_package() {
        let mut contributions = sample();
        contributions.themes.push(UiThemeContribution {
            id: "night".into(),
            display_name: "Night".into(),
            tokens_asset: "../theme.toml".into(),
        });
        assert!(validate_contributions("plugin.test", &contributions).is_err());
    }
}
