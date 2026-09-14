use std::sync::{
    Arc, Mutex, MutexGuard, OnceLock,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result, anyhow};

use super::{
    client::{self, PluginProviderClient},
    ui::{self, client::PluginUiClient},
};

static PORT_SWAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PORT_SWAP_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct PortSwapGuard {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for PortSwapGuard {
    fn drop(&mut self) {
        PORT_SWAP_IN_PROGRESS.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) struct RuntimePortSwapReport {
    pub replaced_provider_client: bool,
    pub replaced_ui_client: bool,
    pub invalidated_ui_pages: usize,
}

pub(crate) fn is_swapping() -> bool {
    PORT_SWAP_IN_PROGRESS.load(Ordering::Acquire)
}

fn begin_swap() -> Result<PortSwapGuard> {
    let guard = PORT_SWAP_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|error| anyhow!("插件 runtime port swap 锁已损坏: {error}"))?;
    PORT_SWAP_IN_PROGRESS.store(true, Ordering::Release);
    Ok(PortSwapGuard { _guard: guard })
}

/// Install one Component adapter into both semantic ports.
///
/// The adapter type is shared so Provider and UI calls always refer to the same runtime generation.
/// Readers fail closed while the two registry slots are being replaced. UI page snapshots are
/// invalidated before the swap, preventing models produced by the previous adapter from surviving a
/// runtime reload. If the second registry update fails, the first is rolled back before the gate is
/// reopened.
#[allow(dead_code)]
pub(crate) fn install<T>(adapter: Arc<T>) -> Result<RuntimePortSwapReport>
where
    T: PluginProviderClient + PluginUiClient + 'static,
{
    let _swap = begin_swap()?;
    let invalidated_ui_pages = if let Some(cache) = ui::page_cache::global() {
        cache.clear()?
    } else {
        0
    };

    let provider_clients = client::initialize();
    let ui_clients = ui::client::initialize();
    let provider_adapter: Arc<dyn PluginProviderClient> = adapter.clone();
    let ui_adapter: Arc<dyn PluginUiClient> = adapter;

    let previous_provider = provider_clients.install(provider_adapter)?;
    let replaced_provider_client = previous_provider.is_some();
    let previous_ui = match ui_clients.install(ui_adapter) {
        Ok(previous) => previous,
        Err(error) => {
            match previous_provider {
                Some(previous) => {
                    let _ = provider_clients.install(previous);
                }
                None => {
                    let _ = provider_clients.clear();
                }
            }
            return Err(error).context("安装插件 UI client 失败，Provider client 已回滚");
        }
    };

    Ok(RuntimePortSwapReport {
        replaced_provider_client,
        replaced_ui_client: previous_ui.is_some(),
        invalidated_ui_pages,
    })
}

/// Remove both semantic ports under the same read gate. This is used by a future Component runtime
/// shutdown/rebuild path. Existing in-flight calls retain their cloned `Arc`; new calls fail closed
/// until both registry slots are cleared.
#[allow(dead_code)]
pub(crate) fn clear() -> Result<RuntimePortSwapReport> {
    let _swap = begin_swap()?;
    let invalidated_ui_pages = if let Some(cache) = ui::page_cache::global() {
        cache.clear()?
    } else {
        0
    };

    let provider_clients = client::initialize();
    let ui_clients = ui::client::initialize();
    let previous_provider = provider_clients.clear()?;
    let replaced_provider_client = previous_provider.is_some();
    let previous_ui = match ui_clients.clear() {
        Ok(previous) => previous,
        Err(error) => {
            if let Some(previous) = previous_provider {
                let _ = provider_clients.install(previous);
            }
            return Err(error).context("清理插件 UI client 失败，Provider client 已回滚");
        }
    };

    Ok(RuntimePortSwapReport {
        replaced_provider_client,
        replaced_ui_client: previous_ui.is_some(),
        invalidated_ui_pages,
    })
}
