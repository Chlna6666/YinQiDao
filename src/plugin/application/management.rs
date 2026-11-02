use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};

use super::{
    abi::{AuthMethod, PluginCapability},
    assets,
    component::gc,
    host::package_manager::PluginPackageManager,
    runtime_ports,
    ui::{
        self,
        client::{PluginUiEvent, UiFieldValue},
        manifest::UiRoutePlacement,
        registry::{self, RegisteredUiRoute},
        schema::{UiNode, UiPageModel},
    },
};

#[path = "management/zip_import.rs"]
mod zip_import;

const DEFAULT_UI_PAGE_CACHE_ENTRIES: usize = 128;
const MAX_PAGE_EVENT_TOAST_BYTES: usize = 4 * 1024;

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
pub struct PluginCandidateProvider {
    pub id: String,
    pub display_name: String,
    pub capabilities: Vec<String>,
    pub auth_methods: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginInstallStatus {
    NewInstall,
    Upgrade { current_version: String },
    SameVersion { current_version: String },
    Downgrade { current_version: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginImportCandidate {
    pub source_path: PathBuf,
    pub package_dir: PathBuf,
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub homepage: Option<String>,
    pub component_file: String,
    pub component_bytes: u64,
    pub providers: Vec<PluginCandidateProvider>,
    pub network_domains: Vec<String>,
    pub install_status: PluginInstallStatus,
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

/// 按 UI revision 缓存的来源显示名映射；列表/行渲染可每帧调用而不重复克隆目录。
pub fn provider_display_labels() -> std::collections::BTreeMap<String, String> {
    use std::sync::Mutex;
    type ProviderLabelCache = (u64, std::collections::BTreeMap<String, String>);
    static CACHE: std::sync::OnceLock<Mutex<Option<ProviderLabelCache>>> =
        std::sync::OnceLock::new();
    let revision = ui_observable_revision();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((cached_revision, labels)) = guard.as_ref()
        && *cached_revision == revision
    {
        return labels.clone();
    }
    let labels = provider_display_names().unwrap_or_default();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((revision, labels.clone()));
    }
    labels
}

/// 多来源 UI：provider_id → 来源显示名（来自已安装插件目录）。
/// 未安装/未知的 provider_id 由调用方回退为原始 id 显示。
pub fn provider_display_names() -> Result<std::collections::BTreeMap<String, String>> {
    let Some(runtime) = super::host::runtime::global() else {
        return Ok(std::collections::BTreeMap::new());
    };
    let catalog = runtime.catalog_snapshot()?;
    let mut names = std::collections::BTreeMap::new();
    for plugin in catalog.plugins() {
        for descriptor in &plugin.manifest.providers {
            names
                .entry(descriptor.id.clone())
                .or_insert_with(|| descriptor.display_name.clone());
        }
    }
    Ok(names)
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
    let manager = manager()?;
    let (result, refresh) = runtime_ports::coordinate_plugin_change(
        || manager.import_directory(path),
        |result| Some(result.plugin_id.clone()),
    )?;
    let _ = assets::invalidate_plugin(&result.plugin_id);
    Ok(PluginImportSummary {
        plugin_id: result.plugin_id,
        version: result.version,
        updated_existing: result.updated_existing,
        enabled: result.enabled,
        ui_registered: result.ui_registered,
        provider_runtime_refresh_pending: refresh.provider_runtime_refresh_pending,
    })
}

/// Import exactly the WASM chosen during preflight. Cargo's output name does not
/// need to match the installed filename declared by plugin.toml.
pub fn import_candidate(candidate: &PluginImportCandidate) -> Result<PluginImportSummary> {
    // The source could change while the confirmation dialog is open.
    let current = inspect_plugin_file(&candidate.source_path)?;
    if current.plugin_id != candidate.plugin_id
        || current.name != candidate.name
        || current.version != candidate.version
        || current.component_file != candidate.component_file
        || current.component_bytes != candidate.component_bytes
    {
        bail!("插件文件在确认期间已更改，请重新选择并检查插件信息");
    }

    if current
        .source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        // Never import the preflight copy: the archive is extracted and verified
        // again after user confirmation, with a fresh bounded temporary directory.
        let extracted = zip_import::extract(&current.source_path)?;
        return import_directory(&extracted.package_dir);
    }
    if !current
        .source_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
    {
        return import_directory(&current.package_dir);
    }

    // Do not copy the Cargo workspace, which may include an enormous target tree.
    let staged = stage_selected_wasm(&current)?;
    import_directory(&staged.package_dir)
}

/// Normalize the selected compiled WASM and its manifest without modifying build outputs.
fn stage_selected_wasm(candidate: &PluginImportCandidate) -> Result<StagedPluginDirectory> {
    if !super::host::catalog::valid_identifier(&candidate.plugin_id) {
        bail!("插件清单中的 id 非法");
    }
    let relative_component = Path::new(&candidate.component_file);
    if relative_component.is_absolute()
        || relative_component.as_os_str().is_empty()
        || relative_component.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || relative_component.extension().and_then(|ext| ext.to_str()) != Some("wasm")
    {
        bail!("插件清单中的 component 路径无效");
    }

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "yinqidao-plugin-import-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&root).with_context(|| format!("创建临时插件目录失败: {}", root.display()))?;
    let package_dir = root.join(&candidate.plugin_id);
    let staged = StagedPluginDirectory { root, package_dir };
    let component_destination = staged.package_dir.join(relative_component);
    if let Some(parent) = component_destination.parent() {
        fs::create_dir_all(parent).context("创建临时组件目录失败")?;
    }
    fs::copy(
        candidate
            .package_dir
            .join(super::host::catalog::PLUGIN_PACKAGE_FILE),
        staged
            .package_dir
            .join(super::host::catalog::PLUGIN_PACKAGE_FILE),
    )
    .context("复制插件清单失败")?;
    fs::copy(&candidate.source_path, component_destination).context("复制所选 WASM 组件失败")?;
    Ok(staged)
}

struct StagedPluginDirectory {
    root: PathBuf,
    package_dir: PathBuf,
}

impl Drop for StagedPluginDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn import_file(file_path: &Path) -> Result<PluginImportSummary> {
    let candidate = inspect_plugin_file(file_path)?;
    import_candidate(&candidate)
}

fn compare_versions(v1: &str, v2: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches(['v', 'V'])
            .split('.')
            .filter_map(|part| part.split(['-', '+']).next().and_then(|p| p.parse().ok()))
            .collect()
    };
    let p1 = parse(v1);
    let p2 = parse(v2);
    if !p1.is_empty() && !p2.is_empty() {
        p1.cmp(&p2)
    } else {
        v1.cmp(v2)
    }
}

fn capability_display_name(cap: &PluginCapability) -> &'static str {
    match cap {
        PluginCapability::Authentication => "账号认证",
        PluginCapability::Search => "搜索",
        PluginCapability::Metadata => "元数据",
        PluginCapability::Lyrics => "歌词",
        PluginCapability::Artwork => "封面",
        PluginCapability::Streaming => "在线播放",
        PluginCapability::Playlists => "歌单管理",
        PluginCapability::MediaCollections => "合辑/专辑",
        PluginCapability::CloudLibrary => "云音乐库",
        PluginCapability::LikeSync => "红心同步",
        PluginCapability::Recommendations => "每日推荐",
        PluginCapability::PlaybackEvents => "播放上报",
        PluginCapability::UserProfile => "用户画像",
        PluginCapability::Recognition => "听歌识曲",
    }
}

fn auth_method_display_name(method: &AuthMethod) -> &'static str {
    match method {
        AuthMethod::QrCode => "二维码扫码",
        AuthMethod::CustomForm => "手机号/密码登录",
        AuthMethod::CookieImport => "Cookie 导入",
        AuthMethod::BrowserOAuth => "网页授权 (OAuth)",
        AuthMethod::DeviceCode => "设备码授权",
    }
}

pub fn inspect_plugin_file(file_path: &Path) -> Result<PluginImportCandidate> {
    let canonical = fs::canonicalize(file_path)
        .with_context(|| format!("读取插件文件路径失败: {}", file_path.display()))?;
    if canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        // Extract only into a transient private directory, then use exactly the
        // existing manifest/component validation path, including the UI preview.
        let extracted = zip_import::extract(&canonical)?;
        let mut candidate = inspect_plugin_file(&extracted.package_dir)?;
        candidate.source_path = canonical.clone();
        candidate.package_dir = canonical;
        return Ok(candidate);
    }

    let (package_dir, manifest_path, chosen_component_path) = if canonical.is_dir() {
        let manifest = canonical.join(super::host::catalog::PLUGIN_PACKAGE_FILE);
        (canonical.clone(), manifest, None)
    } else {
        let parent = canonical
            .parent()
            .ok_or_else(|| anyhow!("无法解析插件文件所在目录"))?
            .to_path_buf();
        let extension = canonical
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_default();

        if extension == "toml" {
            (parent, canonical.clone(), None)
        } else if extension == "wasm" {
            let direct = parent.join(super::host::catalog::PLUGIN_PACKAGE_FILE);
            if direct.is_file() {
                (parent, direct, Some(canonical.clone()))
            } else {
                let mut current = parent.as_path();
                let mut found = None;
                for _ in 0..4 {
                    if let Some(p) = current.parent() {
                        let candidate = p.join(super::host::catalog::PLUGIN_PACKAGE_FILE);
                        if candidate.is_file() {
                            found = Some((p.to_path_buf(), candidate));
                            break;
                        }
                        current = p;
                    }
                }
                let (pkg_dir, manifest) = found.ok_or_else(|| {
                    anyhow!(
                        "未在所选 .wasm 文件同级或上级目录中找到 {} 清单文件。请确保插件包含清单文件与 WASM 组件。",
                        super::host::catalog::PLUGIN_PACKAGE_FILE
                    )
                })?;
                (pkg_dir, manifest, Some(canonical.clone()))
            }
        } else {
            bail!("不支持的文件类型（.{extension}）。请选择 .wasm 插件文件或 plugin.toml 清单");
        }
    };

    if !manifest_path.is_file() {
        bail!(
            "未找到插件清单 {}（查找路径: {}）",
            super::host::catalog::PLUGIN_PACKAGE_FILE,
            manifest_path.display()
        );
    }

    let manifest_content = fs::read_to_string(&manifest_path)
        .with_context(|| format!("读取插件清单失败: {}", manifest_path.display()))?;
    let package: super::host::catalog::PluginPackageFile = toml::from_str(&manifest_content)
        .with_context(|| format!("解析插件清单失败: {}", manifest_path.display()))?;

    if package.manifest.name.trim().is_empty() {
        bail!("插件清单中的 name 不能为空");
    }
    if package.manifest.version.trim().is_empty() {
        bail!("插件清单中的 version 不能为空");
    }

    // Validate component wasm file
    let component_path = package_dir.join(&package.component);
    // The explicitly selected compiled WASM wins over any stale provider.wasm.
    let resolved_component = if let Some(chosen) = chosen_component_path {
        chosen
    } else if component_path.is_file() {
        component_path
    } else {
        bail!(
            "找不到清单声明的插件组件: {}（查找路径: {}）",
            package.component,
            component_path.display()
        );
    };

    let comp_meta = fs::metadata(&resolved_component)
        .with_context(|| format!("读取插件组件失败: {}", resolved_component.display()))?;
    if comp_meta.len() < 4 {
        bail!("插件组件文件过小或已损坏: {}", resolved_component.display());
    }
    let mut header = [0u8; 4];
    let mut f = fs::File::open(&resolved_component)
        .with_context(|| format!("打开插件组件失败: {}", resolved_component.display()))?;
    f.read_exact(&mut header)
        .with_context(|| "读取插件组件魔数失败")?;
    if &header != b"\0asm" {
        bail!("所选文件不是有效的 WebAssembly 组件二进制（缺少 WASM 魔数）");
    }

    let installed = list_installed().unwrap_or_default();
    let existing = installed
        .iter()
        .find(|p| p.plugin_id == package.manifest.id);
    let install_status = match existing {
        None => PluginInstallStatus::NewInstall,
        Some(installed_plugin) => {
            match compare_versions(&package.manifest.version, &installed_plugin.version) {
                std::cmp::Ordering::Equal => PluginInstallStatus::SameVersion {
                    current_version: installed_plugin.version.clone(),
                },
                std::cmp::Ordering::Greater => PluginInstallStatus::Upgrade {
                    current_version: installed_plugin.version.clone(),
                },
                std::cmp::Ordering::Less => PluginInstallStatus::Downgrade {
                    current_version: installed_plugin.version.clone(),
                },
            }
        }
    };

    let providers = package
        .manifest
        .providers
        .iter()
        .map(|p| PluginCandidateProvider {
            id: p.id.clone(),
            display_name: p.display_name.clone(),
            capabilities: p
                .capabilities
                .iter()
                .map(capability_display_name)
                .map(String::from)
                .collect(),
            auth_methods: p
                .auth_methods
                .iter()
                .map(auth_method_display_name)
                .map(String::from)
                .collect(),
        })
        .collect();

    Ok(PluginImportCandidate {
        source_path: canonical,
        package_dir,
        plugin_id: package.manifest.id,
        name: package.manifest.name,
        version: package.manifest.version,
        description: package.manifest.description,
        homepage: package.manifest.homepage,
        component_file: package.component,
        component_bytes: comp_meta.len(),
        providers,
        network_domains: package.manifest.network_domains,
        install_status,
    })
}

pub fn set_enabled(plugin_id: &str, enabled: bool) -> Result<bool> {
    let manager = manager()?;
    let refresh_id = plugin_id.to_owned();
    let (changed, _refresh) = runtime_ports::coordinate_plugin_change(
        || manager.set_enabled(plugin_id, enabled),
        move |changed| changed.then_some(refresh_id),
    )?;
    if changed && !enabled {
        let _ = assets::invalidate_plugin(plugin_id);
    }
    Ok(changed)
}

pub fn uninstall(plugin_id: &str) -> Result<bool> {
    let manager = manager()?;
    let refresh_id = plugin_id.to_owned();
    let (removed, _refresh) = runtime_ports::coordinate_plugin_change(
        || manager.uninstall(plugin_id),
        move |removed| removed.then_some(refresh_id),
    )?;
    if removed {
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

/// Monotonic Host-owned change token for retained plugin UI surfaces. This is intentionally cheap
/// and never invokes guest code; cache reads/LRU touches do not advance it.
pub fn ui_observable_revision() -> u64 {
    page_cache()
        .and_then(|cache| cache.observable_revision())
        .unwrap_or_default()
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
    let toast = validate_page_event_toast(response.toast)?;
    let model = response
        .page
        .unwrap_or_else(|| current.model.as_ref().clone());
    let snapshot = page_snapshot_to_summary(cache.publish(ticket, model)?);
    preload_snapshot_images(&snapshot).await;
    Ok(PluginPageEventResult {
        snapshot,
        toast,
        close: response.close,
    })
}

fn validate_page_event_toast(toast: Option<String>) -> Result<Option<String>> {
    if let Some(value) = toast.as_deref()
        && (value.len() > MAX_PAGE_EVENT_TOAST_BYTES || value.contains('\0'))
    {
        bail!("插件页面 event toast 超过 Host 文本限制");
    }
    Ok(toast)
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
        | UiNode::Section { children, .. } => children
            .iter()
            .find_map(|child| find_action(child, action_id)),
        _ => None,
    }
}

fn find_field(node: &UiNode, field_id: &str) -> Option<UiFieldKind> {
    match node {
        UiNode::Input {
            field_id: current, ..
        } if current == field_id => Some(UiFieldKind::Input),
        UiNode::Select {
            field_id: current, ..
        } if current == field_id => Some(UiFieldKind::Select),
        UiNode::Toggle {
            field_id: current, ..
        } if current == field_id => Some(UiFieldKind::Toggle),
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children }
        | UiNode::Section { children, .. } => children
            .iter()
            .find_map(|child| find_field(child, field_id)),
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
            field_id,
            selected: Some(selected),
            ..
        } => {
            fields.insert(field_id.clone(), UiFieldValue::Text(selected.clone()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_event_toast_is_bounded_before_publication() {
        assert_eq!(validate_page_event_toast(None).expect("none"), None);
        assert_eq!(
            validate_page_event_toast(Some("ok".into())).expect("short"),
            Some("ok".into())
        );
        assert!(validate_page_event_toast(Some("bad\0toast".into())).is_err());
        assert!(
            validate_page_event_toast(Some("x".repeat(MAX_PAGE_EVENT_TOAST_BYTES + 1))).is_err()
        );
    }

    #[test]
    fn import_file_rejects_unsupported_extension() {
        let temp = std::env::temp_dir().join(format!("yinqidao-test-file-{}", std::process::id()));
        fs::create_dir_all(&temp).expect("create temp");
        let txt = temp.join("test.txt");
        fs::write(&txt, "hello").expect("write txt");

        let err = import_file(&txt).expect_err("should reject txt");
        assert!(err.to_string().contains("不支持的文件类型"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn inspect_plugin_file_from_wasm_and_toml() {
        let temp =
            std::env::temp_dir().join(format!("yinqidao-test-inspect-{}", std::process::id()));
        fs::create_dir_all(&temp).expect("create temp");

        let toml_content = r#"
package_schema = 1
component = "provider.wasm"

id = "test-plugin"
name = "测试插件"
version = "1.0.0"
abi_version = 1
description = "这是一个测试插件"
network_domains = ["api.example.com"]

[[providers]]
id = "test-provider"
display_name = "测试服务"
capabilities = ["search", "lyrics"]
auth_methods = ["qr_code"]
"#;
        let toml_path = temp.join("plugin.toml");
        fs::write(&toml_path, toml_content).expect("write toml");

        let wasm_bytes = b"\0asm\x01\0\0\0";
        let wasm_path = temp.join("provider.wasm");
        fs::write(&wasm_path, wasm_bytes).expect("write wasm");

        // Inspect via wasm path
        let candidate_from_wasm = inspect_plugin_file(&wasm_path).expect("inspect wasm");
        assert_eq!(candidate_from_wasm.plugin_id, "test-plugin");
        assert_eq!(candidate_from_wasm.name, "测试插件");
        assert_eq!(candidate_from_wasm.version, "1.0.0");
        assert_eq!(candidate_from_wasm.description, "这是一个测试插件");
        assert_eq!(candidate_from_wasm.component_file, "provider.wasm");
        assert_eq!(candidate_from_wasm.component_bytes, 8);
        assert_eq!(candidate_from_wasm.network_domains, vec!["api.example.com"]);
        assert_eq!(candidate_from_wasm.providers.len(), 1);
        assert_eq!(candidate_from_wasm.providers[0].id, "test-provider");
        assert_eq!(candidate_from_wasm.providers[0].display_name, "测试服务");
        assert_eq!(
            candidate_from_wasm.providers[0].capabilities,
            vec!["搜索", "歌词"]
        );
        assert_eq!(
            candidate_from_wasm.providers[0].auth_methods,
            vec!["二维码扫码"]
        );

        // Inspect via toml path
        let candidate_from_toml = inspect_plugin_file(&toml_path).expect("inspect toml");
        assert_eq!(candidate_from_toml.plugin_id, "test-plugin");
        assert_eq!(candidate_from_toml.component_bytes, 8);

        // Cargo's filename is different from the installed component name.
        // The selected output must win even when a stale provider.wasm exists.
        let cargo_output = temp.join("target/wasm32-wasip2/release");
        fs::create_dir_all(&cargo_output).expect("create cargo output");
        let cargo_wasm = cargo_output.join("yinqidao_qqmusic_plugin.wasm");
        let newer_wasm = b"\0asm\x01\0\0\0extra";
        fs::write(&cargo_wasm, newer_wasm).expect("write cargo wasm");
        let renamed = inspect_plugin_file(&cargo_wasm).expect("accept compiled filename");
        assert_eq!(renamed.component_file, "provider.wasm");
        assert_eq!(renamed.component_bytes, newer_wasm.len() as u64);
        // Windows CI may return an 8.3 short temp path, whereas plugin inspection
        // canonicalizes it into an extended-length long path (\\\\?\\ prefix).
        assert_eq!(
            renamed.package_dir,
            fs::canonicalize(&temp).expect("canonical test package directory")
        );
        let staged = stage_selected_wasm(&renamed).expect("stage selected WASM");
        assert_eq!(
            fs::read(staged.package_dir.join("provider.wasm")).expect("staged component"),
            newer_wasm.to_vec()
        );
        assert!(staged.package_dir.join("plugin.toml").is_file());
        let stage_root = staged.root.clone();
        drop(staged);
        assert!(!stage_root.exists(), "temporary import must be cleaned up");

        // Corrupted wasm error check in isolated directory
        let bad_temp =
            std::env::temp_dir().join(format!("yinqidao-test-inspect-bad-{}", std::process::id()));
        fs::create_dir_all(&bad_temp).expect("create bad temp");
        let toml_corrupt = toml_content.replace("provider.wasm", "corrupt.wasm");
        fs::write(bad_temp.join("plugin.toml"), toml_corrupt).expect("write corrupt toml");
        let bad_wasm_path = bad_temp.join("corrupt.wasm");
        fs::write(&bad_wasm_path, b"not_wasm_binary").expect("write corrupt wasm");

        let err = inspect_plugin_file(&bad_wasm_path).expect_err("should reject bad wasm header");
        assert!(
            err.to_string()
                .contains("不是有效的 WebAssembly 组件二进制")
        );

        let _ = fs::remove_dir_all(temp);
        let _ = fs::remove_dir_all(bad_temp);
    }
}
