use std::{
    collections::HashSet,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow};

use crate::{
    plugin_host::PluginHostState,
    plugins::{AccountState, PluginAccount},
};

static PLUGIN_SESSIONS: OnceLock<Arc<RwLock<PluginSessionCoordinator>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginAccountKey {
    pub plugin_id: String,
    pub provider_id: String,
    pub account_id: String,
}

impl PluginAccountKey {
    pub fn new(
        plugin_id: impl Into<String>,
        provider_id: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            provider_id: provider_id.into(),
            account_id: account_id.into(),
        }
    }
}

impl From<&PluginAccount> for PluginAccountKey {
    fn from(account: &PluginAccount) -> Self {
        Self::new(
            account.plugin_id.clone(),
            account.provider_id.clone(),
            account.account_id.clone(),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginSessionState {
    /// Account metadata exists, but the current process has not yet validated the stored Secret or
    /// refreshed the provider session. Pending sessions must never participate in service routing.
    PendingValidation,
    Authenticated,
    Expired,
    LoggedOut,
}

#[derive(Debug, Default)]
pub struct PluginSessionCoordinator {
    pending_validation: HashSet<PluginAccountKey>,
    startup_errors: Vec<String>,
}

impl PluginSessionCoordinator {
    fn quarantine_restored_sessions(host: &Arc<RwLock<PluginHostState>>) -> Self {
        let mut coordinator = Self::default();
        let restored = match host.read() {
            Ok(host) => host.router().accounts().to_vec(),
            Err(error) => {
                coordinator
                    .startup_errors
                    .push(format!("读取插件账号用于会话恢复失败: {error}"));
                return coordinator;
            }
        };

        // `Expired` accounts are also queued once at process start: a refresh token may still be
        // valid even though the short-lived provider session expired before the previous shutdown.
        // `LoggedOut` is explicit user intent and is therefore never retried automatically.
        let candidates = restored
            .iter()
            .filter(|account| {
                matches!(
                    account.state,
                    AccountState::Authenticated | AccountState::Expired
                )
            })
            .map(PluginAccountKey::from)
            .collect::<Vec<_>>();
        coordinator
            .pending_validation
            .extend(candidates.iter().cloned());

        // The account index can say `Authenticated` only about the process that wrote it. Before a
        // new process validates its Secret/session, downgrade that state in the routing model. This
        // keeps PluginServiceRouter fail-closed and preserves built-in/local fallbacks.
        let authenticated = restored
            .iter()
            .filter(|account| account.state == AccountState::Authenticated)
            .map(PluginAccountKey::from)
            .collect::<Vec<_>>();
        if authenticated.is_empty() {
            return coordinator;
        }

        let mut host = match host.write() {
            Ok(host) => host,
            Err(error) => {
                coordinator.startup_errors.push(format!(
                    "隔离历史插件登录状态失败，插件路由必须保持禁用直到重启: {error}"
                ));
                return coordinator;
            }
        };
        for key in authenticated {
            match host.mark_account_state(
                &key.plugin_id,
                &key.provider_id,
                &key.account_id,
                AccountState::Expired,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    coordinator.startup_errors.push(format!(
                        "隔离历史插件登录状态时账号消失: {}/{}/{}",
                        key.plugin_id, key.provider_id, key.account_id
                    ));
                }
                Err(error) => {
                    coordinator.startup_errors.push(format!(
                        "隔离历史插件登录状态失败 {}/{}/{}: {error:#}",
                        key.plugin_id, key.provider_id, key.account_id
                    ));
                }
            }
        }
        coordinator
    }

    pub fn pending_accounts(&self) -> impl Iterator<Item = &PluginAccountKey> {
        self.pending_validation.iter()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_validation.len()
    }

    pub fn startup_errors(&self) -> &[String] {
        &self.startup_errors
    }

    pub fn state_for(&self, account: &PluginAccount) -> PluginSessionState {
        if self
            .pending_validation
            .contains(&PluginAccountKey::from(account))
        {
            return PluginSessionState::PendingValidation;
        }
        match account.state {
            AccountState::Authenticated => PluginSessionState::Authenticated,
            AccountState::Expired => PluginSessionState::Expired,
            AccountState::LoggedOut => PluginSessionState::LoggedOut,
        }
    }

    /// Called only after the plugin runtime has proved that its stored Secret/session is valid or
    /// has refreshed it successfully. This is the only restore path that re-enables routing.
    pub fn mark_validated(
        &mut self,
        host: &Arc<RwLock<PluginHostState>>,
        key: &PluginAccountKey,
    ) -> Result<bool> {
        if !self.pending_validation.contains(key) {
            return Ok(false);
        }
        let changed = host
            .write()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?
            .mark_account_state(
                &key.plugin_id,
                &key.provider_id,
                &key.account_id,
                AccountState::Authenticated,
            )?;
        if changed {
            self.pending_validation.remove(key);
        }
        Ok(changed)
    }

    /// Finish one restore attempt without enabling the provider. The account stays `Expired` so UI
    /// can still show it and the next application start may retry refresh if Secret material exists.
    pub fn mark_validation_failed(
        &mut self,
        host: &Arc<RwLock<PluginHostState>>,
        key: &PluginAccountKey,
    ) -> Result<bool> {
        let changed = host
            .write()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?
            .mark_account_state(
                &key.plugin_id,
                &key.provider_id,
                &key.account_id,
                AccountState::Expired,
            )?;
        self.pending_validation.remove(key);
        Ok(changed)
    }

    pub fn logout(
        &mut self,
        host: &Arc<RwLock<PluginHostState>>,
        key: &PluginAccountKey,
    ) -> Result<bool> {
        let changed = host
            .write()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?
            .mark_account_state(
                &key.plugin_id,
                &key.provider_id,
                &key.account_id,
                AccountState::LoggedOut,
            )?;
        self.pending_validation.remove(key);
        Ok(changed)
    }

    /// Fresh login results do not need the restore quarantine because the current process just
    /// obtained the session. Account identity/capability validation still happens in PluginHostState.
    pub fn register_authenticated_account(
        &mut self,
        host: &Arc<RwLock<PluginHostState>>,
        mut account: PluginAccount,
    ) -> Result<()> {
        account.state = AccountState::Authenticated;
        let key = PluginAccountKey::from(&account);
        host.write()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?
            .upsert_account(account)?;
        self.pending_validation.remove(&key);
        Ok(())
    }
}

pub fn initialize(
    host: &Arc<RwLock<PluginHostState>>,
) -> Arc<RwLock<PluginSessionCoordinator>> {
    PLUGIN_SESSIONS
        .get_or_init(|| {
            Arc::new(RwLock::new(
                PluginSessionCoordinator::quarantine_restored_sessions(host),
            ))
        })
        .clone()
}

pub fn global() -> Option<Arc<RwLock<PluginSessionCoordinator>>> {
    PLUGIN_SESSIONS.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(state: AccountState) -> PluginAccount {
        PluginAccount {
            plugin_id: "plugin.test".into(),
            provider_id: "test".into(),
            account_id: "account".into(),
            display_name: "Account".into(),
            avatar_url: None,
            state,
            capabilities: Vec::new(),
            priority: 0,
            is_default: true,
        }
    }

    #[test]
    fn pending_overlay_never_reports_authenticated() {
        let account = account(AccountState::Expired);
        let key = PluginAccountKey::from(&account);
        let coordinator = PluginSessionCoordinator {
            pending_validation: HashSet::from([key]),
            startup_errors: Vec::new(),
        };
        assert_eq!(
            coordinator.state_for(&account),
            PluginSessionState::PendingValidation
        );
    }

    #[test]
    fn logged_out_accounts_are_not_implicitly_pending() {
        let account = account(AccountState::LoggedOut);
        let coordinator = PluginSessionCoordinator::default();
        assert_eq!(
            coordinator.state_for(&account),
            PluginSessionState::LoggedOut
        );
    }
}
