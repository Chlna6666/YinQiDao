use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use anyhow::{Context, Result, anyhow};

use super::{
    client::{self, PluginProviderClient},
    component::{gc, registry as component_registry},
    host::runtime::{self as host_runtime, PluginCallKey, PluginHostServices},
    ui::{
        self,
        client::{
            PluginUiClient, PluginUiEvent, PluginUiFuture, PluginUiResponse, UiCommandContext,
            UiCommandResponse,
        },
        schema::UiPageModel,
    },
};

static PORT_SWAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PORT_SWAP_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static PACKAGE_MUTATION_GENERATION: AtomicU64 = AtomicU64::new(1);
static HOT_SWAP_ADAPTER: OnceLock<Mutex<Option<Arc<dyn PluginRuntimeHotSwap>>>> = OnceLock::new();

struct PortSwapGuard {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for PortSwapGuard {
    fn drop(&mut self) {
        PORT_SWAP_IN_PROGRESS.store(false, Ordering::Release);
    }
}

/// Runtime-neutral generation hook implemented by the private Component adapter.
///
/// Package management calls this while Provider/UI reads are fail-closed. The adapter must advance
/// its plugin generation and drop every plugin-local compiled/warm state so calls created after this
/// method returns can only resolve the newly committed package. Checked-out old instances may finish
/// existing calls, but lifecycle generation tickets must prevent them from re-entering warm pools.
/// No Wasmtime type may cross this boundary.
pub(crate) trait PluginRuntimeHotSwap: Send + Sync {
    fn refresh_plugin(&self, plugin_id: &str) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimePortSwapReport {
    pub replaced_provider_client: bool,
    pub replaced_ui_client: bool,
    pub invalidated_ui_pages: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimePluginRefreshReport {
    pub invalidated_component_snapshot: bool,
    pub released_runtime_resources: usize,
    pub invalidated_ui_pages: usize,
    pub adapter_refreshed: bool,
    /// True means the package operation committed, but runtime refresh could not be proven safe.
    /// Provider/UI ports are cleared before the swap gate is released, so this is fail-closed and a
    /// later adapter reinstall/restart is required instead of continuing on stale guest code.
    pub provider_runtime_refresh_pending: bool,
}

pub(crate) fn is_swapping() -> bool {
    PORT_SWAP_IN_PROGRESS.load(Ordering::Acquire)
}

/// Monotonic Host-owned token for package mutations coordinated through this runtime gate.
/// UI/application snapshots may compare it without touching package-manager or runtime locks.
pub(crate) fn package_mutation_generation() -> u64 {
    PACKAGE_MUTATION_GENERATION.load(Ordering::Acquire)
}

fn bump_package_mutation_generation() {
    PACKAGE_MUTATION_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn begin_swap() -> Result<PortSwapGuard> {
    let guard = PORT_SWAP_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| anyhow!("插件 runtime port swap 锁已损坏: {error}"))?;
    PORT_SWAP_IN_PROGRESS.store(true, Ordering::Release);
    Ok(PortSwapGuard { _guard: guard })
}

fn hot_swap_adapter() -> &'static Mutex<Option<Arc<dyn PluginRuntimeHotSwap>>> {
    HOT_SWAP_ADAPTER.get_or_init(|| Mutex::new(None))
}

/// Execute one package-management mutation while new Provider/UI calls are fail-closed, then
/// invalidate every Host-owned plugin runtime generation before releasing the gate.
///
/// `plugin_id` runs only after the mutation succeeds, which lets imports discover the canonical id
/// from the validated package while still keeping the entire commit inside the same swap window.
/// Returning `None` means the operation was a no-op and no generation needs to change.
///
/// A package operation can fail after an atomic filesystem replacement but before every registry
/// tail step has completed. Such an error therefore cannot safely reopen the previous adapter. The
/// error path clears all executable runtime ports/caches before releasing the swap gate.
pub(crate) fn coordinate_plugin_change<T, F, K>(
    change: F,
    plugin_id: K,
) -> Result<(T, RuntimePluginRefreshReport)>
where
    F: FnOnce() -> Result<T>,
    K: FnOnce(&T) -> Option<String>,
{
    let _swap = begin_swap()?;
    let result = match change() {
        Ok(result) => result,
        Err(error) => {
            fail_closed_after_mutation_error();
            // The package manager documents that an error may happen after the atomic filesystem
            // replacement. Conservatively invalidate Host snapshots even when the final outcome is
            // uncertain; an extra refresh is safe, a stale Provider/account snapshot is not.
            bump_package_mutation_generation();
            return Err(error);
        }
    };
    let Some(plugin_id) = plugin_id(&result) else {
        return Ok((result, RuntimePluginRefreshReport::default()));
    };
    let refresh = refresh_plugin_under_swap(&plugin_id);
    bump_package_mutation_generation();
    Ok((result, refresh))
}

fn fail_closed_after_mutation_error() {
    debug_assert!(is_swapping());
    if let Some(cache) = ui::page_cache::global()
        && let Err(error) = cache.clear()
    {
        tracing::error!(%error, "插件管理失败后清空 UI page cache 失败");
    }
    if let Some(components) = component_registry::global()
        && let Err(error) = components.clear()
    {
        tracing::error!(%error, "插件管理失败后清空 Component snapshot cache 失败");
    }
    if let Err(error) = clear_ports_under_swap() {
        tracing::error!(%error, "插件管理失败后清空 runtime ports 失败");
    }
}

fn refresh_plugin_under_swap(plugin_id: &str) -> RuntimePluginRefreshReport {
    debug_assert!(is_swapping());
    let mut report = RuntimePluginRefreshReport::default();
    let mut refresh_failed = false;

    if let Some(cache) = ui::page_cache::global() {
        match cache.invalidate_plugin(plugin_id) {
            Ok(invalidated) => report.invalidated_ui_pages = invalidated,
            Err(error) => {
                refresh_failed = true;
                tracing::error!(%error, %plugin_id, "热替换时失效插件 UI page generation 失败");
            }
        }
    }

    if let Some(components) = component_registry::global() {
        match components.invalidate(plugin_id) {
            Ok(invalidated) => report.invalidated_component_snapshot = invalidated,
            Err(error) => {
                refresh_failed = true;
                tracing::error!(%error, %plugin_id, "热替换时失效插件 Component snapshot 失败");
            }
        }
    }

    if let Some(controller) = gc::global() {
        match controller.invalidate_plugin(plugin_id) {
            Ok(released) => report.released_runtime_resources = released,
            Err(error) => {
                refresh_failed = true;
                tracing::error!(%error, %plugin_id, "热替换时回收插件 warm runtime 资源失败");
            }
        }
    }

    let adapter = match hot_swap_adapter().lock() {
        Ok(adapter) => adapter.clone(),
        Err(error) => {
            refresh_failed = true;
            tracing::error!(%error, %plugin_id, "读取插件 hot-swap adapter 失败");
            None
        }
    };
    if let Some(adapter) = adapter {
        match adapter.refresh_plugin(plugin_id) {
            Ok(()) => report.adapter_refreshed = true,
            Err(error) => {
                refresh_failed = true;
                tracing::error!(%error, %plugin_id, "插件 Component adapter generation 刷新失败");
            }
        }
    }

    if refresh_failed {
        report.provider_runtime_refresh_pending = true;
        if let Err(error) = clear_ports_under_swap() {
            tracing::error!(%error, %plugin_id, "热替换失败后清空插件 runtime ports 失败");
        }
    }
    report
}

/// Host-owned proxy for every plugin-level UI export. Provider calls already pass through
/// `PluginServiceFrontend`; this proxy gives UI page/event/command calls the same concurrency,
/// deadline and circuit policy without making `plugin::ui` depend on Host runtime internals.
struct BudgetedUiAdapter<T> {
    inner: Arc<T>,
    runtime: Arc<PluginHostServices>,
}

impl<T> PluginUiClient for BudgetedUiAdapter<T>
where
    T: PluginUiClient + Send + Sync + 'static,
{
    fn load_page<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
    ) -> PluginUiFuture<'a, UiPageModel> {
        Box::pin(async move {
            self.runtime
                .execute_guest_call(
                    PluginCallKey::plugin(plugin_id),
                    self.inner.load_page(plugin_id, page_id),
                )
                .await
        })
    }

    fn handle_event<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
        event: PluginUiEvent,
    ) -> PluginUiFuture<'a, PluginUiResponse> {
        Box::pin(async move {
            self.runtime
                .execute_guest_call(
                    PluginCallKey::plugin(plugin_id),
                    self.inner.handle_event(plugin_id, page_id, event),
                )
                .await
        })
    }

    fn invoke_command<'a>(
        &'a self,
        plugin_id: &'a str,
        command_id: &'a str,
        context: UiCommandContext,
    ) -> PluginUiFuture<'a, UiCommandResponse> {
        Box::pin(async move {
            self.runtime
                .execute_guest_call(
                    PluginCallKey::plugin(plugin_id),
                    self.inner.invoke_command(plugin_id, command_id, context),
                )
                .await
        })
    }
}

/// Install one Component adapter into both semantic ports and the package hot-swap hook.
///
/// The adapter type is shared so Provider and UI calls always refer to the same runtime generation.
/// Provider calls are budgeted by `PluginServiceFrontend`; UI calls are published through a
/// `BudgetedUiAdapter` proxy using the same Host runtime policy. Readers fail closed while all three
/// slots are replaced. UI page snapshots are invalidated before the swap, preventing models produced
/// by the previous adapter from surviving a runtime reload.
#[allow(dead_code)]
pub(crate) fn install<T>(adapter: Arc<T>) -> Result<RuntimePortSwapReport>
where
    T: PluginProviderClient + PluginUiClient + PluginRuntimeHotSwap + 'static,
{
    let _swap = begin_swap()?;
    let invalidated_ui_pages = if let Some(cache) = ui::page_cache::global() {
        cache.clear()?
    } else {
        0
    };

    let runtime = host_runtime::global()
        .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化，拒绝安装 Component adapter"))?;
    let provider_clients = client::initialize();
    let ui_clients = ui::client::initialize();
    let provider_adapter: Arc<dyn PluginProviderClient> = adapter.clone();
    let hot_adapter: Arc<dyn PluginRuntimeHotSwap> = adapter.clone();
    let ui_adapter: Arc<dyn PluginUiClient> = Arc::new(BudgetedUiAdapter {
        inner: adapter,
        runtime,
    });

    let previous_provider = provider_clients.install(provider_adapter)?;
    let replaced_provider_client = previous_provider.is_some();
    let previous_ui = match ui_clients.install(ui_adapter) {
        Ok(previous) => previous,
        Err(error) => {
            restore_provider(&provider_clients, previous_provider.clone());
            return Err(error).context("安装插件 UI client 失败，Provider client 已回滚");
        }
    };

    if let Err(error) = hot_swap_adapter()
        .lock()
        .map(|mut slot| {
            slot.replace(hot_adapter);
        })
        .map_err(|error| anyhow!("插件 hot-swap adapter registry 锁已损坏: {error}"))
    {
        restore_ui(&ui_clients, previous_ui.clone());
        restore_provider(&provider_clients, previous_provider);
        return Err(error).context("安装插件 hot-swap adapter 失败，Provider/UI client 已回滚");
    }

    Ok(RuntimePortSwapReport {
        replaced_provider_client,
        replaced_ui_client: previous_ui.is_some(),
        invalidated_ui_pages,
    })
}

fn restore_provider(
    clients: &Arc<client::PluginClientRegistry>,
    previous: Option<Arc<dyn PluginProviderClient>>,
) {
    match previous {
        Some(previous) => {
            let _ = clients.install(previous);
        }
        None => {
            let _ = clients.clear();
        }
    }
}

fn restore_ui(
    clients: &Arc<ui::client::PluginUiClientRegistry>,
    previous: Option<Arc<dyn PluginUiClient>>,
) {
    match previous {
        Some(previous) => {
            let _ = clients.install(previous);
        }
        None => {
            let _ = clients.clear();
        }
    }
}

/// Clear every executable runtime slot without rollback. All slots are attempted even if one lock
/// is poisoned, so a failure in one registry cannot keep an otherwise-clearable stale adapter live.
fn clear_ports_under_swap() -> Result<(bool, bool)> {
    debug_assert!(is_swapping());
    let provider_clients = client::initialize();
    let ui_clients = ui::client::initialize();
    let mut first_error = None;

    let replaced_provider_client = match provider_clients.clear() {
        Ok(previous) => previous.is_some(),
        Err(error) => {
            first_error = Some(anyhow!("清理插件 Provider client 失败: {error:#}"));
            false
        }
    };
    let replaced_ui_client = match ui_clients.clear() {
        Ok(previous) => previous.is_some(),
        Err(error) => {
            if first_error.is_none() {
                first_error = Some(anyhow!("清理插件 UI client 失败: {error:#}"));
            }
            false
        }
    };
    if let Err(error) = hot_swap_adapter()
        .lock()
        .map(|mut adapter| {
            adapter.take();
        })
    {
        if first_error.is_none() {
            first_error = Some(anyhow!("清理插件 hot-swap adapter 失败: {error}"));
        }
    }

    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok((replaced_provider_client, replaced_ui_client))
    }
}

/// Remove both semantic ports and the hot-swap hook under the same read gate. Existing in-flight
/// calls retain their cloned `Arc`; new calls fail closed until all runtime slots are cleared.
#[allow(dead_code)]
pub(crate) fn clear() -> Result<RuntimePortSwapReport> {
    let _swap = begin_swap()?;
    let invalidated_ui_pages = if let Some(cache) = ui::page_cache::global() {
        cache.clear()?
    } else {
        0
    };
    let (replaced_provider_client, replaced_ui_client) = clear_ports_under_swap()?;

    Ok(RuntimePortSwapReport {
        replaced_provider_client,
        replaced_ui_client,
        invalidated_ui_pages,
    })
}
