#![allow(
    dead_code,
    clippy::chunks_exact_to_as_chunks,
    clippy::assertions_on_constants,
    clippy::field_reassign_with_default,
    clippy::unnecessary_sort_by,
    clippy::needless_borrow,
    clippy::manual_clamp,
    clippy::manual_is_multiple_of,
    clippy::manual_map,
    clippy::too_many_arguments,
    clippy::clone_on_copy,
    clippy::collapsible_if
)]

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod artwork;
mod audio;
mod audio_policy;
mod desktop_lyrics;
mod global_shortcuts;
mod gpu;
mod hotkeys;
mod library;
mod lucide_assets {
    include!(concat!(env!("OUT_DIR"), "/lucide_assets.rs"));
}
pub mod logger;
mod lyrics;
pub mod media_controls;
mod model;
mod online;
mod plugin_client;
mod plugin_compiled_cache;
mod plugin_components;
mod plugin_engine_policy;
mod plugin_host;
mod plugin_http;
mod plugin_permissions;
mod plugin_route_gate;
mod plugin_runtime;
mod plugin_security;
mod plugin_secrets;
mod plugin_sessions;
mod plugins;
mod preferences;
pub mod runtime;
mod settings;
mod ui;
pub(crate) use ui::audio_debug_window;
mod window_platform;

use anyhow::Result;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowCornerPreference,
    WindowIconSource, WindowOptions, px, size,
};
use settings::ConfigStore;
use ui::MusicApp;

fn main() -> Result<()> {
    let app_runtime = runtime::initialize_app_runtime()?;
    let io_handle = app_runtime.io_handle().clone();

    let (config, base_dir) = io_handle.block_on(async {
        let config_store = ConfigStore::discover()
            .unwrap_or_else(|_| ConfigStore::from_path(std::path::PathBuf::from("config.toml")));
        let config = config_store.load().unwrap_or_default();
        let base_dir = config_store
            .path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        (config, base_dir)
    });
    audio_policy::set_audio_runtime_policy(audio_policy::policy_from_config(&config));
    hotkeys::set_enabled(config.lyrics_shortcuts.enabled);

    let _log_guard = logger::init_logging(&config.log, &base_dir);
    tracing::info!(
        "音栖岛启动中... 运行模式: 异步多线程, 日志级别: {}",
        config.log.level
    );

    let plugin_engine_policy = plugin_engine_policy::PluginEnginePolicy::default();
    plugin_engine_policy.validate()?;
    let plugin_compiled_cache =
        plugin_compiled_cache::initialize(plugin_engine_policy.max_compiled_artifact_bytes);
    let plugin_host = plugin_host::initialize(&base_dir);
    let plugin_sessions = plugin_sessions::initialize(&plugin_host);
    let plugin_components = plugin_components::initialize(&base_dir);
    let plugin_catalog = match plugin_host.read() {
        Ok(host) => Some(host.catalog().clone()),
        Err(error) => {
            tracing::error!(%error, "读取插件目录用于权限初始化失败");
            None
        }
    };
    let plugin_permissions = plugin_catalog
        .as_ref()
        .map(|catalog| plugin_permissions::initialize(&base_dir, catalog));
    let plugin_runtime = match (plugin_catalog.as_ref(), plugin_permissions.as_ref()) {
        (Some(catalog), Some(permissions)) => Some(plugin_runtime::initialize(
            catalog.clone(),
            permissions.clone(),
            std::sync::Arc::new(plugin_secrets::MemorySecretStore::default()),
        )),
        _ => None,
    };

    tracing::info!(
        selected_wasmtime = plugin_components.selected_runtime_version(),
        cache_root = %plugin_components.cache_root().display(),
        max_compiled_artifact_bytes = plugin_compiled_cache.max_artifact_bytes(),
        "插件 Component 懒加载与 compiled cache 边界已初始化"
    );
    tracing::info!(
        max_store_memory_mib = plugin_engine_policy.max_memory_mib(),
        max_compiled_artifact_mib = plugin_engine_policy.max_compiled_artifact_mib(),
        fuel_per_call = plugin_engine_policy.fuel_per_call,
        epoch_tick_ms = plugin_engine_policy.epoch_tick_interval.as_millis(),
        epoch_deadline_ticks = plugin_engine_policy.epoch_deadline_ticks(),
        "Wasmtime Engine/Store 资源策略已校验"
    );
    if let Some(runtime) = plugin_runtime.as_ref() {
        tracing::info!(
            installed_plugins = runtime.catalog().plugins().len(),
            secret_backend = "memory-nonpersistent",
            "插件 Host service runtime 已初始化"
        );
    } else {
        tracing::warn!("插件 Host service runtime 未初始化，Catalog 或权限状态不可用");
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
        Err(error) => {
            tracing::error!(%error, "插件宿主状态锁已损坏");
        }
    }
    match plugin_sessions.read() {
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
    if let Some(plugin_permissions) = plugin_permissions.as_ref() {
        match plugin_permissions.read() {
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

    ensure_gpui_outside_tokio_runtime()?;

    lucide_assets::install();
    let window_icon = embedded_window_icon()?;
    let app = Application::new()
        .with_assets(lucide_gpui::Assets)
        .with_default_window_icon(window_icon);
    app.run(move |cx: &mut App| {
        gpui_tokio::init_from_handle(cx, io_handle);
        gpui_router::init(cx);

        let mut options = WindowOptions::default();
        let bounds = Bounds::centered(None, size(px(1120.0), px(720.0)), cx);
        options.window_bounds = Some(WindowBounds::Windowed(bounds));
        options.window_min_size = Some(size(px(800.0), px(560.0)));
        options.is_resizable = true;
        options.is_minimizable = true;
        options.is_movable = true;

        #[cfg(windows)]
        {
            options.titlebar = Some(TitlebarOptions {
                title: Some("音栖岛".into()),
                appears_transparent: true,
                ..Default::default()
            });
            options.window_background = gpui::WindowBackgroundAppearance::Opaque;
            options.window_corner_preference = WindowCornerPreference::Rounded;
        }

        let main_window = match cx.open_window(options, |_, cx| cx.new(|_| MusicApp::new(true))) {
            Ok(window) => window,
            Err(error) => {
                tracing::error!("打开音栖岛窗口失败: {error:#}");
                cx.quit();
                return;
            }
        };

        desktop_lyrics::initialize(main_window, cx);
        global_shortcuts::install_event_bridge(main_window, cx);
        audio::set_audio_debug_enabled(false);

        let main_window_id = main_window.window_id();
        cx.on_window_closed(move |cx| {
            let main_still_open = cx
                .windows()
                .into_iter()
                .any(|window| window.window_id() == main_window_id);
            if !main_still_open {
                desktop_lyrics::shutdown(cx);
                audio_debug_window::shutdown(cx);
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });

    hotkeys::shutdown();
    Ok(())
}

fn embedded_window_icon() -> Result<WindowIconSource> {
    let icon = image::load_from_memory_with_format(
        include_bytes!("../assets/brand/app-icon.png"),
        image::ImageFormat::Png,
    )?
    .into_rgba8();
    let (width, height) = icon.dimensions();
    WindowIconSource::from_rgba(width, height, icon.into_raw())
}

fn ensure_gpui_outside_tokio_runtime() -> Result<()> {
    anyhow::ensure!(
        tokio::runtime::Handle::try_current().is_err(),
        "GPUI event loop must not run inside a Tokio runtime context"
    );
    Ok(())
}
