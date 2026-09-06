mod artwork;
mod audio;
mod audio_debug_window;
mod audio_policy;
mod desktop_lyrics;
mod global_shortcuts;
mod gpu;
mod hotkeys;
mod library;
pub mod logger;
mod lyrics;
pub mod media_controls;
mod model;
mod online;
mod preferences;
pub mod runtime;
mod settings;
mod ui;
mod window_platform;

use anyhow::Result;
use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
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

    ensure_gpui_outside_tokio_runtime()?;

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

        let main_window = match cx.open_window(options, |_, cx| cx.new(|_| MusicApp::new(true))) {
            Ok(window) => window,
            Err(error) => {
                tracing::error!("打开音栖岛窗口失败: {error:#}");
                cx.quit();
                return;
            }
        };

        desktop_lyrics::start_ui_service(main_window.clone(), cx);
        global_shortcuts::start_ui_service(main_window.clone(), cx);

        if audio_debug_window::requested()
            && let Err(error) = audio_debug_window::open(cx)
        {
            tracing::warn!(error = %error, "Audio Laboratory debug 窗口打开失败");
        }

        // 主窗口是进程生命周期所有者。桌面歌词/Audio Laboratory 都只是辅助窗口，
        // 关闭主窗口后必须同步销毁，不能继续让 GPUI event loop 存活。
        let lifecycle_main = main_window.clone();
        cx.on_window_closed(move |cx| {
            if lifecycle_main
                .update(cx, |_app, _window, _cx| ())
                .is_err()
            {
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

fn ensure_gpui_outside_tokio_runtime() -> Result<()> {
    anyhow::ensure!(
        tokio::runtime::Handle::try_current().is_err(),
        "GPUI event loop must not run inside a Tokio runtime context"
    );
    Ok(())
}
