use std::{
    collections::HashSet,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow};

use crate::{
    plugin_host::PluginHostState,
    plugin_secrets::SecretStoreProtection,
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
    fn quarantine_restored_sessions(
        host: &Arc<RwLock<PluginHostState>>,
        secret_protection: SecretStoreProtection,
    ) -> Self {
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

        let restorable = restored
            .iter()
            .filter(|account| {
                matches!(
                    account.state,
                    AccountState::Authenticated | AccountState::Expired
                )
            })
            .collect::<Vec<_>>();

        // Only a persistent protected Secret backend can make cross-process refresh meaningful.
        // With the development memory backend, queueing PendingValidation would promise a restore
        // path even though every refresh token/cookie disappeared with the previous process.
        if secret_protection.is_persistent() {
            coordinator.pending_validation.extend(
                restorable
                    .iter()
                    .map(|account| PluginAccountKey::from(*account)),
            );
        } else if !restorable.is_empty() {
            coordinator.startup_errors.push(format!(
                "当前插件 Secret 后端为 {secret_protection:?}，不支持跨进程会话恢复；历史账号保持 Expired"
            ));
        }

        // The account index can say `Authenticated` only about the process that wrote it. Before a
        // new process validates its Secret/session, downgrade that state in the routing model. This
        // is required for both persistent and ephemeral Secret backends.
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

    /// Remove restore-overlay keys that no longer have an account row in the live Host router.
    ///
    /// Package update/uninstall can remove providers or invalidate account capabilities. The router
    /// is authoritative for reachability, so stale pending keys must not outlive those account rows
    /// and later affect a reinstall that reuses the same plugin/account identity.
    pub fn retain_host_accounts(&mut self, accounts: &[PluginAccount]) -> usize {
        let valid = accounts
            .iter()
            .map(PluginAccountKey::from)
            .collect::<HashSet<_>>();
        let before = self.pending_validation.len();
        self.pending_validation.retain(|key| valid.contains(key));
        before.saturating_sub(self.pending_validation.len())
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
    /// can still show it. A future application start retries only when the active Secret backend is
    /// persistent and can still contain refresh material.
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
    secret_protection: SecretStoreProtection,
) -> Arc<RwLock<PluginSessionCoordinator>> {
    PLUGIN_SESSIONS
        .get_or_init(|| {
            Arc::new(RwLock::new(
                PluginSessionCoordinator::quarantine_restored_sessions(host, secret_protection),
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
    fn retain_host_accounts_drops_unreachable_pending_keys() {
        let kept = account(AccountState::Expired);
        let mut removed = kept.clone();
        removed.account_id = "removed".into();
        let mut coordinator = PluginSessionCoordinator {
            pending_validation: HashSet::from([
                PluginAccountKey::from(&kept),
                PluginAccountKey::from(&removed),
            ]),
            startup_errors: Vec::new(),
        };

        assert_eq!(coordinator.retain_host_accounts(std::slice::from_ref(&kept)), 1);
        assert_eq!(coordinator.pending_count(), 1);
        assert!(coordinator.pending_validation.contains(&PluginAccountKey::from(&kept)));
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

    #[test]
    fn ephemeral_secret_backend_is_never_persistent() {
        assert!(!SecretStoreProtection::Ephemeral.is_persistent());
        assert!(SecretStoreProtection::OsProtected.is_persistent());
        assert!(SecretStoreProtection::HostEncrypted.is_persistent());
    }
}
