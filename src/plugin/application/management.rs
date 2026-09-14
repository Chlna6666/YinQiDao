use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    assets,
    component::gc,
    host::package_manager::PluginPackageManager,
    ui::{
        self,
        client::{PluginUiEvent, UiFieldValue},
        manifest::UiRoutePlacement,
        registry::{self, RegisteredUiRoute},
        schema::{UiNode, UiPageModel},
    },
};

const DEFAULT_UI_PAGE_CACHE_ENTRIES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSummary {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub provider_count: usize,
    pub route_count: usize,
    pub page_count: usize,
    pub theme_count: usize,
    pub network_domain_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginImportSummary {
    pub plugin_id: String,
    pub version: String,
    pub updated_existing: bool,
    pub enabled: bool,
    pub ui_registered: bool,
    pub provider_runtime_refresh_pending: bool,
}

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

#[derive(Clone, Debug)]
pub struct PluginPageSnapshot {
    pub plugin_id: String,
    pub page_id: String,
    pub revision: u64,
    pub model: Arc<UiPageModel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginFieldValue {
    Text(String),
    Bool(bool),
}

#[derive(Clone, Debug)]
pub struct PluginPageEventResult {
    pub snapshot: PluginPageSnapshot,
    pub toast: Option<String>,
    pub close: bool,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PluginGcCollectionSummary {
    pub released_memory_resources: usize,
    pub removed_disk_buckets: usize,
    pub disk_bytes_before: u64,
    pub disk_bytes_after: u64,
}

fn manager() -> Result<std::sync::Arc<PluginPackageManager>> {
    super::host::package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))
}

fn page_cache() -> Result<Arc<ui::page_cache::PluginUiPageCache>> {
    if let Some(cache) = ui::page_cache::global() {
        return Ok(cache);
    }
    ui::page_cache::initialize(DEFAULT_UI_PAGE_CACHE_ENTRIES)
}

pub fn list_installed() -> Result<Vec<PluginSummary>> {
    Ok(manager()?
        .list_installed()?
        .into_iter()
        .map(|plugin| PluginSummary {
            plugin_id: plugin.plugin_id,
            name: plugin.name,
            version: plugin.version,
            enabled: plugin.enabled,
            provider_count: plugin.provider_count,
            route_count: plugin.route_count,
            page_count: plugin.page_count,
            theme_count: plugin.theme_count,
            network_domain_count: plugin.network_domain_count,
        })
        .collect())
}

pub fn import_directory(path: &Path) -> Result<PluginImportSummary> {
    let result = manager()?.import_directory(path)?;
    if let Some(cache) = ui::page_cache::global() {
        let _ = cache.invalidate_plugin(&result.plugin_id);
    }
    let _ = assets::invalidate_plugin(&result.plugin_id);
    Ok(PluginImportSummary {
        plugin_id: result.plugin_id,
        version: result.version,
        updated_existing: result.updated_existing,
        enabled: result.enabled,
        ui_registered: result.ui_registered,
        provider_runtime_refresh_pending: result.provider_runtime_refresh_pending,
    })
}

pub fn set_enabled(plugin_id: &str, enabled: bool) -> Result<bool> {
    let changed = manager()?.set_enabled(plugin_id, enabled)?;
    if changed && !enabled {
        if let Some(cache) = ui::page_cache::global() {
            let _ = cache.invalidate_plugin(plugin_id);
        }
        let _ = assets::invalidate_plugin(plugin_id);
    }
    Ok(changed)
}

pub fn uninstall(plugin_id: &str) -> Result<bool> {
    let removed = manager()?.uninstall(plugin_id)?;
    if removed {
        if let Some(cache) = ui::page_cache::global() {
            let _ = cache.invalidate_plugin(plugin_id);
        }
        let _ = assets::invalidate_plugin(plugin_id);
    }
    Ok(removed)
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

pub fn ui_client_ready() -> bool {
    ui::client::global()
        .or_else(|| Some(ui::client::initialize()))
        .and_then(|clients| clients.is_ready().ok())
        .unwrap_or(false)
}

pub fn page_snapshot(plugin_id: &str, page_id: &str) -> Result<Option<PluginPageSnapshot>> {
    ensure_page_access(plugin_id, page_id)?;
    Ok(page_cache()?
        .get(plugin_id, page_id)?
        .map(page_snapshot_to_summary))
}

/// Load one page on an ordinary async path. The generation ticket is acquired before invoking the
/// guest, so update/uninstall/disable invalidation can make an in-flight result unpublishable.
pub async fn load_page(plugin_id: &str, page_id: &str) -> Result<PluginPageSnapshot> {
    ensure_page_access(plugin_id, page_id)?;
    let clients = ui::client::global().unwrap_or_else(ui::client::initialize);
    let client = clients
        .client()?
        .ok_or_else(|| anyhow!("插件 Component UI runtime 尚未就绪"))?;
    let cache = page_cache()?;
    let ticket = cache.begin_load(plugin_id, page_id)?;
    let model = client
        .load_page(plugin_id, page_id)
        .await
        .with_context(|| format!("加载插件页面失败: {plugin_id}/{page_id}"))?;
    let snapshot = page_snapshot_to_summary(cache.publish(ticket, model)?);
    preload_snapshot_images(&snapshot).await;
    Ok(snapshot)
}

pub async fn dispatch_action(
    plugin_id: &str,
    page_id: &str,
    action_id: &str,
) -> Result<PluginPageEventResult> {
    let current = page_snapshot(plugin_id, page_id)?
        .ok_or_else(|| anyhow!("插件页面尚未加载: {plugin_id}/{page_id}"))?;
    match find_action(&current.model.root, action_id) {
        Some(false) => {}
        Some(true) => bail!("插件 UI action 已禁用: {action_id}"),
        None => bail!("插件 UI action 不存在于当前页面: {action_id}"),
    }

    let mut fields = BTreeMap::new();
    collect_fields(&current.model.root, &mut fields);
    dispatch_event(
        plugin_id,
        page_id,
        current,
        PluginUiEvent::Action {
            action_id: action_id.to_owned(),
            fields,
        },
    )
    .await
}

pub async fn dispatch_field_changed(
    plugin_id: &str,
    page_id: &str,
    field_id: &str,
    value: PluginFieldValue,
) -> Result<PluginPageEventResult> {
    let current = page_snapshot(plugin_id, page_id)?
        .ok_or_else(|| anyhow!("插件页面尚未加载: {plugin_id}/{page_id}"))?;
    let Some(kind) = find_field(&current.model.root, field_id) else {
        bail!("插件 UI field 不存在于当前页面: {field_id}");
    };
    let value = match (kind, value) {
        (UiFieldKind::Input | UiFieldKind::Select, PluginFieldValue::Text(value)) => {
            UiFieldValue::Text(value)
        }
        (UiFieldKind::Toggle, PluginFieldValue::Bool(value)) => UiFieldValue::Bool(value),
        _ => bail!("插件 UI field value 类型不匹配: {field_id}"),
    };
    dispatch_event(
        plugin_id,
        page_id,
        current,
        PluginUiEvent::FieldChanged {
            field_id: field_id.to_owned(),
            value,
        },
    )
    .await
}

async fn dispatch_event(
    plugin_id: &str,
    page_id: &str,
    current: PluginPageSnapshot,
    event: PluginUiEvent,
) -> Result<PluginPageEventResult> {
    ensure_page_access(plugin_id, page_id)?;
    let clients = ui::client::global().unwrap_or_else(ui::client::initialize);
    let client = clients
        .client()?
        .ok_or_else(|| anyhow!("插件 Component UI runtime 尚未就绪"))?;
    let cache = page_cache()?;
    let ticket = cache.begin_update(plugin_id, page_id, current.revision)?;
    let response = client
        .handle_event(plugin_id, page_id, event)
        .await
        .with_context(|| format!("处理插件页面事件失败: {plugin_id}/{page_id}"))?;

    // Re-check Host ownership after guest execution. A disable/uninstall/update during the call must
    // fail closed even if the guest returned a seemingly valid response.
    ensure_page_access(plugin_id, page_id)?;
    let model = response
        .page
        .unwrap_or_else(|| current.model.as_ref().clone());
    let snapshot = page_snapshot_to_summary(cache.publish(ticket, model)?);
    preload_snapshot_images(&snapshot).await;
    Ok(PluginPageEventResult {
        snapshot,
        toast: response.toast,
        close: response.close,
    })
}

/// Preload static image assets away from GPUI paint. Asset failures are isolated to the image node:
/// the validated page stays usable and the renderer falls back to its alt/placeholder surface.
async fn preload_snapshot_images(snapshot: &PluginPageSnapshot) {
    let plugin_id = snapshot.plugin_id.clone();
    let task_plugin_id = plugin_id.clone();
    let model = snapshot.model.clone();
    match tokio::task::spawn_blocking(move || {
        assets::preload_page_images(&task_plugin_id, model.as_ref())
    })
    .await
    {
        Ok(Ok(report)) => {
            for failure in report.failures {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    asset = %failure.asset,
                    error = %failure.error,
                    "插件页面图片预取失败，保留占位渲染"
                );
            }
        }
        Ok(Err(error)) => tracing::warn!(
            plugin_id = %plugin_id,
            %error,
            "插件页面图片预取被 Host 资源策略拒绝"
        ),
        Err(error) => tracing::warn!(
            plugin_id = %plugin_id,
            %error,
            "插件页面图片预取任务异常退出"
        ),
    }
}

fn ensure_page_access(plugin_id: &str, page_id: &str) -> Result<()> {
    if !manager()?.is_enabled(plugin_id) {
        bail!("插件已禁用，拒绝加载 UI 页面: {plugin_id}");
    }
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    let qualified = format!("plugin:{plugin_id}/{page_id}");
    if registry.page(&qualified).is_none() {
        bail!("插件页面未注册: {qualified}");
    }
    Ok(())
}

fn page_snapshot_to_summary(snapshot: ui::page_cache::PluginUiPageSnapshot) -> PluginPageSnapshot {
    PluginPageSnapshot {
        plugin_id: snapshot.key.plugin_id,
        page_id: snapshot.key.page_id,
        revision: snapshot.revision,
        model: snapshot.model,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiFieldKind {
    Input,
    Select,
    Toggle,
}

fn find_action(node: &UiNode, action_id: &str) -> Option<bool> {
    match node {
        UiNode::Button {
            action_id: current,
            disabled,
            ..
        } if current == action_id => Some(*disabled),
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children }
        | UiNode::Section { children, .. } => {
            children.iter().find_map(|child| find_action(child, action_id))
        }
        _ => None,
    }
}

fn find_field(node: &UiNode, field_id: &str) -> Option<UiFieldKind> {
    match node {
        UiNode::Input { field_id: current, .. } if current == field_id => Some(UiFieldKind::Input),
        UiNode::Select { field_id: current, .. } if current == field_id => Some(UiFieldKind::Select),
        UiNode::Toggle { field_id: current, .. } if current == field_id => Some(UiFieldKind::Toggle),
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children }
        | UiNode::Section { children, .. } => {
            children.iter().find_map(|child| find_field(child, field_id))
        }
        _ => None,
    }
}

fn collect_fields(node: &UiNode, fields: &mut BTreeMap<String, UiFieldValue>) {
    match node {
        UiNode::Input {
            field_id, value, ..
        } => {
            fields.insert(field_id.clone(), UiFieldValue::Text(value.clone()));
        }
        UiNode::Select {
            field_id, selected, ..
        } => {
            if let Some(selected) = selected {
                fields.insert(field_id.clone(), UiFieldValue::Text(selected.clone()));
            }
        }
        UiNode::Toggle {
            field_id, value, ..
        } => {
            fields.insert(field_id.clone(), UiFieldValue::Bool(*value));
        }
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children }
        | UiNode::Section { children, .. } => {
            for child in children {
                collect_fields(child, fields);
            }
        }
        _ => {}
    }
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
pub fn collect_host_resources_now() -> Result<PluginGcCollectionSummary> {
    let stats = gc::global()
        .ok_or_else(|| anyhow!("插件 Host GC 尚未初始化"))?
        .sweep_once()?;
    if let Some(cache) = ui::page_cache::global() {
        let _ = cache.len()?;
    }
    Ok(PluginGcCollectionSummary {
        released_memory_resources: stats.released_memory_resources,
        removed_disk_buckets: stats.removed_disk_buckets,
        disk_bytes_before: stats.disk_bytes_before,
        disk_bytes_after: stats.disk_bytes_after,
    })
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
