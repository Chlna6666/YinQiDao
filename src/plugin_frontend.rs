use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{Result, anyhow, bail};

use crate::{
    plugin_client::PluginClientRegistry,
    plugin_host::PluginHostState,
    plugin_route_gate::{GatedRoutePlan, plan_routes},
    plugin_runtime::{PluginCallKey, PluginHostServices},
    plugin_sessions::PluginSessionCoordinator,
    plugins::{
        PluginLyricDocument, PluginRoute, RemoteTrack, RoutingPolicy, ServiceKind, SourceTrackRef,
        TrackQuery,
    },
};

static PLUGIN_FRONTEND: OnceLock<Arc<PluginServiceFrontend>> = OnceLock::new();

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCallFailure {
    pub route: PluginRoute,
    pub error: String,
}

#[derive(Clone, Debug)]
pub struct PluginSingleResult<T> {
    pub value: Option<T>,
    pub route: Option<PluginRoute>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    /// False means no Wasmtime/provider adapter is installed yet. Callers should use normal
    /// built-in/local fallback rather than treating that as a provider failure.
    pub client_ready: bool,
}

impl<T> PluginSingleResult<T> {
    fn unavailable(plan: GatedRoutePlan) -> Self {
        Self {
            value: None,
            route: None,
            plan,
            failures: Vec::new(),
            client_ready: false,
        }
    }
}

/// Unified authenticated-plugin execution frontend.
///
/// This is the only layer ordinary online features should call. It freezes the security/routing
/// order as: current-process session gate -> runtime health gate -> provider client snapshot ->
/// Host call permit/deadline -> operation. Wasmtime adapters therefore cannot become the authority
/// for account eligibility, concurrency, timeout, rate-limit or circuit policy.
pub struct PluginServiceFrontend {
    host: Arc<RwLock<PluginHostState>>,
    sessions: Arc<RwLock<PluginSessionCoordinator>>,
    runtime: Arc<PluginHostServices>,
    clients: Arc<PluginClientRegistry>,
}

impl std::fmt::Debug for PluginServiceFrontend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginServiceFrontend")
            .field("client_ready", &self.clients.is_ready().unwrap_or(false))
            .finish_non_exhaustive()
    }
}

impl PluginServiceFrontend {
    pub fn new(
        host: Arc<RwLock<PluginHostState>>,
        sessions: Arc<RwLock<PluginSessionCoordinator>>,
        runtime: Arc<PluginHostServices>,
        clients: Arc<PluginClientRegistry>,
    ) -> Self {
        Self {
            host,
            sessions,
            runtime,
            clients,
        }
    }

    /// Build a route plan while respecting the global lock order: sessions -> host.
    ///
    /// Session mutation paths hold the session write lock before touching Host account state, so
    /// taking these read locks in the opposite order would create an ABBA deadlock opportunity.
    pub fn plan(&self, service: ServiceKind, policy: &RoutingPolicy) -> Result<GatedRoutePlan> {
        let sessions = self
            .sessions
            .read()
            .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?;
        let host = self
            .host
            .read()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?;
        Ok(plan_routes(
            host.router(),
            &sessions,
            self.runtime.as_ref(),
            service,
            policy,
        ))
    }

    /// Resolve a local/canonical identity through authenticated provider APIs.
    ///
    /// Single-route planning still retains all eligible accounts for execution-time retry. If the
    /// selected account becomes saturated between planning and permit acquisition, or the guest call
    /// fails, the next eligible account is attempted before the caller falls back to built-ins.
    pub async fn resolve_track(
        &self,
        query: &TrackQuery,
        policy: &RoutingPolicy,
    ) -> Result<PluginSingleResult<RemoteTrack>> {
        let plan = self.plan(ServiceKind::Metadata, policy)?;
        let Some(client) = self.clients.client()? else {
            return Ok(PluginSingleResult::unavailable(plan));
        };

        let mut failures = Vec::new();
        for route in &plan.eligible_routes {
            let key = PluginCallKey::provider(&route.plugin_id, &route.provider_id);
            let call = client.resolve_track(
                &route.plugin_id,
                &route.provider_id,
                Some(&route.account_id),
                query,
            );
            match self.runtime.execute_guest_call(key, call).await {
                Ok(Some(track)) => {
                    return Ok(PluginSingleResult {
                        value: Some(track),
                        route: Some(route.clone()),
                        plan,
                        failures,
                        client_ready: true,
                    });
                }
                Ok(None) => {}
                Err(error) => failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("{error:#}"),
                }),
            }
        }

        Ok(PluginSingleResult {
            value: None,
            route: None,
            plan,
            failures,
            client_ready: true,
        })
    }

    /// Resolve lyrics for an already-resolved provider track.
    ///
    /// Metadata and lyrics remain independent capabilities, but when the metadata provider itself
    /// has a healthy lyrics route it is tried first. Other accounts of that same provider may retry
    /// the operation; callers can then perform a separate cross-provider lyric-quality fallback.
    pub async fn lyrics_for_route(
        &self,
        metadata_route: &PluginRoute,
        track: &SourceTrackRef,
    ) -> Result<PluginSingleResult<PluginLyricDocument>> {
        if track.provider_id != metadata_route.provider_id {
            bail!(
                "歌词 source provider 与 metadata route 不一致: source={}, route={}",
                track.provider_id,
                metadata_route.provider_id
            );
        }
        let policy = RoutingPolicy {
            preferred_provider: Some(metadata_route.provider_id.clone()),
            ..RoutingPolicy::default()
        };
        let mut plan = self.plan(ServiceKind::Lyrics, &policy)?;
        // A SourceTrackRef is provider-specific. Passing it to another provider would be an identity
        // violation, so this operation retries only accounts of the same plugin/provider. A future
        // cross-provider lyrics fallback must first resolve the TrackQuery on that provider.
        plan.eligible_routes.retain(|route| {
            route.plugin_id == metadata_route.plugin_id
                && route.provider_id == metadata_route.provider_id
        });
        plan.plan.plugin_routes = plan.eligible_routes.first().cloned().into_iter().collect();

        let Some(client) = self.clients.client()? else {
            return Ok(PluginSingleResult::unavailable(plan));
        };

        let mut failures = Vec::new();
        for route in &plan.eligible_routes {
            let key = PluginCallKey::provider(&route.plugin_id, &route.provider_id);
            let call = client.lyrics(
                &route.plugin_id,
                &route.provider_id,
                Some(&route.account_id),
                track,
            );
            match self.runtime.execute_guest_call(key, call).await {
                Ok(Some(lyrics)) => {
                    return Ok(PluginSingleResult {
                        value: Some(lyrics),
                        route: Some(route.clone()),
                        plan,
                        failures,
                        client_ready: true,
                    });
                }
                Ok(None) => {}
                Err(error) => failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("{error:#}"),
                }),
            }
        }

        Ok(PluginSingleResult {
            value: None,
            route: None,
            plan,
            failures,
            client_ready: true,
        })
    }
}

pub fn initialize(
    host: Arc<RwLock<PluginHostState>>,
    sessions: Arc<RwLock<PluginSessionCoordinator>>,
    runtime: Arc<PluginHostServices>,
    clients: Arc<PluginClientRegistry>,
) -> Arc<PluginServiceFrontend> {
    PLUGIN_FRONTEND
        .get_or_init(|| Arc::new(PluginServiceFrontend::new(host, sessions, runtime, clients)))
        .clone()
}

pub fn global() -> Option<Arc<PluginServiceFrontend>> {
    PLUGIN_FRONTEND.get().cloned()
}
