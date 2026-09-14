use std::{
    fmt::Write as _,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow, bail};

use crate::{
    lyrics::LyricsDocument,
    plugin_client::PluginClientRegistry,
    plugin_host::PluginHostState,
    plugin_route_gate::{GatedRoutePlan, plan_routes},
    plugin_runtime::{PluginCallKey, PluginHostServices},
    plugin_sessions::PluginSessionCoordinator,
    plugins::{
        LyricLine as PluginLyricLine, PluginLyricDocument, PluginRoute, RemoteTrack, RoutingPolicy,
        ServiceKind, SourceTrackRef, TrackQuery,
    },
};

const MAX_PLUGIN_LYRIC_LINES: usize = 10_000;
const MAX_PLUGIN_LYRIC_WORDS_PER_LINE: usize = 4_096;
const MAX_PLUGIN_LYRIC_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PLUGIN_LYRIC_TTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLUGIN_LYRIC_SOURCE_BYTES: usize = 1_024;

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

    /// Resolve a local/canonical identity through authenticated provider APIs.
    ///
    /// Single-route planning still retains all eligible accounts for execution-time retry. If the
    /// selected account becomes saturated between planning and permit acquisition, or the guest call
    /// fails, the next eligible account is attempted before the caller falls back to built-ins.
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

    /// Resolve lyrics for an already-resolved provider track.
    ///
    /// Metadata and lyrics remain independent capabilities, but when the metadata provider itself
    /// has a healthy lyrics route it is tried first. Other accounts of that same provider may retry
    /// the operation; callers can then perform a separate cross-provider lyric-quality fallback.
    pub async fn lyrics_for_route(
        &self,
        metadata_route: &PluginRoute,
        track: &SourceTrackRef,
    ) -> Result<PluginSingleResult<PluginLyricDocument>> {
        if track.provider_id != metadata_route.provider_id {
            bail!(
                "歌词 source provider 与 metadata route 不一致: source={}, route={}",
                track.provider_id,
                metadata_route.provider_id
            );
        }
        let policy = RoutingPolicy {
            preferred_provider: Some(metadata_route.provider_id.clone()),
            ..RoutingPolicy::default()
        };
        let mut plan = self.plan(ServiceKind::Lyrics, &policy)?;
        // A SourceTrackRef is provider-specific. Passing it to another provider would be an identity
        // violation, so this operation retries only accounts of the same plugin/provider. A future
        // cross-provider lyrics fallback must first resolve the TrackQuery on that provider.
        plan.eligible_routes.retain(|route| {
            route.plugin_id == metadata_route.plugin_id
                && route.provider_id == metadata_route.provider_id
        });
        plan.plan.plugin_routes = plan.eligible_routes.first().cloned().into_iter().collect();

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
}

/// Convert structured guest lyrics into a persistence-safe player document.
///
/// The guest never supplies markup. The Host emits canonical TTML with escaped text, then feeds it
/// through the same parser used for local/online TTML. Library persistence already stores `synced`,
/// so authored line/word timing and inline translations survive application restarts.
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
    use crate::plugins::{LyricWord as PluginLyricWord, PluginLyricDocument};

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
