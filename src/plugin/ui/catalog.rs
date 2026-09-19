use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::plugin::host::catalog::{InstalledPlugin, PluginCatalog};

use super::{
    manifest::{PluginUiContributions, validate_contributions},
    registry::{PluginUiRegistrationDelta, PluginUiRegistry},
};

const PLUGIN_PACKAGE_FILE: &str = "plugin.toml";

#[derive(Debug, Default, Deserialize)]
struct PluginUiPackageFile {
    #[serde(default)]
    ui: PluginUiContributions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginUiLoadFailure {
    pub plugin_id: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginUiCatalogSyncReport {
    pub registered_plugins: usize,
    pub skipped_disabled_plugins: usize,
    pub routes: usize,
    pub pages: usize,
    pub commands: usize,
    pub home_sections: usize,
    pub themes: usize,
    pub failures: Vec<PluginUiLoadFailure>,
}

pub fn load_plugin_contributions(plugin: &InstalledPlugin) -> Result<PluginUiContributions> {
    let descriptor = plugin.package_dir.join(PLUGIN_PACKAGE_FILE);
    let content = fs::read_to_string(&descriptor)
        .with_context(|| format!("读取插件 UI 清单失败: {}", descriptor.display()))?;
    let package: PluginUiPackageFile = toml::from_str(&content)
        .with_context(|| format!("解析插件 UI 清单失败: {}", descriptor.display()))?;
    validate_contributions(&plugin.manifest.id, &package.ui)?;
    validate_static_assets(plugin, &package.ui)?;
    Ok(package.ui)
}

pub fn register_plugin(
    registry: &mut PluginUiRegistry,
    plugin: &InstalledPlugin,
) -> Result<PluginUiRegistrationDelta> {
    let contributions = load_plugin_contributions(plugin)?;
    registry.replace_plugin(&plugin.manifest.id, contributions)
}

pub fn sync_from_catalog(
    catalog: &PluginCatalog,
    registry: &mut PluginUiRegistry,
) -> PluginUiCatalogSyncReport {
    sync_from_catalog_filtered(catalog, registry, |_| true)
}

/// Register only enabled plugins. Disabled plugins are explicitly removed from the registry so an
/// old in-process snapshot can never keep a sidebar/settings/theme contribution alive after disable.
pub fn sync_from_catalog_filtered<F>(
    catalog: &PluginCatalog,
    registry: &mut PluginUiRegistry,
    mut is_enabled: F,
) -> PluginUiCatalogSyncReport
where
    F: FnMut(&str) -> bool,
{
    let mut report = PluginUiCatalogSyncReport::default();
    for plugin in catalog.plugins() {
        if !is_enabled(&plugin.manifest.id) {
            registry.remove_plugin(&plugin.manifest.id);
            report.skipped_disabled_plugins = report.skipped_disabled_plugins.saturating_add(1);
            continue;
        }
        match register_plugin(registry, plugin) {
            Ok(delta) => {
                report.registered_plugins = report.registered_plugins.saturating_add(1);
                report.routes = report.routes.saturating_add(delta.added_routes);
                report.pages = report.pages.saturating_add(delta.added_pages);
                report.commands = report.commands.saturating_add(delta.added_commands);
                report.home_sections = report
                    .home_sections
                    .saturating_add(delta.added_home_sections);
                report.themes = report.themes.saturating_add(delta.added_themes);
            }
            Err(error) => report.failures.push(PluginUiLoadFailure {
                plugin_id: plugin.manifest.id.clone(),
                error: format!("{error:#}"),
            }),
        }
    }
    report
}

fn validate_static_assets(
    plugin: &InstalledPlugin,
    contributions: &PluginUiContributions,
) -> Result<()> {
    for theme in &contributions.themes {
        validate_asset(
            &plugin.package_dir,
            Path::new(&theme.tokens_asset),
            "Theme token asset",
        )?;
    }
    Ok(())
}

fn validate_asset(package_dir: &Path, relative: &Path, label: &str) -> Result<()> {
    let path = package_dir.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("{label} 不存在: {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("{label} 必须是插件包内普通文件: {}", path.display());
    }
    let canonical_package = fs::canonicalize(package_dir)
        .with_context(|| format!("规范化插件目录失败: {}", package_dir.display()))?;
    let canonical_asset = fs::canonicalize(&path)
        .with_context(|| format!("规范化插件 UI asset 失败: {}", path.display()))?;
    if !canonical_asset.starts_with(&canonical_package) {
        bail!("{label} 通过路径或链接逃逸插件目录: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_without_ui_section_is_valid_empty_contribution() {
        let parsed: PluginUiPackageFile = toml::from_str("id = 'ignored'").expect("toml");
        assert!(parsed.ui.routes.is_empty());
        assert!(parsed.ui.pages.is_empty());
        assert!(parsed.ui.themes.is_empty());
    }
}
