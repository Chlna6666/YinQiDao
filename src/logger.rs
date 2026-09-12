use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::settings::LogConfig;

pub fn init_logging(config: &LogConfig, base_dir: &Path) -> Option<WorkerGuard> {
    let level_str = match config.level.to_lowercase().as_str() {
        // “debug” means application diagnostics, not every dependency's internal compiler/driver
        // trace. Keep third-party crates at a sane baseline so Naga/WGPU shader overload dumps do
        // not flood stdout while YinQiDao's own debug events remain visible. RUST_LOG still wins.
        "debug" => {
            "info,yin_qi_dao=debug,gpui=info,wgpu_core=warn,wgpu_hal=warn,naga=warn,symphonia=warn,reqwest=info"
        }
        "warn" | "warning" => "warn,yin_qi_dao=warn",
        "error" => "error,yin_qi_dao=error",
        _ => "info,yin_qi_dao=info,symphonia=warn,reqwest=info",
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level_str));

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_thread_names(true)
        .with_ansi(true);

    if config.file_logging {
        let logs_dir: PathBuf = base_dir.join("logs");
        if let Err(err) = std::fs::create_dir_all(&logs_dir) {
            eprintln!("创建日志目录失败: {err}");
            let subscriber = tracing_subscriber::registry()
                .with(filter)
                .with(stdout_layer);
            let _ = subscriber.try_init();
            return None;
        }

        let file_appender = tracing_appender::rolling::daily(&logs_dir, "yinqidao.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_thread_names(true)
            .with_writer(non_blocking);

        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer)
            .with(file_layer);

        let _ = subscriber.try_init();
        Some(guard)
    } else {
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(stdout_layer);
        let _ = subscriber.try_init();
        None
    }
}
