use std::{
    sync::{Arc, OnceLock},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
use wasmtime::component::{HasSelf, Linker, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use super::policy::PluginEnginePolicy;
use crate::plugin::{
    abi::KeyValue,
    host::{
        http::{PluginHttpRequest, PluginHttpResponse},
        runtime::{PluginHostServices, PluginStoreContext},
    },
};

const MAX_PLUGIN_LOG_BYTES: usize = 8 * 1024;
static WASMTIME_RUNTIME: OnceLock<Arc<PluginWasmtimeRuntime>> = OnceLock::new();

// wasmtime::component::bindgen! generates canonical ABI glue containing unavoidable unsafe code.
// Keep the exception inside this private generated-binding module; handwritten Host policy remains
// covered by the crate-level unsafe_code lint.
#[allow(unsafe_code)]
mod bindings {
    wasmtime::component::bindgen!({
        world: "music-plugin",
        path: "plugins/wit",
        imports: { default: async | trappable },
        exports: { default: async },
    });
}

use bindings::yinqidao::music_plugin::{host as wit_host, types as wit_types};

/// Private Store data for one plugin Component instance.
///
/// The plugin id is injected by the Host through `PluginStoreContext`; the guest never chooses its
/// own plugin namespace. WASI starts with an empty capability context and raw networking is disabled
/// explicitly. All network egress must therefore go through `host.http-request`.
struct PluginStoreData {
    host: PluginStoreContext,
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
}

impl WasiView for PluginStoreData {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl wit_host::Host for PluginStoreData {
    async fn http_request(
        &mut self,
        request: wit_host::HttpRequestData,
    ) -> wasmtime::Result<Result<wit_host::HttpResponse, String>> {
        let plugin_id = self.host.plugin_id().to_owned();
        let services = self.host.services().clone();
        let provider_id = request.provider_id;
        let account_id = request.account_id;
        let host_request = PluginHttpRequest {
            method: request.method,
            url: request.url,
            headers: request
                .headers
                .into_iter()
                .map(|header| KeyValue {
                    key: header.key,
                    value: header.value,
                })
                .collect(),
            body: request.body,
        };

        Ok(services
            .http_request(
                &plugin_id,
                &provider_id,
                account_id.as_deref(),
                host_request,
            )
            .await
            .map(http_response_to_wit)
            .map_err(|error| error.to_string()))
    }

    async fn secret_get(
        &mut self,
        scope: wit_host::SecretScope,
        key: String,
    ) -> wasmtime::Result<Result<Option<Vec<u8>>, String>> {
        Ok(self
            .host
            .services()
            .secret_get(
                self.host.plugin_id(),
                &scope.provider_id,
                scope.account_id.as_deref(),
                &key,
            )
            .map_err(|error| error.to_string()))
    }

    async fn secret_set(
        &mut self,
        scope: wit_host::SecretScope,
        key: String,
        value: Vec<u8>,
    ) -> wasmtime::Result<Result<bool, String>> {
        Ok(self
            .host
            .services()
            .secret_set(
                self.host.plugin_id(),
                &scope.provider_id,
                scope.account_id.as_deref(),
                &key,
                &value,
            )
            .map(|()| true)
            .map_err(|error| error.to_string()))
    }

    async fn secret_delete(
        &mut self,
        scope: wit_host::SecretScope,
        key: String,
    ) -> wasmtime::Result<Result<bool, String>> {
        Ok(self
            .host
            .services()
            .secret_delete(
                self.host.plugin_id(),
                &scope.provider_id,
                scope.account_id.as_deref(),
                &key,
            )
            .map_err(|error| error.to_string()))
    }

    async fn now_ms(&mut self) -> wasmtime::Result<u64> {
        Ok(self.host.services().now_ms())
    }

    async fn log(
        &mut self,
        level: wit_host::LogLevel,
        message: String,
    ) -> wasmtime::Result<()> {
        let message = bounded_log_message(&message);
        let plugin_id = self.host.plugin_id();
        match level {
            wit_host::LogLevel::Trace => tracing::trace!(plugin_id, %message, "plugin guest"),
            wit_host::LogLevel::Debug => tracing::debug!(plugin_id, %message, "plugin guest"),
            wit_host::LogLevel::Info => tracing::info!(plugin_id, %message, "plugin guest"),
            wit_host::LogLevel::Warn => tracing::warn!(plugin_id, %message, "plugin guest"),
            wit_host::LogLevel::Error => tracing::error!(plugin_id, %message, "plugin guest"),
        }
        Ok(())
    }
}

fn http_response_to_wit(response: PluginHttpResponse) -> wit_host::HttpResponse {
    wit_host::HttpResponse {
        status: response.status,
        headers: response
            .headers
            .into_iter()
            .map(|header| wit_types::KeyValue {
                key: header.key,
                value: header.value,
            })
            .collect(),
        body: response.body,
    }
}

fn bounded_log_message(message: &str) -> &str {
    if message.len() <= MAX_PLUGIN_LOG_BYTES {
        return message;
    }
    let mut end = MAX_PLUGIN_LOG_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
}

/// Process-wide Wasmtime engine and security policy.
///
/// Component bytes remain lazy-loaded by `component::registry`; constructing this object does not
/// read or compile any plugin package. Linkers and Stores are intentionally per-instantiation so no
/// guest-owned resource table or WASI state is shared across accounts/routes.
pub(crate) struct PluginWasmtimeRuntime {
    engine: Engine,
    services: Arc<PluginHostServices>,
    policy: PluginEnginePolicy,
}

impl PluginWasmtimeRuntime {
    fn new(services: Arc<PluginHostServices>, policy: PluginEnginePolicy) -> Result<Self> {
        policy.validate()?;

        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);

        let engine = Engine::new(&config).context("创建 Wasmtime 插件 Engine 失败")?;
        Ok(Self {
            engine,
            services,
            policy,
        })
    }

    fn linker(&self) -> Result<Linker<PluginStoreData>> {
        let mut linker = Linker::new(&self.engine);
        bindings::MusicPlugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .context("注册音栖岛插件 Host imports 失败")?;
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)
            .context("注册最小 WASI P2 imports 失败")?;
        Ok(linker)
    }

    fn store(&self, plugin_id: &str) -> Result<Store<PluginStoreData>> {
        let host = PluginStoreContext::new(plugin_id, self.services.clone())?;
        let mut wasi = WasiCtx::builder();
        // Defense in depth: deny WASI socket creation and name lookup. Plugins must use the
        // Host-mediated HTTP import, whose DNS/SSRF/permission checks remain authoritative.
        wasi.allow_ip_name_lookup(false)
            .allow_tcp(false)
            .allow_udp(false);

        let limits = StoreLimitsBuilder::new()
            .memory_size(self.policy.max_memory_bytes)
            .table_elements(self.policy.max_table_elements)
            .instances(self.policy.max_instances)
            .memories(self.policy.max_memories)
            .tables(self.policy.max_tables)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            PluginStoreData {
                host,
                wasi: wasi.build(),
                table: ResourceTable::new(),
                limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.policy.fuel_per_call)
            .context("设置插件 Store fuel 失败")?;
        store.set_epoch_deadline(self.policy.epoch_deadline_ticks());
        Ok(store)
    }

    /// Validate generated Host/WASI linker wiring before publishing the process-wide runtime.
    fn validate_linker(&self) -> Result<()> {
        let _ = self.linker()?;
        Ok(())
    }
}

fn start_epoch_driver(engine: Engine, interval: Duration) -> Result<()> {
    thread::Builder::new()
        .name("yinqidao-plugin-epoch".into())
        .spawn(move || loop {
            thread::sleep(interval);
            engine.increment_epoch();
        })
        .map(|_| ())
        .map_err(|error| anyhow!("启动 Wasmtime epoch driver 失败: {error}"))
}

pub(crate) fn initialize(
    services: Arc<PluginHostServices>,
    policy: PluginEnginePolicy,
) -> Result<Arc<PluginWasmtimeRuntime>> {
    if let Some(runtime) = WASMTIME_RUNTIME.get() {
        return Ok(runtime.clone());
    }

    let runtime = Arc::new(PluginWasmtimeRuntime::new(services, policy)?);
    runtime.validate_linker()?;
    start_epoch_driver(runtime.engine.clone(), runtime.policy.epoch_tick_interval)?;

    match WASMTIME_RUNTIME.set(runtime.clone()) {
        Ok(()) => Ok(runtime),
        Err(_) => WASMTIME_RUNTIME
            .get()
            .cloned()
            .ok_or_else(|| anyhow!("Wasmtime 插件 runtime 初始化竞态失败")),
    }
}

pub(crate) fn global() -> Option<Arc<PluginWasmtimeRuntime>> {
    WASMTIME_RUNTIME.get().cloned()
}
