use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};

use crate::{
    plugin_host::PluginCatalog,
    plugin_http::{PluginHttpExecutor, PluginHttpRequest, PluginHttpResponse},
    plugin_permissions::PluginPermissionState,
    plugin_security::SecretSlot,
    plugin_secrets::PluginSecretStore,
};

#[derive(Clone, Debug)]
pub struct PluginRuntimeLimits {
    pub max_concurrent_calls_per_route: usize,
    pub failure_threshold: u32,
    pub circuit_open_for: Duration,
}

impl Default for PluginRuntimeLimits {
    fn default() -> Self {
        Self {
            max_concurrent_calls_per_route: 4,
            failure_threshold: 5,
            circuit_open_for: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginCallKey {
    pub plugin_id: String,
    pub provider_id: Option<String>,
}

impl PluginCallKey {
    pub fn plugin(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            provider_id: None,
        }
    }

    pub fn provider(
        plugin_id: impl Into<String>,
        provider_id: impl Into<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            provider_id: Some(provider_id.into()),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct RouteHealth {
    in_flight: usize,
    consecutive_failures: u32,
    circuit_open_until: Option<Instant>,
}

#[derive(Debug)]
struct RuntimeHealthState {
    limits: PluginRuntimeLimits,
    routes: HashMap<PluginCallKey, RouteHealth>,
}

impl RuntimeHealthState {
    fn new(mut limits: PluginRuntimeLimits) -> Self {
        limits.max_concurrent_calls_per_route = limits.max_concurrent_calls_per_route.max(1);
        limits.failure_threshold = limits.failure_threshold.max(1);
        Self {
            limits,
            routes: HashMap::new(),
        }
    }
}

/// Permit representing one exported plugin call.
///
/// The Wasmtime integration must acquire a permit before invoking a guest export and finish it as
/// success/failure afterwards. Dropping an unfinished permit is treated as a failure, which makes
/// cancellation/panic/timeout fail closed and feeds the circuit breaker.
pub struct PluginCallPermit {
    key: PluginCallKey,
    health: Arc<Mutex<RuntimeHealthState>>,
    finished: bool,
}

impl PluginCallPermit {
    pub fn finish_success(mut self) -> Result<()> {
        self.finish(true)
    }

    pub fn finish_failure(mut self) -> Result<()> {
        self.finish(false)
    }

    fn finish(&mut self, success: bool) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let mut health = self
            .health
            .lock()
            .map_err(|error| anyhow!("插件运行健康状态锁已损坏: {error}"))?;
        let limits = health.limits.clone();
        let route = health.routes.entry(self.key.clone()).or_default();
        route.in_flight = route.in_flight.saturating_sub(1);
        if success {
            route.consecutive_failures = 0;
            route.circuit_open_until = None;
        } else {
            route.consecutive_failures = route.consecutive_failures.saturating_add(1);
            if route.consecutive_failures >= limits.failure_threshold {
                route.circuit_open_until = Some(Instant::now() + limits.circuit_open_for);
            }
        }
        self.finished = true;
        Ok(())
    }
}

impl Drop for PluginCallPermit {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Ok(mut health) = self.health.lock() else {
            return;
        };
        let limits = health.limits.clone();
        let route = health.routes.entry(self.key.clone()).or_default();
        route.in_flight = route.in_flight.saturating_sub(1);
        route.consecutive_failures = route.consecutive_failures.saturating_add(1);
        if route.consecutive_failures >= limits.failure_threshold {
            route.circuit_open_until = Some(Instant::now() + limits.circuit_open_for);
        }
        self.finished = true;
    }
}

/// Runtime-neutral Host services that generated Wasmtime Component bindings will delegate to.
///
/// This type deliberately contains no Wasmtime objects. That keeps permission, Secret, networking,
/// rate/circuit logic independently testable and prevents the engine integration from becoming the
/// authority for security decisions.
#[derive(Clone)]
pub struct PluginHostServices {
    catalog: PluginCatalog,
    permissions: Arc<RwLock<PluginPermissionState>>,
    http: PluginHttpExecutor,
    secrets: Arc<dyn PluginSecretStore>,
    health: Arc<Mutex<RuntimeHealthState>>,
}

impl PluginHostServices {
    pub fn new(
        catalog: PluginCatalog,
        permissions: Arc<RwLock<PluginPermissionState>>,
        http: PluginHttpExecutor,
        secrets: Arc<dyn PluginSecretStore>,
        limits: PluginRuntimeLimits,
    ) -> Self {
        Self {
            catalog,
            permissions,
            http,
            secrets,
            health: Arc::new(Mutex::new(RuntimeHealthState::new(limits))),
        }
    }

    pub fn catalog(&self) -> &PluginCatalog {
        &self.catalog
    }

    pub fn acquire_call(&self, key: PluginCallKey) -> Result<PluginCallPermit> {
        self.validate_call_key(&key)?;
        let mut health = self
            .health
            .lock()
            .map_err(|error| anyhow!("插件运行健康状态锁已损坏: {error}"))?;
        let limits = health.limits.clone();
        let route = health.routes.entry(key.clone()).or_default();
        if let Some(until) = route.circuit_open_until {
            if until > Instant::now() {
                bail!(
                    "插件路由熔断中: {}/{}",
                    key.plugin_id,
                    key.provider_id.as_deref().unwrap_or("*")
                );
            }
            route.circuit_open_until = None;
            route.consecutive_failures = 0;
        }
        if route.in_flight >= limits.max_concurrent_calls_per_route {
            bail!(
                "插件路由并发超过限制 {}: {}/{}",
                limits.max_concurrent_calls_per_route,
                key.plugin_id,
                key.provider_id.as_deref().unwrap_or("*")
            );
        }
        route.in_flight += 1;
        Ok(PluginCallPermit {
            key,
            health: self.health.clone(),
            finished: false,
        })
    }

    pub async fn http_request(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account_id: Option<&str>,
        request: PluginHttpRequest,
    ) -> Result<PluginHttpResponse> {
        let plugin = self
            .catalog
            .plugin(plugin_id)
            .ok_or_else(|| anyhow!("未安装插件: {plugin_id}"))?;
        if plugin.provider(provider_id).is_none() {
            bail!("插件 {plugin_id} 未声明 provider {provider_id}");
        }
        validate_optional_account_id(account_id)?;
        let grant = self
            .permissions
            .read()
            .map_err(|error| anyhow!("插件权限状态锁已损坏: {error}"))?
            .grant_for(plugin_id)
            .cloned()
            .ok_or_else(|| anyhow!("插件 {plugin_id} 尚未获得网络权限"))?;
        self.http.execute(&plugin.manifest, &grant, request).await
    }

    pub fn secret_get(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account_id: Option<&str>,
        key: &str,
    ) -> Result<Option<Vec<u8>>> {
        let slot = self.secret_slot(plugin_id, provider_id, account_id, key)?;
        self.secrets.get(&slot)
    }

    pub fn secret_set(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account_id: Option<&str>,
        key: &str,
        value: &[u8],
    ) -> Result<()> {
        let slot = self.secret_slot(plugin_id, provider_id, account_id, key)?;
        self.secrets.set(&slot, value)
    }

    pub fn secret_delete(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account_id: Option<&str>,
        key: &str,
    ) -> Result<bool> {
        let slot = self.secret_slot(plugin_id, provider_id, account_id, key)?;
        self.secrets.delete(&slot)
    }

    pub fn revoke_all_plugin_secrets(&self, plugin_id: &str) -> Result<usize> {
        if self.catalog.plugin(plugin_id).is_none() {
            bail!("未安装插件: {plugin_id}");
        }
        self.secrets.delete_plugin(plugin_id)
    }

    pub fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    fn secret_slot(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account_id: Option<&str>,
        key: &str,
    ) -> Result<SecretSlot> {
        let plugin = self
            .catalog
            .plugin(plugin_id)
            .ok_or_else(|| anyhow!("未安装插件: {plugin_id}"))?;
        if plugin.provider(provider_id).is_none() {
            bail!("插件 {plugin_id} 未声明 provider {provider_id}");
        }
        validate_optional_account_id(account_id)?;
        match account_id {
            Some(account_id) => SecretSlot::account(plugin_id, provider_id, account_id, key),
            None => SecretSlot::provider(plugin_id, provider_id, key),
        }
    }

    fn validate_call_key(&self, key: &PluginCallKey) -> Result<()> {
        let plugin = self
            .catalog
            .plugin(&key.plugin_id)
            .ok_or_else(|| anyhow!("未安装插件: {}", key.plugin_id))?;
        if let Some(provider_id) = key.provider_id.as_deref()
            && plugin.provider(provider_id).is_none()
        {
            bail!("插件 {} 未声明 provider {provider_id}", key.plugin_id);
        }
        Ok(())
    }
}

fn validate_optional_account_id(account_id: Option<&str>) -> Result<()> {
    if let Some(account_id) = account_id
        && (account_id.trim().is_empty() || account_id.len() > 512 || account_id.contains('\0'))
    {
        bail!("插件 account id 非法");
    }
    Ok(())
}

/// Per-Store identity passed to Wasmtime generated Host import traits.
///
/// `plugin_id` is injected by the Host when a specific component is instantiated; the guest never
/// supplies or overrides it. Provider/account ids remain explicit WIT parameters and are validated
/// against the plugin catalog before Host services are reached.
#[derive(Clone)]
pub struct PluginStoreContext {
    plugin_id: String,
    services: Arc<PluginHostServices>,
}

impl PluginStoreContext {
    pub fn new(plugin_id: impl Into<String>, services: Arc<PluginHostServices>) -> Result<Self> {
        let plugin_id = plugin_id.into();
        if services.catalog.plugin(&plugin_id).is_none() {
            bail!("不能为未安装插件创建 Store context: {plugin_id}");
        }
        Ok(Self {
            plugin_id,
            services,
        })
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn services(&self) -> &Arc<PluginHostServices> {
        &self.services
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_permit_counts_as_failure_and_eventually_opens_circuit() {
        let health = Arc::new(Mutex::new(RuntimeHealthState::new(PluginRuntimeLimits {
            max_concurrent_calls_per_route: 1,
            failure_threshold: 2,
            circuit_open_for: Duration::from_secs(60),
        })));
        let key = PluginCallKey::provider("plugin.test", "qqmusic");

        for _ in 0..2 {
            {
                let mut state = health.lock().expect("health");
                state.routes.entry(key.clone()).or_default().in_flight += 1;
            }
            drop(PluginCallPermit {
                key: key.clone(),
                health: health.clone(),
                finished: false,
            });
        }

        let state = health.lock().expect("health");
        let route = state.routes.get(&key).expect("route");
        assert_eq!(route.consecutive_failures, 2);
        assert!(route.circuit_open_until.is_some());
    }

    #[test]
    fn empty_account_context_is_rejected() {
        assert!(validate_optional_account_id(Some(" ")).is_err());
        assert!(validate_optional_account_id(Some("user-1")).is_ok());
        assert!(validate_optional_account_id(None).is_ok());
    }
}
