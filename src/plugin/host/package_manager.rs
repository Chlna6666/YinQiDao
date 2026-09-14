use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::plugin::{
    component::{gc, registry as component_registry},
    ui::{catalog as ui_catalog, registry as ui_registry},
};

use super::{catalog::PluginCatalog, permissions, runtime, sessions};

const PLUGIN_STATE_SCHEMA_VERSION: u32 = 1;
const PLUGIN_STATE_FILE: &str = "plugin-state.json";
const IMPORT_STAGING_DIR: &str = "plugin-import-staging";
const IMPORT_TRASH_DIR: &str = "plugin-trash";
const MAX_PACKAGE_FILES: usize = 4_096;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_SINGLE_FILE_BYTES: u64 = 128 * 1024 * 1024;

static PLUGIN_PACKAGE_MANAGER: OnceLock<Arc<PluginPackageManager>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginStateFile {
    schema_version: u32,
    #[serde(default)]
    disabled_plugins: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPluginSummary {
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
pub struct PluginImportResult {
    pub plugin_id: String,
    pub version: String,
    pub updated_existing: bool,
    pub enabled: bool,
    pub ui_registered: bool,
    /// The runtime-neutral Host catalog is currently a startup snapshot. UI registration and package
    /// files update immediately, while provider execution becomes fully hot-swappable when the
    /// Wasmtime adapter moves its catalog behind the same live manager.
    pub provider_runtime_refresh_pending: bool,
}

#[derive(Debug, Default)]
struct CopyBudget {
    files: usize,
    bytes: u64,
}

#[derive(Debug)]
pub struct PluginPackageManager {
    base_dir: PathBuf,
    plugin_root: PathBuf,
    state_path: PathBuf,
    disabled_plugins: Mutex<HashSet<String>>,
    operation_lock: Mutex<()>,
}

impl PluginPackageManager {
    pub fn load(base_dir: &Path) -> Self {
        let state_path = base_dir.join(PLUGIN_STATE_FILE);
        let disabled_plugins = load_disabled_plugins(&state_path).unwrap_or_else(|error| {
            tracing::warn!(%error, "加载插件启停状态失败，按全部启用处理");
            HashSet::new()
        });
        Self {
            base_dir: base_dir.to_path_buf(),
            plugin_root: base_dir.join("plugins"),
            state_path,
            disabled_plugins: Mutex::new(disabled_plugins),
            operation_lock: Mutex::new(()),
        }
    }

    pub fn plugin_root(&self) -> &Path {
        &self.plugin_root
    }

    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.disabled_plugins
            .lock()
            .map(|disabled| !disabled.contains(plugin_id))
            .unwrap_or(false)
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledPluginSummary>> {
        let catalog = PluginCatalog::discover(self.plugin_root.clone());
        let disabled = self
            .disabled_plugins
            .lock()
            .map_err(|error| anyhow!("插件启停状态锁已损坏: {error}"))?
            .clone();
        let mut summaries = Vec::with_capacity(catalog.plugins().len());
        for plugin in catalog.plugins() {
            let ui = ui_catalog::load_plugin_contributions(plugin).unwrap_or_default();
            summaries.push(InstalledPluginSummary {
                plugin_id: plugin.manifest.id.clone(),
                name: plugin.manifest.name.clone(),
                version: plugin.manifest.version.clone(),
                enabled: !disabled.contains(&plugin.manifest.id),
                provider_count: plugin.manifest.providers.len(),
                route_count: ui.routes.len(),
                page_count: ui.pages.len(),
                theme_count: ui.themes.len(),
                network_domain_count: plugin.manifest.network_domains.len(),
            });
        }
        summaries.sort_by(|left, right| left.name.cmp(&right.name).then(left.plugin_id.cmp(&right.plugin_id)));
        Ok(summaries)
    }

    /// Import one unpacked plugin directory selected by the user.
    ///
    /// The source tree is copied into a bounded Host staging directory, validated there as a normal
    /// plugin package, and only then atomically renamed into `<config>/plugins/<plugin-id>`. Symlinks
    /// and non-regular filesystem objects are rejected instead of followed.
    pub fn import_directory(&self, source: &Path) -> Result<PluginImportResult> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|error| anyhow!("插件包管理锁已损坏: {error}"))?;

        let source_meta = fs::symlink_metadata(source)
            .with_context(|| format!("读取插件导入目录失败: {}", source.display()))?;
        if !source_meta.is_dir() || source_meta.file_type().is_symlink() {
            bail!("插件导入源必须是普通目录，不能是符号链接");
        }
        let canonical_source = fs::canonicalize(source)
            .with_context(|| format!("规范化插件导入目录失败: {}", source.display()))?;
        let folder_name = canonical_source
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("插件导入目录缺少有效目录名"))?;

        fs::create_dir_all(&self.plugin_root)
            .with_context(|| format!("创建插件目录失败: {}", self.plugin_root.display()))?;
        let nonce = unique_nonce();
        let staging_root = self
            .base_dir
            .join(IMPORT_STAGING_DIR)
            .join(format!("{}-{nonce}", std::process::id()));
        let staged_package = staging_root.join(folder_name);
        fs::create_dir_all(&staging_root)
            .with_context(|| format!("创建插件 staging 目录失败: {}", staging_root.display()))?;

        let import_result = (|| -> Result<PluginImportResult> {
            let mut budget = CopyBudget::default();
            copy_tree_bounded(&canonical_source, &staged_package, &mut budget)?;

            let staged_catalog = PluginCatalog::discover(staging_root.clone());
            if !staged_catalog.failures().is_empty() {
                let errors = staged_catalog
                    .failures()
                    .iter()
                    .map(|failure| failure.error.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                bail!("插件 staging 校验失败: {errors}");
            }
            if staged_catalog.plugins().len() != 1 {
                bail!("插件导入目录必须且只能包含一个有效插件包");
            }
            let staged_plugin = &staged_catalog.plugins()[0];
            ui_catalog::load_plugin_contributions(staged_plugin)
                .context("插件 UI contributions 校验失败")?;

            let plugin_id = staged_plugin.manifest.id.clone();
            let version = staged_plugin.manifest.version.clone();
            let destination = self.plugin_root.join(&plugin_id);
            let updated_existing = destination.exists();
            let trash_parent = self
                .base_dir
                .join(IMPORT_TRASH_DIR)
                .join(format!("{}-{nonce}", std::process::id()));
            let backup = trash_parent.join(&plugin_id);

            if updated_existing {
                fs::create_dir_all(&trash_parent)
                    .with_context(|| format!("创建插件更新备份目录失败: {}", trash_parent.display()))?;
                fs::rename(&destination, &backup).with_context(|| {
                    format!("暂存旧插件失败: {} -> {}", destination.display(), backup.display())
                })?;
            }

            if let Err(error) = fs::rename(&staged_package, &destination) {
                if updated_existing && backup.exists() {
                    let _ = fs::rename(&backup, &destination);
                }
                return Err(error).with_context(|| {
                    format!("提交插件安装失败: {} -> {}", staged_package.display(), destination.display())
                });
            }

            let installed_catalog = PluginCatalog::discover(self.plugin_root.clone());
            let installed = installed_catalog
                .plugin(&plugin_id)
                .ok_or_else(|| anyhow!("插件提交后重新发现失败: {plugin_id}"))?;
            let enabled = self.is_enabled(&plugin_id);
            let ui_registered = if enabled {
                let registry = ui_registry::global()
                    .ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
                ui_catalog::register_plugin(
                    &mut registry
                        .write()
                        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?,
                    installed,
                )?;
                true
            } else {
                let _ = ui_registry::unregister_plugin(&plugin_id);
                false
            };

            if let Some(components) = component_registry::global() {
                let _ = components.invalidate(&plugin_id);
            }
            if let Some(gc) = gc::global() {
                let _ = gc.invalidate_plugin(&plugin_id);
            }

            if backup.exists() {
                let _ = fs::remove_dir_all(&backup);
            }
            if trash_parent.exists() {
                let _ = fs::remove_dir(&trash_parent);
            }

            Ok(PluginImportResult {
                plugin_id,
                version,
                updated_existing,
                enabled,
                ui_registered,
                provider_runtime_refresh_pending: true,
            })
        })();

        let _ = fs::remove_dir_all(&staging_root);
        import_result
    }

    pub fn set_enabled(&self, plugin_id: &str, enabled: bool) -> Result<bool> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|error| anyhow!("插件包管理锁已损坏: {error}"))?;
        let catalog = PluginCatalog::discover(self.plugin_root.clone());
        let plugin = catalog
            .plugin(plugin_id)
            .ok_or_else(|| anyhow!("未安装插件: {plugin_id}"))?;

        let mut disabled = self
            .disabled_plugins
            .lock()
            .map_err(|error| anyhow!("插件启停状态锁已损坏: {error}"))?;
        let changed = if enabled {
            disabled.remove(plugin_id)
        } else {
            disabled.insert(plugin_id.to_owned())
        };
        if !changed {
            return Ok(false);
        }
        save_disabled_plugins(&self.state_path, &disabled)?;
        drop(disabled);

        if enabled {
            let registry = ui_registry::global()
                .ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
            ui_catalog::register_plugin(
                &mut registry
                    .write()
                    .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?,
                plugin,
            )?;
        } else {
            let _ = ui_registry::unregister_plugin(plugin_id)?;
            if let Some(components) = component_registry::global() {
                let _ = components.invalidate(plugin_id);
            }
            if let Some(gc) = gc::global() {
                let _ = gc.invalidate_plugin(plugin_id);
            }
        }
        Ok(true)
    }

    pub fn uninstall(&self, plugin_id: &str) -> Result<bool> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|error| anyhow!("插件包管理锁已损坏: {error}"))?;
        let catalog = PluginCatalog::discover(self.plugin_root.clone());
        let Some(plugin) = catalog.plugin(plugin_id) else {
            return Ok(false);
        };

        let nonce = unique_nonce();
        let trash_parent = self
            .base_dir
            .join(IMPORT_TRASH_DIR)
            .join(format!("{}-{nonce}", std::process::id()));
        let trash = trash_parent.join(plugin_id);
        fs::create_dir_all(&trash_parent)
            .with_context(|| format!("创建插件卸载暂存目录失败: {}", trash_parent.display()))?;
        fs::rename(&plugin.package_dir, &trash).with_context(|| {
            format!("将插件移动到卸载暂存区失败: {}", plugin.package_dir.display())
        })?;

        let _ = ui_registry::unregister_plugin(plugin_id);
        if let Some(components) = component_registry::global() {
            let _ = components.invalidate(plugin_id);
        }
        if let Some(gc) = gc::global() {
            let _ = gc.invalidate_plugin(plugin_id);
        }
        if let Some(runtime) = runtime::global() {
            let _ = runtime.revoke_all_plugin_secrets(plugin_id);
        }
        if let Some(permission_state) = permissions::global()
            && let Ok(mut permission_state) = permission_state.write()
        {
            let _ = permission_state.revoke(plugin_id);
        }
        if let Some(host) = super::catalog::global() {
            let accounts = host
                .read()
                .map(|host| {
                    host.router()
                        .accounts()
                        .iter()
                        .filter(|account| account.plugin_id == plugin_id)
                        .map(|account| {
                            (
                                account.plugin_id.clone(),
                                account.provider_id.clone(),
                                account.account_id.clone(),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if let Ok(mut host) = host.write() {
                for (plugin_id, provider_id, account_id) in accounts {
                    let _ = host.remove_account(&plugin_id, &provider_id, &account_id);
                }
            }
        }
        if let Some(session_state) = sessions::global()
            && let Ok(mut sessions) = session_state.write()
        {
            // Account rows were removed above; pending overlays are harmless but should not survive
            // a reinstall under the same id. Reinitialization on next process rebuilds the set from
            // the persisted account index. Runtime session-map compaction is completed with the
            // Wasmtime hot-reload integration.
            let _ = &mut *sessions;
        }

        {
            let mut disabled = self
                .disabled_plugins
                .lock()
                .map_err(|error| anyhow!("插件启停状态锁已损坏: {error}"))?;
            if disabled.remove(plugin_id) {
                save_disabled_plugins(&self.state_path, &disabled)?;
            }
        }

        fs::remove_dir_all(&trash)
            .with_context(|| format!("删除插件卸载暂存目录失败: {}", trash.display()))?;
        let _ = fs::remove_dir(&trash_parent);
        Ok(true)
    }
}

fn load_disabled_plugins(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("读取插件启停状态失败: {}", path.display()))?;
    let file: PluginStateFile = serde_json::from_str(&content)
        .with_context(|| format!("解析插件启停状态失败: {}", path.display()))?;
    if file.schema_version != PLUGIN_STATE_SCHEMA_VERSION {
        bail!(
            "不支持的插件启停状态版本 v{}，当前仅支持 v{}",
            file.schema_version,
            PLUGIN_STATE_SCHEMA_VERSION
        );
    }
    Ok(file.disabled_plugins.into_iter().collect())
}

fn save_disabled_plugins(path: &Path, disabled: &HashSet<String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建插件状态目录失败: {}", parent.display()))?;
    }
    let mut disabled_plugins = disabled.iter().cloned().collect::<Vec<_>>();
    disabled_plugins.sort();
    let payload = serde_json::to_vec_pretty(&PluginStateFile {
        schema_version: PLUGIN_STATE_SCHEMA_VERSION,
        disabled_plugins,
    })
    .context("序列化插件启停状态失败")?;
    fs::write(path, payload)
        .with_context(|| format!("写入插件启停状态失败: {}", path.display()))
}

fn copy_tree_bounded(source: &Path, destination: &Path, budget: &mut CopyBudget) -> Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("读取插件包条目失败: {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("插件包禁止符号链接: {}", source.display());
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination)
            .with_context(|| format!("创建插件 staging 子目录失败: {}", destination.display()))?;
        for entry in fs::read_dir(source)
            .with_context(|| format!("读取插件包目录失败: {}", source.display()))?
        {
            let entry = entry?;
            copy_tree_bounded(&entry.path(), &destination.join(entry.file_name()), budget)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("插件包仅允许普通文件/目录: {}", source.display());
    }
    if metadata.len() > MAX_SINGLE_FILE_BYTES {
        bail!("插件包单文件超过 {} bytes: {}", MAX_SINGLE_FILE_BYTES, source.display());
    }
    budget.files = budget.files.saturating_add(1);
    budget.bytes = budget.bytes.saturating_add(metadata.len());
    if budget.files > MAX_PACKAGE_FILES {
        bail!("插件包文件数超过 {MAX_PACKAGE_FILES}");
    }
    if budget.bytes > MAX_PACKAGE_BYTES {
        bail!("插件包总体积超过 {MAX_PACKAGE_BYTES} bytes");
    }
    fs::copy(source, destination)
        .with_context(|| format!("复制插件包文件失败: {}", source.display()))?;
    Ok(())
}

fn unique_nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub fn initialize(base_dir: &Path) -> Arc<PluginPackageManager> {
    PLUGIN_PACKAGE_MANAGER
        .get_or_init(|| Arc::new(PluginPackageManager::load(base_dir)))
        .clone()
}

pub fn global() -> Option<Arc<PluginPackageManager>> {
    PLUGIN_PACKAGE_MANAGER.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_defaults_to_all_enabled() {
        let path = std::env::temp_dir().join(format!("yinqidao-plugin-state-missing-{}", unique_nonce()));
        assert!(load_disabled_plugins(&path).expect("state").is_empty());
    }
}
