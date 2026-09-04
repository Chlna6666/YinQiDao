use std::sync::OnceLock;
use tokio::runtime::{Builder as TokioRuntimeBuilder, Handle, Runtime};

static APP_RUNTIME: OnceLock<AppRuntime> = OnceLock::new();
static COMPUTE_POOL: OnceLock<Result<rayon::ThreadPool, String>> = OnceLock::new();

pub struct AppRuntime {
    io: Runtime,
}

impl AppRuntime {
    fn build() -> anyhow::Result<Self> {
        let logical_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let io_workers = logical_threads.max(2);

        let io = TokioRuntimeBuilder::new_multi_thread()
            .enable_all()
            .worker_threads(io_workers)
            .thread_name("yinqidao-io")
            .build()?;

        Ok(Self { io })
    }

    pub fn io_handle(&self) -> &Handle {
        self.io.handle()
    }

    pub fn spawn_blocking<T, F>(&self, operation: F) -> tokio::task::JoinHandle<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.io.spawn_blocking(operation)
    }
}

pub fn initialize_app_runtime() -> anyhow::Result<&'static AppRuntime> {
    if let Some(runtime) = APP_RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = AppRuntime::build()?;
    let _ = APP_RUNTIME.set(runtime);
    Ok(APP_RUNTIME.get().expect("runtime must be initialized"))
}

pub fn app_runtime() -> Result<&'static AppRuntime, String> {
    APP_RUNTIME
        .get()
        .ok_or_else(|| "应用运行时尚未初始化".to_string())
}

pub fn io_handle() -> Result<Handle, String> {
    Ok(app_runtime()?.io_handle().clone())
}

pub fn spawn_blocking<T, F>(operation: F) -> Result<tokio::task::JoinHandle<T>, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Ok(app_runtime()?.spawn_blocking(operation))
}

pub fn compute_pool() -> Result<&'static rayon::ThreadPool, String> {
    match COMPUTE_POOL.get_or_init(|| {
        let logical_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        let worker_threads = logical_threads.saturating_sub(2).clamp(1, 4);
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_threads)
            .thread_name(|index| format!("yinqidao-compute-{index}"))
            .build()
            .map_err(|error| format!("创建后台计算线程池失败: {error}"))
    }) {
        Ok(pool) => Ok(pool),
        Err(error) => Err(error.clone()),
    }
}
