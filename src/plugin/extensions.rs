use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    host::{catalog::PluginCatalog, package_manager},
    ui::{
        manifest::UiCommandPlacement,
        registry,
        theme::{PluginThemeTokens, validate_theme_tokens},
    },
};

const MAX_THEME_TOKENS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginCommandSurface {
    CommandPalette,
    TrackContext,
    PlaylistContext,
    PageLocal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandSummary {
    pub plugin_id: String,
    pub qualified_id: String,
    pub title: String,
    pub surfaces: Vec<PluginCommandSurface>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginHomeSectionSummary {
    pub plugin_id: String,
    pub qualified_id: String,
    pub title: String,
    pub order: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginThemeSummary {
    pub plugin_id: String,
    pub qualified_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginThemeSnapshot {
    pub plugin_id: String,
    pub qualified_id: String,
    pub display_name: String,
    pub background: Option<String>,
    pub surface: Option<String>,
    pub surface_elevated: Option<String>,
    pub text_primary: Option<String>,
    pub text_secondary: Option<String>,
    pub accent: Option<String>,
    pub border: Option<String>,
    pub success: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub radius_small: Option<u16>,
    pub radius_medium: Option<u16>,
    pub radius_large: Option<u16>,
}

/// Snapshot all registered commands for one Host surface.
///
/// The registry contains only enabled, validated plugin contributions. This call does not execute
/// guest code and does not touch plugin files, so it is safe to cache from ordinary UI controller
/// code. Rendering code should consume that cached snapshot instead of taking the registry lock per
/// frame.
pub fn commands(surface: PluginCommandSurface) -> Result<Vec<PluginCommandSummary>> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    let mut commands = registry
        .commands()
        .filter(|command| {
            command
                .contribution
                .placements
                .iter()
                .any(|placement| command_surface(*placement) == surface)
        })
        .map(|command| PluginCommandSummary {
            plugin_id: command.plugin_id.clone(),
            qualified_id: command.qualified_id.clone(),
            title: command.contribution.title.clone(),
            surfaces: command
                .contribution
                .placements
                .iter()
                .copied()
                .map(command_surface)
                .collect(),
        })
        .collect::<Vec<_>>();
    commands.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then_with(|| left.qualified_id.cmp(&right.qualified_id))
    });
    Ok(commands)
}

/// Return Host-validated Home extension metadata. Dynamic section contents are intentionally not
/// loaded here; the future Home controller loads immutable page-model snapshots asynchronously.
pub fn home_sections() -> Result<Vec<PluginHomeSectionSummary>> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    let mut sections = registry
        .home_sections()
        .map(|section| PluginHomeSectionSummary {
            plugin_id: section.plugin_id.clone(),
            qualified_id: section.qualified_id.clone(),
            title: section.contribution.title.clone(),
            order: section.contribution.order,
        })
        .collect::<Vec<_>>();
    sections.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.qualified_id.cmp(&right.qualified_id))
    });
    Ok(sections)
}

pub fn themes() -> Result<Vec<PluginThemeSummary>> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    let mut themes = registry
        .themes()
        .map(|theme| PluginThemeSummary {
            plugin_id: theme.plugin_id.clone(),
            qualified_id: theme.qualified_id.clone(),
            display_name: theme.contribution.display_name.clone(),
        })
        .collect::<Vec<_>>();
    themes.sort_by(|left, right| {
        left.display_name
            .cmp(&right.display_name)
            .then_with(|| left.qualified_id.cmp(&right.qualified_id))
    });
    Ok(themes)
}

/// Load one static plugin theme on an ordinary controller/worker path.
///
/// This is intentionally Host-owned: the guest never receives a per-frame paint callback and never
/// controls theme cache lifetime. The asset path was checked during catalog registration, but it is
/// revalidated here to close the install/update TOCTOU window before the bytes are consumed.
pub fn load_theme(qualified_id: &str) -> Result<PluginThemeSnapshot> {
    let registered = {
        let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
        let registry = registry
            .read()
            .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
        registry
            .themes()
            .find(|theme| theme.qualified_id == qualified_id)
            .cloned()
            .ok_or_else(|| anyhow!("插件 Theme 未注册: {qualified_id}"))?
    };

    let manager = package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))?;
    if !manager.is_enabled(&registered.plugin_id) {
        bail!("插件已禁用，拒绝加载 Theme: {}", registered.plugin_id);
    }

    let catalog = PluginCatalog::discover(manager.plugin_root().to_path_buf());
    let plugin = catalog
        .plugin(&registered.plugin_id)
        .ok_or_else(|| anyhow!("插件包已不存在: {}", registered.plugin_id))?;
    let relative = Path::new(&registered.contribution.tokens_asset);
    let asset = plugin.package_dir.join(relative);
    let metadata = fs::symlink_metadata(&asset)
        .with_context(|| format!("读取插件 Theme asset 失败: {}", asset.display()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        bail!("插件 Theme asset 必须是普通文件且不能是符号链接");
    }
    if metadata.len() > MAX_THEME_TOKENS_BYTES {
        bail!("插件 Theme asset 超过 {} bytes Host 上限", MAX_THEME_TOKENS_BYTES);
    }

    let canonical_package = fs::canonicalize(&plugin.package_dir)
        .with_context(|| format!("规范化插件目录失败: {}", plugin.package_dir.display()))?;
    let canonical_asset = fs::canonicalize(&asset)
        .with_context(|| format!("规范化插件 Theme asset 失败: {}", asset.display()))?;
    if !canonical_asset.starts_with(&canonical_package) {
        bail!("插件 Theme asset 逃逸插件目录，已拒绝");
    }

    let content = fs::read_to_string(&canonical_asset)
        .with_context(|| format!("读取插件 Theme token 文件失败: {}", canonical_asset.display()))?;
    if content.len() as u64 > MAX_THEME_TOKENS_BYTES {
        bail!("插件 Theme token 文本超过 Host 上限");
    }
    let tokens: PluginThemeTokens = match canonical_asset
        .extension()
        .and_then(|value| value.to_str())
    {
        Some("toml") => toml::from_str(&content).context("解析插件 Theme TOML 失败")?,
        Some("json") => serde_json::from_str(&content).context("解析插件 Theme JSON 失败")?,
        _ => bail!("插件 Theme token 文件扩展名不受支持"),
    };
    validate_theme_tokens(&tokens)?;

    Ok(theme_snapshot(
        registered.plugin_id,
        registered.qualified_id,
        registered.contribution.display_name,
        tokens,
    ))
}

fn command_surface(placement: UiCommandPlacement) -> PluginCommandSurface {
    match placement {
        UiCommandPlacement::CommandPalette => PluginCommandSurface::CommandPalette,
        UiCommandPlacement::TrackContext => PluginCommandSurface::TrackContext,
        UiCommandPlacement::PlaylistContext => PluginCommandSurface::PlaylistContext,
        UiCommandPlacement::PageLocal => PluginCommandSurface::PageLocal,
    }
}

fn theme_snapshot(
    plugin_id: String,
    qualified_id: String,
    display_name: String,
    tokens: PluginThemeTokens,
) -> PluginThemeSnapshot {
    PluginThemeSnapshot {
        plugin_id,
        qualified_id,
        display_name,
        background: tokens.background,
        surface: tokens.surface,
        surface_elevated: tokens.surface_elevated,
        text_primary: tokens.text_primary,
        text_secondary: tokens.text_secondary,
        accent: tokens.accent,
        border: tokens.border,
        success: tokens.success,
        warning: tokens.warning,
        error: tokens.error,
        radius_small: tokens.radius_small,
        radius_medium: tokens.radius_medium,
        radius_large: tokens.radius_large,
    }
}
