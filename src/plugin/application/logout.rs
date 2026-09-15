use anyhow::{Result, anyhow, bail};

use super::{
    frontend::{PluginLogoutResult, PluginServiceFrontend},
    host::{
        catalog,
        secrets,
        security::SecretSlot,
        sessions::PluginAccountKey,
    },
};

const MAX_PLUGIN_LOGOUT_ACCOUNTS: usize = 512;
const MAX_PLUGIN_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginAccountLogoutOutcome {
    pub key: PluginAccountKey,
    pub local_state_changed: bool,
    pub remote_acknowledged: bool,
    pub remote_error: Option<String>,
    /// Failure before the normal single-account logout result could be produced. Secret cleanup is
    /// still attempted so an application-side failure cannot leave usable credentials behind.
    pub operation_error: Option<String>,
    /// Account-scoped Host Secret entries removed after the guest logout attempt.
    pub secrets_revoked: usize,
    pub secret_cleanup_error: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginLogoutAllResult {
    pub accounts: Vec<PluginAccountLogoutOutcome>,
    /// Residual scope Secrets removed after per-account cleanup. For Provider logout this contains
    /// provider-scope/orphan entries for that Provider; for plugin logout it covers the whole plugin.
    pub secrets_revoked: usize,
    pub secret_cleanup_error: Option<String>,
}

impl PluginLogoutAllResult {
    pub fn local_failures(&self) -> usize {
        self.accounts
            .iter()
            .filter(|outcome| outcome.operation_error.is_some())
            .count()
    }

    pub fn remote_failures(&self) -> usize {
        self.accounts
            .iter()
            .filter(|outcome| {
                outcome.operation_error.is_none()
                    && (!outcome.remote_acknowledged || outcome.remote_error.is_some())
            })
            .count()
    }

    pub fn secret_cleanup_failures(&self) -> usize {
        let scope_failure = if self.secret_cleanup_error.is_some() { 1 } else { 0 };
        scope_failure
            + self
                .accounts
                .iter()
                .filter(|outcome| outcome.secret_cleanup_error.is_some())
                .count()
    }
}

impl PluginServiceFrontend {
    /// Secure single-account logout entry for application/UI callers.
    ///
    /// The existing frontend logout primitive remains responsible for the authoritative local
    /// SessionCoordinator transition and best-effort guest logout. This wrapper always follows it
    /// with exact Host account-namespace Secret deletion. Cleanup happens after the guest call so a
    /// buggy guest cannot recreate a cookie/token during its own logout callback and leave it behind.
    pub async fn logout_account(&self, key: &PluginAccountKey) -> Result<PluginAccountLogoutOutcome> {
        validate_account_key(key)?;

        let (local_state_changed, remote_acknowledged, remote_error, operation_error) =
            match self.logout(key).await {
                Ok(PluginLogoutResult {
                    local_state_changed,
                    remote_acknowledged,
                    remote_error,
                }) => (
                    local_state_changed,
                    remote_acknowledged,
                    remote_error,
                    None,
                ),
                Err(error) => (false, false, None, Some(format!("{error:#}"))),
            };

        let (secrets_revoked, secret_cleanup_error) = match secrets::global() {
            Some(store) => match store.delete_account(
                &key.plugin_id,
                &key.provider_id,
                &key.account_id,
            ) {
                Ok(removed) => (removed, None),
                Err(error) => (0, Some(format!("{error:#}"))),
            },
            None => (0, Some("插件 Secret backend 尚未初始化".into())),
        };

        Ok(PluginAccountLogoutOutcome {
            key: key.clone(),
            local_state_changed,
            remote_acknowledged,
            remote_error,
            operation_error,
            secrets_revoked,
            secret_cleanup_error,
        })
    }

    /// Log out every account owned by one Provider and revoke that Provider's complete Secret scope
    /// without touching sibling Providers from the same plugin.
    pub async fn logout_provider_all(
        &self,
        plugin_id: &str,
        provider_id: &str,
    ) -> Result<PluginLogoutAllResult> {
        validate_provider_scope(plugin_id, provider_id)?;
        let keys = account_keys(plugin_id, Some(provider_id))?;
        let mut result = logout_keys(self, keys).await?;

        match secrets::global() {
            Some(store) => match store.delete_provider(plugin_id, provider_id) {
                Ok(removed) => result.secrets_revoked = removed,
                Err(error) => result.secret_cleanup_error = Some(format!("{error:#}")),
            },
            None => {
                result.secret_cleanup_error = Some("插件 Secret backend 尚未初始化".into());
            }
        }
        Ok(result)
    }

    /// Log out every account owned by one exact plugin and then revoke the plugin's complete Host
    /// Secret namespace.
    ///
    /// A failure for one account never stops sibling accounts. Per-account cleanup first removes
    /// exact account namespaces; the final plugin-wide wipe removes provider-scope/orphan secrets and
    /// also runs when the Host currently has zero account rows for the plugin.
    pub async fn logout_plugin_all(&self, plugin_id: &str) -> Result<PluginLogoutAllResult> {
        validate_plugin_id(plugin_id)?;
        let keys = account_keys(plugin_id, None)?;
        let mut result = logout_keys(self, keys).await?;

        match secrets::global() {
            Some(store) => match store.delete_plugin(plugin_id) {
                Ok(removed) => result.secrets_revoked = removed,
                Err(error) => result.secret_cleanup_error = Some(format!("{error:#}")),
            },
            None => {
                result.secret_cleanup_error = Some("插件 Secret backend 尚未初始化".into());
            }
        }
        Ok(result)
    }
}

async fn logout_keys(
    frontend: &PluginServiceFrontend,
    keys: Vec<PluginAccountKey>,
) -> Result<PluginLogoutAllResult> {
    if keys.len() > MAX_PLUGIN_LOGOUT_ACCOUNTS {
        bail!(
            "插件账号数量超过批量退出上限 {}: {}",
            MAX_PLUGIN_LOGOUT_ACCOUNTS,
            keys.len()
        );
    }

    let mut result = PluginLogoutAllResult {
        accounts: Vec::with_capacity(keys.len()),
        ..PluginLogoutAllResult::default()
    };
    for key in keys {
        match frontend.logout_account(&key).await {
            Ok(outcome) => result.accounts.push(outcome),
            Err(error) => result.accounts.push(PluginAccountLogoutOutcome {
                key,
                local_state_changed: false,
                remote_acknowledged: false,
                remote_error: None,
                operation_error: Some(format!("{error:#}")),
                secrets_revoked: 0,
                secret_cleanup_error: None,
            }),
        }
    }
    Ok(result)
}

fn account_keys(plugin_id: &str, provider_id: Option<&str>) -> Result<Vec<PluginAccountKey>> {
    let host = catalog::global().ok_or_else(|| anyhow!("插件宿主状态尚未初始化"))?;
    let host = host
        .read()
        .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?;
    let mut keys = host
        .router()
        .accounts()
        .iter()
        .filter(|account| {
            account.plugin_id == plugin_id
                && provider_id.is_none_or(|provider_id| account.provider_id == provider_id)
        })
        .map(PluginAccountKey::from)
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| {
        left.provider_id
            .cmp(&right.provider_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    Ok(keys)
}

fn validate_plugin_id(plugin_id: &str) -> Result<()> {
    if plugin_id.trim().is_empty()
        || plugin_id.len() > MAX_PLUGIN_ID_BYTES
        || plugin_id.contains('\0')
    {
        bail!("插件 id 非法");
    }
    Ok(())
}

fn validate_provider_scope(plugin_id: &str, provider_id: &str) -> Result<()> {
    let _ = SecretSlot::provider(
        plugin_id.to_owned(),
        provider_id.to_owned(),
        "logout_probe",
    )?;
    Ok(())
}

fn validate_account_key(key: &PluginAccountKey) -> Result<()> {
    // Reuse the Host Secret namespace validator without exposing or persisting a real probe key.
    let _ = SecretSlot::account(
        key.plugin_id.clone(),
        key.provider_id.clone(),
        key.account_id.clone(),
        "logout_probe",
    )?;
    Ok(())
}
