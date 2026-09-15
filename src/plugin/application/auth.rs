use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};

use super::{
    abi::{
        AccountState, AuthPollResult, KeyValue, PluginAccount, PluginCapability, ProviderAccount,
    },
    client,
    frontend::PluginServiceFrontend,
    host::{
        catalog,
        runtime::{self as host_runtime, PluginCallKey},
        sessions,
    },
};

const MAX_AUTH_CHALLENGE_ID_BYTES: usize = 1_024;
const MAX_AUTH_SUBMIT_FIELDS: usize = 64;
const MAX_AUTH_FIELD_ID_BYTES: usize = 256;
const MAX_AUTH_FIELD_VALUE_BYTES: usize = 64 * 1_024;
const MAX_AUTH_SUBMIT_PAYLOAD_BYTES: usize = 128 * 1_024;
const MAX_PLUGIN_ACCOUNT_TEXT_BYTES: usize = 8 * 1_024;

impl PluginServiceFrontend {
    /// Submit one Host-owned authentication form without persisting raw user input in application
    /// state. `AuthChallenge.fields` uses `KeyValue` as `field-id -> display label`; submitted
    /// values use the same stable field ids and exist only for this guest call. Long-lived cookies,
    /// refresh tokens or device secrets must still be persisted by the guest through Host
    /// `secret-set`, never through ordinary application config.
    pub async fn auth_submit(
        &self,
        plugin_id: &str,
        provider_id: &str,
        challenge_id: &str,
        values: &[KeyValue],
    ) -> Result<AuthPollResult> {
        validate_challenge_id(challenge_id)?;
        validate_submission(values)?;

        let runtime = host_runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let provider = runtime
            .catalog()
            .provider(plugin_id, provider_id)
            .ok_or_else(|| anyhow!("未安装插件 Provider: {plugin_id}/{provider_id}"))?;
        if !provider
            .capabilities
            .contains(&PluginCapability::Authentication)
        {
            bail!("插件 Provider 未声明 authentication capability");
        }

        let clients = client::global().ok_or_else(|| anyhow!("插件 Provider client 尚未初始化"))?;
        let client = clients
            .client()?
            .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))?;
        let result = runtime
            .execute_guest_call(
                PluginCallKey::provider(plugin_id, provider_id),
                client.auth_submit(plugin_id, provider_id, challenge_id, values),
            )
            .await?;

        if let AuthPollResult::Authenticated(account) = &result {
            accept_authenticated_account(plugin_id, provider_id, account.clone())?;
        }
        Ok(result)
    }
}

fn validate_challenge_id(challenge_id: &str) -> Result<()> {
    if challenge_id.trim().is_empty()
        || challenge_id.len() > MAX_AUTH_CHALLENGE_ID_BYTES
        || challenge_id.contains('\0')
    {
        bail!("插件 auth challenge id 非法");
    }
    Ok(())
}

fn validate_submission(values: &[KeyValue]) -> Result<()> {
    if values.len() > MAX_AUTH_SUBMIT_FIELDS {
        bail!("插件 auth submit fields 数量超过限制");
    }

    let mut field_ids = HashSet::with_capacity(values.len());
    let mut payload_bytes = 0usize;
    for field in values {
        if field.key.trim().is_empty()
            || field.key.len() > MAX_AUTH_FIELD_ID_BYTES
            || field.key.contains('\0')
        {
            bail!("插件 auth submit field id 非法");
        }
        if !field_ids.insert(field.key.as_str()) {
            bail!("插件 auth submit field id 重复: {}", field.key);
        }
        if field.value.len() > MAX_AUTH_FIELD_VALUE_BYTES || field.value.contains('\0') {
            bail!("插件 auth submit field value 非法或超过大小限制");
        }
        payload_bytes = payload_bytes
            .checked_add(field.key.len())
            .and_then(|bytes| bytes.checked_add(field.value.len()))
            .ok_or_else(|| anyhow!("插件 auth submit payload 大小溢出"))?;
        if payload_bytes > MAX_AUTH_SUBMIT_PAYLOAD_BYTES {
            bail!("插件 auth submit payload 超过大小限制");
        }
    }
    Ok(())
}

fn accept_authenticated_account(
    plugin_id: &str,
    provider_id: &str,
    account: ProviderAccount,
) -> Result<()> {
    validate_provider_account(&account, provider_id)?;

    let host = catalog::global().ok_or_else(|| anyhow!("插件宿主状态尚未初始化"))?;
    let sessions = sessions::global().ok_or_else(|| anyhow!("插件会话状态尚未初始化"))?;
    let mut sessions = sessions
        .write()
        .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?;

    // Global lock order is sessions -> host. Preserve an existing account's priority/default while
    // fresh authentication updates provider-owned display/capability metadata.
    let (priority, is_default) = {
        let host = host
            .read()
            .map_err(|error| anyhow!("插件宿主状态锁已损坏: {error}"))?;
        let provider_accounts = host.router().accounts().iter().filter(|existing| {
            existing.plugin_id == plugin_id && existing.provider_id == provider_id
        });
        let mut existing_priority = None;
        let mut existing_default = false;
        let mut provider_has_default = false;
        for existing in provider_accounts {
            provider_has_default |= existing.is_default;
            if existing.account_id == account.account_id {
                existing_priority = Some(existing.priority);
                existing_default = existing.is_default;
            }
        }
        (
            existing_priority.unwrap_or(0),
            if existing_priority.is_some() {
                existing_default
            } else {
                !provider_has_default
            },
        )
    };

    sessions.register_authenticated_account(
        &host,
        PluginAccount {
            plugin_id: plugin_id.to_owned(),
            provider_id: provider_id.to_owned(),
            account_id: account.account_id,
            display_name: account.display_name,
            avatar_url: account.avatar_url,
            state: AccountState::Authenticated,
            capabilities: account.capabilities,
            priority,
            is_default,
        },
    )
}

fn validate_provider_account(account: &ProviderAccount, provider_id: &str) -> Result<()> {
    if account.provider_id != provider_id {
        bail!(
            "插件账号 provider 不匹配: expected={provider_id}, actual={}",
            account.provider_id
        );
    }
    if account.account_id.trim().is_empty()
        || account.account_id.len() > 512
        || account.account_id.contains('\0')
    {
        bail!("插件 account id 非法");
    }
    let text_bytes = account
        .display_name
        .len()
        .saturating_add(account.avatar_url.as_ref().map_or(0, String::len));
    if text_bytes > MAX_PLUGIN_ACCOUNT_TEXT_BYTES {
        bail!("插件账号展示信息超过大小限制");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, value: &str) -> KeyValue {
        KeyValue {
            key: key.into(),
            value: value.into(),
        }
    }

    #[test]
    fn auth_submit_accepts_unique_bounded_fields() {
        validate_submission(&[field("cookie", "MUSIC_U=secret"), field("csrf", "token")])
            .expect("valid submission");
    }

    #[test]
    fn auth_submit_rejects_duplicate_field_ids() {
        let error = validate_submission(&[field("cookie", "a"), field("cookie", "b")])
            .expect_err("duplicate field must fail");
        assert!(error.to_string().contains("重复"));
    }

    #[test]
    fn auth_submit_rejects_oversized_value() {
        let error = validate_submission(&[field(
            "cookie",
            &"x".repeat(MAX_AUTH_FIELD_VALUE_BYTES + 1),
        )])
        .expect_err("oversized value must fail");
        assert!(error.to_string().contains("大小限制"));
    }
}
