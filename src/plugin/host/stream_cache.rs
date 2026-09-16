use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use md5::{Digest, Md5};
use tokio::sync::{Semaphore, mpsc};

use crate::plugins::{KeyValue, PluginRoute, StreamDescriptor, StreamRequest};

use super::{
    http::{
        PluginHttpExecutor, PluginHttpRequest, PluginHttpStreamBodyLimits,
        PluginHttpStreamResponse,
    },
    permissions,
    runtime::{PluginCallKey, PluginHostServices},
    stream_cache_gc::{self, PluginStreamCacheLease},
};

const STREAM_CACHE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_RANGE_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_MAX_STREAM_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_MAX_CONCURRENT_DOWNLOADS: usize = 2;
const DEFAULT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_CACHE_IDENTITY_BYTES: usize = 32 * 1024;
const WRITER_QUEUE_DEPTH: usize = 4;

static PLUGIN_STREAM_CACHE: OnceLock<Arc<PluginStreamCache>> = OnceLock::new();
static TEMP_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct PluginMaterializedStream {
    pub path: PathBuf,
    pub bytes: u64,
    pub cache_hit: bool,
    /// Keep the finalized bucket pinned for as long as the caller owns this materialization.
    /// The lease is intentionally opaque outside the Host cache implementation.
    _lease: Arc<PluginStreamCacheLease>,
}

#[derive(Clone, Debug)]
struct CachedStreamEntry {
    path: PathBuf,
    bytes: u64,
    cache_hit: bool,
}

#[derive(Clone, Debug)]
pub struct PluginStreamCache {
    root: PathBuf,
    range_chunk_bytes: u64,
    max_stream_bytes: u64,
    cache_ttl: Duration,
    http: PluginHttpExecutor,
    download_gate: Arc<Semaphore>,
}

impl PluginStreamCache {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            range_chunk_bytes: DEFAULT_RANGE_CHUNK_BYTES,
            max_stream_bytes: DEFAULT_MAX_STREAM_BYTES,
            cache_ttl: DEFAULT_CACHE_TTL,
            http: PluginHttpExecutor::default(),
            download_gate: Arc::new(Semaphore::new(DEFAULT_MAX_CONCURRENT_DOWNLOADS)),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Materialize one already validated StreamDescriptor into a Host-owned, seekable local file.
    ///
    /// Network reads stay on the async control path. Response chunks are pushed through a bounded
    /// channel into one blocking writer, so the complete track is never collected into a Vec and
    /// neither Tokio workers nor the realtime audio callback perform filesystem I/O. The dedicated
    /// Host HTTP streaming path shares permission, DNS pinning, redirect, proxy and header policy
    /// with ordinary plugin HTTP without expanding the ordinary 8 MiB response budget.
    pub async fn materialize(
        &self,
        runtime: &PluginHostServices,
        route: &PluginRoute,
        request: &StreamRequest,
        descriptor: &StreamDescriptor,
    ) -> Result<PluginMaterializedStream> {
        let identity = cache_identity(route, request, descriptor)?;
        let locator = locator_hex(&identity);
        let audio_name = format!("audio.{}", codec_extension(descriptor.codec.as_deref()));
        let bucket = self.root.join(&locator);

        if let Some(hit) = self
            .lookup_cached(&bucket, &identity, &audio_name)
            .await?
        {
            return self.pin_entry(hit);
        }

        // Bound disk/network pressure independently from provider guest-call concurrency. Waiting
        // callers re-check the cache after acquiring the permit so a just-finished peer avoids a
        // duplicate transfer.
        let _download_permit = self
            .download_gate
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| anyhow!("插件 stream download gate 已关闭"))?;
        if let Some(hit) = self
            .lookup_cached(&bucket, &identity, &audio_name)
            .await?
        {
            return self.pin_entry(hit);
        }

        // Validate the route before allocating a temp writer. `download_stream` repeats this gate,
        // live catalog lookup and permission lookup before every Range request so disabling a plugin,
        // removing a provider/domain or revoking network grants stops subsequent chunks fail-closed.
        runtime.route_health(&PluginCallKey::provider(
            &route.plugin_id,
            &route.provider_id,
        ))?;

        let nonce = TEMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let temp_dir = self.root.join(format!(
            ".{locator}.tmp-{}-{nonce}",
            std::process::id()
        ));
        let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(WRITER_QUEUE_DEPTH);

        let writer_temp_dir = temp_dir.clone();
        let writer_identity = identity.clone();
        let writer_audio_name = audio_name.clone();
        let writer_max_stream_bytes = self.max_stream_bytes;
        let writer = tokio::task::spawn_blocking(move || {
            write_temp_entry(
                &writer_temp_dir,
                &writer_identity,
                &writer_audio_name,
                writer_max_stream_bytes,
                writer_rx,
            )
        });

        let download_result = match tokio::time::timeout(
            DEFAULT_DOWNLOAD_TIMEOUT,
            download_stream(
                &self.http,
                runtime,
                route,
                descriptor,
                self.range_chunk_bytes,
                self.max_stream_bytes,
                &writer_tx,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(anyhow!(
                "插件 stream 下载超过总时限 {} ms",
                DEFAULT_DOWNLOAD_TIMEOUT.as_millis()
            )),
        };
        drop(writer_tx);

        let writer_result = writer
            .await
            .context("等待插件 stream cache 写入任务失败")?;

        let downloaded = match download_result {
            Ok(downloaded) => downloaded,
            Err(error) => {
                let _ = writer_result;
                cleanup_temp_dir(temp_dir).await;
                return Err(error);
            }
        };
        let written = match writer_result {
            Ok(written) => written,
            Err(error) => {
                cleanup_temp_dir(temp_dir).await;
                return Err(error);
            }
        };
        if downloaded == 0 || written != downloaded {
            cleanup_temp_dir(temp_dir).await;
            bail!(
                "插件 stream cache 写入长度不一致: downloaded={downloaded}, written={written}"
            );
        }

        let commit_temp_dir = temp_dir.clone();
        let commit_bucket = bucket.clone();
        let commit_identity = identity.clone();
        let commit_audio_name = audio_name.clone();
        let commit_max_stream_bytes = self.max_stream_bytes;
        let commit_cache_ttl = self.cache_ttl;
        let committed = tokio::task::spawn_blocking(move || {
            commit_temp_entry(
                &commit_temp_dir,
                &commit_bucket,
                &commit_identity,
                &commit_audio_name,
                commit_max_stream_bytes,
                commit_cache_ttl,
            )
        })
        .await
        .context("等待插件 stream cache 提交任务失败")??;

        // Pin before capacity GC so the file just handed to the decoder cannot be removed between
        // materialization and open(). Older unpinned buckets remain eligible for eviction.
        let materialized = self.pin_entry(committed)?;
        let gc_root = self.root.clone();
        let gc_stats = tokio::task::spawn_blocking(move || {
            stream_cache_gc::prune(
                &gc_root,
                stream_cache_gc::PluginStreamCacheGcPolicy::default(),
            )
        })
        .await
        .context("等待插件 stream cache GC 任务失败")??;
        if gc_stats.over_budget_bytes > 0 {
            tracing::warn!(
                over_budget_bytes = gc_stats.over_budget_bytes,
                pinned_buckets = gc_stats.pinned_buckets,
                "插件 stream cache 因活跃 lease 暂时超过容量预算"
            );
        }
        Ok(materialized)
    }

    async fn lookup_cached(
        &self,
        bucket: &Path,
        identity: &[u8],
        audio_name: &str,
    ) -> Result<Option<CachedStreamEntry>> {
        let lookup_bucket = bucket.to_path_buf();
        let lookup_identity = identity.to_vec();
        let lookup_audio_name = audio_name.to_owned();
        let max_stream_bytes = self.max_stream_bytes;
        let cache_ttl = self.cache_ttl;
        tokio::task::spawn_blocking(move || {
            cached_entry(
                &lookup_bucket,
                &lookup_identity,
                &lookup_audio_name,
                max_stream_bytes,
                cache_ttl,
            )
        })
        .await
        .context("等待插件 stream cache 查询任务失败")?
    }

    fn pin_entry(&self, entry: CachedStreamEntry) -> Result<PluginMaterializedStream> {
        let lease = stream_cache_gc::pin_materialized_path(&self.root, &entry.path)?;
        Ok(PluginMaterializedStream {
            path: entry.path,
            bytes: entry.bytes,
            cache_hit: entry.cache_hit,
            _lease: lease,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_stream(
    http: &PluginHttpExecutor,
    runtime: &PluginHostServices,
    route: &PluginRoute,
    descriptor: &StreamDescriptor,
    range_chunk_bytes: u64,
    max_stream_bytes: u64,
    writer: &mpsc::Sender<Vec<u8>>,
) -> Result<u64> {
    if range_chunk_bytes == 0 || range_chunk_bytes > 8 * 1024 * 1024 {
        bail!("插件 stream range chunk 配置非法");
    }
    if max_stream_bytes == 0 {
        bail!("插件 stream 最大文件大小配置非法");
    }

    let mut offset = 0u64;
    let mut expected_total = None;
    let mut if_range = None;

    loop {
        if descriptor
            .expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms <= runtime.now_ms())
        {
            bail!("插件 stream descriptor 在下载完成前已过期");
        }
        if offset >= max_stream_bytes {
            bail!("插件 stream 超过 Host 最大文件大小限制");
        }

        // Re-check live Host authority before every network chunk. The active request itself cannot
        // be retroactively cancelled by a grant update, but the very next Range must observe plugin
        // disable, package/provider changes and permission revocation instead of using a stale grant.
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
        let permission_state = permissions::global()
            .ok_or_else(|| anyhow!("插件权限状态尚未初始化"))?;
        let grant = permission_state
            .read()
            .map_err(|error| anyhow!("插件权限状态锁已损坏: {error}"))?
            .grant_for(&route.plugin_id)
            .cloned()
            .ok_or_else(|| anyhow!("插件 {} 尚未获得网络权限", route.plugin_id))?;

        let requested_end = offset
            .saturating_add(range_chunk_bytes.saturating_sub(1))
            .min(max_stream_bytes - 1);
        let requested_bytes = requested_end
            .checked_sub(offset)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| anyhow!("插件 stream range 长度溢出"))?;
        let headers = build_range_headers(
            &descriptor.headers,
            offset,
            requested_end,
            if_range.as_deref(),
        );
        let response = http
            .execute_stream(
                &plugin.manifest,
                &grant,
                PluginHttpRequest {
                    method: "GET".into(),
                    url: descriptor.url.clone(),
                    headers,
                    body: Vec::new(),
                },
                PluginHttpStreamBodyLimits {
                    max_full_body: max_stream_bytes,
                    max_partial_body: requested_bytes,
                    allow_full_response: offset == 0,
                },
                writer,
            )
            .await
            .context("插件 stream Host HTTP 请求失败")?;

        if response.status == 429 {
            let retry_after = response_header(&response, "retry-after")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let _ = runtime.record_rate_limit(
                &PluginCallKey::provider(&route.plugin_id, &route.provider_id),
                retry_after,
            );
        }

        if offset == 0 && response.status == 200 {
            if response.body_bytes == 0 {
                bail!("插件 stream HTTP body 为空");
            }
            if response.body_bytes > max_stream_bytes {
                bail!("插件 stream 超过 Host 最大文件大小限制");
            }
            if let Some(content_length) = response_header(&response, "content-length") {
                let declared = content_length
                    .trim()
                    .parse::<u64>()
                    .context("插件 stream Content-Length 非法")?;
                if declared != response.body_bytes {
                    bail!(
                        "插件 stream Content-Length 与实际响应不一致: declared={declared}, actual={}",
                        response.body_bytes
                    );
                }
            }
            return Ok(response.body_bytes);
        }

        if response.status != 206 {
            bail!("插件 stream range 请求返回非 206 状态: {}", response.status);
        }

        let content_range = parse_content_range(&response)?;
        if content_range.start != offset {
            bail!(
                "插件 stream Content-Range 起点不连续: expected={offset}, actual={}",
                content_range.start
            );
        }
        if content_range.end > requested_end {
            bail!(
                "插件 stream Content-Range 超出请求范围: requested_end={requested_end}, actual_end={}",
                content_range.end
            );
        }
        if content_range.total > max_stream_bytes {
            bail!(
                "插件 stream 总大小 {} 超过 Host 限制 {}",
                content_range.total,
                max_stream_bytes
            );
        }
        if let Some(expected_total) = expected_total {
            if expected_total != content_range.total {
                bail!(
                    "插件 stream 分块下载期间总大小发生变化: expected={expected_total}, actual={}",
                    content_range.total
                );
            }
        } else {
            expected_total = Some(content_range.total);
            if_range = select_if_range_validator(&response);
        }

        let expected_body_len = content_range
            .end
            .checked_sub(content_range.start)
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| anyhow!("插件 stream Content-Range 长度溢出"))?;
        if response.body_bytes != expected_body_len {
            bail!(
                "插件 stream range body 长度不匹配: expected={expected_body_len}, actual={}",
                response.body_bytes
            );
        }

        offset = content_range
            .end
            .checked_add(1)
            .ok_or_else(|| anyhow!("插件 stream range offset 溢出"))?;
        if offset == content_range.total {
            return Ok(offset);
        }
        if offset > content_range.total {
            bail!("插件 stream Content-Range 超过总大小");
        }
    }
}

fn build_range_headers(
    guest_headers: &[KeyValue],
    start: u64,
    end: u64,
    if_range: Option<&str>,
) -> Vec<KeyValue> {
    let mut headers = guest_headers
        .iter()
        .filter(|header| {
            !header.key.eq_ignore_ascii_case("range")
                && !header.key.eq_ignore_ascii_case("if-range")
                && !header.key.eq_ignore_ascii_case("accept-encoding")
        })
        .cloned()
        .collect::<Vec<_>>();
    headers.push(KeyValue {
        key: "Accept-Encoding".into(),
        value: "identity".into(),
    });
    headers.push(KeyValue {
        key: "Range".into(),
        value: format!("bytes={start}-{end}"),
    });
    if let Some(if_range) = if_range {
        headers.push(KeyValue {
            key: "If-Range".into(),
            value: if_range.to_owned(),
        });
    }
    headers
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ParsedContentRange {
    start: u64,
    end: u64,
    total: u64,
}

fn parse_content_range(response: &PluginHttpStreamResponse) -> Result<ParsedContentRange> {
    let value = response_header(response, "content-range")
        .ok_or_else(|| anyhow!("插件 stream 206 响应缺少 Content-Range"))?
        .trim();
    let (unit, value) = value
        .split_once(' ')
        .ok_or_else(|| anyhow!("插件 stream Content-Range 格式非法"))?;
    if !unit.eq_ignore_ascii_case("bytes") {
        bail!("插件 stream Content-Range unit 非 bytes");
    }
    let (range, total) = value
        .split_once('/')
        .ok_or_else(|| anyhow!("插件 stream Content-Range 缺少总大小"))?;
    if total == "*" {
        bail!("插件 stream Content-Range 必须提供可验证总大小");
    }
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| anyhow!("插件 stream Content-Range 范围非法"))?;
    let start = start
        .trim()
        .parse::<u64>()
        .context("插件 stream Content-Range start 非法")?;
    let end = end
        .trim()
        .parse::<u64>()
        .context("插件 stream Content-Range end 非法")?;
    let total = total
        .trim()
        .parse::<u64>()
        .context("插件 stream Content-Range total 非法")?;
    if total == 0 || start > end || end >= total {
        bail!("插件 stream Content-Range 数值关系非法");
    }
    Ok(ParsedContentRange { start, end, total })
}

fn response_header<'a>(response: &'a PluginHttpStreamResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn select_if_range_validator(response: &PluginHttpStreamResponse) -> Option<String> {
    if let Some(etag) = response_header(response, "etag") {
        let etag = etag.trim();
        if !etag.is_empty() && !etag.starts_with("W/") {
            return Some(etag.to_owned());
        }
    }
    response_header(response, "last-modified")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn cache_identity(
    route: &PluginRoute,
    request: &StreamRequest,
    descriptor: &StreamDescriptor,
) -> Result<Vec<u8>> {
    if request.track.provider_id != route.provider_id {
        bail!("插件 stream cache source provider 与 route 不一致");
    }

    let mut identity = Vec::with_capacity(1024);
    identity.extend_from_slice(b"YINQIDAO-PLUGIN-STREAM-CACHE\0");
    identity.extend_from_slice(&STREAM_CACHE_SCHEMA_VERSION.to_le_bytes());
    push_text(&mut identity, &route.plugin_id, "plugin id")?;
    push_text(&mut identity, &route.provider_id, "provider id")?;
    push_text(&mut identity, &route.account_id, "account id")?;
    push_text(
        &mut identity,
        &request.track.provider_id,
        "source provider id",
    )?;
    push_text(&mut identity, &request.track.source_id, "source id")?;
    push_optional_text(&mut identity, request.quality.as_deref(), "quality")?;
    push_optional_text(&mut identity, descriptor.codec.as_deref(), "codec")?;
    push_optional_u32(&mut identity, descriptor.bitrate);
    push_optional_u32(&mut identity, descriptor.sample_rate);
    push_optional_u16(&mut identity, descriptor.channels);

    if identity.len() > MAX_CACHE_IDENTITY_BYTES {
        bail!("插件 stream cache identity 超过大小限制");
    }
    Ok(identity)
}

fn push_text(output: &mut Vec<u8>, value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 8 * 1024 || value.contains('\0') {
        bail!("插件 stream cache {field} 非法");
    }
    let length = u32::try_from(value.len()).map_err(|_| anyhow!("插件 stream cache 文本过长"))?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_optional_text(output: &mut Vec<u8>, value: Option<&str>, field: &str) -> Result<()> {
    match value {
        Some(value) => {
            output.push(1);
            push_text(output, value, field)?;
        }
        None => output.push(0),
    }
    Ok(())
}

fn push_optional_u32(output: &mut Vec<u8>, value: Option<u32>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_le_bytes());
        }
        None => output.push(0),
    }
}

fn push_optional_u16(output: &mut Vec<u8>, value: Option<u16>) {
    match value {
        Some(value) => {
            output.push(1);
            output.extend_from_slice(&value.to_le_bytes());
        }
        None => output.push(0),
    }
}

fn locator_hex(identity: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(identity);
    format!("{:x}", hasher.finalize())
}

fn codec_extension(codec: Option<&str>) -> &'static str {
    match codec.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("flac") => "flac",
        Some("mp3") | Some("mpeg") | Some("audio/mpeg") => "mp3",
        Some("aac") => "aac",
        Some("alac") | Some("m4a") | Some("mp4a") => "m4a",
        Some("ogg") | Some("vorbis") => "ogg",
        Some("opus") => "opus",
        Some("wav") | Some("wave") | Some("pcm") => "wav",
        Some("avs3") | Some("av3a") | Some("audio-vivid") => "av3a",
        _ => "media",
    }
}

fn write_temp_entry(
    temp_dir: &Path,
    identity: &[u8],
    audio_name: &str,
    max_stream_bytes: u64,
    mut receiver: mpsc::Receiver<Vec<u8>>,
) -> Result<u64> {
    let result = (|| {
        let root = temp_dir
            .parent()
            .ok_or_else(|| anyhow!("插件 stream cache 临时目录缺少父目录"))?;
        fs::create_dir_all(root)
            .with_context(|| format!("创建插件 stream cache root 失败: {}", root.display()))?;
        fs::create_dir(temp_dir).with_context(|| {
            format!(
                "创建插件 stream cache 临时目录失败: {}",
                temp_dir.display()
            )
        })?;

        let audio_path = temp_dir.join(audio_name);
        let mut audio = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&audio_path)
            .with_context(|| {
                format!(
                    "创建插件 stream cache 临时音频失败: {}",
                    audio_path.display()
                )
            })?;

        let mut written = 0u64;
        while let Some(chunk) = receiver.blocking_recv() {
            written = written
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| anyhow!("插件 stream cache 写入长度溢出"))?;
            if written > max_stream_bytes {
                bail!("插件 stream cache 写入超过 Host 最大文件大小");
            }
            audio
                .write_all(&chunk)
                .context("写入插件 stream cache 音频失败")?;
        }
        if written == 0 {
            bail!("插件 stream cache 不接受空音频");
        }
        audio.sync_all().context("同步插件 stream cache 音频失败")?;
        drop(audio);

        let identity_path = temp_dir.join("identity.bin");
        let mut identity_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&identity_path)
            .with_context(|| {
                format!(
                    "创建插件 stream cache identity 失败: {}",
                    identity_path.display()
                )
            })?;
        identity_file
            .write_all(identity)
            .context("写入插件 stream cache identity 失败")?;
        identity_file
            .sync_all()
            .context("同步插件 stream cache identity 失败")?;
        Ok(written)
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(temp_dir);
    }
    result
}

fn cached_entry(
    bucket: &Path,
    expected_identity: &[u8],
    audio_name: &str,
    max_stream_bytes: u64,
    cache_ttl: Duration,
) -> Result<Option<CachedStreamEntry>> {
    let bucket_metadata = match fs::symlink_metadata(bucket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("读取插件 stream cache bucket metadata 失败: {}", bucket.display())
            });
        }
    };
    if bucket_metadata.file_type().is_symlink() || !bucket_metadata.file_type().is_dir() {
        bail!("插件 stream cache locator 被非普通目录对象占用");
    }

    let identity_path = bucket.join("identity.bin");
    let identity_metadata = match fs::symlink_metadata(&identity_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::remove_dir_all(bucket).with_context(|| {
                format!("清理不完整插件 stream cache 失败: {}", bucket.display())
            })?;
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "读取插件 stream cache identity metadata 失败: {}",
                    identity_path.display()
                )
            });
        }
    };
    if identity_metadata.file_type().is_symlink()
        || !identity_metadata.file_type().is_file()
        || identity_metadata.len() > MAX_CACHE_IDENTITY_BYTES as u64
    {
        bail!("插件 stream cache identity 文件非法");
    }
    let stored_identity = fs::read(&identity_path).with_context(|| {
        format!(
            "读取插件 stream cache identity 失败: {}",
            identity_path.display()
        )
    })?;
    if stored_identity != expected_identity {
        bail!("插件 stream cache locator digest 冲突，已 fail-closed");
    }

    let audio_path = bucket.join(audio_name);
    let audio_metadata = match fs::symlink_metadata(&audio_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::remove_dir_all(bucket).with_context(|| {
                format!("清理不完整插件 stream cache 失败: {}", bucket.display())
            })?;
            return Ok(None);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "读取插件 stream cache 音频 metadata 失败: {}",
                    audio_path.display()
                )
            });
        }
    };
    if audio_metadata.file_type().is_symlink()
        || !audio_metadata.file_type().is_file()
        || audio_metadata.len() == 0
        || audio_metadata.len() > max_stream_bytes
    {
        fs::remove_dir_all(bucket).with_context(|| {
            format!("清理非法插件 stream cache 失败: {}", bucket.display())
        })?;
        return Ok(None);
    }

    if audio_metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age > cache_ttl)
    {
        fs::remove_dir_all(bucket).with_context(|| {
            format!("清理过期插件 stream cache 失败: {}", bucket.display())
        })?;
        return Ok(None);
    }

    Ok(Some(CachedStreamEntry {
        path: audio_path,
        bytes: audio_metadata.len(),
        cache_hit: true,
    }))
}

fn commit_temp_entry(
    temp_dir: &Path,
    bucket: &Path,
    expected_identity: &[u8],
    audio_name: &str,
    max_stream_bytes: u64,
    cache_ttl: Duration,
) -> Result<CachedStreamEntry> {
    if let Some(hit) = cached_entry(
        bucket,
        expected_identity,
        audio_name,
        max_stream_bytes,
        cache_ttl,
    )? {
        let _ = fs::remove_dir_all(temp_dir);
        return Ok(hit);
    }

    match fs::rename(temp_dir, bucket) {
        Ok(()) => {}
        Err(rename_error) if fs::symlink_metadata(bucket).is_ok() => {
            if let Some(hit) = cached_entry(
                bucket,
                expected_identity,
                audio_name,
                max_stream_bytes,
                cache_ttl,
            )? {
                let _ = fs::remove_dir_all(temp_dir);
                return Ok(hit);
            }
            let _ = fs::remove_dir_all(temp_dir);
            return Err(rename_error).context("并发提交插件 stream cache 失败");
        }
        Err(error) => {
            let _ = fs::remove_dir_all(temp_dir);
            return Err(error).context("提交插件 stream cache 失败");
        }
    }

    let audio_path = bucket.join(audio_name);
    let metadata = fs::symlink_metadata(&audio_path).with_context(|| {
        format!(
            "读取已提交插件 stream cache metadata 失败: {}",
            audio_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("已提交插件 stream cache 音频不是普通文件");
    }
    Ok(CachedStreamEntry {
        path: audio_path,
        bytes: metadata.len(),
        cache_hit: false,
    })
}

async fn cleanup_temp_dir(path: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || fs::remove_dir_all(path)).await;
}

pub fn initialize(base_dir: &Path) -> Arc<PluginStreamCache> {
    PLUGIN_STREAM_CACHE
        .get_or_init(|| {
            Arc::new(PluginStreamCache::new(
                base_dir.join("plugin-cache").join("streams"),
            ))
        })
        .clone()
}

pub fn global() -> Option<Arc<PluginStreamCache>> {
    PLUGIN_STREAM_CACHE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::SourceTrackRef;

    fn route() -> PluginRoute {
        PluginRoute {
            plugin_id: "plugin.test".into(),
            provider_id: "provider".into(),
            account_id: "account-a".into(),
            priority: 0,
            is_default: true,
        }
    }

    fn request() -> StreamRequest {
        StreamRequest {
            track: SourceTrackRef {
                provider_id: "provider".into(),
                source_id: "track-1".into(),
            },
            quality: Some("lossless".into()),
        }
    }

    #[test]
    fn content_range_parser_rejects_unknown_or_invalid_total() {
        let response = |value: &str| PluginHttpStreamResponse {
            status: 206,
            headers: vec![KeyValue {
                key: "Content-Range".into(),
                value: value.into(),
            }],
            body_bytes: 0,
        };
        assert_eq!(
            parse_content_range(&response("bytes 0-99/100")).expect("range"),
            ParsedContentRange {
                start: 0,
                end: 99,
                total: 100
            }
        );
        assert!(parse_content_range(&response("bytes 0-99/*")).is_err());
        assert!(parse_content_range(&response("bytes 100-99/101")).is_err());
        assert!(parse_content_range(&response("items 0-99/100")).is_err());
    }

    #[test]
    fn range_headers_replace_guest_transport_controls() {
        let headers = build_range_headers(
            &[
                KeyValue {
                    key: "Authorization".into(),
                    value: "Bearer token".into(),
                },
                KeyValue {
                    key: "Range".into(),
                    value: "bytes=1-2".into(),
                },
                KeyValue {
                    key: "Accept-Encoding".into(),
                    value: "gzip".into(),
                },
            ],
            0,
            1023,
            Some("\"etag\""),
        );
        assert_eq!(
            headers
                .iter()
                .filter(|header| header.key.eq_ignore_ascii_case("range"))
                .count(),
            1
        );
        assert!(headers.iter().any(|header| {
            header.key.eq_ignore_ascii_case("accept-encoding") && header.value == "identity"
        }));
        assert!(headers.iter().any(|header| {
            header.key.eq_ignore_ascii_case("if-range") && header.value == "\"etag\""
        }));
        assert!(headers.iter().any(|header| {
            header.key.eq_ignore_ascii_case("authorization")
                && header.value == "Bearer token"
        }));
    }

    #[test]
    fn cache_identity_never_embeds_signed_url_or_headers() {
        let descriptor = StreamDescriptor {
            url: "https://cdn.example.com/audio?token=secret-token".into(),
            headers: vec![KeyValue {
                key: "Authorization".into(),
                value: "Bearer secret-header".into(),
            }],
            codec: Some("flac".into()),
            bitrate: Some(1_411_200),
            sample_rate: Some(44_100),
            channels: Some(2),
            expires_at_ms: Some(999_999),
        };
        let identity = cache_identity(&route(), &request(), &descriptor).expect("identity");
        let text = String::from_utf8_lossy(&identity);
        assert!(!text.contains("secret-token"));
        assert!(!text.contains("secret-header"));
        assert!(text.contains("track-1"));
        assert!(text.contains("lossless"));
    }

    #[test]
    fn unknown_codec_never_becomes_a_path_fragment() {
        assert_eq!(codec_extension(Some("../../escape")), "media");
        assert_eq!(codec_extension(Some("flac")), "flac");
        assert_eq!(codec_extension(Some("audio/mpeg")), "mp3");
    }

    #[test]
    fn weak_etag_is_not_used_for_if_range() {
        let weak = PluginHttpStreamResponse {
            status: 206,
            headers: vec![
                KeyValue {
                    key: "ETag".into(),
                    value: "W/\"weak\"".into(),
                },
                KeyValue {
                    key: "Last-Modified".into(),
                    value: "Wed, 16 Sep 2026 00:00:00 GMT".into(),
                },
            ],
            body_bytes: 0,
        };
        assert_eq!(
            select_if_range_validator(&weak).as_deref(),
            Some("Wed, 16 Sep 2026 00:00:00 GMT")
        );
    }

    #[test]
    fn download_concurrency_is_bounded() {
        let cache = PluginStreamCache::new(PathBuf::from("cache"));
        assert_eq!(
            cache.download_gate.available_permits(),
            DEFAULT_MAX_CONCURRENT_DOWNLOADS
        );
    }

    #[test]
    fn download_timeout_is_finite() {
        assert!(!DEFAULT_DOWNLOAD_TIMEOUT.is_zero());
    }
}
