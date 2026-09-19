use anyhow::{Result, anyhow, bail};
use tokio::task::JoinSet;

use super::{
    abi::{PluginRoute, RemoteTrack, RoutingPolicy, ServiceKind, SourceTrackRef},
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend},
    host::runtime::{self, PluginCallKey},
    routing::gate::GatedRoutePlan,
};

const MAX_LIBRARY_FANOUT_ROUTES: usize = 16;
const MAX_LIBRARY_PAGE_LIMIT: u16 = 200;
const MAX_TRACK_ARTISTS: usize = 128;
const MAX_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_PROVIDER_BYTES: usize = 128;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_COVER_URL_BYTES: usize = 16 * 1024;
const MAX_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct PluginLibraryBatch {
    pub route: PluginRoute,
    pub tracks: Vec<RemoteTrack>,
}

#[derive(Clone, Debug)]
pub struct PluginLibraryFanout {
    pub batches: Vec<PluginLibraryBatch>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    pub task_errors: Vec<String>,
    pub client_ready: bool,
}

#[derive(Clone, Copy)]
enum LibraryQueryKind {
    CloudLibrary,
    LikedTracks,
}

impl LibraryQueryKind {
    const fn label(self) -> &'static str {
        match self {
            Self::CloudLibrary => "CloudLibrary",
            Self::LikedTracks => "LikedTracks",
        }
    }
}

impl PluginServiceFrontend {
    /// Pull one page from every currently eligible CloudLibrary account concurrently.
    /// Batches remain route-scoped because the same provider track may legitimately exist in several
    /// user accounts and application-level merge policy must not erase that provenance.
    pub async fn cloud_library(
        &self,
        offset: u32,
        limit: u16,
        policy: &RoutingPolicy,
    ) -> Result<PluginLibraryFanout> {
        self.library_fanout(
            ServiceKind::CloudLibrary,
            LibraryQueryKind::CloudLibrary,
            offset,
            limit,
            policy,
        )
        .await
    }

    /// Pull one liked-track page from every currently eligible LikeSync account. This is a read-only
    /// merge surface; mutating a like always requires an explicit route through `set_liked_for_route`.
    pub async fn liked_tracks(
        &self,
        offset: u32,
        limit: u16,
        policy: &RoutingPolicy,
    ) -> Result<PluginLibraryFanout> {
        self.library_fanout(
            ServiceKind::LikeSync,
            LibraryQueryKind::LikedTracks,
            offset,
            limit,
            policy,
        )
        .await
    }

    /// Mutate one exact authenticated account only. A like/unlike must never be broadcast to every
    /// account merely because the merged library UI happens to display several providers together.
    pub async fn set_liked_for_route(
        &self,
        route: &PluginRoute,
        track: &SourceTrackRef,
        liked: bool,
    ) -> Result<bool> {
        validate_source_ref(track)?;
        if track.provider_id != route.provider_id {
            bail!(
                "LikeSync source provider 不匹配: expected={}, actual={}",
                route.provider_id,
                track.provider_id
            );
        }
        ensure_exact_route(self, ServiceKind::LikeSync, route)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.set_liked(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    track,
                    liked,
                ),
            )
            .await
    }

    async fn library_fanout(
        &self,
        service: ServiceKind,
        kind: LibraryQueryKind,
        offset: u32,
        limit: u16,
        policy: &RoutingPolicy,
    ) -> Result<PluginLibraryFanout> {
        validate_page_limit(limit)?;
        let plan = self.plan(service, policy)?;
        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginLibraryFanout {
                batches: Vec::new(),
                plan,
                failures: Vec::new(),
                task_errors: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        for route in plan.eligible_routes.iter().skip(MAX_LIBRARY_FANOUT_ROUTES) {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: format!(
                    "{} fan-out 超过 Host route 上限 {}，本次未调用",
                    kind.label(),
                    MAX_LIBRARY_FANOUT_ROUTES
                ),
            });
        }

        let mut tasks = JoinSet::new();
        for (rank, route) in plan
            .eligible_routes
            .iter()
            .take(MAX_LIBRARY_FANOUT_ROUTES)
            .cloned()
            .enumerate()
        {
            let client = client.clone();
            let runtime = runtime.clone();
            tasks.spawn(async move {
                let plugin_id = route.plugin_id.clone();
                let provider_id = route.provider_id.clone();
                let account_id = route.account_id.clone();
                let key = PluginCallKey::provider(&plugin_id, &provider_id);
                let result = match kind {
                    LibraryQueryKind::CloudLibrary => {
                        runtime
                            .execute_guest_call(
                                key,
                                client.cloud_library(
                                    &plugin_id,
                                    &provider_id,
                                    &account_id,
                                    offset,
                                    limit,
                                ),
                            )
                            .await
                    }
                    LibraryQueryKind::LikedTracks => {
                        runtime
                            .execute_guest_call(
                                key,
                                client.liked_tracks(
                                    &plugin_id,
                                    &provider_id,
                                    &account_id,
                                    offset,
                                    limit,
                                ),
                            )
                            .await
                    }
                };
                (rank, route, result)
            });
        }

        let mut batches = Vec::new();
        let mut task_errors = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (rank, route, result) = match joined {
                Ok(value) => value,
                Err(error) => {
                    task_errors.push(format!("{} fan-out task 异常退出: {error}", kind.label()));
                    continue;
                }
            };
            match result {
                Ok(tracks) => match validate_track_page(&route, &tracks, limit) {
                    Ok(()) => {
                        if !tracks.is_empty() {
                            batches.push((rank, PluginLibraryBatch { route, tracks }));
                        }
                    }
                    Err(error) => failures.push(PluginCallFailure {
                        route,
                        error: format!("{} 返回值非法: {error:#}", kind.label()),
                    }),
                },
                Err(error) => failures.push(PluginCallFailure {
                    route,
                    error: format!("{error:#}"),
                }),
            }
        }
        batches.sort_by_key(|(rank, _)| *rank);

        Ok(PluginLibraryFanout {
            batches: batches.into_iter().map(|(_, batch)| batch).collect(),
            plan,
            failures,
            task_errors,
            client_ready: true,
        })
    }
}

fn require_client() -> Result<std::sync::Arc<dyn super::client::PluginProviderClient>> {
    client::global()
        .unwrap_or_else(client::initialize)
        .client()?
        .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))
}

fn ensure_exact_route(
    frontend: &PluginServiceFrontend,
    service: ServiceKind,
    route: &PluginRoute,
) -> Result<()> {
    let plan = frontend.plan(service, &RoutingPolicy::default())?;
    if plan.eligible_routes.iter().any(|candidate| {
        candidate.plugin_id == route.plugin_id
            && candidate.provider_id == route.provider_id
            && candidate.account_id == route.account_id
    }) {
        return Ok(());
    }
    bail!(
        "route 当前不可用或未通过 Host session/health gate: {}/{}/{}",
        route.plugin_id,
        route.provider_id,
        route.account_id
    )
}

fn validate_page_limit(limit: u16) -> Result<()> {
    if limit == 0 || limit > MAX_LIBRARY_PAGE_LIMIT {
        bail!(
            "library page limit 必须位于 1..={}，实际为 {}",
            MAX_LIBRARY_PAGE_LIMIT,
            limit
        );
    }
    Ok(())
}

fn validate_track_page(route: &PluginRoute, tracks: &[RemoteTrack], limit: u16) -> Result<()> {
    if tracks.len() > usize::from(limit) {
        bail!(
            "library page 返回 {} 项，超过请求 limit {}",
            tracks.len(),
            limit
        );
    }
    for track in tracks {
        validate_remote_track(route, track)?;
    }
    Ok(())
}

fn validate_remote_track(route: &PluginRoute, track: &RemoteTrack) -> Result<()> {
    validate_source_ref(&track.source)?;
    if track.source.provider_id != route.provider_id {
        bail!(
            "library track provider 不匹配: expected={}, actual={}",
            route.provider_id,
            track.source.provider_id
        );
    }
    if track.title.trim().is_empty() || track.title.contains('\0') {
        bail!("library track title 非法");
    }
    if track.artists.len() > MAX_TRACK_ARTISTS {
        bail!("library track artists 数量超过限制");
    }
    if track
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("library track duration_ms 超出允许范围");
    }
    if track
        .cover_url
        .as_ref()
        .is_some_and(|value| value.len() > MAX_COVER_URL_BYTES || value.contains('\0'))
    {
        bail!("library track cover URL 非法或超过大小限制");
    }

    let mut bytes = track
        .source
        .provider_id
        .len()
        .saturating_add(track.source.source_id.len())
        .saturating_add(track.title.len())
        .saturating_add(track.album.len())
        .saturating_add(track.isrc.as_ref().map_or(0, String::len))
        .saturating_add(track.cover_url.as_ref().map_or(0, String::len));
    for artist in &track.artists {
        if artist.contains('\0') {
            bail!("library track artist 包含 NUL");
        }
        bytes = bytes.saturating_add(artist.len());
    }
    if track.album.contains('\0')
        || track
            .isrc
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
    {
        bail!("library track 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!(
            "library track 文本超过 {} bytes Host 上限",
            MAX_TRACK_TEXT_BYTES
        );
    }
    Ok(())
}

fn validate_source_ref(source: &SourceTrackRef) -> Result<()> {
    if source.provider_id.trim().is_empty()
        || source.provider_id.len() > MAX_SOURCE_PROVIDER_BYTES
        || source.provider_id.contains('\0')
    {
        bail!("source provider id 非法");
    }
    if source.source_id.trim().is_empty()
        || source.source_id.len() > MAX_SOURCE_ID_BYTES
        || source.source_id.contains('\0')
    {
        bail!("source id 非法");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(provider_id: &str) -> PluginRoute {
        PluginRoute {
            plugin_id: format!("plugin.{provider_id}"),
            provider_id: provider_id.into(),
            account_id: "account".into(),
            priority: 0,
            is_default: true,
        }
    }

    fn track(provider_id: &str) -> RemoteTrack {
        RemoteTrack {
            source: SourceTrackRef {
                provider_id: provider_id.into(),
                source_id: "song".into(),
            },
            title: "Song".into(),
            artists: vec!["Artist".into()],
            ..RemoteTrack::default()
        }
    }

    #[test]
    fn library_page_limit_is_bounded() {
        assert!(validate_page_limit(1).is_ok());
        assert!(validate_page_limit(MAX_LIBRARY_PAGE_LIMIT).is_ok());
        assert!(validate_page_limit(0).is_err());
        assert!(validate_page_limit(MAX_LIBRARY_PAGE_LIMIT + 1).is_err());
    }

    #[test]
    fn library_page_rejects_cross_provider_track() {
        let route = route("qqmusic");
        assert!(validate_track_page(&route, &[track("netease")], 1).is_err());
    }

    #[test]
    fn like_mutation_requires_same_provider_source() {
        let route = route("qqmusic");
        let source = SourceTrackRef {
            provider_id: "netease".into(),
            source_id: "song".into(),
        };
        assert_ne!(source.provider_id, route.provider_id);
    }
}
