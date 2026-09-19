use std::{
    collections::{HashMap, HashSet},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Result, anyhow, bail};

use super::{
    abi::{
        AccountState, AuthChallenge, AuthChallengeKind, AuthMethod, AuthPollResult, KeyValue,
        PluginAccount, PluginCapability, ProviderAccount,
    },
    client,
    frontend::PluginServiceFrontend,
    host::{
        catalog, package_manager,
        runtime::{self as host_runtime, PluginCallKey},
        sessions,
    },
    runtime_ports,
};

const MAX_AUTH_CHALLENGE_ID_BYTES: usize = 1_024;
const MAX_AUTH_SUBMIT_FIELDS: usize = 64;
const MAX_AUTH_FIELD_ID_BYTES: usize = 256;
const MAX_AUTH_FIELD_LABEL_BYTES: usize = 4 * 1_024;
const MAX_AUTH_FIELD_VALUE_BYTES: usize = 64 * 1_024;
const MAX_AUTH_SUBMIT_PAYLOAD_BYTES: usize = 128 * 1_024;
const MAX_AUTH_DENIED_REASON_BYTES: usize = 8 * 1_024;
const MAX_PLUGIN_ACCOUNT_TEXT_BYTES: usize = 8 * 1_024;
const MAX_ACTIVE_AUTH_FLOWS: usize = 32;

static AUTH_FLOWS: OnceLock<Mutex<HashMap<u64, ActiveAuthFlow>>> = OnceLock::new();
static NEXT_AUTH_FLOW_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct PluginAuthFlowSnapshot {
    pub flow_id: u64,
    pub plugin_id: String,
    pub provider_id: String,
    pub method: AuthMethod,
    pub challenge: AuthChallenge,
}

#[derive(Clone, Debug)]
struct ActiveAuthFlow {
    plugin_id: String,
    provider_id: String,
    method: AuthMethod,
    challenge: AuthChallenge,
    package_generation: u64,
}

impl ActiveAuthFlow {
    fn snapshot(&self, flow_id: u64) -> PluginAuthFlowSnapshot {
        PluginAuthFlowSnapshot {
            flow_id,
            plugin_id: self.plugin_id.clone(),
            provider_id: self.provider_id.clone(),
            method: self.method,
            challenge: self.challenge.clone(),
        }
    }
}

impl PluginServiceFrontend {
    /// Submit one authentication form directly through the runtime-neutral Provider port.
    ///
    /// Ordinary Host UI should prefer `auth_flow_submit`, which binds values to a tracked challenge
    /// and closes cancel/update races. This lower-level method remains useful to non-UI controller
    /// code that already owns equivalent challenge lifetime guarantees.
    pub async fn auth_submit(
        &self,
        plugin_id: &str,
        provider_id: &str,
        challenge_id: &str,
        values: &[KeyValue],
    ) -> Result<AuthPollResult> {
        validate_challenge_id(challenge_id)?;
        validate_submission(values)?;
        ensure_plugin_enabled(plugin_id)?;
        let package_generation = runtime_ports::package_mutation_generation();
        let result = execute_auth_submit(plugin_id, provider_id, challenge_id, values).await?;
        if runtime_ports::package_mutation_generation() != package_generation {
            bail!("插件在认证表单提交期间已更新，旧认证结果已丢弃");
        }
        if let AuthPollResult::Authenticated(account) = &result {
            accept_authenticated_account(plugin_id, provider_id, account.clone())?;
        }
        Ok(result)
    }

    /// Begin one Host-owned authentication flow and retain only challenge metadata. Submitted form
    /// values are never stored in this registry. The flow is tied to the package mutation generation
    /// so update/disable/uninstall makes every old challenge fail closed.
    pub async fn auth_flow_begin(
        &self,
        plugin_id: &str,
        provider_id: &str,
        method: AuthMethod,
    ) -> Result<PluginAuthFlowSnapshot> {
        ensure_plugin_enabled(plugin_id)?;
        let package_generation = runtime_ports::package_mutation_generation();
        let challenge = self.auth_begin(plugin_id, provider_id, method).await?;
        validate_tracked_challenge(method, &challenge)?;

        if runtime_ports::package_mutation_generation() != package_generation {
            bail!("插件在认证 challenge 创建期间已更新，旧 challenge 已丢弃");
        }

        let mut flows = lock_auth_flows()?;
        let current_generation = runtime_ports::package_mutation_generation();
        if current_generation != package_generation {
            bail!("插件在认证 challenge 入队期间已更新，旧 challenge 已丢弃");
        }
        prune_stale_auth_flows(&mut flows, current_generation);
        if flows.len() >= MAX_ACTIVE_AUTH_FLOWS {
            bail!(
                "Host 活跃插件认证 flow 已达到 {} 个上限",
                MAX_ACTIVE_AUTH_FLOWS
            );
        }
        let flow_id = allocate_flow_id(&flows);
        let flow = ActiveAuthFlow {
            plugin_id: plugin_id.to_owned(),
            provider_id: provider_id.to_owned(),
            method,
            challenge,
            package_generation,
        };
        let snapshot = flow.snapshot(flow_id);
        flows.insert(flow_id, flow);
        Ok(snapshot)
    }

    /// Clone one Host-owned challenge snapshot for GPUI/controller rendering. No submitted secret
    /// value is ever retained, so this snapshot is safe for normal UI state.
    pub fn auth_flow_snapshot(&self, flow_id: u64) -> Result<Option<PluginAuthFlowSnapshot>> {
        let _ = self;
        let mut flows = lock_auth_flows()?;
        let Some(flow) = flows.get(&flow_id).cloned() else {
            return Ok(None);
        };
        if flow.package_generation != runtime_ports::package_mutation_generation() {
            flows.remove(&flow_id);
            return Ok(None);
        }
        Ok(Some(flow.snapshot(flow_id)))
    }

    /// Poll QR/browser/device-code authentication without allowing a canceled or stale in-flight
    /// result to resurrect an account. Terminal results consume the flow before account publication.
    pub async fn auth_flow_poll(&self, flow_id: u64) -> Result<AuthPollResult> {
        let _ = self;
        let flow = current_flow(flow_id)?;
        let result = execute_auth_poll(
            &flow.plugin_id,
            &flow.provider_id,
            &flow.challenge.challenge_id,
        )
        .await?;
        commit_flow_result(flow_id, &flow, result)
    }

    /// Submit a Host-rendered form. The field set must exactly match the original challenge; extra,
    /// missing or duplicate field ids are rejected before guest code executes.
    pub async fn auth_flow_submit(
        &self,
        flow_id: u64,
        values: &[KeyValue],
    ) -> Result<AuthPollResult> {
        let _ = self;
        let flow = current_flow(flow_id)?;
        if flow.challenge.kind != AuthChallengeKind::Form {
            bail!("当前认证 flow 不是表单 challenge，拒绝提交字段");
        }
        validate_submission(values)?;
        validate_submission_against_challenge(&flow.challenge, values)?;
        let result = execute_auth_submit(
            &flow.plugin_id,
            &flow.provider_id,
            &flow.challenge.challenge_id,
            values,
        )
        .await?;
        commit_flow_result(flow_id, &flow, result)
    }

    /// Cancel locally first, then best-effort notify the guest. Once this method starts, any
    /// concurrent poll/submit result loses its flow ticket and cannot publish Authenticated state.
    pub async fn auth_flow_cancel(&self, flow_id: u64) -> Result<bool> {
        let flow = {
            let mut flows = lock_auth_flows()?;
            flows
                .remove(&flow_id)
                .ok_or_else(|| anyhow!("插件认证 flow 已不存在或已结束: {flow_id}"))?
        };
        if flow.package_generation != runtime_ports::package_mutation_generation() {
            bail!("插件认证 flow 已因插件更新失效");
        }
        self.auth_cancel(
            &flow.plugin_id,
            &flow.provider_id,
            &flow.challenge.challenge_id,
        )
        .await
    }
}

fn auth_flows() -> &'static Mutex<HashMap<u64, ActiveAuthFlow>> {
    AUTH_FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock_auth_flows() -> Result<std::sync::MutexGuard<'static, HashMap<u64, ActiveAuthFlow>>> {
    auth_flows()
        .lock()
        .map_err(|error| anyhow!("插件认证 flow registry 锁已损坏: {error}"))
}

fn prune_stale_auth_flows(
    flows: &mut HashMap<u64, ActiveAuthFlow>,
    current_generation: u64,
) -> usize {
    let before = flows.len();
    flows.retain(|_, flow| flow.package_generation == current_generation);
    before.saturating_sub(flows.len())
}

fn allocate_flow_id(flows: &HashMap<u64, ActiveAuthFlow>) -> u64 {
    loop {
        let id = NEXT_AUTH_FLOW_ID.fetch_add(1, Ordering::Relaxed);
        if id != 0 && !flows.contains_key(&id) {
            return id;
        }
    }
}

fn current_flow(flow_id: u64) -> Result<ActiveAuthFlow> {
    let mut flows = lock_auth_flows()?;
    let flow = flows
        .get(&flow_id)
        .cloned()
        .ok_or_else(|| anyhow!("插件认证 flow 已不存在或已结束: {flow_id}"))?;
    if flow.package_generation != runtime_ports::package_mutation_generation() {
        flows.remove(&flow_id);
        bail!("插件认证 flow 已因插件更新失效");
    }
    ensure_plugin_enabled(&flow.plugin_id)?;
    Ok(flow)
}

fn commit_flow_result(
    flow_id: u64,
    expected: &ActiveAuthFlow,
    result: AuthPollResult,
) -> Result<AuthPollResult> {
    let terminal = !matches!(result, AuthPollResult::Pending);
    let mut flows = lock_auth_flows()?;
    let Some(current) = flows.get(&flow_id) else {
        bail!("认证结果已丢弃：flow 已取消或被替换");
    };
    let still_current = current.package_generation == expected.package_generation
        && current.package_generation == runtime_ports::package_mutation_generation()
        && current.plugin_id == expected.plugin_id
        && current.provider_id == expected.provider_id
        && current.challenge.challenge_id == expected.challenge.challenge_id;
    if !still_current {
        flows.remove(&flow_id);
        bail!("认证结果已丢弃：插件或 challenge 代际已变化");
    }

    if terminal {
        flows.remove(&flow_id);
    }
    drop(flows);

    if terminal {
        if let AuthPollResult::Authenticated(account) = &result {
            accept_authenticated_account(
                &expected.plugin_id,
                &expected.provider_id,
                account.clone(),
            )?;
        }
    }
    Ok(result)
}

async fn execute_auth_poll(
    plugin_id: &str,
    provider_id: &str,
    challenge_id: &str,
) -> Result<AuthPollResult> {
    validate_challenge_id(challenge_id)?;
    ensure_plugin_enabled(plugin_id)?;
    let runtime = require_auth_runtime(plugin_id, provider_id)?;
    let client = require_provider_client()?;
    let result = runtime
        .execute_guest_call(
            PluginCallKey::provider(plugin_id, provider_id),
            client.auth_poll(plugin_id, provider_id, challenge_id),
        )
        .await?;
    validate_auth_result(&result, provider_id)?;
    Ok(result)
}

async fn execute_auth_submit(
    plugin_id: &str,
    provider_id: &str,
    challenge_id: &str,
    values: &[KeyValue],
) -> Result<AuthPollResult> {
    validate_challenge_id(challenge_id)?;
    validate_submission(values)?;
    ensure_plugin_enabled(plugin_id)?;
    let runtime = require_auth_runtime(plugin_id, provider_id)?;
    let client = require_provider_client()?;
    let result = runtime
        .execute_guest_call(
            PluginCallKey::provider(plugin_id, provider_id),
            client.auth_submit(plugin_id, provider_id, challenge_id, values),
        )
        .await?;
    validate_auth_result(&result, provider_id)?;
    Ok(result)
}

fn require_auth_runtime(
    plugin_id: &str,
    provider_id: &str,
) -> Result<std::sync::Arc<host_runtime::PluginHostServices>> {
    let runtime = host_runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
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
    Ok(runtime)
}

fn require_provider_client() -> Result<std::sync::Arc<dyn client::PluginProviderClient>> {
    let clients = client::global().ok_or_else(|| anyhow!("插件 Provider client 尚未初始化"))?;
    clients
        .client()?
        .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))
}

fn ensure_plugin_enabled(plugin_id: &str) -> Result<()> {
    let manager = package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))?;
    if !manager.is_enabled(plugin_id) {
        bail!("插件已禁用，拒绝继续认证: {plugin_id}");
    }
    Ok(())
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

fn validate_auth_result(result: &AuthPollResult, provider_id: &str) -> Result<()> {
    match result {
        AuthPollResult::Authenticated(account) => validate_provider_account(account, provider_id),
        AuthPollResult::Denied(reason) => {
            if reason.len() > MAX_AUTH_DENIED_REASON_BYTES || reason.contains('\0') {
                bail!("插件认证拒绝原因超过 Host 文本限制或包含 NUL");
            }
            Ok(())
        }
        AuthPollResult::Pending | AuthPollResult::Expired => Ok(()),
    }
}

fn validate_tracked_challenge(method: AuthMethod, challenge: &AuthChallenge) -> Result<()> {
    validate_challenge_id(&challenge.challenge_id)?;
    let expected_kind = match method {
        AuthMethod::QrCode => AuthChallengeKind::QrCode,
        AuthMethod::BrowserOAuth => AuthChallengeKind::Browser,
        AuthMethod::DeviceCode => AuthChallengeKind::DeviceCode,
        AuthMethod::CookieImport | AuthMethod::CustomForm => AuthChallengeKind::Form,
    };
    if challenge.kind != expected_kind {
        bail!(
            "插件认证 challenge kind 与请求方式不匹配: method={method:?}, kind={:?}",
            challenge.kind
        );
    }

    match challenge.kind {
        AuthChallengeKind::QrCode => {
            if challenge.qr_payload.as_deref().is_none_or(str::is_empty) {
                bail!("QR 认证 challenge 缺少 qr_payload");
            }
            if !challenge.fields.is_empty() {
                bail!("QR 认证 challenge 不应包含 form fields");
            }
        }
        AuthChallengeKind::Browser => {
            validate_https_uri(challenge.verification_uri.as_deref(), "Browser OAuth")?;
            if !challenge.fields.is_empty() {
                bail!("Browser OAuth challenge 不应包含 form fields");
            }
        }
        AuthChallengeKind::DeviceCode => {
            validate_https_uri(challenge.verification_uri.as_deref(), "Device Code")?;
            if challenge.user_code.as_deref().is_none_or(str::is_empty) {
                bail!("Device Code challenge 缺少 user_code");
            }
            if !challenge.fields.is_empty() {
                bail!("Device Code challenge 不应包含 form fields");
            }
        }
        AuthChallengeKind::Form => validate_form_fields(&challenge.fields)?,
    }
    Ok(())
}

fn validate_https_uri(uri: Option<&str>, context: &str) -> Result<()> {
    let Some(uri) = uri.filter(|uri| !uri.is_empty()) else {
        bail!("{context} challenge 缺少 verification_uri");
    };
    if uri.contains('\0')
        || !uri
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        bail!("{context} verification_uri 必须使用 HTTPS");
    }
    Ok(())
}

fn validate_form_fields(fields: &[KeyValue]) -> Result<()> {
    if fields.is_empty() || fields.len() > MAX_AUTH_SUBMIT_FIELDS {
        bail!("插件 form challenge fields 数量非法");
    }
    let mut ids = HashSet::with_capacity(fields.len());
    for field in fields {
        if field.key.trim().is_empty()
            || field.key.len() > MAX_AUTH_FIELD_ID_BYTES
            || field.key.contains('\0')
        {
            bail!("插件 form challenge field id 非法");
        }
        if !ids.insert(field.key.as_str()) {
            bail!("插件 form challenge field id 重复: {}", field.key);
        }
        if field.value.trim().is_empty()
            || field.value.len() > MAX_AUTH_FIELD_LABEL_BYTES
            || field.value.contains('\0')
        {
            bail!("插件 form challenge field label 非法或超过大小限制");
        }
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

fn validate_submission_against_challenge(
    challenge: &AuthChallenge,
    values: &[KeyValue],
) -> Result<()> {
    if values.len() != challenge.fields.len() {
        bail!("插件 auth submit field 数量与 challenge 不一致");
    }
    let expected = challenge
        .fields
        .iter()
        .map(|field| field.key.as_str())
        .collect::<HashSet<_>>();
    if values
        .iter()
        .any(|field| !expected.contains(field.key.as_str()))
    {
        bail!("插件 auth submit 包含 challenge 未声明的 field id");
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
    if text_bytes > MAX_PLUGIN_ACCOUNT_TEXT_BYTES
        || account.display_name.contains('\0')
        || account
            .avatar_url
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
    {
        bail!("插件账号展示信息超过大小限制或包含 NUL");
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

    fn form_challenge() -> AuthChallenge {
        AuthChallenge {
            challenge_id: "challenge".into(),
            kind: AuthChallengeKind::Form,
            verification_uri: None,
            user_code: None,
            qr_payload: None,
            fields: vec![field("cookie", "Cookie"), field("csrf", "CSRF Token")],
            expires_at_ms: None,
        }
    }

    fn active_flow(package_generation: u64) -> ActiveAuthFlow {
        ActiveAuthFlow {
            plugin_id: "plugin.test".into(),
            provider_id: "test".into(),
            method: AuthMethod::CookieImport,
            challenge: form_challenge(),
            package_generation,
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
        let error =
            validate_submission(&[field("cookie", &"x".repeat(MAX_AUTH_FIELD_VALUE_BYTES + 1))])
                .expect_err("oversized value must fail");
        assert!(error.to_string().contains("大小限制"));
    }

    #[test]
    fn auth_result_rejects_unbounded_or_nul_denied_reason() {
        assert!(validate_auth_result(&AuthPollResult::Denied("denied".into()), "test").is_ok());
        assert!(
            validate_auth_result(
                &AuthPollResult::Denied("x".repeat(MAX_AUTH_DENIED_REASON_BYTES + 1)),
                "test"
            )
            .is_err()
        );
        assert!(
            validate_auth_result(&AuthPollResult::Denied("bad\0reason".into()), "test").is_err()
        );
    }

    #[test]
    fn provider_account_rejects_nul_display_metadata() {
        let account = ProviderAccount {
            account_id: "account".into(),
            provider_id: "test".into(),
            display_name: "bad\0name".into(),
            avatar_url: None,
            capabilities: Vec::new(),
        };
        assert!(validate_provider_account(&account, "test").is_err());
    }

    #[test]
    fn stale_auth_flows_are_pruned_before_capacity_check() {
        let mut flows = HashMap::from([
            (1, active_flow(7)),
            (2, active_flow(8)),
            (3, active_flow(7)),
        ]);
        assert_eq!(prune_stale_auth_flows(&mut flows, 8), 2);
        assert_eq!(flows.len(), 1);
        assert!(flows.contains_key(&2));
    }

    #[test]
    fn form_submission_must_match_original_field_set() {
        let challenge = form_challenge();
        validate_submission_against_challenge(
            &challenge,
            &[field("cookie", "a"), field("csrf", "b")],
        )
        .expect("exact field set");
        assert!(
            validate_submission_against_challenge(
                &challenge,
                &[field("cookie", "a"), field("other", "b")],
            )
            .is_err()
        );
    }

    #[test]
    fn auth_method_must_match_challenge_kind() {
        let challenge = form_challenge();
        validate_tracked_challenge(AuthMethod::CookieImport, &challenge).expect("cookie form");
        assert!(validate_tracked_challenge(AuthMethod::QrCode, &challenge).is_err());
    }

    #[test]
    fn external_browser_challenges_require_https() {
        assert!(validate_https_uri(Some("https://example.com/login"), "test").is_ok());
        assert!(validate_https_uri(Some("http://example.com/login"), "test").is_err());
        assert!(validate_https_uri(Some("javascript:alert(1)"), "test").is_err());
    }
}
