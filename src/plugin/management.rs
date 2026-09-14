use std::path::Path;

use anyhow::{Result, anyhow};

use super::{
    component::gc,
    host::package_manager::{InstalledPluginSummary, PluginImportResult, PluginPackageManager},
    ui::{
        manifest::UiRoutePlacement,
        registry::{self, RegisteredUiRoute},
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginRouteSummary {
    pub plugin_id: String,
    pub qualified_id: String,
    pub title: String,
    pub page_id: String,
    pub icon: Option<String>,
    pub placement: UiRoutePlacement,
    pub order: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginGcOverview {
    pub sweep_seconds: u64,
    pub warm_instance_idle_seconds: u64,
    pub compiled_component_idle_seconds: u64,
    pub max_warm_instances_per_route: usize,
    pub max_warm_instances_total: usize,
    pub max_compiled_components: usize,
    pub max_disk_cache_bytes: u64,
}

fn manager() -> Result<std::sync::Arc<PluginPackageManager>> {
    super::host::package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))
}

pub fn list_installed() -> Result<Vec<InstalledPluginSummary>> {
    manager()?.list_installed()
}

pub fn import_directory(path: &Path) -> Result<PluginImportResult> {
    manager()?.import_directory(path)
}

pub fn set_enabled(plugin_id: &str, enabled: bool) -> Result<bool> {
    manager()?.set_enabled(plugin_id, enabled)
}

pub fn uninstall(plugin_id: &str) -> Result<bool> {
    manager()?.uninstall(plugin_id)
}

pub fn route_summary(qualified_id: &str) -> Result<Option<PluginRouteSummary>> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    Ok(registry.route(qualified_id).map(route_to_summary))
}

pub fn routes_for_placement(placement: UiRoutePlacement) -> Result<Vec<PluginRouteSummary>> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    let mut routes = registry
        .routes()
        .filter(|route| route.contribution.placement == placement)
        .map(route_to_summary)
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.qualified_id.cmp(&right.qualified_id))
    });
    Ok(routes)
}

pub fn sidebar_routes() -> Result<Vec<PluginRouteSummary>> {
    routes_for_placement(UiRoutePlacement::Sidebar)
}

pub fn settings_routes() -> Result<Vec<PluginRouteSummary>> {
    routes_for_placement(UiRoutePlacement::Settings)
}

pub fn gc_overview() -> Option<PluginGcOverview> {
    let gc = gc::global()?;
    let policy = gc.policy();
    Some(PluginGcOverview {
        sweep_seconds: policy.sweep_interval.as_secs(),
        warm_instance_idle_seconds: policy.warm_instance_idle_ttl.as_secs(),
        compiled_component_idle_seconds: policy.compiled_component_idle_ttl.as_secs(),
        max_warm_instances_per_route: policy.max_warm_instances_per_route,
        max_warm_instances_total: policy.max_warm_instances_total,
        max_compiled_components: policy.max_compiled_components,
        max_disk_cache_bytes: policy.max_disk_cache_bytes,
    })
}

/// Manual Host maintenance hook for the settings diagnostics surface. This never invokes guest
/// code; it only collects Host-owned idle pools and compiled-cache buckets.
pub fn collect_host_resources_now() -> Result<super::component::gc::PluginGcStats> {
    gc::global()
        .ok_or_else(|| anyhow!("插件 Host GC 尚未初始化"))?
        .sweep_once()
}

fn route_to_summary(route: &RegisteredUiRoute) -> PluginRouteSummary {
    PluginRouteSummary {
        plugin_id: route.plugin_id.clone(),
        qualified_id: route.qualified_id.clone(),
        title: route.contribution.title.clone(),
        page_id: route.contribution.page_id.clone(),
        icon: route.contribution.icon.clone(),
        placement: route.contribution.placement,
        order: route.contribution.order,
    }
}
