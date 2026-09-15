use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use tokio::task::JoinSet;

use super::{
    abi::{PluginRoute, RemoteTrack, RoutingPolicy, ServiceKind},
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend},
    host::runtime::{self, PluginCallKey},
    routing::gate::GatedRoutePlan,
};

const MAX_SEARCH_FANOUT_ROUTES: usize = 16;
const MAX_SEARCH_QUERY_BYTES: usize = 4 * 1024;
const MAX_SEARCH_LIMIT: u16 = 100;
const MAX_TRACK_ARTISTS: usize = 128;
const MAX_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_PROVIDER_BYTES: usize = 128;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_COVER_URL_BYTES: usize = 16 * 1024;
const MAX_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct PluginSearchBatch {
    pub route: PluginRoute,
    pub tracks: Vec<RemoteTrack>,
}

#[derive(Clone, Debug)]
pub struct PluginSearchFanout {
    pub batches: Vec<PluginSearchBatch>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    /// Join-level failures cannot safely retain a route payload because a task may unwind before
    /// returning its Host-owned identity.
    pub task_errors: Vec<String>,
    pub client_ready: bool,
}

impl PluginServiceFrontend {
    /// Search every currently eligible authenticated Search route concurrently.
    ///
    /// Route eligibility is decided entirely by the Host session/health gate before guest code runs.
    /// Guest output is validated, bounded and de-duplicated by provider/source identity before it is
    /// exposed to application/UI code. Route rank is preserved so a request-scoped account/provider
    /// preference wins duplicate resolution without becoming global state.
    pub async fn search(
        &self,
        query: &str,
        limit: u16,
        policy: &RoutingPolicy,
    ) -> Result<PluginSearchFanout> {
        validate_search_request(query, limit)?;
        let plan = self.plan(ServiceKind::Search, policy)?;
        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginSearchFanout {
                batches: Vec::new(),
                plan,
                failures: Vec::new(),
                task_errors: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        for route in plan.eligible_routes.iter().skip(MAX_SEARCH_FANOUT_ROUTES) {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: format!(
                    "Search fan-out 超过 Host route 上限 {}，本次未调用",
                    MAX_SEARCH_FANOUT_ROUTES
                ),
            });
        }

        let mut tasks = JoinSet::new();
        for (rank, route) in plan
            .eligible_routes
            .iter()
            .take(MAX_SEARCH_FANOUT_ROUTES)
            .cloned()
            .enumerate()
        {
            let client = client.clone();
            let runtime = runtime.clone();
            let query = query.to_owned();
            tasks.spawn(async move {
                let plugin_id = route.plugin_id.clone();
                let provider_id = route.provider_id.clone();
                let account_id = route.account_id.clone();
                let result = runtime
                    .execute_guest_call(
                        PluginCallKey::provider(&plugin_id, &provider_id),
                        client.search(
                            &plugin_id,
                            &provider_id,
                            Some(&account_id),
                            &query,
                            limit,
                        ),
                    )
                    .await;
                (rank, route, result)
            });
        }

        let mut ranked_batches = Vec::new();
        let mut task_errors = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (rank, route, result) = match joined {
                Ok(value) => value,
                Err(error) => {
                    task_errors.push(format!("Search fan-out task 异常退出: {error}"));
                    continue;
                }
            };

            match result {
                Ok(tracks) => match validate_search_tracks(&route, &tracks, limit) {
                    Ok(()) => {
                        if !tracks.is_empty() {
                            ranked_batches.push((rank, PluginSearchBatch { route, tracks }));
                        }
                    }
                    Err(error) => failures.push(PluginCallFailure {
                        route,
                        error: format!("Search 返回值非法: {error:#}"),
                    }),
                },
                Err(error) => failures.push(PluginCallFailure {
                    route,
                    error: format!("{error:#}"),
                }),
            }
        }

        ranked_batches.sort_by_key(|(rank, _)| *rank);
        deduplicate_ranked_batches(&mut ranked_batches);

        Ok(PluginSearchFanout {
            batches: ranked_batches
                .into_iter()
                .map(|(_, batch)| batch)
                .collect(),
            plan,
            failures,
            task_errors,
            client_ready: true,
        })
    }
}

fn validate_search_request(query: &str, limit: u16) -> Result<()> {
    if query.trim().is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES || query.contains('\0') {
        bail!("Search query 为空、包含 NUL 或超过 {} bytes Host 上限", MAX_SEARCH_QUERY_BYTES);
    }
    if limit == 0 || limit > MAX_SEARCH_LIMIT {
        bail!("Search limit 必须位于 1..={}，实际为 {}", MAX_SEARCH_LIMIT, limit);
    }
    Ok(())
}

fn validate_search_tracks(route: &PluginRoute, tracks: &[RemoteTrack], limit: u16) -> Result<()> {
    if tracks.len() > usize::from(limit) {
        bail!(
            "Search 返回 {} 项，超过请求 limit {}",
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
    let source = &track.source;
    if source.provider_id.trim().is_empty()
        || source.provider_id.len() > MAX_SOURCE_PROVIDER_BYTES
        || source.provider_id.contains('\0')
    {
        bail!("Search track source provider id 非法");
    }
    if source.source_id.trim().is_empty()
        || source.source_id.len() > MAX_SOURCE_ID_BYTES
        || source.source_id.contains('\0')
    {
        bail!("Search track source id 非法");
    }
    if source.provider_id != route.provider_id {
        bail!(
            "Search track provider 不匹配: expected={}, actual={}",
            route.provider_id,
            source.provider_id
        );
    }
    if track.title.trim().is_empty() || track.title.contains('\0') {
        bail!("Search track title 非法");
    }
    if track.artists.len() > MAX_TRACK_ARTISTS {
        bail!("Search track artists 数量超过限制");
    }
    if track
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("Search track duration_ms 超出允许范围");
    }
    if track
        .cover_url
        .as_ref()
        .is_some_and(|value| value.len() > MAX_COVER_URL_BYTES || value.contains('\0'))
    {
        bail!("Search track cover URL 非法或超过大小限制");
    }

    let mut bytes = source
        .provider_id
        .len()
        .saturating_add(source.source_id.len())
        .saturating_add(track.title.len())
        .saturating_add(track.album.len())
        .saturating_add(track.isrc.as_ref().map_or(0, String::len))
        .saturating_add(track.cover_url.as_ref().map_or(0, String::len));
    for artist in &track.artists {
        if artist.contains('\0') {
            bail!("Search track artist 包含 NUL");
        }
        bytes = bytes.saturating_add(artist.len());
    }
    if track.album.contains('\0')
        || track.isrc.as_ref().is_some_and(|value| value.contains('\0'))
    {
        bail!("Search track 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!("Search track 文本超过 {} bytes Host 上限", MAX_TRACK_TEXT_BYTES);
    }
    Ok(())
}

fn deduplicate_ranked_batches(batches: &mut Vec<(usize, PluginSearchBatch)>) {
    let mut seen = HashSet::<(String, String)>::new();
    for (_, batch) in batches.iter_mut() {
        batch.tracks.retain(|track| {
            seen.insert((
                track.source.provider_id.clone(),
                track.source.source_id.clone(),
            ))
        });
    }
    batches.retain(|(_, batch)| !batch.tracks.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::abi::SourceTrackRef;

    fn route(provider_id: &str, account_id: &str) -> PluginRoute {
        PluginRoute {
            plugin_id: format!("plugin.{provider_id}"),
            provider_id: provider_id.into(),
            account_id: account_id.into(),
            priority: 0,
            is_default: true,
        }
    }

    fn track(provider_id: &str, source_id: &str) -> RemoteTrack {
        RemoteTrack {
            source: SourceTrackRef {
                provider_id: provider_id.into(),
                source_id: source_id.into(),
            },
            title: format!("Song {source_id}"),
            artists: vec!["Artist".into()],
            ..RemoteTrack::default()
        }
    }

    #[test]
    fn search_request_is_bounded() {
        assert!(validate_search_request("song", 1).is_ok());
        assert!(validate_search_request(" ", 1).is_err());
        assert!(validate_search_request("song", 0).is_err());
        assert!(validate_search_request("song", MAX_SEARCH_LIMIT + 1).is_err());
        assert!(validate_search_request(&"x".repeat(MAX_SEARCH_QUERY_BYTES + 1), 1).is_err());
    }

    #[test]
    fn search_rejects_cross_provider_result() {
        let route = route("qqmusic", "account");
        assert!(validate_search_tracks(&route, &[track("netease", "song")], 1).is_err());
    }

    #[test]
    fn search_deduplicates_lower_ranked_account_results() {
        let mut batches = vec![
            (
                0,
                PluginSearchBatch {
                    route: route("qqmusic", "preferred"),
                    tracks: vec![track("qqmusic", "same"), track("qqmusic", "first")],
                },
            ),
            (
                1,
                PluginSearchBatch {
                    route: route("qqmusic", "backup"),
                    tracks: vec![track("qqmusic", "same"), track("qqmusic", "second")],
                },
            ),
        ];
        deduplicate_ranked_batches(&mut batches);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].1.tracks.len(), 2);
        assert_eq!(batches[1].1.tracks.len(), 1);
        assert_eq!(batches[1].1.tracks[0].source.source_id, "second");
    }
}
