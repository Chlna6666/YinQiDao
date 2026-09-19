use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use tokio::task::JoinSet;

use super::{
    abi::{
        PlaylistDescriptor, PluginRoute, RemoteTrack, RoutingPolicy, ServiceKind, SourceTrackRef,
    },
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend},
    host::runtime::{self, PluginCallKey},
    routing::gate::GatedRoutePlan,
};

const MAX_PLAYLIST_FANOUT_ROUTES: usize = 16;
const MAX_PLAYLISTS_PER_ROUTE: usize = 512;
const MAX_PLAYLIST_TRACK_LIMIT: u16 = 200;
const MAX_PLAYLIST_MUTATION_TRACKS: usize = 200;
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_PLAYLIST_NAME_BYTES: usize = 4 * 1024;
const MAX_COVER_URL_BYTES: usize = 16 * 1024;
const MAX_TRACK_ARTISTS: usize = 128;
const MAX_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct PluginPlaylistBatch {
    pub route: PluginRoute,
    pub playlists: Vec<PlaylistDescriptor>,
}

#[derive(Clone, Debug)]
pub struct PluginPlaylistFanout {
    pub batches: Vec<PluginPlaylistBatch>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    /// Join-level failures have no trustworthy route payload because a panicking task can unwind
    /// before it returns its Host-owned route identity.
    pub task_errors: Vec<String>,
    pub client_ready: bool,
}

impl PluginServiceFrontend {
    /// Fetch playlist summaries from all currently eligible authenticated providers concurrently.
    /// Each call still passes through the normal session/health/permit/deadline gates; guest output
    /// is bounded and validated before it reaches application/UI code.
    pub async fn playlists(&self, policy: &RoutingPolicy) -> Result<PluginPlaylistFanout> {
        let plan = self.plan(ServiceKind::Playlists, policy)?;
        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginPlaylistFanout {
                batches: Vec::new(),
                plan,
                failures: Vec::new(),
                task_errors: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        for route in plan.eligible_routes.iter().skip(MAX_PLAYLIST_FANOUT_ROUTES) {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: format!(
                    "Playlists fan-out 超过 Host route 上限 {}，本次未调用",
                    MAX_PLAYLIST_FANOUT_ROUTES
                ),
            });
        }

        let mut tasks = JoinSet::new();
        for (rank, route) in plan
            .eligible_routes
            .iter()
            .take(MAX_PLAYLIST_FANOUT_ROUTES)
            .cloned()
            .enumerate()
        {
            let client = client.clone();
            let runtime = runtime.clone();
            tasks.spawn(async move {
                let plugin_id = route.plugin_id.clone();
                let provider_id = route.provider_id.clone();
                let account_id = route.account_id.clone();
                let result = runtime
                    .execute_guest_call(
                        PluginCallKey::provider(&plugin_id, &provider_id),
                        client.playlists(&plugin_id, &provider_id, &account_id),
                    )
                    .await;
                (rank, route, result)
            });
        }

        let mut batches = Vec::new();
        let mut task_errors = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (rank, route, result) = match joined {
                Ok(value) => value,
                Err(error) => {
                    task_errors.push(format!("Playlists fan-out task 异常退出: {error}"));
                    continue;
                }
            };
            match result {
                Ok(playlists) => match validate_playlist_batch(&route, &playlists) {
                    Ok(()) => {
                        if !playlists.is_empty() {
                            batches.push((rank, PluginPlaylistBatch { route, playlists }));
                        }
                    }
                    Err(error) => failures.push(PluginCallFailure {
                        route,
                        error: format!("Playlists 返回值非法: {error:#}"),
                    }),
                },
                Err(error) => failures.push(PluginCallFailure {
                    route,
                    error: format!("{error:#}"),
                }),
            }
        }
        batches.sort_by_key(|(rank, _)| *rank);

        Ok(PluginPlaylistFanout {
            batches: batches.into_iter().map(|(_, batch)| batch).collect(),
            plan,
            failures,
            task_errors,
            client_ready: true,
        })
    }

    /// Fetch one provider playlist page. Duplicate tracks are preserved because playlist order and
    /// repeated occurrences are provider data, not a Host de-duplication surface.
    pub async fn playlist_tracks(
        &self,
        route: &PluginRoute,
        playlist_id: &str,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<RemoteTrack>> {
        validate_playlist_id(playlist_id)?;
        validate_playlist_limit(limit)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let tracks = runtime
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_tracks(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    playlist_id,
                    offset,
                    limit,
                ),
            )
            .await?;
        validate_playlist_tracks(route, &tracks, limit)?;
        Ok(tracks)
    }

    pub async fn playlist_create(
        &self,
        route: &PluginRoute,
        name: &str,
    ) -> Result<PlaylistDescriptor> {
        validate_playlist_name(name)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let playlist = runtime
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_create(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    name,
                ),
            )
            .await?;
        validate_playlist_descriptor(route, &playlist)?;
        Ok(playlist)
    }

    /// Cross-provider refs are rejected here; duplicate refs are intentionally preserved so a
    /// provider that supports repeated playlist entries can keep exact caller ordering semantics.
    pub async fn playlist_add(
        &self,
        route: &PluginRoute,
        playlist_id: &str,
        tracks: &[SourceTrackRef],
    ) -> Result<bool> {
        validate_playlist_id(playlist_id)?;
        validate_playlist_mutation_tracks(route, tracks)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_add(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    playlist_id,
                    tracks,
                ),
            )
            .await
    }

    pub async fn playlist_remove(
        &self,
        route: &PluginRoute,
        playlist_id: &str,
        tracks: &[SourceTrackRef],
    ) -> Result<bool> {
        validate_playlist_id(playlist_id)?;
        validate_playlist_mutation_tracks(route, tracks)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_remove(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    playlist_id,
                    tracks,
                ),
            )
            .await
    }

    pub async fn playlist_rename(
        &self,
        route: &PluginRoute,
        playlist_id: &str,
        name: &str,
    ) -> Result<bool> {
        validate_playlist_id(playlist_id)?;
        validate_playlist_name(name)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_rename(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    playlist_id,
                    name,
                ),
            )
            .await
    }

    pub async fn playlist_delete(&self, route: &PluginRoute, playlist_id: &str) -> Result<bool> {
        validate_playlist_id(playlist_id)?;
        ensure_playlist_route(self, route)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.playlist_delete(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    playlist_id,
                ),
            )
            .await
    }
}

fn require_client() -> Result<std::sync::Arc<dyn super::client::PluginProviderClient>> {
    client::global()
        .unwrap_or_else(client::initialize)
        .client()?
        .ok_or_else(|| anyhow!("插件 Provider client 尚未就绪"))
}

fn ensure_playlist_route(frontend: &PluginServiceFrontend, route: &PluginRoute) -> Result<()> {
    let plan = frontend.plan(ServiceKind::Playlists, &RoutingPolicy::default())?;
    if plan.eligible_routes.iter().any(|candidate| {
        candidate.plugin_id == route.plugin_id
            && candidate.provider_id == route.provider_id
            && candidate.account_id == route.account_id
    }) {
        return Ok(());
    }
    bail!(
        "Playlist route 当前不可用或未通过 Host session/health gate: {}/{}/{}",
        route.plugin_id,
        route.provider_id,
        route.account_id
    )
}

fn validate_playlist_batch(route: &PluginRoute, playlists: &[PlaylistDescriptor]) -> Result<()> {
    if playlists.len() > MAX_PLAYLISTS_PER_ROUTE {
        bail!(
            "单 route Playlists 返回 {} 项，超过 Host 上限 {}",
            playlists.len(),
            MAX_PLAYLISTS_PER_ROUTE
        );
    }
    let mut seen = HashSet::with_capacity(playlists.len());
    for playlist in playlists {
        validate_playlist_descriptor(route, playlist)?;
        if !seen.insert(playlist.source_id.as_str()) {
            bail!("Playlists 返回重复 source id: {}", playlist.source_id);
        }
    }
    Ok(())
}

fn validate_playlist_descriptor(route: &PluginRoute, playlist: &PlaylistDescriptor) -> Result<()> {
    validate_provider_id(&playlist.provider_id)?;
    if playlist.provider_id != route.provider_id {
        bail!(
            "playlist provider 不匹配: expected={}, actual={}",
            route.provider_id,
            playlist.provider_id
        );
    }
    validate_playlist_id(&playlist.source_id)?;
    validate_playlist_name(&playlist.name)?;
    if let Some(cover_url) = playlist.cover_url.as_deref()
        && (cover_url.len() > MAX_COVER_URL_BYTES || cover_url.contains('\0'))
    {
        bail!("playlist cover URL 非法或超过大小限制");
    }
    Ok(())
}

fn validate_playlist_tracks(route: &PluginRoute, tracks: &[RemoteTrack], limit: u16) -> Result<()> {
    if tracks.len() > usize::from(limit) {
        bail!(
            "playlist-tracks 返回 {} 项，超过请求 limit {}",
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
            "playlist track provider 不匹配: expected={}, actual={}",
            route.provider_id,
            track.source.provider_id
        );
    }
    if track.title.trim().is_empty() || track.title.contains('\0') {
        bail!("playlist track title 非法");
    }
    if track.artists.len() > MAX_TRACK_ARTISTS {
        bail!("playlist track artists 数量超过限制");
    }
    if track
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("playlist track duration_ms 超出允许范围");
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
            bail!("playlist track artist 包含 NUL");
        }
        bytes = bytes.saturating_add(artist.len());
    }
    if track.album.contains('\0')
        || track
            .isrc
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
        || track
            .cover_url
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
    {
        bail!("playlist track 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!("playlist track 文本超过 {} bytes", MAX_TRACK_TEXT_BYTES);
    }
    Ok(())
}

fn validate_playlist_mutation_tracks(route: &PluginRoute, tracks: &[SourceTrackRef]) -> Result<()> {
    if tracks.is_empty() || tracks.len() > MAX_PLAYLIST_MUTATION_TRACKS {
        bail!(
            "playlist mutation tracks 数量必须位于 1..={}，实际为 {}",
            MAX_PLAYLIST_MUTATION_TRACKS,
            tracks.len()
        );
    }
    for track in tracks {
        validate_source_ref(track)?;
        if track.provider_id != route.provider_id {
            bail!(
                "playlist mutation 不接受跨 provider source: expected={}, actual={}",
                route.provider_id,
                track.provider_id
            );
        }
    }
    Ok(())
}

fn validate_playlist_limit(limit: u16) -> Result<()> {
    if limit == 0 || limit > MAX_PLAYLIST_TRACK_LIMIT {
        bail!(
            "playlist-tracks limit 必须位于 1..={}，实际为 {}",
            MAX_PLAYLIST_TRACK_LIMIT,
            limit
        );
    }
    Ok(())
}

fn validate_provider_id(provider_id: &str) -> Result<()> {
    if provider_id.trim().is_empty()
        || provider_id.len() > MAX_PROVIDER_ID_BYTES
        || provider_id.contains('\0')
    {
        bail!("provider id 非法");
    }
    Ok(())
}

fn validate_playlist_id(playlist_id: &str) -> Result<()> {
    if playlist_id.trim().is_empty()
        || playlist_id.len() > MAX_SOURCE_ID_BYTES
        || playlist_id.contains('\0')
    {
        bail!("playlist source id 非法");
    }
    Ok(())
}

fn validate_playlist_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > MAX_PLAYLIST_NAME_BYTES || name.contains('\0') {
        bail!("playlist name 非法或超过大小限制");
    }
    Ok(())
}

fn validate_source_ref(source: &SourceTrackRef) -> Result<()> {
    validate_provider_id(&source.provider_id)?;
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
            plugin_id: "plugin.test".into(),
            provider_id: provider_id.into(),
            account_id: "account".into(),
            priority: 0,
            is_default: true,
        }
    }

    #[test]
    fn descriptor_rejects_cross_provider_identity() {
        let playlist = PlaylistDescriptor {
            provider_id: "netease".into(),
            source_id: "playlist-1".into(),
            name: "测试歌单".into(),
            ..PlaylistDescriptor::default()
        };
        assert!(validate_playlist_descriptor(&route("qqmusic"), &playlist).is_err());
    }

    #[test]
    fn mutation_rejects_cross_provider_track() {
        let tracks = [SourceTrackRef {
            provider_id: "netease".into(),
            source_id: "song-1".into(),
        }];
        assert!(validate_playlist_mutation_tracks(&route("qqmusic"), &tracks).is_err());
    }

    #[test]
    fn duplicate_tracks_are_valid_playlist_data() {
        let track = RemoteTrack {
            source: SourceTrackRef {
                provider_id: "qqmusic".into(),
                source_id: "song-1".into(),
            },
            title: "Song".into(),
            ..RemoteTrack::default()
        };
        assert!(validate_playlist_tracks(&route("qqmusic"), &[track.clone(), track], 2).is_ok());
    }

    #[test]
    fn playlist_track_limit_is_bounded() {
        assert!(validate_playlist_limit(0).is_err());
        assert!(validate_playlist_limit(MAX_PLAYLIST_TRACK_LIMIT).is_ok());
        assert!(validate_playlist_limit(MAX_PLAYLIST_TRACK_LIMIT + 1).is_err());
    }
}
