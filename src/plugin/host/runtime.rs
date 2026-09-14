use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, OnceLock, RwLock},
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

static PLUGIN_RUNTIME: OnceLock<Arc<PluginHostServices>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PluginRuntimeLimits {
    pub max_concurrent_calls_per_route: usize,
    pub failure_threshold: u32,
    pub circuit_open_for: Duration,
    /// Wall-clock deadline for one guest export invocation. Wasmtime epoch interruption will use
    /// the same policy once the engine integration lands, while this timeout also protects the Host
    /// from a future that stalls outside Wasmtime itself.
    pub call_timeout: Duration,
    /// Backoff used when a provider returns HTTP 429 without an integer Retry-After value.
    pub rate_limit_default_backoff: Duration,
    /// Upper bound for plugin-controlled/server-provided Retry-After delays.
    pub rate_limit_max_backoff: Duration,
}

impl Default for PluginRuntimeLimits {
    fn default() -> Self {
        Self {
            max_concurrent_calls_per_route: 4,
            failure_threshold: 5,
            circuit_open_for: Duration::from_secs(30),
            call_timeout: Duration::from_secs(30),
            rate_limit_default_backoff: Duration::from_secs(30),
            rate_limit_max_backoff: Duration::from_secs(5 * 60),
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
    retry_not_before: Option<Instant>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginRouteHealthSnapshot {
    pub in_flight: usize,
    pub consecutive_failures: u32,
    pub circuit_open_for: Option<Duration>,
    pub retry_after: Option<Duration>,
    /// Snapshot hint only. `acquire_call` remains authoritative because another caller can race
    /// between planning and permit acquisition.
    pub saturated: bool,
}

impl PluginRouteHealthSnapshot {
    pub fn is_available(&self) -> bool {
        self.circuit_open_for.is_none() && self.retry_after.is_none() && !self.saturated
    }
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
        if limits.call_timeout.is_zero() {
            limits.call_timeout = Duration::from_millis(1);
        }
        if limits.rate_limit_max_backoff.is_zero() {
            limits.rate_limit_max_backoff = Duration::from_millis(1);
        }
        limits.rate_limit_default_backoff = limits
            .rate_limit_default_backoff
            .min(limits.rate_limit_max_backoff);
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
        // Deliberately do not clear `retry_not_before` on guest success. A guest may treat a 429 as
        // a handled result; the Host-level provider backoff must still protect future calls.
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
        let now = Instant::now();

        if let Some(until) = route.retry_not_before {
            if let Some(remaining) = until.checked_duration_since(now) {
                if !remaining.is_zero() {
                    bail!(
                        "插件路由处于限流退避: {}/{}，剩余 {} ms",
                        key.plugin_id,
                        key.provider_id.as_deref().unwrap_or("*"),
                        remaining.as_millis()
                    );
                }
            }
            route.retry_not_before = None;
        }

        if let Some(until) = route.circuit_open_until {
            if let Some(remaining) = until.checked_duration_since(now) {
                if !remaining.is_zero() {
                    bail!(
                        "插件路由熔断中: {}/{}，剩余 {} ms",
                        key.plugin_id,
                        key.provider_id.as_deref().unwrap_or("*"),
                        remaining.as_millis()
                    );
                }
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

    /// Execute one guest export under the shared route budget and wall-clock deadline.
    ///
    /// Generated Wasmtime bindings should use this wrapper for every exported provider operation.
    /// Wasmtime epoch interruption will provide engine-level preemption too; this outer deadline is
    /// still required for Host futures/import work around the guest call.
    pub async fn execute_guest_call<T, F>(&self, key: PluginCallKey, call: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let timeout = self
            .health
            .lock()
            .map_err(|error| anyhow!("插件运行健康状态锁已损坏: {error}"))?
            .limits
            .call_timeout;
        let permit = self.acquire_call(key.clone())?;
        match tokio::time::timeout(timeout, call).await {
            Ok(Ok(value)) => {
                permit.finish_success()?;
                Ok(value)
            }
            Ok(Err(error)) => {
                let _ = permit.finish_failure();
                Err(error)
            }
            Err(_) => {
                let _ = permit.finish_failure();
                bail!(
                    "插件调用超过 {} ms: {}/{}",
                    timeout.as_millis(),
                    key.plugin_id,
                    key.provider_id.as_deref().unwrap_or("*")
                )
            }
        }
    }

    /// Record provider throttling independently from the failure circuit. A 429 may be a valid API
    /// response that the guest handles, but future calls still need to respect server backoff.
    pub fn record_rate_limit(
        &self,
        key: &PluginCallKey,
        retry_after: Option<Duration>,
    ) -> Result<Duration> {
        self.validate_call_key(key)?;
        let mut health = self
            .health
            .lock()
            .map_err(|error| anyhow!("插件运行健康状态锁已损坏: {error}"))?;
        let limits = health.limits.clone();
        let applied = retry_after
            .unwrap_or(limits.rate_limit_default_backoff)
            .min(limits.rate_limit_max_backoff);
        let route = health.routes.entry(key.clone()).or_default();
        if applied.is_zero() {
            route.retry_not_before = None;
            return Ok(applied);
        }

        let candidate = Instant::now() + applied;
        route.retry_not_before = Some(match route.retry_not_before {
            Some(existing) if existing > candidate => existing,
            _ => candidate,
        });
        Ok(applied)
    }

    pub fn route_health(&self, key: &PluginCallKey) -> Result<PluginRouteHealthSnapshot> {
        self.validate_call_key(key)?;
        let health = self
            .health
            .lock()
            .map_err(|error| anyhow!("插件运行健康状态锁已损坏: {error}"))?;
        let route = health.routes.get(key).cloned().unwrap_or_default();
        let now = Instant::now();
        Ok(PluginRouteHealthSnapshot {
            in_flight: route.in_flight,
            consecutive_failures: route.consecutive_failures,
            circuit_open_for: remaining_deadline(route.circuit_open_until, now),
            retry_after: remaining_deadline(route.retry_not_before, now),
            saturated: route.in_flight >= health.limits.max_concurrent_calls_per_route,
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
        let response = self.http.execute(&plugin.manifest, &grant, request).await?;
        if response.status == 429 {
            let key = PluginCallKey::provider(plugin_id, provider_id);
            self.record_rate_limit(&key, retry_after_delay(&response))?;
        }
        Ok(response)
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

fn remaining_deadline(until: Option<Instant>, now: Instant) -> Option<Duration> {
    until
        .and_then(|until| until.checked_duration_since(now))
        .filter(|duration| !duration.is_zero())
}

fn retry_after_delay(response: &PluginHttpResponse) -> Option<Duration> {
    response
        .headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("retry-after"))
        .and_then(|header| header.value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
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

    pub fn call_key(&self, provider_id: Option<&str>) -> PluginCallKey {
        match provider_id {
            Some(provider_id) => PluginCallKey::provider(&self.plugin_id, provider_id),
            None => PluginCallKey::plugin(&self.plugin_id),
        }
    }

    pub async fn execute_guest_call<T, F>(
        &self,
        provider_id: Option<&str>,
        call: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.services
            .execute_guest_call(self.call_key(provider_id), call)
            .await
    }
}

/// Initialize the process-wide runtime-neutral Host service façade.
///
/// Wasmtime stores and generated bindings should always share this object so HTTP permissions,
/// Secret storage and route health are consistent across all plugin instances in the process.
pub fn initialize(
    catalog: PluginCatalog,
    permissions: Arc<RwLock<PluginPermissionState>>,
    secrets: Arc<dyn PluginSecretStore>,
) -> Arc<PluginHostServices> {
    PLUGIN_RUNTIME
        .get_or_init(|| {
            Arc::new(PluginHostServices::new(
                catalog,
                permissions,
                PluginHttpExecutor::default(),
                secrets,
                PluginRuntimeLimits::default(),
            ))
        })
        .clone()
}

pub fn global() -> Option<Arc<PluginHostServices>> {
    PLUGIN_RUNTIME.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::KeyValue;

    #[test]
    fn dropped_permit_counts_as_failure_and_eventually_opens_circuit() {
        let health = Arc::new(Mutex::new(RuntimeHealthState::new(PluginRuntimeLimits {
            max_concurrent_calls_per_route: 1,
            failure_threshold: 2,
            circuit_open_for: Duration::from_secs(60),
            ..PluginRuntimeLimits::default()
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
    fn retry_after_seconds_header_is_parsed() {
        let response = PluginHttpResponse {
            status: 429,
            headers: vec![KeyValue {
                key: "Retry-After".into(),
                value: "120".into(),
            }],
            body: Vec::new(),
        };
        assert_eq!(retry_after_delay(&response), Some(Duration::from_secs(120)));
    }

    #[test]
    fn saturated_route_snapshot_is_unavailable() {
        let snapshot = PluginRouteHealthSnapshot {
            in_flight: 4,
            saturated: true,
            ..PluginRouteHealthSnapshot::default()
        };
        assert!(!snapshot.is_available());
    }

    #[test]
    fn expired_deadline_is_not_reported_as_active() {
        let now = Instant::now();
        assert_eq!(remaining_deadline(Some(now), now), None);
    }

    #[test]
    fn empty_account_context_is_rejected() {
        assert!(validate_optional_account_id(Some(" ")).is_err());
        assert!(validate_optional_account_id(Some("user-1")).is_ok());
        assert!(validate_optional_account_id(None).is_ok());
    }
}
