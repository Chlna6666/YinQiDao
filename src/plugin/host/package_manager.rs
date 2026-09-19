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

use super::{
    catalog::{
        PLUGIN_PACKAGE_FILE, PluginCatalog, PluginHostState, PluginPackageFile, valid_identifier,
    },
    permissions, runtime, secrets, sessions,
};

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
    /// The Host security/runtime catalog is refreshed immediately. This remains true until the
    /// private Provider Component adapter itself supports a coordinated hot swap without restart.
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
        summaries.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.plugin_id.cmp(&right.plugin_id))
        });
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
        let staged_folder_name =
            if let Ok(content) = fs::read_to_string(canonical_source.join(PLUGIN_PACKAGE_FILE)) {
                if let Ok(package) = toml::from_str::<PluginPackageFile>(&content) {
                    if valid_identifier(&package.manifest.id) {
                        package.manifest.id
                    } else {
                        folder_name.to_string()
                    }
                } else {
                    folder_name.to_string()
                }
            } else {
                folder_name.to_string()
            };
        let staged_package = staging_root.join(staged_folder_name);
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
            let staged_ui = ui_catalog::load_plugin_contributions(staged_plugin)
                .context("插件 UI contributions 校验失败")?;
            let registry =
                ui_registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;

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
                fs::create_dir_all(&trash_parent).with_context(|| {
                    format!("创建插件更新备份目录失败: {}", trash_parent.display())
                })?;
                fs::rename(&destination, &backup).with_context(|| {
                    format!(
                        "暂存旧插件失败: {} -> {}",
                        destination.display(),
                        backup.display()
                    )
                })?;
            }

            if let Err(error) = fs::rename(&staged_package, &destination) {
                if updated_existing && backup.exists() {
                    let _ = fs::rename(&backup, &destination);
                }
                return Err(error).with_context(|| {
                    format!(
                        "提交插件安装失败: {} -> {}",
                        staged_package.display(),
                        destination.display()
                    )
                });
            }

            let post_commit = (|| -> Result<(PluginCatalog, bool, bool)> {
                let installed_catalog = PluginCatalog::discover(self.plugin_root.clone());
                if installed_catalog.plugin(&plugin_id).is_none() {
                    bail!("插件提交后重新发现失败: {plugin_id}");
                }
                let enabled = self.is_enabled(&plugin_id);
                let mut registry = registry
                    .write()
                    .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
                let ui_registered = if enabled {
                    registry.replace_plugin(&plugin_id, staged_ui)?;
                    true
                } else {
                    registry.remove_plugin(&plugin_id);
                    false
                };
                Ok((installed_catalog, enabled, ui_registered))
            })();

            let (installed_catalog, enabled, ui_registered) = match post_commit {
                Ok(committed) => committed,
                Err(error) => {
                    let rollback = (|| -> Result<()> {
                        if destination.exists() {
                            fs::remove_dir_all(&destination).with_context(|| {
                                format!("删除未完成提交的插件目录失败: {}", destination.display())
                            })?;
                        }
                        if updated_existing {
                            if !backup.exists() {
                                bail!("插件更新回滚缺少旧版本备份: {}", backup.display());
                            }
                            fs::rename(&backup, &destination).with_context(|| {
                                format!(
                                    "恢复旧插件版本失败: {} -> {}",
                                    backup.display(),
                                    destination.display()
                                )
                            })?;
                        }
                        Ok(())
                    })();
                    if let Err(rollback_error) = rollback {
                        return Err(anyhow!(
                            "插件提交后初始化失败: {error:#}; 文件系统回滚同时失败: {rollback_error:#}"
                        ));
                    }
                    return Err(error);
                }
            };

            if let Some(components) = component_registry::global() {
                let _ = components.invalidate(&plugin_id);
            }
            if let Some(gc) = gc::global() {
                let _ = gc.invalidate_plugin(&plugin_id);
            }
            publish_runtime_catalog(&installed_catalog);
            reconcile_host_catalog(&self.base_dir, &installed_catalog);
            // The user confirmed the import dialog — that is the explicit permission grant.
            // Automatically register all manifest-declared network domains so the plugin can
            // make its first Host HTTP call (e.g. QR-login) without a separate approval step.
            grant_manifest_network_permissions(&self.base_dir, &installed_catalog, &plugin_id);

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
        let contributions = if enabled {
            Some(
                ui_catalog::load_plugin_contributions(plugin)
                    .context("插件 UI contributions 校验失败")?,
            )
        } else {
            None
        };

        // Keep the established lock order used by startup synchronization: UI registry first,
        // then the package enabled-state mutex. Holding both makes the registry + persisted state
        // transition observable as one Host transaction to all in-process readers.
        let registry =
            ui_registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
        let mut registry = registry
            .write()
            .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
        let mut disabled = self
            .disabled_plugins
            .lock()
            .map_err(|error| anyhow!("插件启停状态锁已损坏: {error}"))?;
        let Some(next_disabled) = next_disabled_plugins(&disabled, plugin_id, enabled) else {
            return Ok(false);
        };

        // UI contribution replacement is validated before mutation, and PluginUiRegistry is Clone.
        // Keep a snapshot so a state-file write failure can restore the exact previous registry.
        // The monotonic UI generation may still advance on a rolled-back attempt, which only causes
        // conservative cache invalidation and never publishes broader authority.
        let registry_snapshot = registry.clone();
        if enabled {
            let contributions = contributions
                .ok_or_else(|| anyhow!("启用插件缺少已验证 UI contributions: {plugin_id}"))?;
            registry.replace_plugin(plugin_id, contributions)?;
        } else {
            registry.remove_plugin(plugin_id);
        }

        if let Err(error) = save_disabled_plugins(&self.state_path, &next_disabled) {
            *registry = registry_snapshot;
            return Err(error);
        }
        *disabled = next_disabled;
        drop(disabled);
        drop(registry);

        if !enabled {
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
        let registry =
            ui_registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;

        let nonce = unique_nonce();
        let trash_parent = self
            .base_dir
            .join(IMPORT_TRASH_DIR)
            .join(format!("{}-{nonce}", std::process::id()));
        let trash = trash_parent.join(plugin_id);
        fs::create_dir_all(&trash_parent)
            .with_context(|| format!("创建插件卸载暂存目录失败: {}", trash_parent.display()))?;
        fs::rename(&plugin.package_dir, &trash).with_context(|| {
            format!(
                "将插件移动到卸载暂存区失败: {}",
                plugin.package_dir.display()
            )
        })?;

        // Commit all reversible Host metadata before revoking credentials/accounts. If any lock or
        // state-file write fails, the package directory is moved back and callers observe a genuine
        // failed uninstall rather than a half-removed plugin with already-destroyed credentials.
        let metadata_commit = (|| -> Result<()> {
            let mut registry = registry
                .write()
                .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
            let mut disabled = self
                .disabled_plugins
                .lock()
                .map_err(|error| anyhow!("插件启停状态锁已损坏: {error}"))?;
            if let Some(next_disabled) = next_disabled_plugins(&disabled, plugin_id, true) {
                save_disabled_plugins(&self.state_path, &next_disabled)?;
                *disabled = next_disabled;
            }
            registry.remove_plugin(plugin_id);
            Ok(())
        })();
        if let Err(error) = metadata_commit {
            let rollback = fs::rename(&trash, &plugin.package_dir).with_context(|| {
                format!(
                    "卸载失败后恢复插件目录失败: {} -> {}",
                    trash.display(),
                    plugin.package_dir.display()
                )
            });
            if rollback.is_ok() {
                let _ = fs::remove_dir(&trash_parent);
                return Err(error);
            }
            return Err(anyhow!(
                "插件卸载元数据提交失败: {error:#}; 文件系统回滚同时失败: {:#}",
                rollback.expect_err("rollback checked as error")
            ));
        }

        let live_catalog = PluginCatalog::discover(self.plugin_root.clone());
        publish_runtime_catalog(&live_catalog);
        if let Some(components) = component_registry::global() {
            let _ = components.invalidate(plugin_id);
        }
        if let Some(gc) = gc::global() {
            let _ = gc.invalidate_plugin(plugin_id);
        }
        match secrets::global() {
            Some(secret_store) => {
                if let Err(error) = secret_store.delete_plugin(plugin_id) {
                    tracing::error!(
                        plugin_id = %plugin_id,
                        %error,
                        "卸载插件后撤销 Host Secret 失败；敏感凭据可能仍残留"
                    );
                }
            }
            None => tracing::error!(
                plugin_id = %plugin_id,
                "卸载插件时 Host Secret backend 未初始化；无法确认敏感凭据已撤销"
            ),
        }
        if let Some(permission_state) = permissions::global() {
            match permission_state.write() {
                Ok(mut permission_state) => {
                    if let Err(error) = permission_state.revoke(plugin_id) {
                        tracing::error!(
                            plugin_id = %plugin_id,
                            %error,
                            "卸载插件后撤销权限记录失败"
                        );
                    }
                }
                Err(error) => tracing::error!(
                    plugin_id = %plugin_id,
                    %error,
                    "卸载插件时权限状态锁已损坏"
                ),
            }
        }
        if let Some(host) = super::catalog::global() {
            let accounts = match host.read() {
                Ok(host) => host
                    .router()
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
                    .collect::<Vec<_>>(),
                Err(error) => {
                    tracing::error!(
                        plugin_id = %plugin_id,
                        %error,
                        "卸载插件时读取 Host 账号状态失败"
                    );
                    Vec::new()
                }
            };
            match host.write() {
                Ok(mut host) => {
                    for (plugin_id, provider_id, account_id) in accounts {
                        if let Err(error) =
                            host.remove_account(&plugin_id, &provider_id, &account_id)
                        {
                            tracing::error!(
                                %error,
                                plugin_id = %plugin_id,
                                provider_id = %provider_id,
                                account_id = %account_id,
                                "卸载插件后移除账号状态失败"
                            );
                        }
                    }
                }
                Err(error) => tracing::error!(
                    plugin_id = %plugin_id,
                    %error,
                    "卸载插件时 Host 账号状态锁已损坏"
                ),
            }
        }

        if let Err(error) = fs::remove_dir_all(&trash) {
            tracing::warn!(
                %error,
                path = %trash.display(),
                "插件已完成逻辑卸载，但清理卸载暂存目录失败"
            );
        } else {
            let _ = fs::remove_dir(&trash_parent);
        }
        reconcile_host_catalog(&self.base_dir, &live_catalog);
        Ok(true)
    }
}

fn publish_runtime_catalog(catalog: &PluginCatalog) {
    let Some(runtime) = runtime::global() else {
        return;
    };
    if let Err(error) = runtime.replace_catalog(catalog.clone()) {
        tracing::error!(%error, "发布插件 live runtime catalog 失败；后续执行将 fail closed");
    }
}

/// Auto-grant all network domains declared in the plugin manifest after a successful import.
///
/// Importing a plugin is itself the user's permission grant: the import confirmation dialog
/// already shows the full `network_domains` list, so writing a matching `PluginPermissionGrant`
/// at this point avoids a separate, unintuitive approval step.  Only domains that already appear
/// in the manifest are written; the `set_grant` validation will reject anything out of scope.
fn grant_manifest_network_permissions(_base_dir: &Path, catalog: &PluginCatalog, plugin_id: &str) {
    let Some(plugin) = catalog.plugin(plugin_id) else {
        return;
    };
    // Skip plugins that declare no network domains — they don't need a grant record.
    if plugin.manifest.network_domains.is_empty() {
        return;
    }
    let has_playback_events = plugin.manifest.providers.iter().any(|provider| {
        provider
            .capabilities
            .contains(&crate::plugin::abi::PluginCapability::PlaybackEvents)
    });
    let grant = super::security::PluginPermissionGrant {
        plugin_id: plugin_id.to_owned(),
        network_domains: plugin.manifest.network_domains.clone(),
        playback_events: has_playback_events,
    };

    let Some(permissions) = permissions::global() else {
        // Permissions store not yet initialized (e.g. test environment); skip silently.
        tracing::debug!(plugin_id, "插件权限全局状态尚未初始化，跳过导入时自动授权");
        return;
    };
    match permissions.write() {
        Ok(mut state) => {
            if let Err(error) = state.set_grant(catalog, grant) {
                tracing::warn!(
                    plugin_id,
                    %error,
                    "自动授予插件 manifest 网络权限失败，需用户在设置中手动授权"
                );
            } else {
                tracing::debug!(plugin_id, "已自动授予插件 manifest 声明的全部网络域名");
            }
        }
        Err(error) => tracing::warn!(
            %error,
            "写入插件权限状态锁失败，跳过自动网络授权"
        ),
    }
}

/// Reconcile persisted accounts against the newly committed package catalog before replacing the
/// Host router snapshot. Removed providers and capability-shrunk accounts must not survive a package
/// update as ghost routes.
fn reconcile_host_catalog(base_dir: &Path, catalog: &PluginCatalog) {
    let Some(host) = super::catalog::global() else {
        return;
    };

    let invalid_accounts = match host.read() {
        Ok(host) => host
            .router()
            .accounts()
            .iter()
            .filter(
                |account| match catalog.provider(&account.plugin_id, &account.provider_id) {
                    None => true,
                    Some(provider) => account
                        .capabilities
                        .iter()
                        .any(|capability| !provider.capabilities.contains(capability)),
                },
            )
            .map(|account| {
                (
                    account.plugin_id.clone(),
                    account.provider_id.clone(),
                    account.account_id.clone(),
                )
            })
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::error!(%error, "读取插件 Host state 进行 live catalog reconcile 失败");
            return;
        }
    };

    if !invalid_accounts.is_empty() {
        let mut host_state = match host.write() {
            Ok(host_state) => host_state,
            Err(error) => {
                tracing::error!(%error, "写入插件 Host state 进行账号 reconcile 失败");
                return;
            }
        };
        for (plugin_id, provider_id, account_id) in invalid_accounts {
            if let Err(error) = host_state.remove_account(&plugin_id, &provider_id, &account_id) {
                tracing::error!(
                    %error,
                    plugin_id = %plugin_id,
                    provider_id = %provider_id,
                    account_id = %account_id,
                    "移除与新插件 catalog 不兼容的账号失败"
                );
            }
        }
    }

    let refreshed = PluginHostState::load(base_dir);
    match host.write() {
        Ok(mut current) => *current = refreshed,
        Err(error) => {
            tracing::error!(%error, "发布插件 live Host catalog 失败");
            return;
        }
    }

    let Some(session_state) = sessions::global() else {
        return;
    };
    let mut sessions = match session_state.write() {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::error!(%error, "写入插件 Session overlay 进行 live catalog reconcile 失败");
            return;
        }
    };
    let host = match host.read() {
        Ok(host) => host,
        Err(error) => {
            tracing::error!(%error, "读取插件 Host router 进行 Session overlay reconcile 失败");
            return;
        }
    };
    let removed = sessions.retain_host_accounts(host.router().accounts());
    if removed > 0 {
        tracing::debug!(
            removed,
            "已裁剪 live catalog 中不可达的插件 Session overlay"
        );
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

fn next_disabled_plugins(
    current: &HashSet<String>,
    plugin_id: &str,
    enabled: bool,
) -> Option<HashSet<String>> {
    let changed = if enabled {
        current.contains(plugin_id)
    } else {
        !current.contains(plugin_id)
    };
    if !changed {
        return None;
    }

    let mut next = current.clone();
    if enabled {
        next.remove(plugin_id);
    } else {
        next.insert(plugin_id.to_owned());
    }
    Some(next)
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
    fs::write(path, payload).with_context(|| format!("写入插件启停状态失败: {}", path.display()))
}

fn should_skip_package_entry(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower.starts_with('.')
        || lower == "target"
        || lower == "node_modules"
        || lower == "tests"
        || lower == "benches"
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
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if should_skip_package_entry(&name_str) {
                continue;
            }
            copy_tree_bounded(&entry.path(), &destination.join(&name), budget)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        bail!("插件包仅允许普通文件/目录: {}", source.display());
    }
    if metadata.len() > MAX_SINGLE_FILE_BYTES {
        bail!(
            "插件包单文件超过 {} bytes: {}",
            MAX_SINGLE_FILE_BYTES,
            source.display()
        );
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
        let path =
            std::env::temp_dir().join(format!("yinqidao-plugin-state-missing-{}", unique_nonce()));
        assert!(load_disabled_plugins(&path).expect("state").is_empty());
    }

    #[test]
    fn disabled_state_transition_is_copy_on_write() {
        let current = HashSet::from(["plugin.test".to_string()]);
        let enabled = next_disabled_plugins(&current, "plugin.test", true).expect("enable");
        assert!(current.contains("plugin.test"));
        assert!(!enabled.contains("plugin.test"));

        let disabled = next_disabled_plugins(&enabled, "plugin.test", false).expect("disable");
        assert!(disabled.contains("plugin.test"));
        assert!(next_disabled_plugins(&disabled, "plugin.test", false).is_none());
    }

    #[test]
    fn import_directory_from_custom_folder_name_succeeds() {
        let root = std::env::temp_dir().join(format!("yinqidao-pm-test-{}", unique_nonce()));
        fs::create_dir_all(&root).expect("create root");
        let source_dir = root.join("netease-source-folder");
        fs::create_dir_all(&source_dir).expect("create source dir");

        let manifest = r#"package_schema = 1
component = "provider.wasm"
id = "io.yinqidao.netease"
name = "网易云音乐"
version = "0.1.0"
abi_version = 1
description = "网易云测试插件"
network_domains = ["music.163.com"]

[[providers]]
id = "netease"
display_name = "网易云音乐"
capabilities = ["authentication"]
auth_methods = ["qr_code"]
"#;
        fs::write(source_dir.join(PLUGIN_PACKAGE_FILE), manifest).expect("write manifest");
        fs::write(source_dir.join("provider.wasm"), b"\0asm").expect("write wasm");

        let app_base = root.join("app_base");
        fs::create_dir_all(&app_base).expect("create app base");
        let _ = ui_registry::initialize();

        let manager = PluginPackageManager::load(&app_base);
        let result = manager
            .import_directory(&source_dir)
            .expect("import succeeds with custom source folder name");
        assert_eq!(result.plugin_id, "io.yinqidao.netease");
        assert_eq!(result.version, "0.1.0");
        assert!(
            app_base
                .join("plugins")
                .join("io.yinqidao.netease")
                .join(PLUGIN_PACKAGE_FILE)
                .is_file()
        );

        let installed = manager.list_installed().expect("list installed");
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].plugin_id, "io.yinqidao.netease");
        assert_eq!(installed[0].name, "网易云音乐");
        assert!(installed[0].enabled);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_directory_skips_target_and_dotfiles() {
        let root = std::env::temp_dir().join(format!("yinqidao-pm-skip-{}", unique_nonce()));
        fs::create_dir_all(&root).expect("create root");
        let source_dir = root.join("source-plugin");
        fs::create_dir_all(&source_dir).expect("create source dir");

        let manifest = r#"package_schema = 1
component = "provider.wasm"
id = "io.yinqidao.netease"
name = "网易云音乐"
version = "0.1.0"
abi_version = 1
description = "网易云测试插件"
network_domains = ["music.163.com"]

[[providers]]
id = "netease"
display_name = "网易云音乐"
capabilities = ["authentication"]
auth_methods = ["qr_code"]
"#;
        fs::write(source_dir.join(PLUGIN_PACKAGE_FILE), manifest).expect("write manifest");
        fs::write(source_dir.join("provider.wasm"), b"\0asm").expect("write wasm");

        let target_dir = source_dir.join("target").join("release");
        fs::create_dir_all(&target_dir).expect("create target dir");
        fs::write(target_dir.join("dummy_artifact.bin"), b"build artifact").expect("write dummy");

        let git_dir = source_dir.join(".git");
        fs::create_dir_all(&git_dir).expect("create git dir");
        fs::write(git_dir.join("config"), b"git config").expect("write git config");

        let app_base = root.join("app_base");
        fs::create_dir_all(&app_base).expect("create app base");
        let _ = ui_registry::initialize();

        let manager = PluginPackageManager::load(&app_base);
        let result = manager
            .import_directory(&source_dir)
            .expect("import succeeds skipping target");
        assert_eq!(result.plugin_id, "io.yinqidao.netease");

        let installed_plugin = app_base.join("plugins").join("io.yinqidao.netease");
        assert!(installed_plugin.join(PLUGIN_PACKAGE_FILE).is_file());
        assert!(installed_plugin.join("provider.wasm").is_file());
        assert!(!installed_plugin.join("target").exists());
        assert!(!installed_plugin.join(".git").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn import_real_netease_folder_skips_huge_target() {
        let root = std::env::temp_dir().join(format!("yinqidao-pm-real-{}", unique_nonce()));
        fs::create_dir_all(&root).expect("create root");
        let app_base = root.join("app_base");
        fs::create_dir_all(&app_base).expect("create app base");
        let _ = ui_registry::initialize();

        let real_netease = Path::new("plugins/netease");
        if real_netease.join("plugin.toml").is_file() {
            let manager = PluginPackageManager::load(&app_base);
            let result = manager
                .import_directory(real_netease)
                .expect("import real netease directory succeeds");
            assert_eq!(result.plugin_id, "io.yinqidao.netease");
            assert!(
                !app_base
                    .join("plugins")
                    .join("io.yinqidao.netease")
                    .join("target")
                    .exists()
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}
