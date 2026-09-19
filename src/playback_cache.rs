use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::{
    lyrics::LyricsDocument,
    model::{Track, TrackId},
    plugin::abi::{PluginRoute, RemoteTrack, SourceTrackRef},
    settings::SavedTrackInfo,
};

const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_CACHE_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;

#[derive(Clone, Debug)]
pub(crate) struct PlaybackAssetCache {
    directory: PathBuf,
    entries_directory: PathBuf,
    lyrics_directory: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Debug)]
pub(crate) struct CachedPluginPlayback {
    pub route: PluginRoute,
    pub source: SourceTrackRef,
    pub remote: RemoteTrack,
    pub cover_url: Option<String>,
    pub lyrics: Option<LyricsDocument>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskRoute {
    plugin_id: String,
    provider_id: String,
    account_id: String,
    priority: i32,
    is_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskLyrics {
    plain: Option<String>,
    synced: Option<String>,
    translation: Option<String>,
    source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskLyricsEntry {
    schema_version: u32,
    source: SourceTrackRef,
    lyrics: DiskLyrics,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiskPlaybackEntry {
    schema_version: u32,
    track: SavedTrackInfo,
    route: DiskRoute,
    source: SourceTrackRef,
    remote: RemoteTrack,
    cover_url: Option<String>,
}

impl PlaybackAssetCache {
    pub(crate) fn new(directory: PathBuf) -> Result<Self> {
        let entries_directory = directory.join("entries");
        let lyrics_directory = directory.join("lyrics");
        fs::create_dir_all(&entries_directory).with_context(|| {
            format!(
                "创建在线播放资源缓存目录失败: {}",
                entries_directory.display()
            )
        })?;
        fs::create_dir_all(&lyrics_directory).with_context(|| {
            format!(
                "创建在线歌词缓存目录失败: {}",
                lyrics_directory.display()
            )
        })?;
        Ok(Self {
            directory,
            entries_directory,
            lyrics_directory,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub(crate) fn load_last(&self, track_id: TrackId) -> Result<Option<CachedPluginPlayback>> {
        let Some(entry) = self.read_playback_entry(&self.directory.join("last.json"))? else {
            return Ok(None);
        };
        if entry.schema_version != CACHE_SCHEMA_VERSION || entry.track.id != track_id {
            return Ok(None);
        }
        validate_playback_entry(&entry)?;
        let lyrics = self.load_lyrics(&entry.source)?;
        Ok(Some(entry.into_runtime(lyrics)))
    }

    pub(crate) fn load_source(
        &self,
        source: &SourceTrackRef,
    ) -> Result<Option<CachedPluginPlayback>> {
        let path = self.playback_entry_path(source);
        let Some(entry) = self.read_playback_entry(&path)? else {
            return Ok(None);
        };
        if entry.schema_version != CACHE_SCHEMA_VERSION {
            return Ok(None);
        }
        validate_playback_entry(&entry)?;
        let lyrics = self.load_lyrics(source)?;
        Ok(Some(entry.into_runtime(lyrics)))
    }

    pub(crate) fn load_lyrics(&self, source: &SourceTrackRef) -> Result<Option<LyricsDocument>> {
        let path = self.lyrics_entry_path(source);
        let Some(entry) = self.read_lyrics_entry(&path)? else {
            return Ok(None);
        };
        if entry.schema_version != CACHE_SCHEMA_VERSION || entry.source != *source {
            return Ok(None);
        }
        validate_source(source)?;
        Ok(Some(entry.lyrics.into_runtime()))
    }

    pub(crate) fn store_playback(
        &self,
        track: &Track,
        route: &PluginRoute,
        source: &SourceTrackRef,
        remote: &RemoteTrack,
        cover_url: Option<&str>,
    ) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| anyhow!("在线播放资源缓存写锁已损坏"))?;

        let entry = DiskPlaybackEntry {
            schema_version: CACHE_SCHEMA_VERSION,
            track: SavedTrackInfo::from(track),
            route: DiskRoute::from(route),
            source: source.clone(),
            remote: remote.clone(),
            cover_url: cover_url
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned),
        };
        validate_playback_entry(&entry)?;
        write_json_atomic(&self.playback_entry_path(source), &entry)?;
        write_json_atomic(&self.directory.join("last.json"), &entry)?;
        prune_directory(&self.entries_directory);
        Ok(())
    }

    pub(crate) fn store_lyrics(
        &self,
        source: &SourceTrackRef,
        lyrics: &LyricsDocument,
    ) -> Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| anyhow!("在线播放资源缓存写锁已损坏"))?;

        validate_source(source)?;
        let entry = DiskLyricsEntry {
            schema_version: CACHE_SCHEMA_VERSION,
            source: source.clone(),
            lyrics: DiskLyrics::from(lyrics),
        };
        write_json_atomic(&self.lyrics_entry_path(source), &entry)?;
        prune_directory(&self.lyrics_directory);
        Ok(())
    }

    fn read_playback_entry(&self, path: &Path) -> Result<Option<DiskPlaybackEntry>> {
        read_json_file(path)
    }

    fn read_lyrics_entry(&self, path: &Path) -> Result<Option<DiskLyricsEntry>> {
        read_json_file(path)
    }

    fn playback_entry_path(&self, source: &SourceTrackRef) -> PathBuf {
        self.entries_directory
            .join(format!("{:016x}.json", source_hash(source)))
    }

    fn lyrics_entry_path(&self, source: &SourceTrackRef) -> PathBuf {
        self.lyrics_directory
            .join(format!("{:016x}.json", source_hash(source)))
    }
}

impl DiskPlaybackEntry {
    fn into_runtime(self, lyrics: Option<LyricsDocument>) -> CachedPluginPlayback {
        CachedPluginPlayback {
            route: self.route.into_runtime(),
            source: self.source,
            remote: self.remote,
            cover_url: self.cover_url,
            lyrics,
        }
    }
}

impl From<&PluginRoute> for DiskRoute {
    fn from(route: &PluginRoute) -> Self {
        Self {
            plugin_id: route.plugin_id.clone(),
            provider_id: route.provider_id.clone(),
            account_id: route.account_id.clone(),
            priority: route.priority,
            is_default: route.is_default,
        }
    }
}

impl DiskRoute {
    fn into_runtime(self) -> PluginRoute {
        PluginRoute {
            plugin_id: self.plugin_id,
            provider_id: self.provider_id,
            account_id: self.account_id,
            priority: self.priority,
            is_default: self.is_default,
        }
    }
}

impl From<&LyricsDocument> for DiskLyrics {
    fn from(lyrics: &LyricsDocument) -> Self {
        Self {
            plain: lyrics.plain.clone(),
            synced: lyrics.synced.clone(),
            translation: lyrics.translation.clone(),
            source: lyrics.source.clone(),
        }
    }
}

impl DiskLyrics {
    fn into_runtime(self) -> LyricsDocument {
        LyricsDocument::from_sources(self.plain, self.synced, self.translation, self.source)
    }
}

fn validate_playback_entry(entry: &DiskPlaybackEntry) -> Result<()> {
    if entry.schema_version != CACHE_SCHEMA_VERSION {
        bail!("在线播放缓存 schema 不兼容");
    }
    validate_source(&entry.source)?;
    if entry.track.id >= 0 {
        bail!("在线播放缓存只接受远端临时 TrackId");
    }
    if entry.route.provider_id != entry.source.provider_id {
        bail!("在线播放缓存 route/source provider 不一致");
    }
    if entry.remote.source != entry.source {
        bail!("在线播放缓存 remote/source 不一致");
    }
    if entry.route.plugin_id.len() > 256
        || entry.route.provider_id.len() > 128
        || entry.route.account_id.len() > 512
        || entry.source.source_id.len() > 1024
        || entry.cover_url.as_ref().is_some_and(|url| url.len() > 8 * 1024)
    {
        bail!("在线播放缓存字段超过安全上限");
    }
    Ok(())
}

fn validate_source(source: &SourceTrackRef) -> Result<()> {
    if source.provider_id.trim().is_empty() || source.source_id.trim().is_empty() {
        bail!("在线播放缓存 source 无效");
    }
    if source.provider_id.len() > 128 || source.source_id.len() > 1024 {
        bail!("在线播放缓存 source 字段超过安全上限");
    }
    Ok(())
}

fn read_json_file<T>(path: &Path) -> Result<Option<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("读取缓存 metadata 失败: {}", path.display()));
        }
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CACHE_FILE_BYTES {
        return Ok(None);
    }

    let bytes = fs::read(path).with_context(|| format!("读取在线播放缓存失败: {}", path.display()))?;
    match serde_json::from_slice::<T>(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "忽略损坏的在线播放资源缓存"
            );
            Ok(None)
        }
    }
}

fn write_json_atomic<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    let bytes = serde_json::to_vec(value).context("序列化在线播放资源缓存失败")?;
    if bytes.len() as u64 > MAX_CACHE_FILE_BYTES {
        bail!("在线播放资源缓存条目超过 {} 字节", MAX_CACHE_FILE_BYTES);
    }

    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, &bytes).with_context(|| format!("写入临时缓存失败: {}", tmp.display()))?;
    if let Err(first_error) = fs::rename(&tmp, path) {
        // Windows rename does not replace an existing destination. Writes are serialized, so the
        // remove+rename fallback cannot race another writer inside this cache instance.
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("替换旧缓存失败: {}", path.display()))?;
            fs::rename(&tmp, path)
                .with_context(|| format!("提交在线播放缓存失败: {}", path.display()))?;
        } else {
            return Err(first_error)
                .with_context(|| format!("提交在线播放缓存失败: {}", path.display()));
        }
    }
    Ok(())
}

fn prune_directory(directory: &Path) {
    let Ok(read_dir) = fs::read_dir(directory) else {
        return;
    };
    let mut entries = read_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().ok()?;
            Some((modified, entry.path()))
        })
        .collect::<Vec<_>>();
    if entries.len() <= MAX_ENTRIES {
        return;
    }
    entries.sort_by_key(|(modified, _)| *modified);
    let remove_count = entries.len().saturating_sub(MAX_ENTRIES);
    for (_, path) in entries.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

fn source_hash(source: &SourceTrackRef) -> u64 {
    stable_hash(&format!(
        "playback-assets-v1\0{}\0{}",
        source.provider_id, source.source_id
    ))
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
