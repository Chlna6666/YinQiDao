use std::{
    path::Path,
    sync::atomic::{AtomicI64, Ordering},
};

use anyhow::{Result, anyhow, bail};

use crate::model::{Track, TrackData, TrackId};

use super::{
    abi::{PluginRoute, RemoteTrack, SourceTrackRef, StreamRequest},
    frontend::{PluginCallFailure, PluginServiceFrontend, PluginSingleResult},
    host::{runtime, stream_cache, stream_cache::PluginMaterializedStream},
};

const MAX_REMOTE_TRACK_ARTISTS: usize = 128;
const MAX_REMOTE_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_REMOTE_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;

// Local Library ids are SQLite INTEGER PRIMARY KEY AUTOINCREMENT values and therefore occupy the
// positive namespace. Remote materializations are process-local only and use negative ids so they
// can enter the existing player queue without being persisted or confused with a Library row.
static NEXT_REMOTE_TRACK_ID: AtomicI64 = AtomicI64::new(-1);

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

    fn into_prepared_track(self, remote: &RemoteTrack) -> Result<PluginPreparedTrack> {
        validate_remote_playback_track(remote)?;
        if self.source != remote.source {
            bail!("远程 Track source 与已物化 Stream source 不一致");
        }
        if self.route.provider_id != remote.source.provider_id {
            bail!("远程 Track provider 与已物化 Stream route 不一致");
        }

        let track_id = allocate_remote_track_id()?;
        let route = self.route.clone();
        let source = self.source.clone();
        let path = self.path().to_path_buf();
        let codec = self
            .codec
            .as_deref()
            .map(str::trim)
            .filter(|codec| !codec.is_empty())
            .unwrap_or("remote")
            .to_owned();
        let sample_rate = self.sample_rate.unwrap_or(0);
        let channels = self.channels.unwrap_or(0);

        let track = Track::new(TrackData {
            id: track_id,
            path,
            title: remote.title.clone(),
            artist: remote.artists.join(", "),
            album: remote.album.clone(),
            year: None,
            genre: None,
            duration_ms: remote.duration_ms.unwrap_or(0),
            codec,
            sample_rate,
            channels,
            // A plugin cover URL is not a trusted local artwork cache key. Artwork continues through
            // the authenticated Host artwork façade instead of teaching local artwork code about URL.
            artwork_key: None,
        })
        .with_playback_backing(self);

        Ok(PluginPreparedTrack {
            track,
            route,
            source,
        })
    }
}

/// Remote track ready for the existing local-path decoder/preloader pipeline.
///
/// `track` contains the PluginPlaybackSource as an opaque backing guard, so every Track clone held by
/// the engine, snapshot or preloader keeps the cache lease alive. Route/source are duplicated here as
/// explicit application provenance for future PlaybackEvents and account-aware UI state.
#[derive(Clone, Debug)]
pub struct PluginPreparedTrack {
    track: Track,
    route: PluginRoute,
    source: SourceTrackRef,
}

impl PluginPreparedTrack {
    pub fn track(&self) -> &Track {
        &self.track
    }

    pub fn route(&self) -> &PluginRoute {
        &self.route
    }

    pub fn source(&self) -> &SourceTrackRef {
        &self.source
    }

    pub fn into_track(self) -> Track {
        self.track
    }
}

impl PluginServiceFrontend {
    /// Resolve one authenticated stream descriptor and materialize it behind the Host networking
    /// boundary before returning anything playback-facing.
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

    /// Complete the safe remote playback bridge up to the existing player boundary:
    /// authenticated provider route -> StreamDescriptor -> Host cache -> local seekable Track.
    ///
    /// No URL/header reaches Track or DecoderStream. The returned Track uses a process-local negative
    /// id and must not be persisted into the local Library database.
    pub async fn prepare_remote_track_for_route(
        &self,
        metadata_route: &PluginRoute,
        remote: &RemoteTrack,
        quality: Option<&str>,
    ) -> Result<PluginSingleResult<PluginPreparedTrack>> {
        validate_remote_playback_track(remote)?;
        let request = StreamRequest {
            track: remote.source.clone(),
            quality: quality.map(str::to_owned),
        };
        let materialized = self
            .materialize_stream_for_route(metadata_route, &request)
            .await?;
        let PluginSingleResult {
            value,
            route,
            plan,
            mut failures,
            client_ready,
        } = materialized;

        let Some(source) = value else {
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures,
                client_ready,
            });
        };
        let source_route = source.route.clone();
        match source.into_prepared_track(remote) {
            Ok(prepared) => Ok(PluginSingleResult {
                value: Some(prepared),
                route: Some(source_route),
                plan,
                failures,
                client_ready,
            }),
            Err(error) => {
                failures.push(PluginCallFailure {
                    route: source_route,
                    error: format!("构造远程播放 Track 失败: {error:#}"),
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

fn allocate_remote_track_id() -> Result<TrackId> {
    let mut current = NEXT_REMOTE_TRACK_ID.load(Ordering::Relaxed);
    loop {
        if current >= 0 || current == i64::MIN {
            bail!("远程临时 TrackId 命名空间已耗尽");
        }
        match NEXT_REMOTE_TRACK_ID.compare_exchange_weak(
            current,
            current - 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Ok(current),
            Err(actual) => current = actual,
        }
    }
}

fn validate_remote_playback_track(remote: &RemoteTrack) -> Result<()> {
    if !remote.playable {
        bail!("远程 Track 标记为不可播放");
    }
    validate_remote_text(&remote.title, "title", false)?;
    validate_remote_text(&remote.album, "album", true)?;
    if remote.artists.len() > MAX_REMOTE_TRACK_ARTISTS {
        bail!("远程 Track artists 数量超过限制");
    }
    let mut total_text_bytes = remote.title.len().saturating_add(remote.album.len());
    for artist in &remote.artists {
        validate_remote_text(artist, "artist", false)?;
        total_text_bytes = total_text_bytes.saturating_add(artist.len());
    }
    if total_text_bytes > MAX_REMOTE_TRACK_TEXT_BYTES {
        bail!("远程 Track 文本总大小超过限制");
    }
    if remote
        .duration_ms
        .is_some_and(|duration| duration > MAX_REMOTE_TRACK_DURATION_MS)
    {
        bail!("远程 Track duration 超过限制");
    }
    Ok(())
}

fn validate_remote_text(value: &str, field: &str, allow_empty: bool) -> Result<()> {
    if (!allow_empty && value.trim().is_empty())
        || value.len() > MAX_REMOTE_TRACK_TEXT_BYTES
        || value.contains('\0')
    {
        bail!("远程 Track {field} 非法");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_track_ids_use_unique_negative_namespace() {
        let first = allocate_remote_track_id().expect("first");
        let second = allocate_remote_track_id().expect("second");
        assert!(first < 0);
        assert!(second < 0);
        assert!(second < first);
    }

    #[test]
    fn remote_playback_validation_rejects_unplayable_or_unbounded_metadata() {
        let mut track = RemoteTrack {
            source: SourceTrackRef {
                provider_id: "provider".into(),
                source_id: "track-1".into(),
            },
            title: "Song".into(),
            artists: vec!["Artist".into()],
            album: "Album".into(),
            duration_ms: Some(180_000),
            isrc: None,
            cover_url: None,
            playable: true,
            explicit: false,
        };
        assert!(validate_remote_playback_track(&track).is_ok());

        track.playable = false;
        assert!(validate_remote_playback_track(&track).is_err());
        track.playable = true;
        track.title = "x".repeat(MAX_REMOTE_TRACK_TEXT_BYTES + 1);
        assert!(validate_remote_playback_track(&track).is_err());
    }
}
