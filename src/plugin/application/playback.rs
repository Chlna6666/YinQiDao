use anyhow::{Result, anyhow, bail};

use super::{
    abi::{
        PlaybackSignal, PlaybackSignalKind, PluginRoute, RoutingPolicy, ServiceKind, SourceTrackRef,
        TrackQuery,
    },
    client,
    frontend::PluginServiceFrontend,
    host::{
        permissions,
        runtime::{self, PluginCallKey},
    },
    runtime_ports,
    streaming::PluginStartedPlayback,
};

const MAX_TRACK_ARTISTS: usize = 128;
const MAX_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_PROVIDER_BYTES: usize = 128;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

impl PluginServiceFrontend {
    /// Report one playback signal using the exact plugin/provider/account provenance that actually
    /// started a remote track. The Host supplies the source reference and timestamp; callers provide
    /// only semantic track metadata plus current transport progress.
    ///
    /// This path is never invoked from the realtime audio callback. UI/application control code must
    /// schedule it on the ordinary async runtime after observing a transport transition.
    pub async fn report_started_playback(
        &self,
        started: &PluginStartedPlayback,
        kind: PlaybackSignalKind,
        track: &TrackQuery,
        position_ms: u64,
        duration_ms: u64,
    ) -> Result<bool> {
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let signal = PlaybackSignal {
            kind,
            track: track.clone(),
            source: Some(started.source().clone()),
            position_ms,
            duration_ms,
            occurred_at_ms: runtime.now_ms(),
        };
        self.report_playback_for_route(started.route(), &signal).await
    }

    /// Report a Host-built playback signal to one exact authenticated account.
    ///
    /// Playback events are privacy-sensitive telemetry. They are never fan-out operations: the
    /// selected route must still pass the current session/capability/health gate and the plugin must
    /// retain an explicit user grant for playback events at the instant the call is dispatched.
    pub async fn report_playback_for_route(
        &self,
        route: &PluginRoute,
        signal: &PlaybackSignal,
    ) -> Result<bool> {
        validate_playback_signal(route, signal)?;
        let package_generation = runtime_ports::package_mutation_generation();
        ensure_exact_route(self, route)?;
        ensure_playback_event_permission(&route.plugin_id)?;

        let clients = client::global().unwrap_or_else(client::initialize);
        let client = clients
            .client()?
            .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))?;
        if runtime_ports::package_mutation_generation() != package_generation {
            bail!("插件在播放事件上报准备期间已更新，旧事件未发送");
        }

        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let accepted = runtime
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.report_playback(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    signal,
                ),
            )
            .await?;

        // An in-flight old adapter may be allowed to finish while package management advances the
        // generation. Never reinterpret that acknowledgement as belonging to the replacement package,
        // and never retry automatically because the old guest may already have recorded the event.
        if runtime_ports::package_mutation_generation() != package_generation {
            bail!("插件在播放事件上报期间已更新，旧 runtime 返回值已丢弃");
        }
        Ok(accepted)
    }
}

fn ensure_exact_route(frontend: &PluginServiceFrontend, route: &PluginRoute) -> Result<()> {
    let plan = frontend.plan(ServiceKind::PlaybackEvents, &RoutingPolicy::default())?;
    if plan.eligible_routes.iter().any(|candidate| {
        candidate.plugin_id == route.plugin_id
            && candidate.provider_id == route.provider_id
            && candidate.account_id == route.account_id
    }) {
        return Ok(());
    }
    bail!(
        "PlaybackEvents route 当前不可用或未通过 Host session/health gate: {}/{}/{}",
        route.plugin_id,
        route.provider_id,
        route.account_id
    )
}

fn ensure_playback_event_permission(plugin_id: &str) -> Result<()> {
    let permissions = permissions::global().ok_or_else(|| anyhow!("插件权限状态尚未初始化"))?;
    let permissions = permissions
        .read()
        .map_err(|error| anyhow!("插件权限状态锁已损坏: {error}"))?;
    let grant = permissions
        .grant_for(plugin_id)
        .ok_or_else(|| anyhow!("插件尚未获得播放事件权限: {plugin_id}"))?;
    if !grant.playback_events {
        bail!("插件播放事件权限未授权: {plugin_id}");
    }
    Ok(())
}

fn validate_playback_signal(route: &PluginRoute, signal: &PlaybackSignal) -> Result<()> {
    validate_track_query(&signal.track)?;
    if let Some(source) = signal.source.as_ref() {
        validate_source_ref(source)?;
        if source.provider_id != route.provider_id {
            bail!(
                "PlaybackEvents source provider 不匹配: expected={}, actual={}",
                route.provider_id,
                source.provider_id
            );
        }
    }
    if signal.position_ms > MAX_TRACK_DURATION_MS {
        bail!("PlaybackEvents position_ms 超出 Host 上限");
    }
    if signal.duration_ms > MAX_TRACK_DURATION_MS {
        bail!("PlaybackEvents duration_ms 超出 Host 上限");
    }
    Ok(())
}

fn validate_track_query(query: &TrackQuery) -> Result<()> {
    if query.artists.len() > MAX_TRACK_ARTISTS {
        bail!("PlaybackEvents track query artists 数量超过限制");
    }
    if query
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("PlaybackEvents track query duration_ms 超出允许范围");
    }

    let mut bytes = query
        .title
        .len()
        .saturating_add(query.album.len())
        .saturating_add(query.isrc.as_ref().map_or(0, String::len))
        .saturating_add(
            query
                .musicbrainz_recording_id
                .as_ref()
                .map_or(0, String::len),
        )
        .saturating_add(query.fingerprint_id.as_ref().map_or(0, String::len));
    for artist in &query.artists {
        if artist.contains('\0') {
            bail!("PlaybackEvents track query artist 包含 NUL");
        }
        bytes = bytes.saturating_add(artist.len());
    }
    if query.title.contains('\0')
        || query.album.contains('\0')
        || query.isrc.as_ref().is_some_and(|value| value.contains('\0'))
        || query
            .musicbrainz_recording_id
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
        || query
            .fingerprint_id
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
    {
        bail!("PlaybackEvents track query 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!("PlaybackEvents track query 文本超过 {} bytes", MAX_TRACK_TEXT_BYTES);
    }
    Ok(())
}

fn validate_source_ref(source: &SourceTrackRef) -> Result<()> {
    if source.provider_id.trim().is_empty()
        || source.provider_id.len() > MAX_SOURCE_PROVIDER_BYTES
        || source.provider_id.contains('\0')
    {
        bail!("PlaybackEvents source provider id 非法");
    }
    if source.source_id.trim().is_empty()
        || source.source_id.len() > MAX_SOURCE_ID_BYTES
        || source.source_id.contains('\0')
    {
        bail!("PlaybackEvents source id 非法");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> PluginRoute {
        PluginRoute {
            plugin_id: "plugin.test".into(),
            provider_id: "provider-a".into(),
            account_id: "account-a".into(),
            priority: 0,
            is_default: true,
        }
    }

    fn query() -> TrackQuery {
        TrackQuery {
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            duration_ms: Some(180_000),
            ..TrackQuery::default()
        }
    }

    #[test]
    fn playback_signal_rejects_cross_provider_source() {
        let signal = PlaybackSignal {
            kind: PlaybackSignalKind::Started,
            track: query(),
            source: Some(SourceTrackRef {
                provider_id: "provider-b".into(),
                source_id: "track-1".into(),
            }),
            position_ms: 0,
            duration_ms: 180_000,
            occurred_at_ms: 1,
        };
        assert!(validate_playback_signal(&route(), &signal).is_err());
    }

    #[test]
    fn playback_signal_accepts_exact_provider_and_bounded_progress() {
        let signal = PlaybackSignal {
            kind: PlaybackSignalKind::Completed,
            track: query(),
            source: Some(SourceTrackRef {
                provider_id: "provider-a".into(),
                source_id: "track-1".into(),
            }),
            position_ms: 180_000,
            duration_ms: 180_000,
            occurred_at_ms: 1,
        };
        assert!(validate_playback_signal(&route(), &signal).is_ok());
    }

    #[test]
    fn playback_signal_rejects_unbounded_or_nul_metadata() {
        let mut signal = PlaybackSignal {
            kind: PlaybackSignalKind::Started,
            track: query(),
            source: None,
            position_ms: MAX_TRACK_DURATION_MS + 1,
            duration_ms: 0,
            occurred_at_ms: 1,
        };
        assert!(validate_playback_signal(&route(), &signal).is_err());

        signal.position_ms = 0;
        signal.track.title = "bad\0title".into();
        assert!(validate_playback_signal(&route(), &signal).is_err());
    }
}
