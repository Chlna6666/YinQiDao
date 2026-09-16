use std::path::Path;

use anyhow::{Result, anyhow};

use super::{
    abi::{PluginRoute, SourceTrackRef, StreamRequest},
    frontend::{PluginCallFailure, PluginServiceFrontend, PluginSingleResult},
    host::{runtime, stream_cache, stream_cache::PluginMaterializedStream},
};

/// Application-facing local playback source produced from a plugin stream descriptor.
///
/// The signed URL and request headers deliberately never cross this boundary. Callers receive only
/// the exact provider/account provenance plus a Host-owned local path. The private materialization
/// keeps the stream-cache lease alive; callers must retain this object for the complete decoder
/// lifetime rather than cloning the path and dropping the source immediately.
#[derive(Clone, Debug)]
pub struct PluginPlaybackSource {
    route: PluginRoute,
    source: SourceTrackRef,
    quality: Option<String>,
    codec: Option<String>,
    bitrate: Option<u32>,
    sample_rate: Option<u32>,
    channels: Option<u16>,
    materialized: PluginMaterializedStream,
}

impl PluginPlaybackSource {
    pub fn route(&self) -> &PluginRoute {
        &self.route
    }

    pub fn source(&self) -> &SourceTrackRef {
        &self.source
    }

    pub fn quality(&self) -> Option<&str> {
        self.quality.as_deref()
    }

    pub fn codec(&self) -> Option<&str> {
        self.codec.as_deref()
    }

    pub fn bitrate(&self) -> Option<u32> {
        self.bitrate
    }

    pub fn sample_rate(&self) -> Option<u32> {
        self.sample_rate
    }

    pub fn channels(&self) -> Option<u16> {
        self.channels
    }

    pub fn path(&self) -> &Path {
        &self.materialized.path
    }

    pub fn bytes(&self) -> u64 {
        self.materialized.bytes
    }

    pub fn cache_hit(&self) -> bool {
        self.materialized.cache_hit
    }
}

impl PluginServiceFrontend {
    /// Resolve one authenticated stream descriptor and materialize it behind the Host networking
    /// boundary before returning anything playback-facing.
    ///
    /// This method intentionally does not manufacture a normal Library `Track`: that model currently
    /// has no provider/account provenance or backing-lease slot. The eventual remote playback bridge
    /// must retain `PluginPlaybackSource` while `DecoderStream` is open, preserving both exact account
    /// attribution and cache lifetime without teaching the audio callback about plugins or network I/O.
    pub async fn materialize_stream_for_route(
        &self,
        metadata_route: &PluginRoute,
        request: &StreamRequest,
    ) -> Result<PluginSingleResult<PluginPlaybackSource>> {
        let resolved = self.stream_for_route(metadata_route, request).await?;
        let PluginSingleResult {
            value,
            route,
            plan,
            mut failures,
            client_ready,
        } = resolved;

        let Some(descriptor) = value else {
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures,
                client_ready,
            });
        };
        let route = route.ok_or_else(|| {
            anyhow!("插件 stream descriptor 缺少对应 route，拒绝 materialize")
        })?;

        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let cache = stream_cache::global().ok_or_else(|| anyhow!("插件 Stream cache 尚未初始化"))?;
        match cache
            .materialize(runtime.as_ref(), &route, request, &descriptor)
            .await
        {
            Ok(materialized) => Ok(PluginSingleResult {
                value: Some(PluginPlaybackSource {
                    route: route.clone(),
                    source: request.track.clone(),
                    quality: request.quality.clone(),
                    codec: descriptor.codec,
                    bitrate: descriptor.bitrate,
                    sample_rate: descriptor.sample_rate,
                    channels: descriptor.channels,
                    materialized,
                }),
                route: Some(route),
                plan,
                failures,
                client_ready,
            }),
            Err(error) => {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("Stream Host materialize 失败: {error:#}"),
                });
                Ok(PluginSingleResult {
                    value: None,
                    route: None,
                    plan,
                    failures,
                    client_ready,
                })
            }
        }
    }
}
