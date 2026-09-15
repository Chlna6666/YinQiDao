use anyhow::{Result, anyhow, bail};

use super::{
    frontend::{PluginLogoutResult, PluginServiceFrontend},
    host::{catalog, runtime as host_runtime, sessions::PluginAccountKey},
};

const MAX_PLUGIN_LOGOUT_ACCOUNTS: usize = 512;
const MAX_PLUGIN_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginAccountLogoutOutcome {
    pub key: PluginAccountKey,
    pub local_state_changed: bool,
    pub remote_acknowledged: bool,
    pub remote_error: Option<String>,
    /// Failure before the normal single-account logout result could be produced. Processing of
    /// sibling accounts continues so one poisoned/invalid provider cannot keep other local sessions
    /// authenticated.
    pub operation_error: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginLogoutAllResult {
    pub accounts: Vec<PluginAccountLogoutOutcome>,
    /// Number of Host-owned Secret entries removed from the plugin namespace after all account
    /// logout attempts. Whole-plugin logout deliberately revokes provider-scope secrets too.
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
}

impl PluginServiceFrontend {
    /// Log out every account owned by one exact plugin and then revoke the plugin's complete Host
    /// Secret namespace.
    ///
    /// This is an application/control-path operation only. Each account reuses the normal
    /// single-account `logout` primitive, whose local SessionCoordinator transition is authoritative
    /// and occurs before best-effort guest cleanup. A remote failure is recorded per account and does
    /// not stop sibling accounts from being logged out locally. Finally the Host removes all plugin
    /// secrets even when there are no remaining account rows, which also cleans orphan/provider-scope
    /// credentials without exposing Secret keys to guest code.
    pub async fn logout_plugin_all(&self, plugin_id: &str) -> Result<PluginLogoutAllResult> {
        validate_plugin_id(plugin_id)?;
        let keys = plugin_account_keys(plugin_id)?;
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
            match self.logout(&key).await {
                Ok(PluginLogoutResult {
                    local_state_changed,
                    remote_acknowledged,
                    remote_error,
                }) => result.accounts.push(PluginAccountLogoutOutcome {
                    key,
                    local_state_changed,
                    remote_acknowledged,
                    remote_error,
                    operation_error: None,
                }),
                Err(error) => result.accounts.push(PluginAccountLogoutOutcome {
                    key,
                    local_state_changed: false,
                    remote_acknowledged: false,
                    remote_error: None,
                    operation_error: Some(format!("{error:#}")),
                }),
            }
        }

        match host_runtime::global() {
            Some(runtime) => match runtime.revoke_all_plugin_secrets(plugin_id) {
                Ok(removed) => result.secrets_revoked = removed,
                Err(error) => result.secret_cleanup_error = Some(format!("{error:#}")),
            },
            None => {
                result.secret_cleanup_error = Some("插件 Host runtime 尚未初始化".into());
            }
        }

        Ok(result)
    }
}

fn plugin_account_keys(plugin_id: &str) -> Result<Vec<PluginAccountKey>> {
    let host = catalog::global().ok_or_else(|| anyhow!("插件宿主状态尚未初始化"))?;
    let host = host
        .read()
        .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?;
    let mut keys = host
        .router()
        .accounts()
        .iter()
        .filter(|account| account.plugin_id == plugin_id)
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
