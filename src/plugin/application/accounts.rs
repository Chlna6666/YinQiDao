use anyhow::{Result, anyhow};

use super::{
    abi::{AuthMethod, PluginCapability},
    host::{catalog, package_manager, permissions, sessions},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginAccountSessionStatus {
    PendingValidation,
    Authenticated,
    Expired,
    LoggedOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginAccountSummary {
    pub account_id: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub state: PluginAccountSessionStatus,
    pub capabilities: Vec<PluginCapability>,
    pub priority: i32,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginProviderSummary {
    pub provider_id: String,
    pub display_name: String,
    pub capabilities: Vec<PluginCapability>,
    pub auth_methods: Vec<AuthMethod>,
    pub accounts: Vec<PluginAccountSummary>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginPermissionSummary {
    pub requested_network_domains: Vec<String>,
    pub granted_network_domains: Vec<String>,
    pub playback_events_requested: bool,
    pub playback_events_granted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginServiceSummary {
    pub plugin_id: String,
    pub name: String,
    pub version: String,
    pub enabled: bool,
    pub providers: Vec<PluginProviderSummary>,
    pub permissions: PluginPermissionSummary,
}

/// Build one immutable application-facing snapshot of every installed service plugin, Provider,
/// account/session state and user permission grant. Secret values and package filesystem paths never
/// cross this boundary.
pub fn service_summaries() -> Result<Vec<PluginServiceSummary>> {
    let session_state = sessions::global().ok_or_else(|| anyhow!("插件会话状态尚未初始化"))?;
    let host_state = catalog::global().ok_or_else(|| anyhow!("插件 Host state 尚未初始化"))?;

    // Preserve the subsystem lock order used by the execution frontend: sessions -> host.
    let sessions = session_state
        .read()
        .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?;
    let host = host_state
        .read()
        .map_err(|error| anyhow!("插件 Host state 锁已损坏: {error}"))?;

    let accounts = host.router().accounts();
    let mut summaries = host
        .catalog()
        .plugins()
        .iter()
        .map(|plugin| {
            let mut providers = plugin
                .manifest
                .providers
                .iter()
                .map(|provider| {
                    let mut provider_accounts = accounts
                        .iter()
                        .filter(|account| {
                            account.plugin_id == plugin.manifest.id
                                && account.provider_id == provider.id
                        })
                        .map(|account| PluginAccountSummary {
                            account_id: account.account_id.clone(),
                            display_name: account.display_name.clone(),
                            avatar_url: account.avatar_url.clone(),
                            state: match sessions.state_for(account) {
                                sessions::PluginSessionState::PendingValidation => {
                                    PluginAccountSessionStatus::PendingValidation
                                }
                                sessions::PluginSessionState::Authenticated => {
                                    PluginAccountSessionStatus::Authenticated
                                }
                                sessions::PluginSessionState::Expired => {
                                    PluginAccountSessionStatus::Expired
                                }
                                sessions::PluginSessionState::LoggedOut => {
                                    PluginAccountSessionStatus::LoggedOut
                                }
                            },
                            capabilities: account.capabilities.clone(),
                            priority: account.priority,
                            is_default: account.is_default,
                        })
                        .collect::<Vec<_>>();
                    provider_accounts.sort_by(|left, right| {
                        right
                            .is_default
                            .cmp(&left.is_default)
                            .then_with(|| right.priority.cmp(&left.priority))
                            .then_with(|| left.display_name.cmp(&right.display_name))
                            .then_with(|| left.account_id.cmp(&right.account_id))
                    });
                    PluginProviderSummary {
                        provider_id: provider.id.clone(),
                        display_name: provider.display_name.clone(),
                        capabilities: provider.capabilities.clone(),
                        auth_methods: provider.auth_methods.clone(),
                        accounts: provider_accounts,
                    }
                })
                .collect::<Vec<_>>();
            providers.sort_by(|left, right| {
                left.display_name
                    .cmp(&right.display_name)
                    .then_with(|| left.provider_id.cmp(&right.provider_id))
            });
            PluginServiceSummary {
                plugin_id: plugin.manifest.id.clone(),
                name: plugin.manifest.name.clone(),
                version: plugin.manifest.version.clone(),
                enabled: true,
                providers,
                permissions: PluginPermissionSummary {
                    requested_network_domains: plugin.manifest.network_domains.clone(),
                    granted_network_domains: Vec::new(),
                    playback_events_requested: plugin.manifest.providers.iter().any(|provider| {
                        provider
                            .capabilities
                            .contains(&PluginCapability::PlaybackEvents)
                    }),
                    playback_events_granted: false,
                },
            }
        })
        .collect::<Vec<_>>();
    drop(host);
    drop(sessions);

    if let Some(manager) = package_manager::global() {
        for plugin in &mut summaries {
            plugin.enabled = manager.is_enabled(&plugin.plugin_id);
        }
    } else {
        for plugin in &mut summaries {
            plugin.enabled = false;
        }
    }

    if let Some(permission_state) = permissions::global() {
        let permission_state = permission_state
            .read()
            .map_err(|error| anyhow!("插件权限状态锁已损坏: {error}"))?;
        for plugin in &mut summaries {
            if let Some(grant) = permission_state.grant_for(&plugin.plugin_id) {
                plugin.permissions.granted_network_domains = grant.network_domains.clone();
                plugin.permissions.playback_events_granted = grant.playback_events;
            }
        }
    }

    summaries.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.plugin_id.cmp(&right.plugin_id))
    });
    Ok(summaries)
}

pub fn set_default_account(plugin_id: &str, provider_id: &str, account_id: &str) -> Result<bool> {
    let host_state = catalog::global().ok_or_else(|| anyhow!("插件 Host state 尚未初始化"))?;
    let changed = host_state
        .write()
        .map_err(|error| anyhow!("插件 Host state 锁已损坏: {error}"))?
        .set_default_account(plugin_id, provider_id, account_id)?;
    if changed {
        sessions::bump_session_generation();
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_validation_is_distinct_from_expired_for_ui() {
        assert_ne!(
            PluginAccountSessionStatus::PendingValidation,
            PluginAccountSessionStatus::Expired
        );
    }
}
