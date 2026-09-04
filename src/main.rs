mod artwork;
mod audio;
mod library;
pub mod logger;
mod lyrics;
pub mod media_controls;
mod model;
mod online;
pub mod runtime;
mod settings;
mod ui;

use anyhow::Result;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};
use settings::ConfigStore;
use ui::MusicApp;

fn main() -> Result<()> {
    // 1. 初始化全局多线程 Tokio 异步运行时 (对标 bmcbl)
    let app_runtime = runtime::initialize_app_runtime()?;
    let io_handle = app_runtime.io_handle().clone();

    // 2. 异步读取配置与数据存储路径
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

    // 3. 初始化包含滚动归档和 debug 级别的 Tracing 日志系统
    let _log_guard = logger::init_logging(&config.log, &base_dir);
    tracing::info!(
        "音栖岛启动中... 运行模式: 异步多线程, 日志级别: {}",
        config.log.level
    );

    // 4. 确保进入 GPUI 主事件循环前，主线程不在 Tokio 运行时上下文中 (对标 bmcbl)
    ensure_gpui_outside_tokio_runtime()?;

    // 5. 进入 GPUI 专属主线程并通过 gpui_tokio 桥接注入 Tokio 句柄
    let app = Application::new().with_assets(lucide_gpui::Assets);
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
        }

        let result = cx.open_window(options, |_, cx| cx.new(|_| MusicApp::new(true)));

        if let Err(error) = result {
            tracing::error!("打开音栖岛窗口失败: {error:#}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });

    Ok(())
}

fn ensure_gpui_outside_tokio_runtime() -> Result<()> {
    anyhow::ensure!(
        tokio::runtime::Handle::try_current().is_err(),
        "GPUI event loop must not run inside a Tokio runtime context"
    );
    Ok(())
}
