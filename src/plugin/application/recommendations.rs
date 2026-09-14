use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use tokio::task::JoinSet;

use super::{
    abi::{
        PluginRoute, RecommendationItem, RecommendationRequest, RemoteTrack, RoutingPolicy,
        ServiceKind, SourceTrackRef, TrackQuery,
    },
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend},
    host::runtime::{self, PluginCallKey},
    routing::gate::GatedRoutePlan,
};

const MAX_RECOMMENDATION_FANOUT_ROUTES: usize = 16;
const MAX_RECOMMENDATION_LIMIT: u16 = 100;
const MAX_RECOMMENDATION_EXCLUDES: usize = 512;
const MAX_RECOMMENDATION_ITEMS_PER_ROUTE: usize = 100;
const MAX_TRACK_ARTISTS: usize = 128;
const MAX_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_SOURCE_PROVIDER_BYTES: usize = 128;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_REASON_BYTES: usize = 8 * 1024;
const MAX_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct PluginRecommendationBatch {
    pub route: PluginRoute,
    pub items: Vec<RecommendationItem>,
}

#[derive(Clone, Debug)]
pub struct PluginRecommendationFanout {
    pub batches: Vec<PluginRecommendationBatch>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    /// Join-level failures have no trustworthy route payload because a panicking task may unwind
    /// before returning its Host-owned route identity.
    pub task_errors: Vec<String>,
    pub client_ready: bool,
}

impl PluginServiceFrontend {
    /// Concurrently request personalized recommendations from the highest-priority authenticated
    /// routes. Each route still passes through the normal health/permit/deadline boundary. The Host
    /// validates and de-duplicates every guest batch before application recommendation fusion sees it.
    pub async fn recommendations(
        &self,
        request: &RecommendationRequest,
        policy: &RoutingPolicy,
    ) -> Result<PluginRecommendationFanout> {
        validate_recommendation_request(request)?;
        let plan = self.plan(ServiceKind::Recommendations, policy)?;
        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginRecommendationFanout {
                batches: Vec::new(),
                plan,
                failures: Vec::new(),
                task_errors: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        let mut tasks = JoinSet::new();
        for route in plan
            .eligible_routes
            .iter()
            .skip(MAX_RECOMMENDATION_FANOUT_ROUTES)
        {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: format!(
                    "Recommendations fan-out 超过 Host route 上限 {}，本次未调用",
                    MAX_RECOMMENDATION_FANOUT_ROUTES
                ),
            });
        }

        for (rank, route) in plan
            .eligible_routes
            .iter()
            .take(MAX_RECOMMENDATION_FANOUT_ROUTES)
            .cloned()
            .enumerate()
        {
            let client = client.clone();
            let runtime = runtime.clone();
            let request = request.clone();
            tasks.spawn(async move {
                let plugin_id = route.plugin_id.clone();
                let provider_id = route.provider_id.clone();
                let account_id = route.account_id.clone();
                let key = PluginCallKey::provider(&plugin_id, &provider_id);
                let result = runtime
                    .execute_guest_call(
                        key,
                        client.recommendations(
                            &plugin_id,
                            &provider_id,
                            &account_id,
                            &request,
                        ),
                    )
                    .await;
                (rank, route, result)
            });
        }

        let excluded = request
            .exclude
            .iter()
            .map(|source| (source.provider_id.clone(), source.source_id.clone()))
            .collect::<HashSet<_>>();
        let mut batches = Vec::new();
        let mut task_errors = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (rank, route, result) = match joined {
                Ok(value) => value,
                Err(error) => {
                    task_errors.push(format!("Recommendations fan-out task 异常退出: {error}"));
                    continue;
                }
            };

            match result {
                Ok(mut items) => {
                    if let Err(error) = validate_recommendation_items(&route, &items) {
                        failures.push(PluginCallFailure {
                            route,
                            error: format!("Recommendations 返回值非法: {error:#}"),
                        });
                        continue;
                    }

                    let mut seen = HashSet::new();
                    items.retain(|item| {
                        let identity = (
                            item.track.source.provider_id.clone(),
                            item.track.source.source_id.clone(),
                        );
                        !excluded.contains(&identity) && seen.insert(identity)
                    });
                    items.truncate(usize::from(request.limit));
                    if !items.is_empty() {
                        batches.push((rank, PluginRecommendationBatch { route, items }));
                    }
                }
                Err(error) => failures.push(PluginCallFailure {
                    route,
                    error: format!("{error:#}"),
                }),
            }
        }
        batches.sort_by_key(|(rank, _)| *rank);

        Ok(PluginRecommendationFanout {
            batches: batches.into_iter().map(|(_, batch)| batch).collect(),
            plan,
            failures,
            task_errors,
            client_ready: true,
        })
    }
}

fn validate_recommendation_request(request: &RecommendationRequest) -> Result<()> {
    if request.limit == 0 || request.limit > MAX_RECOMMENDATION_LIMIT {
        bail!(
            "Recommendations limit 必须位于 1..={}，实际为 {}",
            MAX_RECOMMENDATION_LIMIT,
            request.limit
        );
    }
    if request.exclude.len() > MAX_RECOMMENDATION_EXCLUDES {
        bail!(
            "Recommendations exclude 数量超过 {}",
            MAX_RECOMMENDATION_EXCLUDES
        );
    }
    if let Some(seed) = request.seed.as_ref() {
        validate_track_query(seed)?;
    }
    for source in &request.exclude {
        validate_source_ref(source)?;
    }
    Ok(())
}

fn validate_track_query(query: &TrackQuery) -> Result<()> {
    if query.artists.len() > MAX_TRACK_ARTISTS {
        bail!("track query artists 数量超过限制");
    }
    if query
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("track query duration_ms 超出允许范围");
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
            bail!("track query artist 包含 NUL");
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
        bail!("track query 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!("track query 文本超过 {} bytes", MAX_TRACK_TEXT_BYTES);
    }
    Ok(())
}

fn validate_recommendation_items(route: &PluginRoute, items: &[RecommendationItem]) -> Result<()> {
    if items.len() > MAX_RECOMMENDATION_ITEMS_PER_ROUTE {
        bail!(
            "单 route Recommendations 返回 {} 项，超过 Host 上限 {}",
            items.len(),
            MAX_RECOMMENDATION_ITEMS_PER_ROUTE
        );
    }
    for item in items {
        validate_remote_track(route, &item.track)?;
        if item.score.is_some_and(|score| !score.is_finite()) {
            bail!("Recommendations score 必须是有限浮点数");
        }
        if let Some(reason) = item.reason.as_deref()
            && (reason.len() > MAX_REASON_BYTES || reason.contains('\0'))
        {
            bail!("Recommendations reason 非法或超过大小限制");
        }
    }
    Ok(())
}

fn validate_remote_track(route: &PluginRoute, track: &RemoteTrack) -> Result<()> {
    validate_source_ref(&track.source)?;
    if track.source.provider_id != route.provider_id {
        bail!(
            "recommendation track provider 不匹配: expected={}, actual={}",
            route.provider_id,
            track.source.provider_id
        );
    }
    if track.title.trim().is_empty() || track.title.contains('\0') {
        bail!("recommendation track title 非法");
    }
    if track.artists.len() > MAX_TRACK_ARTISTS {
        bail!("recommendation track artists 数量超过限制");
    }
    if track
        .duration_ms
        .is_some_and(|duration_ms| duration_ms == 0 || duration_ms > MAX_TRACK_DURATION_MS)
    {
        bail!("recommendation track duration_ms 超出允许范围");
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
            bail!("recommendation track artist 包含 NUL");
        }
        bytes = bytes.saturating_add(artist.len());
    }
    if track.album.contains('\0')
        || track.isrc.as_ref().is_some_and(|value| value.contains('\0'))
        || track
            .cover_url
            .as_ref()
            .is_some_and(|value| value.contains('\0'))
    {
        bail!("recommendation track 文本包含 NUL");
    }
    if bytes > MAX_TRACK_TEXT_BYTES {
        bail!("recommendation track 文本超过 {} bytes", MAX_TRACK_TEXT_BYTES);
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
    use crate::plugin::abi::{RecommendationSurface, SourceTrackRef};

    #[test]
    fn request_rejects_zero_or_unbounded_limit() {
        let mut request = RecommendationRequest {
            surface: RecommendationSurface::Home,
            seed: None,
            limit: 0,
            exclude: Vec::new(),
        };
        assert!(validate_recommendation_request(&request).is_err());
        request.limit = MAX_RECOMMENDATION_LIMIT;
        assert!(validate_recommendation_request(&request).is_ok());
        request.limit = MAX_RECOMMENDATION_LIMIT + 1;
        assert!(validate_recommendation_request(&request).is_err());
    }

    #[test]
    fn recommendation_result_rejects_cross_provider_track() {
        let route = PluginRoute {
            plugin_id: "plugin.test".into(),
            provider_id: "qqmusic".into(),
            account_id: "account".into(),
            priority: 0,
            is_default: true,
        };
        let item = RecommendationItem {
            track: RemoteTrack {
                source: SourceTrackRef {
                    provider_id: "netease".into(),
                    source_id: "song".into(),
                },
                title: "Song".into(),
                ..RemoteTrack::default()
            },
            score: Some(0.9),
            reason: None,
        };
        assert!(validate_recommendation_items(&route, &[item]).is_err());
    }
}
