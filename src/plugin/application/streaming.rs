use std::{
    fs,
    path::Path,
    sync::atomic::{AtomicI64, Ordering},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, anyhow, bail};
use md5::{Digest, Md5};

use crate::{
    audio::AudioEngine,
    model::{Track, TrackData, TrackId},
};

use super::{
    abi::{PluginRoute, RemoteTrack, SourceTrackRef, StreamRequest},
    frontend::{PluginCallFailure, PluginServiceFrontend, PluginSingleResult},
    host::{
        runtime::{self, PluginCallKey},
        stream_cache,
        stream_cache::PluginMaterializedStream,
    },
    runtime_ports,
};

const MAX_REMOTE_TRACK_ARTISTS: usize = 128;
const MAX_REMOTE_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_REMOTE_TRACK_DURATION_MS: u64 = 24 * 60 * 60 * 1_000;
const PLUGIN_PACKAGE_FILE: &str = "plugin.toml";
const PACKAGE_REVISION_DOMAIN: &[u8] = b"YINQIDAO-PLUGIN-PACKAGE-REVISION-V1\0";

// Local Library ids are SQLite INTEGER PRIMARY KEY AUTOINCREMENT values and therefore occupy the
// positive namespace. Remote materializations are process-local only and use negative ids so they
// can enter the existing player queue without being persisted or confused with a Library row.
static NEXT_REMOTE_TRACK_ID: AtomicI64 = AtomicI64::new(-1);

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageFileStamp {
    len: u64,
    modified_ns: u128,
}

/// Stable-enough Host cache invalidation stamp for one installed package.
///
/// This is deliberately not a trust/authenticity digest. Package admission and runtime authorization
/// remain the Host's responsibility. The stamp only prevents a persisted audio cache entry produced
/// by an older package revision from being reused after a normal package-manager replacement or app
/// restart. It mirrors the component registry's len/mtime generation idea while staying in the Host
/// application boundary and without exposing Wasmtime types.
#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginPackageRevision {
    manifest_version: String,
    manifest: PackageFileStamp,
    component: PackageFileStamp,
}

impl PluginPackageRevision {
    fn cache_scope(&self) -> String {
        let mut hasher = Md5::new();
        hasher.update(PACKAGE_REVISION_DOMAIN);
        hash_text(&mut hasher, &self.manifest_version);
        hasher.update(self.manifest.len.to_le_bytes());
        hasher.update(self.manifest.modified_ns.to_le_bytes());
        hasher.update(self.component.len.to_le_bytes());
        hasher.update(self.component.modified_ns.to_le_bytes());
        format!("{:x}", hasher.finalize())
    }
}

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

/// Exact provenance for one remote track accepted by the player transport.
///
/// The route is request-scoped and includes the authenticated account id that actually resolved the
/// stream. Callers can retain this value for future PlaybackEvents without guessing a default account
/// or reconstructing provider identity from the local cache path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStartedPlayback {
    track_id: TrackId,
    route: PluginRoute,
    source: SourceTrackRef,
}

impl PluginStartedPlayback {
    pub fn track_id(&self) -> TrackId {
        self.track_id
    }

    pub fn route(&self) -> &PluginRoute {
        &self.route
    }

    pub fn source(&self) -> &SourceTrackRef {
        &self.source
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
        // The process-local mutation generation closes the in-flight hot-update race around the guest
        // descriptor call. The persistent package revision below separately scopes disk-cache reuse
        // across process restarts.
        let package_generation = runtime_ports::package_mutation_generation();
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

        if runtime_ports::package_mutation_generation() != package_generation {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: "插件在 Stream descriptor 解析期间已更新，旧结果已丢弃".into(),
            });
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures,
                client_ready,
            });
        }

        let runtime = runtime::global().ok_or_else(|| anyhow!("插件 Host runtime 尚未初始化"))?;
        let package_revision = match current_package_revision(runtime.as_ref(), &route) {
            Ok(revision) => revision,
            Err(error) => {
                failures.push(PluginCallFailure {
                    route: route.clone(),
                    error: format!("Stream Host package revision 重检失败: {error:#}"),
                });
                return Ok(PluginSingleResult {
                    value: None,
                    route: None,
                    plan,
                    failures,
                    client_ready,
                });
            }
        };
        if runtime_ports::package_mutation_generation() != package_generation {
            failures.push(PluginCallFailure {
                route: route.clone(),
                error: "插件在 Stream cache scope 建立期间已更新，旧结果已丢弃".into(),
            });
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures,
                client_ready,
            });
        }

        // Do not mutate the guest-visible request or the exact account route. Only a private clone is
        // decorated so `stream_cache::cache_identity` sees a new variant after package replacement.
        let cache_request = cache_scoped_request(request, &package_revision);
        let cache = stream_cache::global().ok_or_else(|| anyhow!("插件 Stream cache 尚未初始化"))?;
        match cache
            .materialize(runtime.as_ref(), &route, &cache_request, &descriptor)
            .await
        {
            Ok(materialized) => {
                let revision_check = if runtime_ports::package_mutation_generation()
                    != package_generation
                {
                    Err(anyhow!("插件在 Stream materialize 期间已更新"))
                } else {
                    current_package_revision(runtime.as_ref(), &route).and_then(|current| {
                        if current == package_revision {
                            Ok(())
                        } else {
                            bail!("插件 package revision 在 Stream materialize 期间发生变化")
                        }
                    })
                };
                if let Err(error) = revision_check {
                    // Dropping materialized releases its cache lease. The completed old-revision
                    // bucket may remain until normal GC, but the new package scope can never hit it.
                    drop(materialized);
                    failures.push(PluginCallFailure {
                        route: route.clone(),
                        error: format!("Stream package revision 提交校验失败: {error:#}"),
                    });
                    return Ok(PluginSingleResult {
                        value: None,
                        route: None,
                        plan,
                        failures,
                        client_ready,
                    });
                }

                Ok(PluginSingleResult {
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
                })
            }
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

    /// Resolve, securely materialize and atomically hand one remote track to the existing player.
    ///
    /// This is the first application API that closes the complete remote playback chain. The player
    /// receives only a Host-owned local seekable path through a negative process-local Track id; the
    /// signed URL and headers never enter audio code. `PluginStartedPlayback` preserves the exact
    /// plugin/provider/account/source provenance selected by routing for later PlaybackEvents.
    pub async fn play_remote_track_for_route(
        &self,
        engine: &AudioEngine,
        metadata_route: &PluginRoute,
        remote: &RemoteTrack,
        quality: Option<&str>,
    ) -> Result<PluginSingleResult<PluginStartedPlayback>> {
        let prepared = self
            .prepare_remote_track_for_route(metadata_route, remote, quality)
            .await?;
        let PluginSingleResult {
            value,
            route: _,
            plan,
            mut failures,
            client_ready,
        } = prepared;

        let Some(prepared) = value else {
            return Ok(PluginSingleResult {
                value: None,
                route: None,
                plan,
                failures,
                client_ready,
            });
        };

        let started = PluginStartedPlayback {
            track_id: prepared.track.id,
            route: prepared.route.clone(),
            source: prepared.source.clone(),
        };
        let started_route = started.route.clone();
        if engine.try_play_transient_track(prepared.into_track()) {
            Ok(PluginSingleResult {
                value: Some(started),
                route: Some(started_route),
                plan,
                failures,
                client_ready,
            })
        } else {
            failures.push(PluginCallFailure {
                route: started_route,
                error: "远程 Track 已物化，但播放器控制队列拒绝 transient playback 请求".into(),
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

fn current_package_revision(
    runtime: &runtime::PluginHostServices,
    route: &PluginRoute,
) -> Result<PluginPackageRevision> {
    // This is also the authority gate for cache hits: validate_call_key() checks current package
    // enablement and provider presence before we look at any persisted materialization.
    runtime.route_health(&PluginCallKey::provider(
        &route.plugin_id,
        &route.provider_id,
    ))?;
    let catalog = runtime.catalog_snapshot()?;
    let plugin = catalog
        .plugin(&route.plugin_id)
        .cloned()
        .ok_or_else(|| anyhow!("未安装插件: {}", route.plugin_id))?;
    if plugin.provider(&route.provider_id).is_none() {
        bail!(
            "插件 {} 未声明 provider {}",
            route.plugin_id,
            route.provider_id
        );
    }

    Ok(PluginPackageRevision {
        manifest_version: plugin.manifest.version.clone(),
        manifest: regular_file_stamp(&plugin.package_dir.join(PLUGIN_PACKAGE_FILE), "plugin.toml")?,
        component: regular_file_stamp(&plugin.component_path, "Component")?,
    })
}

fn regular_file_stamp(path: &Path, label: &str) -> Result<PackageFileStamp> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("读取插件 {label} revision metadata 失败: {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("插件 {label} revision 输入不是普通文件: {}", path.display());
    }
    let modified = metadata
        .modified()
        .with_context(|| format!("读取插件 {label} mtime 失败: {}", path.display()))?;
    let modified_ns = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("插件 {label} mtime 早于 UNIX_EPOCH: {}", path.display()))?
        .as_nanos();
    Ok(PackageFileStamp {
        len: metadata.len(),
        modified_ns,
    })
}

fn cache_scoped_request(
    request: &StreamRequest,
    revision: &PluginPackageRevision,
) -> StreamRequest {
    let base_quality = request.quality.as_deref().unwrap_or("");
    let quality_kind = u8::from(request.quality.is_some());
    let private_quality_scope = format!(
        "YQD-HOST-CACHE-V1|{quality_kind}|{}|{base_quality}|{}",
        base_quality.len(),
        revision.cache_scope()
    );
    StreamRequest {
        track: request.track.clone(),
        quality: Some(private_quality_scope),
    }
}

fn hash_text(hasher: &mut Md5, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
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

    fn revision(version: &str, manifest_len: u64, component_len: u64) -> PluginPackageRevision {
        PluginPackageRevision {
            manifest_version: version.into(),
            manifest: PackageFileStamp {
                len: manifest_len,
                modified_ns: 10,
            },
            component: PackageFileStamp {
                len: component_len,
                modified_ns: 20,
            },
        }
    }

    #[test]
    fn remote_track_ids_use_unique_negative_namespace() {
        let first = allocate_remote_track_id().expect("first");
        let second = allocate_remote_track_id().expect("second");
        assert!(first < 0);
        assert!(second < 0);
        assert!(second < first);
    }

    #[test]
    fn package_revision_changes_private_cache_scope() {
        let first = revision("1.0.0", 100, 1_000);
        let second = revision("1.0.0", 100, 1_001);
        assert_ne!(first.cache_scope(), second.cache_scope());

        let request = StreamRequest {
            track: SourceTrackRef {
                provider_id: "provider".into(),
                source_id: "track-1".into(),
            },
            quality: Some("lossless".into()),
        };
        let scoped = cache_scoped_request(&request, &first);
        assert_eq!(scoped.track, request.track);
        assert_ne!(scoped.quality, request.quality);
        assert!(scoped.quality.as_deref().is_some_and(|value| value.contains("lossless")));
    }

    #[test]
    fn package_revision_scope_distinguishes_none_and_empty_quality() {
        let revision = revision("1.0.0", 100, 1_000);
        let base_track = SourceTrackRef {
            provider_id: "provider".into(),
            source_id: "track-1".into(),
        };
        let none = cache_scoped_request(
            &StreamRequest {
                track: base_track.clone(),
                quality: None,
            },
            &revision,
        );
        let empty = cache_scoped_request(
            &StreamRequest {
                track: base_track,
                quality: Some(String::new()),
            },
            &revision,
        );
        assert_ne!(none.quality, empty.quality);
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
