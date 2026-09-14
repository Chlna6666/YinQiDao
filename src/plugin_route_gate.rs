use anyhow::Result;

use crate::{
    plugin_runtime::{PluginCallKey, PluginHostServices, PluginRouteHealthSnapshot},
    plugin_sessions::{PluginSessionCoordinator, PluginSessionState},
    plugins::{
        PluginAccount, PluginRoute, PluginServiceRouter, RoutePlan, RoutingPolicy, ServiceKind,
    },
};

/// Health source is abstract so routing policy can be tested without constructing an HTTP/Secret
/// Host runtime. Production uses `PluginHostServices` directly.
pub trait PluginRouteHealthSource {
    fn health_for(&self, key: &PluginCallKey) -> Result<PluginRouteHealthSnapshot>;
}

impl PluginRouteHealthSource for PluginHostServices {
    fn health_for(&self, key: &PluginCallKey) -> Result<PluginRouteHealthSnapshot> {
        PluginHostServices::route_health(self, key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginRouteRejectionReason {
    /// Current-process session validation is authoritative over persisted account metadata.
    Session(PluginSessionState),
    /// Circuit breaker or provider 429 backoff is active.
    Unhealthy(PluginRouteHealthSnapshot),
    /// Health state itself could not be read. Routing fails closed for this plugin route.
    HealthUnavailable(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedPluginRoute {
    pub route: PluginRoute,
    pub reason: PluginRouteRejectionReason,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatedRoutePlan {
    pub plan: RoutePlan,
    pub rejected: Vec<RejectedPluginRoute>,
}

impl GatedRoutePlan {
    pub fn has_authenticated_plugin(&self) -> bool {
        self.plan.has_authenticated_plugin()
    }
}

/// Build the final plugin route plan after both session and runtime-health gates.
///
/// Ordering is computed over *all* authenticated candidates first. Filtering then happens before a
/// single-route service is truncated to one route, so a preferred/default account under 429/circuit
/// backoff cannot hide a healthy secondary account.
pub fn plan_routes<H: PluginRouteHealthSource>(
    router: &PluginServiceRouter,
    sessions: &PluginSessionCoordinator,
    health: &H,
    service: ServiceKind,
    policy: &RoutingPolicy,
) -> GatedRoutePlan {
    let mut candidates = router
        .accounts()
        .iter()
        .filter(|account| account.supports(service.capability()))
        .map(route_from_account)
        .collect::<Vec<_>>();
    sort_routes(&mut candidates, policy.preferred_provider.as_deref());

    let mut accepted = Vec::with_capacity(candidates.len());
    let mut rejected = Vec::new();
    for route in candidates {
        let Some(account) = find_account(router, &route) else {
            // Route came directly from this router snapshot, so this is defensive only.
            rejected.push(RejectedPluginRoute {
                route,
                reason: PluginRouteRejectionReason::HealthUnavailable(
                    "路由账号在规划过程中消失".into(),
                ),
            });
            continue;
        };

        let session_state = sessions.state_for(account);
        if session_state != PluginSessionState::Authenticated {
            rejected.push(RejectedPluginRoute {
                route,
                reason: PluginRouteRejectionReason::Session(session_state),
            });
            continue;
        }

        let key = PluginCallKey::provider(&route.plugin_id, &route.provider_id);
        match health.health_for(&key) {
            Ok(snapshot) if snapshot.is_available() => accepted.push(route),
            Ok(snapshot) => rejected.push(RejectedPluginRoute {
                route,
                reason: PluginRouteRejectionReason::Unhealthy(snapshot),
            }),
            Err(error) => rejected.push(RejectedPluginRoute {
                route,
                reason: PluginRouteRejectionReason::HealthUnavailable(format!("{error:#}")),
            }),
        }
    }

    if !service.fan_out() {
        accepted.truncate(1);
    }

    GatedRoutePlan {
        plan: RoutePlan {
            service,
            plugin_routes: accepted,
            authenticated_plugin_first: policy.authenticated_plugin_first,
            allow_builtin_fallback: policy.allow_builtin_fallback,
            allow_local_fallback: policy.allow_local_fallback,
        },
        rejected,
    }
}

fn route_from_account(account: &PluginAccount) -> PluginRoute {
    PluginRoute {
        plugin_id: account.plugin_id.clone(),
        provider_id: account.provider_id.clone(),
        account_id: account.account_id.clone(),
        priority: account.priority,
        is_default: account.is_default,
    }
}

fn find_account<'a>(
    router: &'a PluginServiceRouter,
    route: &PluginRoute,
) -> Option<&'a PluginAccount> {
    router.accounts().iter().find(|account| {
        account.plugin_id == route.plugin_id
            && account.provider_id == route.provider_id
            && account.account_id == route.account_id
    })
}

fn sort_routes(routes: &mut [PluginRoute], preferred_provider: Option<&str>) {
    routes.sort_by(|left, right| {
        let left_preferred =
            preferred_provider.is_some_and(|provider| provider == left.provider_id);
        let right_preferred =
            preferred_provider.is_some_and(|provider| provider == right.provider_id);
        right_preferred
            .cmp(&left_preferred)
            .then_with(|| right.is_default.cmp(&left.is_default))
            .then_with(|| right.priority.cmp(&left.priority))
            .then_with(|| left.provider_id.cmp(&right.provider_id))
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use anyhow::anyhow;

    use super::*;
    use crate::plugins::{AccountState, PluginCapability};

    #[derive(Default)]
    struct MockHealth {
        values: HashMap<PluginCallKey, Result<PluginRouteHealthSnapshot, String>>,
    }

    impl PluginRouteHealthSource for MockHealth {
        fn health_for(&self, key: &PluginCallKey) -> Result<PluginRouteHealthSnapshot> {
            match self.values.get(key) {
                Some(Ok(snapshot)) => Ok(snapshot.clone()),
                Some(Err(error)) => Err(anyhow!(error.clone())),
                None => Ok(PluginRouteHealthSnapshot::default()),
            }
        }
    }

    fn account(
        provider_id: &str,
        account_id: &str,
        priority: i32,
        capability: PluginCapability,
    ) -> PluginAccount {
        PluginAccount {
            plugin_id: format!("plugin.{provider_id}"),
            provider_id: provider_id.into(),
            account_id: account_id.into(),
            display_name: account_id.into(),
            avatar_url: None,
            state: AccountState::Authenticated,
            capabilities: vec![capability],
            priority,
            is_default: true,
        }
    }

    #[test]
    fn unhealthy_primary_does_not_hide_healthy_secondary_for_single_route() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account("netease", "primary", 100, PluginCapability::Lyrics));
        router.upsert_account(account("qqmusic", "secondary", 10, PluginCapability::Lyrics));
        let sessions = PluginSessionCoordinator::default();
        let mut health = MockHealth::default();
        health.values.insert(
            PluginCallKey::provider("plugin.netease", "netease"),
            Ok(PluginRouteHealthSnapshot {
                retry_after: Some(Duration::from_secs(30)),
                ..PluginRouteHealthSnapshot::default()
            }),
        );

        let gated = plan_routes(
            &router,
            &sessions,
            &health,
            ServiceKind::Lyrics,
            &RoutingPolicy::default(),
        );
        assert_eq!(gated.plan.plugin_routes.len(), 1);
        assert_eq!(gated.plan.plugin_routes[0].provider_id, "qqmusic");
        assert_eq!(gated.rejected.len(), 1);
    }

    #[test]
    fn preferred_provider_still_falls_back_when_health_gate_rejects_it() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account("netease", "a", 100, PluginCapability::Recognition));
        router.upsert_account(account("qqmusic", "b", 1, PluginCapability::Recognition));
        let sessions = PluginSessionCoordinator::default();
        let mut health = MockHealth::default();
        health.values.insert(
            PluginCallKey::provider("plugin.qqmusic", "qqmusic"),
            Ok(PluginRouteHealthSnapshot {
                circuit_open_for: Some(Duration::from_secs(20)),
                ..PluginRouteHealthSnapshot::default()
            }),
        );
        let policy = RoutingPolicy {
            preferred_provider: Some("qqmusic".into()),
            ..RoutingPolicy::default()
        };

        let gated = plan_routes(
            &router,
            &sessions,
            &health,
            ServiceKind::Recognition,
            &policy,
        );
        assert_eq!(gated.plan.plugin_routes[0].provider_id, "netease");
    }

    #[test]
    fn fan_out_keeps_all_healthy_routes_and_skips_health_errors() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account("netease", "a", 10, PluginCapability::Search));
        router.upsert_account(account("qqmusic", "b", 5, PluginCapability::Search));
        let sessions = PluginSessionCoordinator::default();
        let mut health = MockHealth::default();
        health.values.insert(
            PluginCallKey::provider("plugin.qqmusic", "qqmusic"),
            Err("health lock unavailable".into()),
        );

        let gated = plan_routes(
            &router,
            &sessions,
            &health,
            ServiceKind::Search,
            &RoutingPolicy::default(),
        );
        assert_eq!(gated.plan.plugin_routes.len(), 1);
        assert_eq!(gated.plan.plugin_routes[0].provider_id, "netease");
        assert!(matches!(
            gated.rejected[0].reason,
            PluginRouteRejectionReason::HealthUnavailable(_)
        ));
    }
}
