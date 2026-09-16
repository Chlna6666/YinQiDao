use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use tokio::task::JoinSet;

use super::{
    abi::{
        CollectionRecommendationItem, CollectionRecommendationRequest, MediaCollection,
        MediaCollectionKind, MediaCollectionRef, PluginAccountPreference, PluginRoute,
        RoutingPolicy, ServiceKind, UserProfile,
    },
    client,
    frontend::{PluginCallFailure, PluginServiceFrontend},
    host::runtime::{self, PluginCallKey},
    routing::gate::GatedRoutePlan,
};

const MAX_MEDIA_FANOUT_ROUTES: usize = 16;
const MAX_MEDIA_COLLECTIONS_PER_ROUTE: usize = 512;
const MAX_MEDIA_PAGE_LIMIT: u16 = 200;
const MAX_COLLECTION_RECOMMENDATION_LIMIT: u16 = 100;
const MAX_COLLECTION_EXCLUDES: usize = 512;
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_ACCOUNT_ID_BYTES: usize = 4 * 1024;
const MAX_SOURCE_ID_BYTES: usize = 4 * 1024;
const MAX_TITLE_BYTES: usize = 8 * 1024;
const MAX_SUBTITLE_BYTES: usize = 16 * 1024;
const MAX_ARTWORK_URL_BYTES: usize = 16 * 1024;
const MAX_PROFILE_BIO_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug)]
pub struct PluginMediaCollectionBatch {
    pub route: PluginRoute,
    pub collections: Vec<MediaCollection>,
}

#[derive(Clone, Debug)]
pub struct PluginMediaCollectionFanout {
    pub batches: Vec<PluginMediaCollectionBatch>,
    pub plan: GatedRoutePlan,
    pub failures: Vec<PluginCallFailure>,
    pub task_errors: Vec<String>,
    pub client_ready: bool,
}

impl PluginServiceFrontend {
    /// Fetch one saved collection page from every eligible authenticated account concurrently.
    /// Provider-specific protocol shapes are normalized by the guest before reaching this boundary.
    pub async fn media_collections(
        &self,
        policy: &RoutingPolicy,
        kind: MediaCollectionKind,
        offset: u32,
        limit: u16,
    ) -> Result<PluginMediaCollectionFanout> {
        validate_page_limit(limit)?;
        let plan = self.plan(ServiceKind::MediaCollections, policy)?;
        let clients = client::global().unwrap_or_else(client::initialize);
        let Some(client) = clients.client()? else {
            return Ok(PluginMediaCollectionFanout {
                batches: Vec::new(),
                plan,
                failures: Vec::new(),
                task_errors: Vec::new(),
                client_ready: false,
            });
        };
        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;

        let mut failures = Vec::new();
        for route in plan.eligible_routes.iter().skip(MAX_MEDIA_FANOUT_ROUTES) {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: format!(
                    "MediaCollections fan-out 超过 Host route 上限 {}，本次未调用",
                    MAX_MEDIA_FANOUT_ROUTES
                ),
            });
        }

        let mut tasks = JoinSet::new();
        for (rank, route) in plan
            .eligible_routes
            .iter()
            .take(MAX_MEDIA_FANOUT_ROUTES)
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
                        client.media_collections(
                            &plugin_id,
                            &provider_id,
                            &account_id,
                            kind,
                            offset,
                            limit,
                        ),
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
                    task_errors.push(format!("MediaCollections fan-out task 异常退出: {error}"));
                    continue;
                }
            };
            match result {
                Ok(collections) => match validate_collection_batch(&route, kind, &collections, limit)
                {
                    Ok(()) => {
                        if !collections.is_empty() {
                            batches.push((rank, PluginMediaCollectionBatch { route, collections }));
                        }
                    }
                    Err(error) => failures.push(PluginCallFailure {
                        route,
                        error: format!("MediaCollections 返回值非法: {error:#}"),
                    }),
                },
                Err(error) => failures.push(PluginCallFailure {
                    route,
                    error: format!("{error:#}"),
                }),
            }
        }
        batches.sort_by_key(|(rank, _)| *rank);

        Ok(PluginMediaCollectionFanout {
            batches: batches.into_iter().map(|(_, batch)| batch).collect(),
            plan,
            failures,
            task_errors,
            client_ready: true,
        })
    }

    /// Mutations are always bound to one exact account route; merged library views must never
    /// broadcast a save/un-save operation across providers or sibling accounts.
    pub async fn set_media_saved(
        &self,
        route: &PluginRoute,
        collection: &MediaCollectionRef,
        saved: bool,
    ) -> Result<bool> {
        validate_collection_ref(route, collection)?;
        ensure_exact_route(self, route, ServiceKind::MediaCollections)?;
        let client = require_client()?;
        runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.set_media_saved(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    collection,
                    saved,
                ),
            )
            .await
    }

    /// Collection recommendations remain route-scoped so provider ranking/fusion stays in the Host
    /// application layer rather than becoming guest-controlled policy.
    pub async fn collection_recommendations(
        &self,
        route: &PluginRoute,
        request: &CollectionRecommendationRequest,
    ) -> Result<Vec<CollectionRecommendationItem>> {
        validate_recommendation_request(route, request)?;
        ensure_exact_route(self, route, ServiceKind::MediaCollections)?;
        let client = require_client()?;
        let items = runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.collection_recommendations(
                    &route.plugin_id,
                    &route.provider_id,
                    &route.account_id,
                    request,
                ),
            )
            .await?;
        validate_recommendation_items(route, request, &items)?;
        Ok(items)
    }

    pub async fn user_profile(&self, route: &PluginRoute) -> Result<UserProfile> {
        ensure_exact_route(self, route, ServiceKind::UserProfile)?;
        let client = require_client()?;
        let profile = runtime::global()
            .ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?
            .execute_guest_call(
                PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                client.user_profile(&route.plugin_id, &route.provider_id, &route.account_id),
            )
            .await?;
        validate_user_profile(route, &profile)?;
        Ok(profile)
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
    route: &PluginRoute,
    service: ServiceKind,
) -> Result<()> {
    let policy = RoutingPolicy {
        preferred_account: Some(PluginAccountPreference {
            plugin_id: route.plugin_id.clone(),
            provider_id: route.provider_id.clone(),
            account_id: route.account_id.clone(),
        }),
        ..RoutingPolicy::default()
    };
    let plan = frontend.plan(service, &policy)?;
    if plan.eligible_routes.iter().any(|candidate| same_route(candidate, route)) {
        return Ok(());
    }
    bail!(
        "Plugin route 当前不可用或未通过 Host session/health gate: {}/{}/{}",
        route.plugin_id,
        route.provider_id,
        route.account_id
    )
}

fn same_route(left: &PluginRoute, right: &PluginRoute) -> bool {
    left.plugin_id == right.plugin_id
        && left.provider_id == right.provider_id
        && left.account_id == right.account_id
}

fn validate_collection_batch(
    route: &PluginRoute,
    kind: MediaCollectionKind,
    collections: &[MediaCollection],
    limit: u16,
) -> Result<()> {
    if collections.len() > MAX_MEDIA_COLLECTIONS_PER_ROUTE {
        bail!(
            "单 route MediaCollections 返回 {} 项，超过 Host 上限 {}",
            collections.len(),
            MAX_MEDIA_COLLECTIONS_PER_ROUTE
        );
    }
    if collections.len() > usize::from(limit) {
        bail!(
            "MediaCollections 返回 {} 项，超过请求 limit {}",
            collections.len(),
            limit
        );
    }

    let mut seen = HashSet::with_capacity(collections.len());
    for collection in collections {
        validate_collection(route, collection)?;
        if collection.source.kind != kind {
            bail!("MediaCollections 返回了请求 kind 之外的集合");
        }
        if !seen.insert((collection.source.kind, collection.source.source_id.as_str())) {
            bail!(
                "MediaCollections 返回重复 source: {:?}/{}",
                collection.source.kind,
                collection.source.source_id
            );
        }
    }
    Ok(())
}

fn validate_collection(route: &PluginRoute, collection: &MediaCollection) -> Result<()> {
    validate_collection_ref(route, &collection.source)?;
    validate_required_text("collection title", &collection.title, MAX_TITLE_BYTES)?;
    validate_optional_text("collection subtitle", collection.subtitle.as_deref(), MAX_SUBTITLE_BYTES)?;
    validate_optional_text(
        "collection artwork URL",
        collection.artwork_url.as_deref(),
        MAX_ARTWORK_URL_BYTES,
    )?;
    Ok(())
}

fn validate_collection_ref(route: &PluginRoute, source: &MediaCollectionRef) -> Result<()> {
    validate_provider_id(&source.provider_id)?;
    if source.provider_id != route.provider_id {
        bail!(
            "media collection provider 不匹配: expected={}, actual={}",
            route.provider_id,
            source.provider_id
        );
    }
    if source.source_id.trim().is_empty()
        || source.source_id.len() > MAX_SOURCE_ID_BYTES
        || source.source_id.contains('\0')
    {
        bail!("media collection source id 非法");
    }
    Ok(())
}

fn validate_recommendation_request(
    route: &PluginRoute,
    request: &CollectionRecommendationRequest,
) -> Result<()> {
    if request.limit == 0 || request.limit > MAX_COLLECTION_RECOMMENDATION_LIMIT {
        bail!(
            "collection recommendation limit 必须位于 1..={}，实际为 {}",
            MAX_COLLECTION_RECOMMENDATION_LIMIT,
            request.limit
        );
    }
    if request.exclude.len() > MAX_COLLECTION_EXCLUDES {
        bail!(
            "collection recommendation exclude 超过 Host 上限 {}",
            MAX_COLLECTION_EXCLUDES
        );
    }
    if let Some(seed) = request.seed.as_ref() {
        validate_collection_ref(route, seed)?;
        if seed.kind != request.kind {
            bail!("collection recommendation seed kind 与请求 kind 不一致");
        }
    }
    for source in &request.exclude {
        validate_collection_ref(route, source)?;
        if source.kind != request.kind {
            bail!("collection recommendation exclude kind 与请求 kind 不一致");
        }
    }
    Ok(())
}

fn validate_recommendation_items(
    route: &PluginRoute,
    request: &CollectionRecommendationRequest,
    items: &[CollectionRecommendationItem],
) -> Result<()> {
    if items.len() > usize::from(request.limit) {
        bail!(
            "collection-recommendations 返回 {} 项，超过请求 limit {}",
            items.len(),
            request.limit
        );
    }
    let excluded = request
        .exclude
        .iter()
        .map(|source| (source.kind, source.source_id.as_str()))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::with_capacity(items.len());
    for item in items {
        validate_collection(route, &item.collection)?;
        if item.collection.source.kind != request.kind {
            bail!("collection-recommendations 返回了请求 kind 之外的集合");
        }
        if item.score.is_some_and(|score| !score.is_finite()) {
            bail!("collection recommendation score 必须为有限值");
        }
        validate_optional_text("collection recommendation reason", item.reason.as_deref(), MAX_SUBTITLE_BYTES)?;
        let key = (
            item.collection.source.kind,
            item.collection.source.source_id.as_str(),
        );
        if excluded.contains(&key) {
            bail!("collection-recommendations 返回了明确排除的集合");
        }
        if !seen.insert(key) {
            bail!("collection-recommendations 返回重复集合");
        }
    }
    Ok(())
}

fn validate_user_profile(route: &PluginRoute, profile: &UserProfile) -> Result<()> {
    validate_provider_id(&profile.provider_id)?;
    if profile.provider_id != route.provider_id {
        bail!(
            "user profile provider 不匹配: expected={}, actual={}",
            route.provider_id,
            profile.provider_id
        );
    }
    if profile.account_id != route.account_id
        || profile.account_id.is_empty()
        || profile.account_id.len() > MAX_ACCOUNT_ID_BYTES
        || profile.account_id.contains('\0')
    {
        bail!("user profile account id 与 route 不匹配或非法");
    }
    validate_required_text("user profile display name", &profile.display_name, MAX_TITLE_BYTES)?;
    validate_optional_text("user profile avatar URL", profile.avatar_url.as_deref(), MAX_ARTWORK_URL_BYTES)?;
    validate_optional_text("user profile bio", profile.bio.as_deref(), MAX_PROFILE_BIO_BYTES)?;
    Ok(())
}

fn validate_page_limit(limit: u16) -> Result<()> {
    if limit == 0 || limit > MAX_MEDIA_PAGE_LIMIT {
        bail!(
            "MediaCollections limit 必须位于 1..={}，实际为 {}",
            MAX_MEDIA_PAGE_LIMIT,
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

fn validate_required_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.contains('\0') {
        bail!("{label} 非法或超过 {max_bytes} bytes");
    }
    Ok(())
}

fn validate_optional_text(label: &str, value: Option<&str>, max_bytes: usize) -> Result<()> {
    if let Some(value) = value
        && (value.len() > max_bytes || value.contains('\0'))
    {
        bail!("{label} 非法或超过 {max_bytes} bytes");
    }
    Ok(())
}
