use std::{
    fmt::Write as _,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow, bail};

use crate::lyrics::LyricsDocument;

use super::{
    abi::{
        AccountState, AuthChallenge, AuthMethod, AuthPollResult, LyricLine as PluginLyricLine,
        PluginAccount, PluginCapability, PluginLyricDocument, PluginRoute, ProviderAccount,
        RemoteTrack, RoutingPolicy, ServiceKind, SourceTrackRef, TrackQuery,
    },
    client::{PluginClientRegistry, PluginProviderClient},
    host::{
        catalog::PluginHostState,
        http::PluginHttpRequest,
        runtime::{PluginCallKey, PluginHostServices},
        sessions::{PluginAccountKey, PluginSessionCoordinator},
    },
    routing::gate::{GatedRoutePlan, plan_routes},
};

const MAX_PLUGIN_LYRIC_LINES: usize = 10_000;
const MAX_PLUGIN_LYRIC_WORDS_PER_LINE: usize = 4_096;
const MAX_PLUGIN_LYRIC_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PLUGIN_LYRIC_TTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLUGIN_LYRIC_SOURCE_BYTES: usize = 1_024;
const MAX_PLUGIN_ACCOUNTS_PER_PROVIDER: usize = 64;
const MAX_PLUGIN_ACCOUNT_TEXT_BYTES: usize = 8 * 1024;
const MAX_AUTH_CHALLENGE_ID_BYTES: usize = 1_024;
const MAX_AUTH_CHALLENGE_PAYLOAD_BYTES: usize = 128 * 1024;
const MAX_AUTH_CHALLENGE_FIELDS: usize = 64;

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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginLogoutResult {
    pub local_state_changed: bool,
    pub remote_acknowledged: bool,
    pub remote_error: Option<String>,
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

    /// Begin one Host-approved authentication flow. The requested auth method must be declared by
    /// the installed provider manifest; a guest cannot silently expose a new login surface.
    pub async fn auth_begin(
        &self,
        plugin_id: &str,
        provider_id: &str,
        method: AuthMethod,
    ) -> Result<AuthChallenge> {
        self.validate_auth_method(plugin_id, provider_id, method)?;
        let client = self.require_client()?;
        let key = PluginCallKey::provider(plugin_id, provider_id);
        let challenge = self
            .runtime
            .execute_guest_call(key, client.auth_begin(plugin_id, provider_id, method))
            .await?;
        validate_auth_challenge(&challenge)?;
        Ok(challenge)
    }

    /// Poll one authentication challenge and register a successful account through Host state.
    pub async fn auth_poll(
        &self,
        plugin_id: &str,
        provider_id: &str,
        challenge_id: &str,
    ) -> Result<AuthPollResult> {
        self.validate_auth_provider(plugin_id, provider_id)?;
        validate_challenge_id(challenge_id)?;
        let client = self.require_client()?;
        let key = PluginCallKey::provider(plugin_id, provider_id);
        let result = self
            .runtime
            .execute_guest_call(
                key,
                client.auth_poll(plugin_id, provider_id, challenge_id),
            )
            .await?;
        if let AuthPollResult::Authenticated(account) = &result {
            self.accept_authenticated_account(plugin_id, provider_id, account.clone())?;
        }
        Ok(result)
    }

    pub async fn auth_cancel(
        &self,
        plugin_id: &str,
        provider_id: &str,
        challenge_id: &str,
    ) -> Result<bool> {
        self.validate_auth_provider(plugin_id, provider_id)?;
        validate_challenge_id(challenge_id)?;
        let client = self.require_client()?;
        self.runtime
            .execute_guest_call(
                PluginCallKey::provider(plugin_id, provider_id),
                client.auth_cancel(plugin_id, provider_id, challenge_id),
            )
            .await
    }

    /// Query provider-owned sessions and validate a previously quarantined account.
    pub async fn restore_pending_account(&self, key: &PluginAccountKey) -> Result<bool> {
        self.validate_auth_provider(&key.plugin_id, &key.provider_id)?;
        let client = self.require_client()?;
        let accounts = self
            .runtime
            .execute_guest_call(
                PluginCallKey::provider(&key.plugin_id, &key.provider_id),
                client.accounts(&key.plugin_id, &key.provider_id),
            )
            .await?;
        validate_provider_accounts(&accounts, &key.provider_id)?;

        if let Some(account) = accounts
            .into_iter()
            .find(|account| account.account_id == key.account_id)
        {
            self.accept_authenticated_account(&key.plugin_id, &key.provider_id, account)?;
            return Ok(true);
        }

        self.sessions
            .write()
            .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?
            .mark_validation_failed(&self.host, key)?;
        Ok(false)
    }

    /// Local logout is authoritative and happens before best-effort remote cleanup.
    pub async fn logout(&self, key: &PluginAccountKey) -> Result<PluginLogoutResult> {
        let local_state_changed = self
            .sessions
            .write()
            .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?
            .logout(&self.host, key)?;

        let Some(client) = self.clients.client()? else {
            return Ok(PluginLogoutResult {
                local_state_changed,
                remote_acknowledged: false,
                remote_error: Some("插件 Provider client 尚未就绪".into()),
            });
        };

        match self
            .runtime
            .execute_guest_call(
                PluginCallKey::provider(&key.plugin_id, &key.provider_id),
                client.logout(&key.plugin_id, &key.provider_id, &key.account_id),
            )
            .await
        {
            Ok(remote_acknowledged) => Ok(PluginLogoutResult {
                local_state_changed,
                remote_acknowledged,
                remote_error: None,
            }),
            Err(error) => Ok(PluginLogoutResult {
                local_state_changed,
                remote_acknowledged: false,
                remote_error: Some(format!("{error:#}")),
            }),
        }
    }

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

    pub async fn lyrics_for_route(
        &self,
        metadata_route: &PluginRoute,
        track: &SourceTrackRef,
    ) -> Result<PluginSingleResult<PluginLyricDocument>> {
        self.validate_source_route(metadata_route, track, "歌词")?;
        let mut plan = self.same_provider_plan(ServiceKind::Lyrics, metadata_route)?;
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

    /// Fetch authenticated artwork through the Host HTTP boundary. The guest returns only a
    /// descriptor; it never owns the socket or response body.
    pub async fn artwork_for_route(
        &self,
        metadata_route: &PluginRoute,
        track: &SourceTrackRef,
    ) -> Result<PluginSingleResult<Vec<u8>>> {
        self.validate_source_route(metadata_route, track, "封面")?;
        let mut plan = self.same_provider_plan(ServiceKind::Artwork, metadata_route)?;
        let Some(client) = self.clients.client()? else {
            return Ok(PluginSingleResult::unavailable(plan));
        };

        let mut failures = Vec::new();
        for route in &plan.eligible_routes {
            let key = PluginCallKey::provider(&route.plugin_id, &route.provider_id);
            let descriptor = match self
                .runtime
                .execute_guest_call(
                    key,
                    client.artwork(
                        &route.plugin_id,
                        &route.provider_id,
                        Some(&route.account_id),
                        track,
                    ),
                )
                .await
            {
                Ok(Some(descriptor)) => descriptor,
                Ok(None) => continue,
                Err(error) => {
                    failures.push(PluginCallFailure {
                        route: route.clone(),
                        error: format!("{error:#}"),
                    });
                    continue;
                }
            };

            if descriptor.url.trim().is_empty() {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: "插件 artwork descriptor URL 为空".into(),
                });
                continue;
            }
            if descriptor
                .expires_at_ms
                .is_some_and(|expires_at_ms| expires_at_ms <= self.runtime.now_ms())
            {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: "插件 artwork descriptor 已过期".into(),
                });
                continue;
            }

            let response = match self
                .runtime
                .http_request(
                    &route.plugin_id,
                    &route.provider_id,
                    Some(&route.account_id),
                    PluginHttpRequest {
                        method: "GET".into(),
                        url: descriptor.url,
                        headers: descriptor.headers,
                        body: Vec::new(),
                    },
                )
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    failures.push(PluginCallFailure {
                        route: route.clone(),
                        error: format!("artwork Host HTTP 失败: {error:#}"),
                    });
                    continue;
                }
            };
            if !(200..300).contains(&response.status) {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("artwork HTTP 状态码 {}", response.status),
                });
                continue;
            }
            if response.body.is_empty() {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: "artwork HTTP body 为空".into(),
                });
                continue;
            }
            return Ok(PluginSingleResult {
                value: Some(response.body),
                route: Some(route.clone()),
                plan,
                failures,
                client_ready: true,
            });
        }

        Ok(PluginSingleResult {
            value: None,
            route: None,
            plan,
            failures,
            client_ready: true,
        })
    }

    fn same_provider_plan(
        &self,
        service: ServiceKind,
        metadata_route: &PluginRoute,
    ) -> Result<GatedRoutePlan> {
        let policy = RoutingPolicy {
            preferred_provider: Some(metadata_route.provider_id.clone()),
            ..RoutingPolicy::default()
        };
        let mut plan = self.plan(service, &policy)?;
        plan.eligible_routes.retain(|route| {
            route.plugin_id == metadata_route.plugin_id
                && route.provider_id == metadata_route.provider_id
        });
        plan.plan.plugin_routes = plan.eligible_routes.first().cloned().into_iter().collect();
        Ok(plan)
    }

    fn validate_source_route(
        &self,
        metadata_route: &PluginRoute,
        track: &SourceTrackRef,
        operation: &str,
    ) -> Result<()> {
        if track.provider_id != metadata_route.provider_id {
            bail!(
                "{operation} source provider 与 metadata route 不一致: source={}, route={}",
                track.provider_id,
                metadata_route.provider_id
            );
        }
        Ok(())
    }

    fn require_client(&self) -> Result<Arc<dyn PluginProviderClient>> {
        self.clients
            .client()?
            .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))
    }

    fn validate_auth_provider(&self, plugin_id: &str, provider_id: &str) -> Result<()> {
        let provider = self
            .runtime
            .catalog()
            .provider(plugin_id, provider_id)
            .ok_or_else(|| anyhow!("未安装插件 Provider: {plugin_id}/{provider_id}"))?;
        if !provider
            .capabilities
            .contains(&PluginCapability::Authentication)
        {
            bail!("插件 Provider 未声明 authentication capability");
        }
        Ok(())
    }

    fn validate_auth_method(
        &self,
        plugin_id: &str,
        provider_id: &str,
        method: AuthMethod,
    ) -> Result<()> {
        self.validate_auth_provider(plugin_id, provider_id)?;
        let provider = self
            .runtime
            .catalog()
            .provider(plugin_id, provider_id)
            .ok_or_else(|| anyhow!("未安装插件 Provider: {plugin_id}/{provider_id}"))?;
        if !provider.auth_methods.contains(&method) {
            bail!("插件 Provider 未声明登录方式 {method:?}");
        }
        Ok(())
    }

    fn accept_authenticated_account(
        &self,
        plugin_id: &str,
        provider_id: &str,
        account: ProviderAccount,
    ) -> Result<()> {
        validate_provider_account(&account, provider_id)?;
        let mut sessions = self
            .sessions
            .write()
            .map_err(|error| anyhow!("插件会话状态锁已损坏: {error}"))?;

        let (priority, is_default) = {
            let host = self
                .host
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
            &self.host,
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

fn validate_auth_challenge(challenge: &AuthChallenge) -> Result<()> {
    validate_challenge_id(&challenge.challenge_id)?;
    if challenge.fields.len() > MAX_AUTH_CHALLENGE_FIELDS {
        bail!("插件 auth challenge fields 数量超过限制");
    }
    let mut payload_bytes = challenge
        .verification_uri
        .as_ref()
        .map_or(0usize, String::len)
        .saturating_add(challenge.user_code.as_ref().map_or(0, String::len))
        .saturating_add(challenge.qr_payload.as_ref().map_or(0, String::len));
    for field in &challenge.fields {
        payload_bytes = payload_bytes
            .saturating_add(field.key.len())
            .saturating_add(field.value.len());
    }
    if payload_bytes > MAX_AUTH_CHALLENGE_PAYLOAD_BYTES {
        bail!("插件 auth challenge payload 超过大小限制");
    }
    Ok(())
}

fn validate_provider_accounts(accounts: &[ProviderAccount], provider_id: &str) -> Result<()> {
    if accounts.len() > MAX_PLUGIN_ACCOUNTS_PER_PROVIDER {
        bail!(
            "插件 Provider 返回账号数超过 {} 限制",
            MAX_PLUGIN_ACCOUNTS_PER_PROVIDER
        );
    }
    for account in accounts {
        validate_provider_account(account, provider_id)?;
    }
    Ok(())
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

pub fn plugin_lyrics_to_player(
    document: PluginLyricDocument,
    source_prefix: &str,
) -> Result<Option<LyricsDocument>> {
    if document.source.len() > MAX_PLUGIN_LYRIC_SOURCE_BYTES {
        bail!("插件歌词 source 超过大小限制");
    }
    if document.lines.len() > MAX_PLUGIN_LYRIC_LINES {
        bail!("插件歌词行数超过 {} 限制", MAX_PLUGIN_LYRIC_LINES);
    }

    let plain = document.plain.filter(|value| !value.trim().is_empty());
    let mut input_bytes = plain.as_ref().map_or(0usize, String::len);
    for line in &document.lines {
        if line.words.len() > MAX_PLUGIN_LYRIC_WORDS_PER_LINE {
            bail!(
                "插件歌词单行 word 数超过 {} 限制",
                MAX_PLUGIN_LYRIC_WORDS_PER_LINE
            );
        }
        input_bytes = checked_lyric_bytes(input_bytes, line.text.len())?;
        if let Some(translation) = line.translation.as_ref() {
            input_bytes = checked_lyric_bytes(input_bytes, translation.len())?;
        }
        for word in &line.words {
            input_bytes = checked_lyric_bytes(input_bytes, word.text.len())?;
        }
    }

    let synced = serialize_plugin_ttml(&document.lines)?;
    if plain.is_none() && synced.is_none() {
        return Ok(None);
    }

    let guest_source = document.source.trim();
    let source = if guest_source.is_empty() {
        source_prefix.to_owned()
    } else if source_prefix.trim().is_empty() {
        guest_source.to_owned()
    } else {
        format!("{} · {}", source_prefix.trim(), guest_source)
    };
    Ok(Some(LyricsDocument::from_sources(
        plain,
        synced,
        None,
        source,
    )))
}

fn checked_lyric_bytes(current: usize, additional: usize) -> Result<usize> {
    let total = current
        .checked_add(additional)
        .ok_or_else(|| anyhow!("插件歌词文本大小溢出"))?;
    if total > MAX_PLUGIN_LYRIC_INPUT_BYTES {
        bail!("插件歌词文本超过 {} bytes 限制", MAX_PLUGIN_LYRIC_INPUT_BYTES);
    }
    Ok(total)
}

fn serialize_plugin_ttml(lines: &[PluginLyricLine]) -> Result<Option<String>> {
    let meaningful = lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .collect::<Vec<_>>();
    if meaningful.is_empty() {
        return Ok(None);
    }

    let mut output = String::with_capacity((meaningful.len() * 128).min(64 * 1024));
    output.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?><tt><body><div>");
    for line in meaningful {
        write!(&mut output, "<p begin=\"{}ms\">", line.timestamp_ms)
            .map_err(|_| anyhow!("构建插件 TTML 失败"))?;

        if authored_words_are_consistent(line) {
            for word in &line.words {
                write!(&mut output, "<span begin=\"{}ms\"", word.timestamp_ms)
                    .map_err(|_| anyhow!("构建插件 TTML 失败"))?;
                if let Some(duration_ms) = word.duration_ms.filter(|duration| *duration > 0) {
                    write!(&mut output, " dur=\"{duration_ms}ms\"")
                        .map_err(|_| anyhow!("构建插件 TTML 失败"))?;
                }
                output.push('>');
                push_xml_text(&mut output, &word.text)?;
                output.push_str("</span>");
                ensure_ttml_limit(&output)?;
            }
        } else {
            push_xml_text(&mut output, &line.text)?;
        }

        if let Some(translation) = line
            .translation
            .as_deref()
            .filter(|translation| !translation.trim().is_empty())
        {
            output.push_str("<span role=\"x-translation\">");
            push_xml_text(&mut output, translation)?;
            output.push_str("</span>");
        }
        output.push_str("</p>");
        ensure_ttml_limit(&output)?;
    }
    output.push_str("</div></body></tt>");
    ensure_ttml_limit(&output)?;
    Ok(Some(output))
}

fn authored_words_are_consistent(line: &PluginLyricLine) -> bool {
    if line.words.is_empty() {
        return false;
    }
    let mut previous_timestamp = line.timestamp_ms;
    let mut rebuilt_len = 0usize;
    for word in &line.words {
        if word.timestamp_ms < line.timestamp_ms || word.timestamp_ms < previous_timestamp {
            return false;
        }
        previous_timestamp = word.timestamp_ms;
        rebuilt_len = rebuilt_len.saturating_add(word.text.len());
    }
    if rebuilt_len != line.text.len() {
        return false;
    }

    let mut rebuilt = String::with_capacity(rebuilt_len);
    for word in &line.words {
        rebuilt.push_str(&word.text);
    }
    rebuilt == line.text
}

fn push_xml_text(output: &mut String, value: &str) -> Result<()> {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            character => output.push(character),
        }
        if output.len() > MAX_PLUGIN_LYRIC_TTML_BYTES {
            bail!("插件 canonical TTML 超过 {} bytes 限制", MAX_PLUGIN_LYRIC_TTML_BYTES);
        }
    }
    Ok(())
}

fn ensure_ttml_limit(output: &str) -> Result<()> {
    if output.len() > MAX_PLUGIN_LYRIC_TTML_BYTES {
        bail!("插件 canonical TTML 超过 {} bytes 限制", MAX_PLUGIN_LYRIC_TTML_BYTES);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::abi::{LyricWord as PluginLyricWord, PluginLyricDocument};

    #[test]
    fn plugin_lyrics_round_trip_word_timing_translation_and_xml_text() {
        let document = PluginLyricDocument {
            source: "provider-authored".into(),
            plain: Some("你&好".into()),
            lines: vec![PluginLyricLine {
                timestamp_ms: 1_000,
                text: "你&好".into(),
                translation: Some("<hello & hi>".into()),
                words: vec![
                    PluginLyricWord {
                        timestamp_ms: 1_000,
                        duration_ms: Some(300),
                        text: "你&".into(),
                    },
                    PluginLyricWord {
                        timestamp_ms: 1_300,
                        duration_ms: Some(400),
                        text: "好".into(),
                    },
                ],
            }],
        };

        let player = plugin_lyrics_to_player(document, "插件 QQ")
            .expect("convert")
            .expect("lyrics");
        assert_eq!(player.source, "插件 QQ · provider-authored");
        assert!(player.synced.as_deref().is_some_and(|value| value.contains("&amp;")));
        let line = &player.timed_lines()[0];
        assert_eq!(line.text, "你&好");
        assert_eq!(line.translation.as_deref(), Some("<hello & hi>"));
        assert_eq!(line.words.len(), 2);
        assert_eq!(line.words[0].duration_ms, Some(300));
        assert_eq!(line.words[0].text, "你&");
    }

    #[test]
    fn inconsistent_word_payload_falls_back_to_exact_line_text() {
        let document = PluginLyricDocument {
            source: "test".into(),
            plain: None,
            lines: vec![PluginLyricLine {
                timestamp_ms: 500,
                text: "authoritative line".into(),
                translation: None,
                words: vec![PluginLyricWord {
                    timestamp_ms: 500,
                    duration_ms: Some(100),
                    text: "wrong".into(),
                }],
            }],
        };
        let player = plugin_lyrics_to_player(document, "插件")
            .expect("convert")
            .expect("lyrics");
        assert_eq!(player.timed_lines()[0].text, "authoritative line");
        assert!(player.timed_lines()[0].words.is_empty());
    }

    #[test]
    fn empty_plugin_lyrics_is_none() {
        assert!(
            plugin_lyrics_to_player(PluginLyricDocument::default(), "插件")
                .expect("convert")
                .is_none()
        );
    }
}
