use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use wasmtime::Store;

use super::{
    lifecycle::LazyCompiledComponentCache,
    registry::PluginComponentRegistry,
    wasmtime::{
        PluginStoreData, PluginWasmtimeRuntime,
        bindings::{
            self, exports::yinqidao::music_plugin::ui as wit_ui,
            yinqidao::music_plugin::types as wit_types,
        },
    },
};
use crate::plugin::{
    abi::{
        ArtworkDescriptor, AuthChallenge, AuthChallengeKind, AuthMethod, AuthPollResult,
        CollectionRecommendationItem, CollectionRecommendationRequest,
        CollectionRecommendationSurface, KeyValue, MediaCollection, MediaCollectionKind,
        MediaCollectionRef, PlaybackSignal, PlaybackSignalKind, PlaylistDescriptor,
        PluginCapability, PluginLyricDocument, PluginManifest, ProviderAccount, ProviderDescriptor,
        RecognitionRequest, RecognitionResult, RecommendationItem, RecommendationRequest,
        RecommendationSurface, RemoteTrack, SourceTrackRef, StreamDescriptor, StreamRequest,
        TrackQuery, UserProfile,
    },
    client::{PluginClientFuture, PluginProviderClient},
    host::catalog::PluginHostState,
    runtime_ports::PluginRuntimeHotSwap,
    ui::{
        client::{
            PluginUiClient, PluginUiEvent, PluginUiFuture, PluginUiResponse, UiCommandContext,
            UiCommandResponse, UiCommandSurface, UiFieldValue,
        },
        schema::{UiPageModel, UiSchemaLimits},
        wire::{WireUiNode, WireUiPage, WireUiSelectOption, WireUiSpacerSize},
    },
};

pub(crate) struct WasmtimeComponentAdapter {
    runtime: Arc<PluginWasmtimeRuntime>,
    components: Arc<PluginComponentRegistry>,
    host: Arc<RwLock<PluginHostState>>,
    compiled_cache: LazyCompiledComponentCache<wasmtime::component::Component>,
    ui_limits: UiSchemaLimits,
}

impl WasmtimeComponentAdapter {
    pub(crate) fn new(
        runtime: Arc<PluginWasmtimeRuntime>,
        components: Arc<PluginComponentRegistry>,
        host: Arc<RwLock<PluginHostState>>,
        max_compiled_components: usize,
    ) -> Self {
        Self {
            runtime,
            components,
            host,
            compiled_cache: LazyCompiledComponentCache::new(max_compiled_components),
            ui_limits: UiSchemaLimits::default(),
        }
    }

    async fn instantiate(
        &self,
        plugin_id: &str,
    ) -> Result<(Store<PluginStoreData>, bindings::MusicPlugin)> {
        let plugin = {
            let host = self
                .host
                .read()
                .map_err(|e| anyhow!("插件 Host 状态锁已损坏: {e}"))?;
            host.catalog()
                .plugin(plugin_id)
                .cloned()
                .ok_or_else(|| anyhow!("未在 catalog 中发现插件: {plugin_id}"))?
        };

        let snapshot = self.components.snapshot(&plugin)?;
        let engine = self.runtime.engine().clone();
        let component = self
            .compiled_cache
            .get_or_try_compile(&snapshot, |snapshot| {
                wasmtime::component::Component::from_binary(&engine, &snapshot.bytes)
                    .map_err(|error| anyhow!("编译插件 Component 失败: {error}"))
            })?;

        let mut store = self.runtime.store(plugin_id)?;
        let linker = self.runtime.linker()?;
        let instance = bindings::MusicPlugin::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|error| anyhow!("实例化插件 Component 失败: {error}"))?;

        Ok((store, instance))
    }
}

impl PluginRuntimeHotSwap for WasmtimeComponentAdapter {
    fn refresh_plugin(&self, plugin_id: &str) -> Result<()> {
        let _ = self.compiled_cache.invalidate_plugin(plugin_id)?;
        let _ = self.components.invalidate(plugin_id)?;
        Ok(())
    }
}

impl PluginProviderClient for WasmtimeComponentAdapter {
    fn manifest<'a>(&'a self, plugin_id: &'a str) -> PluginClientFuture<'a, PluginManifest> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let manifest = instance
                .yinqidao_music_plugin_provider()
                .call_manifest(&mut store)
                .await
                .map_err(|e| anyhow!("调用插件 manifest 失败: {e}"))?;
            Ok(manifest.into())
        })
    }

    fn accounts<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
    ) -> PluginClientFuture<'a, Vec<ProviderAccount>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let accounts = instance
                .yinqidao_music_plugin_provider()
                .call_accounts(&mut store, provider_id)
                .await
                .map_err(|e| anyhow!("调用插件 accounts 失败: {e}"))?
                .map_err(|e| anyhow!("插件 accounts 错误: {e}"))?;
            Ok(accounts.into_iter().map(Into::into).collect())
        })
    }

    fn auth_begin<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        method: AuthMethod,
    ) -> PluginClientFuture<'a, AuthChallenge> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let challenge = instance
                .yinqidao_music_plugin_provider()
                .call_auth_begin(&mut store, provider_id, method.into())
                .await
                .map_err(|e| anyhow!("调用插件 auth_begin 失败: {e}"))?
                .map_err(|e| anyhow!("插件 auth_begin 业务错误: {e}"))?;
            Ok(challenge.into())
        })
    }

    fn auth_poll<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
    ) -> PluginClientFuture<'a, AuthPollResult> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let poll_result = instance
                .yinqidao_music_plugin_provider()
                .call_poll_auth(&mut store, provider_id, challenge_id)
                .await
                .map_err(|e| anyhow!("调用插件 poll_auth 失败: {e}"))?
                .map_err(|e| anyhow!("插件 poll_auth 业务错误: {e}"))?;
            Ok(poll_result.into())
        })
    }

    fn auth_submit<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
        values: &'a [KeyValue],
    ) -> PluginClientFuture<'a, AuthPollResult> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_values: Vec<wit_types::KeyValue> =
                values.iter().cloned().map(Into::into).collect();
            let poll_result = instance
                .yinqidao_music_plugin_provider()
                .call_auth_submit(&mut store, provider_id, challenge_id, &wit_values)
                .await
                .map_err(|e| anyhow!("调用插件 auth_submit 失败: {e}"))?
                .map_err(|e| anyhow!("插件 auth_submit 业务错误: {e}"))?;
            Ok(poll_result.into())
        })
    }

    fn auth_cancel<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_auth_cancel(&mut store, provider_id, challenge_id)
                .await
                .map_err(|e| anyhow!("调用插件 auth_cancel 失败: {e}"))?
                .map_err(|e| anyhow!("插件 auth_cancel 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn logout<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_logout(&mut store, provider_id, account_id)
                .await
                .map_err(|e| anyhow!("调用插件 logout 失败: {e}"))?
                .map_err(|e| anyhow!("插件 logout 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn search<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        query: &'a str,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let tracks = instance
                .yinqidao_music_plugin_provider()
                .call_search(&mut store, provider_id, account_id, query, limit)
                .await
                .map_err(|e| anyhow!("调用插件 search 失败: {e}"))?
                .map_err(|e| anyhow!("插件 search 业务错误: {e}"))?;
            Ok(tracks.into_iter().map(Into::into).collect())
        })
    }

    fn resolve_track<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        query: &'a TrackQuery,
    ) -> PluginClientFuture<'a, Option<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_query: wit_types::TrackQuery = query.clone().into();
            let track = instance
                .yinqidao_music_plugin_provider()
                .call_resolve_track(&mut store, provider_id, account_id, &wit_query)
                .await
                .map_err(|e| anyhow!("调用插件 resolve_track 失败: {e}"))?
                .map_err(|e| anyhow!("插件 resolve_track 业务错误: {e}"))?;
            Ok(track.map(Into::into))
        })
    }

    fn lyrics<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        track: &'a SourceTrackRef,
    ) -> PluginClientFuture<'a, Option<PluginLyricDocument>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_track: wit_types::SourceTrackRef = track.clone().into();
            let doc = instance
                .yinqidao_music_plugin_provider()
                .call_lyrics(&mut store, provider_id, account_id, &wit_track)
                .await
                .map_err(|e| anyhow!("调用插件 lyrics 失败: {e}"))?
                .map_err(|e| anyhow!("插件 lyrics 业务错误: {e}"))?;
            Ok(doc.map(Into::into))
        })
    }

    fn artwork<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        track: &'a SourceTrackRef,
    ) -> PluginClientFuture<'a, Option<ArtworkDescriptor>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_track: wit_types::SourceTrackRef = track.clone().into();
            let art = instance
                .yinqidao_music_plugin_provider()
                .call_artwork(&mut store, provider_id, account_id, &wit_track)
                .await
                .map_err(|e| anyhow!("调用插件 artwork 失败: {e}"))?
                .map_err(|e| anyhow!("插件 artwork 业务错误: {e}"))?;
            Ok(art.map(Into::into))
        })
    }

    fn stream<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        request: &'a StreamRequest,
    ) -> PluginClientFuture<'a, StreamDescriptor> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_req: wit_types::StreamRequest = request.clone().into();
            let desc = instance
                .yinqidao_music_plugin_provider()
                .call_stream(&mut store, provider_id, account_id, &wit_req)
                .await
                .map_err(|e| anyhow!("调用插件 stream 失败: {e}"))?
                .map_err(|e| anyhow!("插件 stream 业务错误: {e}"))?;
            Ok(desc.into())
        })
    }

    fn playlists<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
    ) -> PluginClientFuture<'a, Vec<PlaylistDescriptor>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let playlists = instance
                .yinqidao_music_plugin_provider()
                .call_playlists(&mut store, provider_id, account_id)
                .await
                .map_err(|e| anyhow!("调用插件 playlists 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlists 业务错误: {e}"))?;
            Ok(playlists.into_iter().map(Into::into).collect())
        })
    }

    fn playlist_tracks<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let tracks = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_tracks(
                    &mut store,
                    provider_id,
                    account_id,
                    playlist_id,
                    offset,
                    limit,
                )
                .await
                .map_err(|e| anyhow!("调用插件 playlist_tracks 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_tracks 业务错误: {e}"))?;
            Ok(tracks.into_iter().map(Into::into).collect())
        })
    }

    fn playlist_create<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        name: &'a str,
    ) -> PluginClientFuture<'a, PlaylistDescriptor> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let playlist = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_create(&mut store, provider_id, account_id, name)
                .await
                .map_err(|e| anyhow!("调用插件 playlist_create 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_create 业务错误: {e}"))?;
            Ok(playlist.into())
        })
    }

    fn playlist_add<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        tracks: &'a [SourceTrackRef],
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_tracks: Vec<wit_types::SourceTrackRef> =
                tracks.iter().cloned().map(Into::into).collect();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_add(
                    &mut store,
                    provider_id,
                    account_id,
                    playlist_id,
                    &wit_tracks,
                )
                .await
                .map_err(|e| anyhow!("调用插件 playlist_add 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_add 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn playlist_remove<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        tracks: &'a [SourceTrackRef],
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_tracks: Vec<wit_types::SourceTrackRef> =
                tracks.iter().cloned().map(Into::into).collect();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_remove(
                    &mut store,
                    provider_id,
                    account_id,
                    playlist_id,
                    &wit_tracks,
                )
                .await
                .map_err(|e| anyhow!("调用插件 playlist_remove 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_remove 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn playlist_rename<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        name: &'a str,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_rename(&mut store, provider_id, account_id, playlist_id, name)
                .await
                .map_err(|e| anyhow!("调用插件 playlist_rename 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_rename 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn playlist_delete<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_playlist_delete(&mut store, provider_id, account_id, playlist_id)
                .await
                .map_err(|e| anyhow!("调用插件 playlist_delete 失败: {e}"))?
                .map_err(|e| anyhow!("插件 playlist_delete 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn media_collections<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        kind: MediaCollectionKind,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<MediaCollection>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let collections = instance
                .yinqidao_music_plugin_provider()
                .call_media_collections(
                    &mut store,
                    provider_id,
                    account_id,
                    kind.into(),
                    offset,
                    limit,
                )
                .await
                .map_err(|e| anyhow!("调用插件 media_collections 失败: {e}"))?
                .map_err(|e| anyhow!("插件 media_collections 业务错误: {e}"))?;
            Ok(collections.into_iter().map(Into::into).collect())
        })
    }

    fn media_collection_detail<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        collection: &'a MediaCollectionRef,
    ) -> PluginClientFuture<'a, MediaCollection> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_collection: wit_types::MediaCollectionRef = collection.clone().into();
            let detail = instance
                .yinqidao_music_plugin_provider()
                .call_media_collection_detail(&mut store, provider_id, account_id, &wit_collection)
                .await
                .map_err(|e| anyhow!("调用插件 media_collection_detail 失败: {e}"))?
                .map_err(|e| anyhow!("插件 media_collection_detail 业务错误: {e}"))?;
            Ok(detail.into())
        })
    }

    fn collection_tracks<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        collection: &'a MediaCollectionRef,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_collection: wit_types::MediaCollectionRef = collection.clone().into();
            let tracks = instance
                .yinqidao_music_plugin_provider()
                .call_collection_tracks(
                    &mut store,
                    provider_id,
                    account_id,
                    &wit_collection,
                    offset,
                    limit,
                )
                .await
                .map_err(|e| anyhow!("调用插件 collection_tracks 失败: {e}"))?
                .map_err(|e| anyhow!("插件 collection_tracks 业务错误: {e}"))?;
            Ok(tracks.into_iter().map(Into::into).collect())
        })
    }

    fn collection_items<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        collection: &'a MediaCollectionRef,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<MediaCollection>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_collection: wit_types::MediaCollectionRef = collection.clone().into();
            let items = instance
                .yinqidao_music_plugin_provider()
                .call_collection_items(
                    &mut store,
                    provider_id,
                    account_id,
                    &wit_collection,
                    offset,
                    limit,
                )
                .await
                .map_err(|e| anyhow!("调用插件 collection_items 失败: {e}"))?
                .map_err(|e| anyhow!("插件 collection_items 业务错误: {e}"))?;
            Ok(items.into_iter().map(Into::into).collect())
        })
    }

    fn set_media_saved<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        collection: &'a MediaCollectionRef,
        saved: bool,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_collection: wit_types::MediaCollectionRef = collection.clone().into();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_set_media_saved(&mut store, provider_id, account_id, &wit_collection, saved)
                .await
                .map_err(|e| anyhow!("调用插件 set_media_saved 失败: {e}"))?
                .map_err(|e| anyhow!("插件 set_media_saved 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn collection_recommendations<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        request: &'a CollectionRecommendationRequest,
    ) -> PluginClientFuture<'a, Vec<CollectionRecommendationItem>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_req: wit_types::CollectionRecommendationRequest = request.clone().into();
            let items = instance
                .yinqidao_music_plugin_provider()
                .call_collection_recommendations(&mut store, provider_id, account_id, &wit_req)
                .await
                .map_err(|e| anyhow!("调用插件 collection_recommendations 失败: {e}"))?
                .map_err(|e| anyhow!("插件 collection_recommendations 业务错误: {e}"))?;
            Ok(items.into_iter().map(Into::into).collect())
        })
    }

    fn user_profile<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
    ) -> PluginClientFuture<'a, UserProfile> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let profile = instance
                .yinqidao_music_plugin_provider()
                .call_get_user_profile(&mut store, provider_id, account_id)
                .await
                .map_err(|e| anyhow!("调用插件 get_user_profile 失败: {e}"))?
                .map_err(|e| anyhow!("插件 get_user_profile 业务错误: {e}"))?;
            Ok(profile.into())
        })
    }

    fn cloud_library<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let tracks = instance
                .yinqidao_music_plugin_provider()
                .call_cloud_library(&mut store, provider_id, account_id, offset, limit)
                .await
                .map_err(|e| anyhow!("调用插件 cloud_library 失败: {e}"))?
                .map_err(|e| anyhow!("插件 cloud_library 业务错误: {e}"))?;
            Ok(tracks.into_iter().map(Into::into).collect())
        })
    }

    fn liked_tracks<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let tracks = instance
                .yinqidao_music_plugin_provider()
                .call_liked_tracks(&mut store, provider_id, account_id, offset, limit)
                .await
                .map_err(|e| anyhow!("调用插件 liked_tracks 失败: {e}"))?
                .map_err(|e| anyhow!("插件 liked_tracks 业务错误: {e}"))?;
            Ok(tracks.into_iter().map(Into::into).collect())
        })
    }

    fn set_liked<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        track: &'a SourceTrackRef,
        liked: bool,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_track: wit_types::SourceTrackRef = track.clone().into();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_set_liked(&mut store, provider_id, account_id, &wit_track, liked)
                .await
                .map_err(|e| anyhow!("调用插件 set_liked 失败: {e}"))?
                .map_err(|e| anyhow!("插件 set_liked 业务错误: {e}"))?;
            Ok(result)
        })
    }

    fn recommendations<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        request: &'a RecommendationRequest,
    ) -> PluginClientFuture<'a, Vec<RecommendationItem>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_req: wit_types::RecommendationRequest = request.clone().into();
            let items = instance
                .yinqidao_music_plugin_provider()
                .call_recommendations(&mut store, provider_id, account_id, &wit_req)
                .await
                .map_err(|e| anyhow!("调用插件 recommendations 失败: {e:#}"))?
                .map_err(|e| anyhow!("插件 recommendations 业务错误: {e}"))?;
            Ok(items.into_iter().map(Into::into).collect())
        })
    }

    fn recognize<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        request: &'a RecognitionRequest,
    ) -> PluginClientFuture<'a, Option<RecognitionResult>> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_req: wit_types::RecognitionRequest = request.clone().into();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_recognize(&mut store, provider_id, account_id, &wit_req)
                .await
                .map_err(|e| anyhow!("调用插件 recognize 失败: {e}"))?
                .map_err(|e| anyhow!("插件 recognize 业务错误: {e}"))?;
            Ok(result.map(Into::into))
        })
    }

    fn report_playback<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        signal: &'a PlaybackSignal,
    ) -> PluginClientFuture<'a, bool> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_signal: wit_types::PlaybackSignal = signal.clone().into();
            let result = instance
                .yinqidao_music_plugin_provider()
                .call_report_playback(&mut store, provider_id, account_id, &wit_signal)
                .await
                .map_err(|e| anyhow!("调用插件 report_playback 失败: {e}"))?
                .map_err(|e| anyhow!("插件 report_playback 业务错误: {e}"))?;
            Ok(result)
        })
    }
}

impl PluginUiClient for WasmtimeComponentAdapter {
    fn load_page<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
    ) -> PluginUiFuture<'a, UiPageModel> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let page = instance
                .yinqidao_music_plugin_ui()
                .call_load_page(&mut store, page_id)
                .await
                .map_err(|e| anyhow!("调用插件 load_page 失败: {e}"))?
                .map_err(|e| anyhow!("插件 load_page 业务错误: {e}"))?;
            let wire_page: WireUiPage = page.into();
            wire_page.into_page_model(&self.ui_limits)
        })
    }

    fn handle_event<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
        event: PluginUiEvent,
    ) -> PluginUiFuture<'a, PluginUiResponse> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_event = match event {
                PluginUiEvent::Action { action_id, fields } => {
                    wit_ui::UiEvent::Action(wit_ui::ActionEvent {
                        action_id,
                        fields: fields
                            .into_iter()
                            .map(|(k, v)| wit_ui::FieldEntry {
                                field_id: k,
                                value: match v {
                                    UiFieldValue::Text(t) => wit_ui::FieldValue::Text(t),
                                    UiFieldValue::Bool(b) => wit_ui::FieldValue::Boolean(b),
                                },
                            })
                            .collect(),
                    })
                }
                PluginUiEvent::FieldChanged { field_id, value } => {
                    wit_ui::UiEvent::FieldChanged(wit_ui::FieldChangeEvent {
                        field_id,
                        value: match value {
                            UiFieldValue::Text(t) => wit_ui::FieldValue::Text(t),
                            UiFieldValue::Bool(b) => wit_ui::FieldValue::Boolean(b),
                        },
                    })
                }
            };
            let resp = instance
                .yinqidao_music_plugin_ui()
                .call_handle_event(&mut store, page_id, &wit_event)
                .await
                .map_err(|e| anyhow!("调用插件 handle_event 失败: {e}"))?
                .map_err(|e| anyhow!("插件 handle_event 业务错误: {e}"))?;
            let page_model = match resp.page {
                Some(p) => {
                    let wire: WireUiPage = p.into();
                    Some(wire.into_page_model(&self.ui_limits)?)
                }
                None => None,
            };
            Ok(PluginUiResponse {
                page: page_model,
                toast: resp.toast,
                close: resp.close,
            })
        })
    }

    fn invoke_command<'a>(
        &'a self,
        plugin_id: &'a str,
        command_id: &'a str,
        context: UiCommandContext,
    ) -> PluginUiFuture<'a, UiCommandResponse> {
        Box::pin(async move {
            let (mut store, instance) = self.instantiate(plugin_id).await?;
            let wit_ctx = wit_ui::CommandContext {
                surface: match context.surface {
                    UiCommandSurface::CommandPalette => wit_ui::CommandSurface::CommandPalette,
                    UiCommandSurface::TrackContext => wit_ui::CommandSurface::TrackContext,
                    UiCommandSurface::PlaylistContext => wit_ui::CommandSurface::PlaylistContext,
                    UiCommandSurface::PageLocal => wit_ui::CommandSurface::PageLocal,
                },
                page_id: context.page_id,
                track: context.track.map(|t| wit_ui::CommandTrackContext {
                    title: t.title,
                    artists: t.artists,
                    album: t.album,
                    duration_ms: t.duration_ms,
                    provider_id: t.provider_id,
                    source_id: t.source_id,
                }),
                playlist: context.playlist.map(|p| wit_ui::CommandPlaylistContext {
                    name: p.name,
                    provider_id: p.provider_id,
                    source_id: p.source_id,
                }),
            };
            let resp = instance
                .yinqidao_music_plugin_ui()
                .call_invoke_command(&mut store, command_id, &wit_ctx)
                .await
                .map_err(|e| anyhow!("调用插件 invoke_command 失败: {e}"))?
                .map_err(|e| anyhow!("插件 invoke_command 业务错误: {e}"))?;
            Ok(UiCommandResponse {
                toast: resp.toast,
                open_page_id: resp.open_page_id,
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Type conversions: ABI <-> WIT
// ---------------------------------------------------------------------------

impl From<PluginCapability> for wit_types::Capability {
    fn from(c: PluginCapability) -> Self {
        match c {
            PluginCapability::Authentication => wit_types::Capability::Authentication,
            PluginCapability::Search => wit_types::Capability::Search,
            PluginCapability::Metadata => wit_types::Capability::Metadata,
            PluginCapability::Lyrics => wit_types::Capability::Lyrics,
            PluginCapability::Artwork => wit_types::Capability::Artwork,
            PluginCapability::Streaming => wit_types::Capability::Streaming,
            PluginCapability::Playlists => wit_types::Capability::Playlists,
            PluginCapability::MediaCollections => wit_types::Capability::MediaCollections,
            PluginCapability::CloudLibrary => wit_types::Capability::CloudLibrary,
            PluginCapability::Recommendations => wit_types::Capability::Recommendations,
            PluginCapability::Recognition => wit_types::Capability::Recognition,
            PluginCapability::UserProfile => wit_types::Capability::UserProfile,
            PluginCapability::LikeSync => wit_types::Capability::LikeSync,
            PluginCapability::PlaybackEvents => wit_types::Capability::PlaybackEvents,
        }
    }
}

impl From<wit_types::Capability> for PluginCapability {
    fn from(c: wit_types::Capability) -> Self {
        match c {
            wit_types::Capability::Authentication => PluginCapability::Authentication,
            wit_types::Capability::Search => PluginCapability::Search,
            wit_types::Capability::Metadata => PluginCapability::Metadata,
            wit_types::Capability::Lyrics => PluginCapability::Lyrics,
            wit_types::Capability::Artwork => PluginCapability::Artwork,
            wit_types::Capability::Streaming => PluginCapability::Streaming,
            wit_types::Capability::Playlists => PluginCapability::Playlists,
            wit_types::Capability::MediaCollections => PluginCapability::MediaCollections,
            wit_types::Capability::CloudLibrary => PluginCapability::CloudLibrary,
            wit_types::Capability::Recommendations => PluginCapability::Recommendations,
            wit_types::Capability::Recognition => PluginCapability::Recognition,
            wit_types::Capability::UserProfile => PluginCapability::UserProfile,
            wit_types::Capability::LikeSync => PluginCapability::LikeSync,
            wit_types::Capability::PlaybackEvents => PluginCapability::PlaybackEvents,
        }
    }
}

impl From<AuthMethod> for wit_types::AuthMethod {
    fn from(m: AuthMethod) -> Self {
        match m {
            AuthMethod::QrCode => wit_types::AuthMethod::QrCode,
            AuthMethod::BrowserOAuth => wit_types::AuthMethod::BrowserOauth,
            AuthMethod::DeviceCode => wit_types::AuthMethod::DeviceCode,
            AuthMethod::CookieImport => wit_types::AuthMethod::CookieImport,
            AuthMethod::CustomForm => wit_types::AuthMethod::CustomForm,
        }
    }
}

impl From<wit_types::AuthMethod> for AuthMethod {
    fn from(m: wit_types::AuthMethod) -> Self {
        match m {
            wit_types::AuthMethod::QrCode => AuthMethod::QrCode,
            wit_types::AuthMethod::BrowserOauth => AuthMethod::BrowserOAuth,
            wit_types::AuthMethod::DeviceCode => AuthMethod::DeviceCode,
            wit_types::AuthMethod::CookieImport => AuthMethod::CookieImport,
            wit_types::AuthMethod::CustomForm => AuthMethod::CustomForm,
        }
    }
}

impl From<KeyValue> for wit_types::KeyValue {
    fn from(kv: KeyValue) -> Self {
        wit_types::KeyValue {
            key: kv.key,
            value: kv.value,
        }
    }
}

impl From<wit_types::KeyValue> for KeyValue {
    fn from(kv: wit_types::KeyValue) -> Self {
        KeyValue {
            key: kv.key,
            value: kv.value,
        }
    }
}

impl From<wit_types::AuthChallengeKind> for AuthChallengeKind {
    fn from(k: wit_types::AuthChallengeKind) -> Self {
        match k {
            wit_types::AuthChallengeKind::QrCode => AuthChallengeKind::QrCode,
            wit_types::AuthChallengeKind::Browser => AuthChallengeKind::Browser,
            wit_types::AuthChallengeKind::DeviceCode => AuthChallengeKind::DeviceCode,
            wit_types::AuthChallengeKind::Form => AuthChallengeKind::Form,
        }
    }
}

impl From<wit_types::AuthChallenge> for AuthChallenge {
    fn from(c: wit_types::AuthChallenge) -> Self {
        AuthChallenge {
            challenge_id: c.challenge_id,
            kind: c.kind.into(),
            verification_uri: c.verification_uri,
            user_code: c.user_code,
            qr_payload: c.qr_payload,
            fields: c.fields.into_iter().map(Into::into).collect(),
            expires_at_ms: c.expires_at_ms,
        }
    }
}

impl From<wit_types::Account> for ProviderAccount {
    fn from(a: wit_types::Account) -> Self {
        ProviderAccount {
            account_id: a.account_id,
            provider_id: a.provider_id,
            display_name: a.display_name,
            avatar_url: a.avatar_url,
            capabilities: a.capabilities.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::AuthPoll> for AuthPollResult {
    fn from(p: wit_types::AuthPoll) -> Self {
        match p {
            wit_types::AuthPoll::Pending => AuthPollResult::Pending,
            wit_types::AuthPoll::Authenticated(account) => {
                AuthPollResult::Authenticated(account.into())
            }
            wit_types::AuthPoll::Expired => AuthPollResult::Expired,
            wit_types::AuthPoll::Denied(reason) => AuthPollResult::Denied(reason),
        }
    }
}

impl From<SourceTrackRef> for wit_types::SourceTrackRef {
    fn from(r: SourceTrackRef) -> Self {
        wit_types::SourceTrackRef {
            provider_id: r.provider_id,
            source_id: r.source_id,
        }
    }
}

impl From<wit_types::SourceTrackRef> for SourceTrackRef {
    fn from(r: wit_types::SourceTrackRef) -> Self {
        SourceTrackRef {
            provider_id: r.provider_id,
            source_id: r.source_id,
        }
    }
}

impl From<TrackQuery> for wit_types::TrackQuery {
    fn from(q: TrackQuery) -> Self {
        wit_types::TrackQuery {
            title: q.title,
            artists: q.artists,
            album: q.album,
            duration_ms: q.duration_ms,
            isrc: q.isrc,
            musicbrainz_recording_id: q.musicbrainz_recording_id,
            fingerprint_id: q.fingerprint_id,
        }
    }
}

impl From<wit_types::TrackQuery> for TrackQuery {
    fn from(q: wit_types::TrackQuery) -> Self {
        TrackQuery {
            title: q.title,
            artists: q.artists,
            album: q.album,
            duration_ms: q.duration_ms,
            isrc: q.isrc,
            musicbrainz_recording_id: q.musicbrainz_recording_id,
            fingerprint_id: q.fingerprint_id,
        }
    }
}

impl From<wit_types::RemoteTrack> for RemoteTrack {
    fn from(t: wit_types::RemoteTrack) -> Self {
        RemoteTrack {
            source: t.source.into(),
            title: t.title,
            artists: t.artists,
            album: t.album,
            duration_ms: t.duration_ms,
            isrc: t.isrc,
            cover_url: t.cover_url,
            playable: t.playable,
            explicit: t.explicit,
        }
    }
}

impl From<wit_types::LyricWord> for crate::plugin::abi::LyricWord {
    fn from(w: wit_types::LyricWord) -> Self {
        Self {
            timestamp_ms: w.timestamp_ms,
            duration_ms: w.duration_ms,
            text: w.text,
        }
    }
}

impl From<wit_types::LyricLine> for crate::plugin::abi::LyricLine {
    fn from(l: wit_types::LyricLine) -> Self {
        Self {
            timestamp_ms: l.timestamp_ms,
            text: l.text,
            translation: l.translation,
            words: l.words.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::LyricDocument> for PluginLyricDocument {
    fn from(d: wit_types::LyricDocument) -> Self {
        Self {
            source: d.source,
            plain: d.plain,
            lines: d.lines.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::ArtworkDescriptor> for ArtworkDescriptor {
    fn from(a: wit_types::ArtworkDescriptor) -> Self {
        ArtworkDescriptor {
            url: a.url,
            headers: a.headers.into_iter().map(Into::into).collect(),
            expires_at_ms: a.expires_at_ms,
        }
    }
}

impl From<StreamRequest> for wit_types::StreamRequest {
    fn from(r: StreamRequest) -> Self {
        wit_types::StreamRequest {
            track: r.track.into(),
            quality: r.quality,
        }
    }
}

impl From<wit_types::StreamDescriptor> for StreamDescriptor {
    fn from(d: wit_types::StreamDescriptor) -> Self {
        StreamDescriptor {
            url: d.url,
            headers: d.headers.into_iter().map(Into::into).collect(),
            codec: d.codec,
            bitrate: d.bitrate,
            sample_rate: d.sample_rate,
            channels: d.channels,
            expires_at_ms: d.expires_at_ms,
        }
    }
}

impl From<wit_types::Playlist> for PlaylistDescriptor {
    fn from(p: wit_types::Playlist) -> Self {
        PlaylistDescriptor {
            provider_id: p.provider_id,
            source_id: p.source_id,
            name: p.name,
            cover_url: p.cover_url,
            track_count: p.track_count,
            editable: p.editable,
        }
    }
}

impl From<MediaCollectionKind> for wit_types::MediaCollectionKind {
    fn from(k: MediaCollectionKind) -> Self {
        match k {
            MediaCollectionKind::Playlist => wit_types::MediaCollectionKind::Playlist,
            MediaCollectionKind::Album => wit_types::MediaCollectionKind::Album,
            MediaCollectionKind::Artist => wit_types::MediaCollectionKind::Artist,
            MediaCollectionKind::Video => wit_types::MediaCollectionKind::Video,
        }
    }
}

impl From<wit_types::MediaCollectionKind> for MediaCollectionKind {
    fn from(k: wit_types::MediaCollectionKind) -> Self {
        match k {
            wit_types::MediaCollectionKind::Playlist => MediaCollectionKind::Playlist,
            wit_types::MediaCollectionKind::Album => MediaCollectionKind::Album,
            wit_types::MediaCollectionKind::Artist => MediaCollectionKind::Artist,
            wit_types::MediaCollectionKind::Video => MediaCollectionKind::Video,
        }
    }
}

impl From<MediaCollectionRef> for wit_types::MediaCollectionRef {
    fn from(r: MediaCollectionRef) -> Self {
        wit_types::MediaCollectionRef {
            provider_id: r.provider_id,
            kind: r.kind.into(),
            source_id: r.source_id,
        }
    }
}

impl From<wit_types::MediaCollectionRef> for MediaCollectionRef {
    fn from(r: wit_types::MediaCollectionRef) -> Self {
        MediaCollectionRef {
            provider_id: r.provider_id,
            kind: r.kind.into(),
            source_id: r.source_id,
        }
    }
}

impl From<wit_types::MediaCollection> for MediaCollection {
    fn from(c: wit_types::MediaCollection) -> Self {
        MediaCollection {
            source: c.source.into(),
            title: c.title,
            subtitle: c.subtitle,
            artwork_url: c.artwork_url,
            item_count: c.item_count,
            editable: c.editable,
            saved: c.saved,
        }
    }
}

impl From<CollectionRecommendationSurface> for wit_types::CollectionRecommendationSurface {
    fn from(s: CollectionRecommendationSurface) -> Self {
        match s {
            CollectionRecommendationSurface::Home => {
                wit_types::CollectionRecommendationSurface::Home
            }
            CollectionRecommendationSurface::Daily => {
                wit_types::CollectionRecommendationSurface::Daily
            }
            CollectionRecommendationSurface::Discovery => {
                wit_types::CollectionRecommendationSurface::Discovery
            }
            CollectionRecommendationSurface::Similar => {
                wit_types::CollectionRecommendationSurface::Similar
            }
        }
    }
}

impl From<CollectionRecommendationRequest> for wit_types::CollectionRecommendationRequest {
    fn from(r: CollectionRecommendationRequest) -> Self {
        wit_types::CollectionRecommendationRequest {
            surface: r.surface.into(),
            kind: r.kind.into(),
            seed: r.seed.map(Into::into),
            limit: r.limit,
            exclude: r.exclude.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::CollectionRecommendationItem> for CollectionRecommendationItem {
    fn from(item: wit_types::CollectionRecommendationItem) -> Self {
        CollectionRecommendationItem {
            collection: item.collection.into(),
            score: item.score,
            reason: item.reason,
        }
    }
}

impl From<wit_types::UserProfile> for UserProfile {
    fn from(p: wit_types::UserProfile) -> Self {
        UserProfile {
            provider_id: p.provider_id,
            account_id: p.account_id,
            display_name: p.display_name,
            avatar_url: p.avatar_url,
            bio: p.bio,
            level: p.level,
            follower_count: p.follower_count,
            following_count: p.following_count,
            playlist_count: p.playlist_count,
            listen_count: p.listen_count,
        }
    }
}

impl From<PlaybackSignalKind> for wit_types::PlaybackSignalKind {
    fn from(k: PlaybackSignalKind) -> Self {
        match k {
            PlaybackSignalKind::Started => wit_types::PlaybackSignalKind::Started,
            PlaybackSignalKind::Completed => wit_types::PlaybackSignalKind::Completed,
            PlaybackSignalKind::Skipped => wit_types::PlaybackSignalKind::Skipped,
            PlaybackSignalKind::Liked => wit_types::PlaybackSignalKind::Liked,
            PlaybackSignalKind::Unliked => wit_types::PlaybackSignalKind::Unliked,
            PlaybackSignalKind::Disliked => wit_types::PlaybackSignalKind::Disliked,
        }
    }
}

impl From<PlaybackSignal> for wit_types::PlaybackSignal {
    fn from(s: PlaybackSignal) -> Self {
        wit_types::PlaybackSignal {
            kind: s.kind.into(),
            track: s.track.into(),
            source: s.source.map(Into::into),
            position_ms: s.position_ms,
            duration_ms: s.duration_ms,
            occurred_at_ms: s.occurred_at_ms,
        }
    }
}

impl From<RecommendationSurface> for wit_types::RecommendationSurface {
    fn from(s: RecommendationSurface) -> Self {
        match s {
            RecommendationSurface::Home => wit_types::RecommendationSurface::Home,
            RecommendationSurface::DailyMix => wit_types::RecommendationSurface::DailyMix,
            RecommendationSurface::Discovery => wit_types::RecommendationSurface::Discovery,
            RecommendationSurface::SimilarTrack => wit_types::RecommendationSurface::SimilarTrack,
            RecommendationSurface::ArtistRadio => wit_types::RecommendationSurface::ArtistRadio,
            RecommendationSurface::ContinueListening => {
                wit_types::RecommendationSurface::ContinueListening
            }
        }
    }
}

impl From<RecommendationRequest> for wit_types::RecommendationRequest {
    fn from(r: RecommendationRequest) -> Self {
        wit_types::RecommendationRequest {
            surface: r.surface.into(),
            seed: r.seed.map(Into::into),
            limit: r.limit,
            exclude: r.exclude.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::RecommendationItem> for RecommendationItem {
    fn from(i: wit_types::RecommendationItem) -> Self {
        RecommendationItem {
            track: i.track.into(),
            score: i.score,
            reason: i.reason,
        }
    }
}

impl From<RecognitionRequest> for wit_types::RecognitionRequest {
    fn from(r: RecognitionRequest) -> Self {
        wit_types::RecognitionRequest {
            algorithm: r.algorithm,
            fingerprint: r.fingerprint,
            duration_ms: r.duration_ms,
        }
    }
}

impl From<wit_types::RecognitionResult> for RecognitionResult {
    fn from(r: wit_types::RecognitionResult) -> Self {
        RecognitionResult {
            track: r.track.into(),
            confidence: r.confidence,
        }
    }
}

impl From<wit_types::ProviderDescriptor> for ProviderDescriptor {
    fn from(d: wit_types::ProviderDescriptor) -> Self {
        ProviderDescriptor {
            id: d.id,
            display_name: d.display_name,
            capabilities: d.capabilities.into_iter().map(Into::into).collect(),
            auth_methods: d.auth_methods.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_types::PluginManifest> for PluginManifest {
    fn from(m: wit_types::PluginManifest) -> Self {
        PluginManifest {
            id: m.id,
            name: m.name,
            version: m.version,
            abi_version: m.abi_version,
            description: m.description,
            homepage: m.homepage,
            providers: m.providers.into_iter().map(Into::into).collect(),
            network_domains: m.network_domains,
        }
    }
}

// ---------------------------------------------------------------------------
// Type conversions: UI <-> WIT
// ---------------------------------------------------------------------------

impl From<wit_ui::UiPage> for WireUiPage {
    fn from(p: wit_ui::UiPage) -> Self {
        Self {
            root: p.root,
            nodes: p.nodes.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<wit_ui::UiNode> for WireUiNode {
    fn from(node: wit_ui::UiNode) -> Self {
        match node.kind {
            wit_ui::NodeKind::Text(t) => WireUiNode::Text(t),
            wit_ui::NodeKind::Heading(h) => WireUiNode::Heading {
                level: h.level,
                text: h.text,
            },
            wit_ui::NodeKind::Column(c) => WireUiNode::Column(c),
            wit_ui::NodeKind::Row(r) => WireUiNode::Row(r),
            wit_ui::NodeKind::Section(s) => WireUiNode::Section {
                title: s.title,
                children: s.children,
            },
            wit_ui::NodeKind::Card(c) => WireUiNode::Card(c),
            wit_ui::NodeKind::List(l) => WireUiNode::List(l),
            wit_ui::NodeKind::Image(i) => WireUiNode::Image {
                asset: i.asset,
                alt: i.alt,
            },
            wit_ui::NodeKind::Button(b) => WireUiNode::Button {
                label: b.label,
                action_id: b.action_id,
                disabled: b.disabled,
            },
            wit_ui::NodeKind::Input(i) => WireUiNode::Input {
                field_id: i.field_id,
                value: i.value,
                placeholder: i.placeholder,
                secret: i.secret,
            },
            wit_ui::NodeKind::Select(s) => WireUiNode::Select {
                field_id: s.field_id,
                selected: s.selected,
                options: s
                    .options
                    .into_iter()
                    .map(|o| WireUiSelectOption {
                        value: o.value,
                        label: o.label,
                    })
                    .collect(),
            },
            wit_ui::NodeKind::Toggle(t) => WireUiNode::Toggle {
                field_id: t.field_id,
                label: t.label,
                value: t.value,
            },
            wit_ui::NodeKind::Progress(p) => WireUiNode::Progress {
                value_basis_points: p.value_basis_points,
                label: p.label,
            },
            wit_ui::NodeKind::Badge(b) => WireUiNode::Badge(b),
            wit_ui::NodeKind::Divider => WireUiNode::Divider,
            wit_ui::NodeKind::Spacer(s) => WireUiNode::Spacer(match s {
                wit_ui::SpacerSize::Small => WireUiSpacerSize::Small,
                wit_ui::SpacerSize::Medium => WireUiSpacerSize::Medium,
                wit_ui::SpacerSize::Large => WireUiSpacerSize::Large,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::{Arc, RwLock},
    };

    use super::*;
    use crate::plugin::{
        component::{policy::PluginEnginePolicy, registry::PluginComponentRegistry},
        host::{
            catalog::PluginHostState,
            http::PluginHttpExecutor,
            permissions::PluginPermissionState,
            runtime::{PluginHostServices, PluginRuntimeLimits},
            secrets::{MemorySecretStore, PluginSecretStore, ProtectedFileSecretStore},
        },
    };

    #[test]
    fn wasmtime_adapter_instantiates_and_calls_real_netease_plugin() {
        let wasm_file = PathBuf::from("plugins/netease/provider.wasm");
        if !wasm_file.is_file() {
            return;
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        rt.block_on(async {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!("yinqidao-adapter-test-{nonce}"));
            let plugin_dir = root.join("plugins").join("io.yinqidao.netease");
            fs::create_dir_all(&plugin_dir).expect("create test plugin dir");

            let toml_file = PathBuf::from("plugins/netease/plugin.toml");
            fs::copy(&toml_file, plugin_dir.join("plugin.toml")).expect("copy toml");
            fs::copy(&wasm_file, plugin_dir.join("provider.wasm")).expect("copy wasm");

            let host_state = Arc::new(RwLock::new(PluginHostState::load(&root)));
            let catalog = host_state.read().unwrap().catalog().clone();
            let permissions = Arc::new(RwLock::new(PluginPermissionState::load(&root, &catalog)));
            let config_dir = std::env::var("APPDATA")
                .map(|a| PathBuf::from(a).join("YinQiDao").join("config"))
                .ok();
            let secrets: Arc<dyn PluginSecretStore> = if let Some(dir) = &config_dir {
                if let Ok(store) = ProtectedFileSecretStore::new(dir.join("plugin-secrets.dat")) {
                    Arc::new(store)
                } else {
                    Arc::new(MemorySecretStore::default())
                }
            } else {
                Arc::new(MemorySecretStore::default())
            };
            let services = Arc::new(PluginHostServices::new(
                catalog,
                permissions,
                PluginHttpExecutor::default(),
                secrets,
                PluginRuntimeLimits::default(),
            ));
            let policy = PluginEnginePolicy::default();
            let wasmtime_runtime =
                super::super::wasmtime::PluginWasmtimeRuntime::new(services, policy).unwrap();
            let wasmtime_runtime = Arc::new(wasmtime_runtime);

            let components = Arc::new(PluginComponentRegistry::new(
                &root.join("cache"),
                Default::default(),
            ));
            let adapter =
                WasmtimeComponentAdapter::new(wasmtime_runtime, components, host_state, 4);

            // 1. Test manifest call
            let manifest = adapter
                .manifest("io.yinqidao.netease")
                .await
                .expect("manifest call");
            assert_eq!(manifest.id, "io.yinqidao.netease");
            assert_eq!(manifest.name, "网易云音乐");

            // 2. Test auth_begin call
            let challenge = adapter
                .auth_begin("io.yinqidao.netease", "netease", AuthMethod::CookieImport)
                .await
                .expect("auth_begin call");
            assert_eq!(challenge.challenge_id, "cookie-import-v1");
            assert_eq!(challenge.kind, AuthChallengeKind::Form);

            // 3. Test recommendations with real account if available
            let accounts_file = config_dir.map(|d| d.join("plugin-accounts.json"));
            if let Some(acc_path) = accounts_file {
                if acc_path.is_file() {
                    let content = fs::read_to_string(&acc_path).unwrap_or_default();
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(acc) = val
                            .pointer("/accounts/0/account_id")
                            .and_then(|v| v.as_str())
                        {
                            let req = RecommendationRequest {
                                surface: RecommendationSurface::DailyMix,
                                limit: 12,
                                seed: None,
                                exclude: vec![],
                            };
                            let rec_res = adapter
                                .recommendations("io.yinqidao.netease", "netease", acc, &req)
                                .await;
                            assert!(
                                rec_res.is_ok(),
                                "DailyMix recommendations should succeed: {rec_res:?}"
                            );
                            let rec_items = rec_res.unwrap();
                            assert!(
                                !rec_items.is_empty(),
                                "DailyMix should return recommendations"
                            );

                            let home_req = RecommendationRequest {
                                surface: RecommendationSurface::Home,
                                limit: 12,
                                seed: None,
                                exclude: vec![],
                            };
                            let home_res = adapter
                                .recommendations("io.yinqidao.netease", "netease", acc, &home_req)
                                .await;
                            assert!(
                                home_res.is_ok(),
                                "Home recommendations should succeed: {home_res:?}"
                            );

                            let col_req = CollectionRecommendationRequest {
                                kind: MediaCollectionKind::Playlist,
                                surface: CollectionRecommendationSurface::Daily,
                                limit: 8,
                                seed: None,
                                exclude: vec![],
                            };
                            let col_res = adapter
                                .collection_recommendations(
                                    "io.yinqidao.netease",
                                    "netease",
                                    acc,
                                    &col_req,
                                )
                                .await;
                            assert!(
                                col_res.is_ok(),
                                "Collection recommendations should succeed: {col_res:?}"
                            );

                            let pls_res = adapter
                                .playlists("io.yinqidao.netease", "netease", acc)
                                .await;
                            assert!(pls_res.is_ok(), "Playlists should succeed: {pls_res:?}");

                            // Test stream
                            let first_track = &rec_items[0].track;
                            let stream_req = StreamRequest {
                                track: first_track.source.clone(),
                                quality: None,
                            };
                            let stream_res = adapter
                                .stream("io.yinqidao.netease", "netease", acc, &stream_req)
                                .await;
                            assert!(
                                stream_res.is_ok(),
                                "Stream for track {} should succeed: {stream_res:?}",
                                first_track.title
                            );
                            let desc = stream_res.unwrap();
                            assert!(!desc.url.is_empty(), "Stream url should not be empty");

                            // Test search
                            let search_res = adapter
                                .search("io.yinqidao.netease", "netease", Some(acc), "周杰伦", 10)
                                .await;
                            assert!(search_res.is_ok(), "Search should succeed: {search_res:?}");
                            let search_tracks = search_res.unwrap();
                            assert!(!search_tracks.is_empty(), "Search should return tracks");
                        }
                    }
                }
            }

            let _ = fs::remove_dir_all(root);
        });
    }
}
