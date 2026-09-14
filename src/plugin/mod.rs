//! WASM music-service plugin subsystem.
//!
//! Dependency boundary:
//! - `abi` contains runtime-neutral plugin/domain types and must not depend on Host, OnlineServices,
//!   GPUI, audio, or Wasmtime.
//! - `host` owns permissions, secrets, HTTP/network policy, sessions, package management and call
//!   budgets. Guest code is never authoritative for these decisions.
//! - `component` owns Component loading/compiled cache, Host-owned GC and, later, the private
//!   Wasmtime adapter. Generated Wasmtime binding types must not escape this module.
//! - `client` is the semantic port implemented by the Component runtime.
//! - `frontend` is the only ordinary application-facing execution façade.
//! - `management` is the narrow application/UI management façade; UI must not import Host or
//!   Component internals directly.
//! - `extensions` exposes validated command/home/theme DTOs without leaking UI registry internals or
//!   plugin package paths into GPUI.
//! - `commands` owns Host-authorized command invocation and context minimization.
//! - `runtime_ports` coordinates Provider/UI adapter replacement so readers never observe a
//!   half-swapped Component runtime.
//! - `ui` owns validated plugin-level route/page/command/theme contribution models and registries;
//!   it must not depend on GPUI or Wasmtime.
//! - `online` may consume `frontend` plus selected `abi` values, never Host/Component internals.
//! - No path in this subsystem may be invoked from the realtime audio callback.
//!
//! Physical source layout follows the same ownership model: application-facing façades live under
//! `application/`, runtime semantic ports live under `runtime/`, while Host/Component/UI internals
//! remain in their dedicated directories. `#[path]` keeps the public Rust module paths stable.

use std::path::Path;

use anyhow::Result;

pub(crate) mod abi;
#[path = "runtime/client.rs"]
pub(crate) mod client;
#[path = "application/commands.rs"]
pub(crate) mod commands;
#[path = "application/extensions.rs"]
pub(crate) mod extensions;
#[path = "application/frontend.rs"]
pub(crate) mod frontend;
#[path = "application/management.rs"]
pub(crate) mod management;
#[path = "runtime/ports.rs"]
pub(crate) mod runtime_ports;

pub(crate) mod component;
pub(crate) mod host;
pub(crate) mod routing;
pub(crate) mod ui;

/// Initialize the complete plugin control plane.
pub(crate) fn initialize(base_dir: &Path) -> Result<()> {
    let engine_policy = component::policy::PluginEnginePolicy::default();
    engine_policy.validate()?;
    let compiled_cache = component::cache::initialize(engine_policy.max_compiled_artifact_bytes);
    let plugin_host = host::catalog::initialize(base_dir);
    let package_manager = host::package_manager::initialize(base_dir);

    let secret_store = std::sync::Arc::new(host::secrets::MemorySecretStore::default());
    let secret_backend = host::secrets::PluginSecretStore::backend_name(secret_store.as_ref());
    let secret_protection = host::secrets::PluginSecretStore::protection(secret_store.as_ref());
    let sessions = host::sessions::initialize(&plugin_host, secret_protection);
    let components = component::registry::initialize(base_dir);

    let mut gc_policy = component::gc::PluginGcPolicy::default();
    gc_policy.max_warm_instances_per_route = engine_policy.max_pooled_instances_per_route;
    let gc = component::gc::initialize(components.cache_root().to_path_buf(), gc_policy)?;
    let initial_gc = gc.sweep_once()?;

    let ui_registry = ui::registry::initialize();

    let catalog = match plugin_host.read() {
        Ok(host) => Some(host.catalog().clone()),
        Err(error) => {
            tracing::error!(%error, "读取插件目录用于权限初始化失败");
            None
        }
    };

    let ui_sync = match (catalog.as_ref(), ui_registry.write()) {
        (Some(catalog), Ok(mut registry)) => Some(ui::catalog::sync_from_catalog_filtered(
            catalog,
            &mut registry,
            |plugin_id| package_manager.is_enabled(plugin_id),
        )),
        (Some(_), Err(error)) => {
            tracing::error!(%error, "插件 UI contribution registry 锁已损坏");
            None
        }
        (None, _) => None,
    };

    let permissions = catalog
        .as_ref()
        .map(|catalog| host::permissions::initialize(base_dir, catalog));
    let runtime = match (catalog.as_ref(), permissions.as_ref()) {
        (Some(catalog), Some(permissions)) => Some(host::runtime::initialize(
            catalog.clone(),
            permissions.clone(),
            secret_store.clone(),
        )),
        _ => None,
    };
    let clients = client::initialize();
    let ui_clients = ui::client::initialize();
    let frontend = runtime.as_ref().map(|runtime| {
        frontend::initialize(
            plugin_host.clone(),
            sessions.clone(),
            runtime.clone(),
            clients.clone(),
        )
    });

    tracing::info!(
        selected_wasmtime = components.selected_runtime_version(),
        cache_root = %components.cache_root().display(),
        max_compiled_artifact_bytes = compiled_cache.max_artifact_bytes(),
        "插件 Component 懒加载与 compiled cache 边界已初始化"
    );
    tracing::info!(
        max_store_memory_mib = engine_policy.max_memory_mib(),
        max_compiled_artifact_mib = engine_policy.max_compiled_artifact_mib(),
        fuel_per_call = engine_policy.fuel_per_call,
        epoch_tick_ms = engine_policy.epoch_tick_interval.as_millis(),
        epoch_deadline_ticks = engine_policy.epoch_deadline_ticks(),
        "Wasmtime Engine/Store 资源策略已校验"
    );
    tracing::info!(
        gc_sweep_seconds = gc.policy().sweep_interval.as_secs(),
        warm_instance_idle_seconds = gc.policy().warm_instance_idle_ttl.as_secs(),
        max_warm_instances_total = gc.policy().max_warm_instances_total,
        max_warm_instances_per_route = gc.policy().max_warm_instances_per_route,
        max_compiled_components = gc.policy().max_compiled_components,
        max_disk_cache_bytes = gc.policy().max_disk_cache_bytes,
        initial_removed_disk_buckets = initial_gc.removed_disk_buckets,
        initial_disk_bytes_before = initial_gc.disk_bytes_before,
        initial_disk_bytes_after = initial_gc.disk_bytes_after,
        "Host 插件 GC 已初始化；资源回收不暴露给 guest"
    );
    tracing::info!(
        plugin_root = %package_manager.plugin_root().display(),
        installed_plugins = package_manager.list_installed().map(|plugins| plugins.len()).unwrap_or_default(),
        "插件包管理器已初始化"
    );
    if let Some(runtime) = runtime.as_ref() {
        tracing::info!(
            installed_plugins = runtime.catalog().plugins().len(),
            provider_frontend_ready = frontend.is_some(),
            provider_client_ready = clients.is_ready().unwrap_or(false),
            ui_client_ready = ui_clients.is_ready().unwrap_or(false),
            secret_backend,
            secret_persistent = secret_protection.is_persistent(),
            "插件 Host service runtime 已初始化"
        );
    } else {
        tracing::warn!("插件 Host service runtime 未初始化，Catalog 或权限状态不可用");
    }

    if let Some(report) = ui_sync.as_ref() {
        tracing::info!(
            registered_plugins = report.registered_plugins,
            skipped_disabled_plugins = report.skipped_disabled_plugins,
            routes = report.routes,
            pages = report.pages,
            commands = report.commands,
            home_sections = report.home_sections,
            themes = report.themes,
            failures = report.failures.len(),
            "插件静态 UI contributions 已从 plugin.toml 注册"
        );
        for failure in &report.failures {
            tracing::warn!(
                plugin_id = %failure.plugin_id,
                error = %failure.error,
                "插件 UI contribution 加载失败"
            );
        }
    }

    match ui_registry.read() {
        Ok(registry) => tracing::info!(
            routes = registry.routes().count(),
            pages = registry.pages().count(),
            commands = registry.commands().count(),
            home_sections = registry.home_sections().count(),
            themes = registry.themes().count(),
            "插件 UI contribution registry 已初始化"
        ),
        Err(error) => tracing::error!(%error, "插件 UI contribution registry 锁已损坏"),
    }

    match plugin_host.read() {
        Ok(host) => {
            tracing::info!(
                installed_plugins = host.catalog().plugins().len(),
                restored_accounts = host.router().accounts().len(),
                startup_errors = host.startup_errors().len(),
                plugin_root = %host.catalog().root().display(),
                "WASM 插件宿主基础状态已初始化"
            );
            for error in host.startup_errors() {
                tracing::warn!(%error, "插件启动检查失败");
            }
        }
        Err(error) => tracing::error!(%error, "插件宿主状态锁已损坏"),
    }

    match sessions.read() {
        Ok(sessions) => {
            tracing::info!(
                pending_validation = sessions.pending_count(),
                startup_errors = sessions.startup_errors().len(),
                "插件账号会话恢复状态已初始化"
            );
            for error in sessions.startup_errors() {
                tracing::warn!(%error, "插件会话恢复检查失败");
            }
        }
        Err(error) => tracing::error!(%error, "插件会话状态锁已损坏"),
    }

    if let Some(permissions) = permissions.as_ref() {
        match permissions.read() {
            Ok(permissions) => {
                tracing::info!(
                    permission_grants = permissions.grants().len(),
                    startup_errors = permissions.startup_errors().len(),
                    "插件用户权限状态已初始化"
                );
                for error in permissions.startup_errors() {
                    tracing::warn!(%error, "插件权限恢复检查失败");
                }
            }
            Err(error) => tracing::error!(%error, "插件权限状态锁已损坏"),
        }
    }

    Ok(())
}
