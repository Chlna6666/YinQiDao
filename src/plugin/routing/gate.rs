use anyhow::Result;

use super::super::{
    abi::{
        PluginAccount, PluginRoute, PluginServiceRouter, RoutePlan, RoutingPolicy, ServiceKind,
        sort_plugin_routes,
    },
    host::{
        package_manager,
        runtime::{PluginCallKey, PluginHostServices, PluginRouteHealthSnapshot},
        sessions::{PluginSessionCoordinator, PluginSessionState},
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
    /// User-disabled plugins fail closed before session/health evaluation.
    Disabled,
    /// Current-process session validation is authoritative over persisted account metadata.
    Session(PluginSessionState),
    /// Circuit breaker, provider 429 backoff, or route concurrency saturation is active.
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
    /// Public service plan. Single-route services expose only the current best route here.
    pub plan: RoutePlan,
    /// Every currently eligible route in preference/default/priority order. The execution frontend
    /// uses this list to retry a secondary account when permit acquisition races or a guest call
    /// fails after planning.
    pub eligible_routes: Vec<PluginRoute>,
    pub rejected: Vec<RejectedPluginRoute>,
}

impl GatedRoutePlan {
    pub fn has_authenticated_plugin(&self) -> bool {
        !self.eligible_routes.is_empty()
    }
}

/// Build the final plugin route plan after enablement, session and runtime-health gates.
///
/// Ordering is computed over every account declaring the requested capability first. Persisted
/// account state is deliberately not used as a pre-filter: `PluginSessionCoordinator` is the
/// current-process authority for Authenticated/PendingValidation/Expired/LoggedOut. Filtering then
/// happens before a single-route service exposes its current winner, so a preferred/default account
/// under disable/session/429/circuit/saturation cannot hide a healthy secondary account.
/// `eligible_routes` intentionally retains every accepted route for execution-time retry.
pub fn plan_routes<H: PluginRouteHealthSource>(
    router: &PluginServiceRouter,
    sessions: &PluginSessionCoordinator,
    health: &H,
    service: ServiceKind,
    policy: &RoutingPolicy,
) -> GatedRoutePlan {
    let capability = service.capability();
    let mut candidates = router
        .accounts()
        .iter()
        .filter(|account| account.capabilities.contains(&capability))
        .map(route_from_account)
        .collect::<Vec<_>>();
    sort_plugin_routes(&mut candidates, policy);

    let mut eligible_routes = Vec::with_capacity(candidates.len());
    let mut rejected = Vec::new();
    for route in candidates {
        // Missing package manager occurs only in isolated unit tests/very early startup. Once the
        // plugin subsystem is initialized, enablement is authoritative and poisoned state fails
        // closed through `is_enabled`.
        if package_manager::global().is_some_and(|manager| !manager.is_enabled(&route.plugin_id)) {
            rejected.push(RejectedPluginRoute {
                route,
                reason: PluginRouteRejectionReason::Disabled,
            });
            continue;
        }

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
            Ok(snapshot) if snapshot.is_available() => eligible_routes.push(route),
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

    let mut selected_routes = eligible_routes.clone();
    if !service.fan_out() {
        selected_routes.truncate(1);
    }

    GatedRoutePlan {
        plan: RoutePlan {
            service,
            plugin_routes: selected_routes,
            authenticated_plugin_first: policy.authenticated_plugin_first,
            allow_builtin_fallback: policy.allow_builtin_fallback,
            allow_local_fallback: policy.allow_local_fallback,
        },
        eligible_routes,
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

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, time::Duration};

    use anyhow::anyhow;

    use super::*;
    use crate::plugin::abi::{AccountState, PluginAccountPreference, PluginCapability};

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
        router.upsert_account(account(
            "qqmusic",
            "secondary",
            10,
            PluginCapability::Lyrics,
        ));
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
        assert_eq!(gated.eligible_routes.len(), 1);
        assert_eq!(gated.rejected.len(), 1);
    }

    #[test]
    fn single_route_plan_keeps_secondary_execution_candidates() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account(
            "netease",
            "primary",
            100,
            PluginCapability::Metadata,
        ));
        router.upsert_account(account(
            "qqmusic",
            "secondary",
            10,
            PluginCapability::Metadata,
        ));
        let gated = plan_routes(
            &router,
            &PluginSessionCoordinator::default(),
            &MockHealth::default(),
            ServiceKind::Metadata,
            &RoutingPolicy::default(),
        );
        assert_eq!(gated.plan.plugin_routes.len(), 1);
        assert_eq!(gated.eligible_routes.len(), 2);
        assert_eq!(gated.eligible_routes[0].provider_id, "netease");
        assert_eq!(gated.eligible_routes[1].provider_id, "qqmusic");
    }

    #[test]
    fn expired_capable_account_is_rejected_by_session_gate() {
        let mut router = PluginServiceRouter::default();
        let mut expired = account("netease", "expired", 100, PluginCapability::Playlists);
        expired.state = AccountState::Expired;
        router.upsert_account(expired);

        let gated = plan_routes(
            &router,
            &PluginSessionCoordinator::default(),
            &MockHealth::default(),
            ServiceKind::Playlists,
            &RoutingPolicy::default(),
        );

        assert!(gated.eligible_routes.is_empty());
        assert!(gated.plan.plugin_routes.is_empty());
        assert_eq!(gated.rejected.len(), 1);
        assert!(matches!(
            gated.rejected[0].reason,
            PluginRouteRejectionReason::Session(PluginSessionState::Expired)
        ));
    }

    #[test]
    fn logged_out_capable_account_is_rejected_by_session_gate() {
        let mut router = PluginServiceRouter::default();
        let mut logged_out = account("qqmusic", "logged-out", 100, PluginCapability::Search);
        logged_out.state = AccountState::LoggedOut;
        router.upsert_account(logged_out);

        let gated = plan_routes(
            &router,
            &PluginSessionCoordinator::default(),
            &MockHealth::default(),
            ServiceKind::Search,
            &RoutingPolicy::default(),
        );

        assert!(gated.eligible_routes.is_empty());
        assert_eq!(gated.rejected.len(), 1);
        assert!(matches!(
            gated.rejected[0].reason,
            PluginRouteRejectionReason::Session(PluginSessionState::LoggedOut)
        ));
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
    fn preferred_account_wins_over_default_and_priority() {
        let mut router = PluginServiceRouter::default();
        let mut default_account = account("qqmusic", "default", 100, PluginCapability::Metadata);
        default_account.is_default = true;
        let mut selected = account("qqmusic", "selected", 1, PluginCapability::Metadata);
        selected.is_default = false;
        router.upsert_account(default_account);
        router.upsert_account(selected);

        let policy = RoutingPolicy {
            preferred_account: Some(PluginAccountPreference {
                plugin_id: "plugin.qqmusic".into(),
                provider_id: "qqmusic".into(),
                account_id: "selected".into(),
            }),
            ..RoutingPolicy::default()
        };
        let gated = plan_routes(
            &router,
            &PluginSessionCoordinator::default(),
            &MockHealth::default(),
            ServiceKind::Metadata,
            &policy,
        );
        assert_eq!(gated.plan.plugin_routes[0].account_id, "selected");
        assert_eq!(gated.eligible_routes[0].account_id, "selected");
        assert_eq!(gated.eligible_routes[1].account_id, "default");
    }

    #[test]
    fn unavailable_preferred_account_falls_back_to_sibling_before_other_provider() {
        let mut router = PluginServiceRouter::default();
        let mut selected = account("qqmusic", "selected", 1, PluginCapability::Metadata);
        selected.state = AccountState::Expired;
        selected.is_default = false;
        let mut sibling = account("qqmusic", "sibling", 1, PluginCapability::Metadata);
        sibling.is_default = false;
        let other = account("netease", "other", 1_000, PluginCapability::Metadata);
        router.upsert_account(selected);
        router.upsert_account(sibling);
        router.upsert_account(other);

        let policy = RoutingPolicy {
            preferred_account: Some(PluginAccountPreference {
                plugin_id: "plugin.qqmusic".into(),
                provider_id: "qqmusic".into(),
                account_id: "selected".into(),
            }),
            ..RoutingPolicy::default()
        };
        let gated = plan_routes(
            &router,
            &PluginSessionCoordinator::default(),
            &MockHealth::default(),
            ServiceKind::Metadata,
            &policy,
        );
        assert_eq!(gated.rejected.len(), 1);
        assert_eq!(gated.rejected[0].route.account_id, "selected");
        assert_eq!(gated.eligible_routes[0].account_id, "sibling");
        assert_eq!(gated.eligible_routes[1].provider_id, "netease");
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
        assert_eq!(gated.eligible_routes.len(), 1);
        assert!(matches!(
            gated.rejected[0].reason,
            PluginRouteRejectionReason::HealthUnavailable(_)
        ));
    }
}
