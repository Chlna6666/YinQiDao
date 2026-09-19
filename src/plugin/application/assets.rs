use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Cursor, Read},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, anyhow, bail};
use image::{ImageFormat, ImageReader, Limits};

use super::{
    host::{catalog::PluginCatalog, package_manager},
    ui::{
        manifest::validate_relative_asset_path,
        schema::{UiNode, UiPageModel},
    },
};

const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DECODE_ALLOC_BYTES: u64 = 96 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MAX_OUTPUT_DIMENSION: u32 = 2048;
const MAX_NORMALIZED_BYTES: usize = 16 * 1024 * 1024;
const MAX_PAGE_IMAGES: usize = 128;
const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

static PLUGIN_IMAGE_CACHE: OnceLock<PluginImageCache> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PluginImageAsset {
    pub plugin_id: String,
    pub asset: String,
    /// Host-normalized PNG bytes. GPUI never decodes arbitrary guest-controlled file formats.
    pub png: Arc<[u8]>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AssetKey {
    plugin_id: String,
    asset: String,
}

#[derive(Clone, Debug)]
struct CachedImage {
    source: Arc<[u8]>,
    png: Arc<[u8]>,
    width: u32,
    height: u32,
    last_used: u64,
}

impl CachedImage {
    fn retained_bytes(&self) -> usize {
        self.source.len().saturating_add(self.png.len())
    }
}

#[derive(Clone, Debug)]
struct PluginImageLoadTicket {
    plugin_id: String,
    generation: u64,
}

#[derive(Debug, Default)]
struct AssetCacheState {
    entries: HashMap<AssetKey, CachedImage>,
    plugin_generations: HashMap<String, u64>,
    retained_bytes: usize,
    clock: u64,
}

impl AssetCacheState {
    fn next_clock(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn plugin_generation(&self, plugin_id: &str) -> u64 {
        self.plugin_generations.get(plugin_id).copied().unwrap_or(0)
    }
}

#[derive(Debug, Default)]
struct PluginImageCache {
    state: Mutex<AssetCacheState>,
}

impl PluginImageCache {
    fn begin_load(&self, plugin_id: &str) -> Result<PluginImageLoadTicket> {
        let state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件图片缓存锁已损坏: {error}"))?;
        Ok(PluginImageLoadTicket {
            plugin_id: plugin_id.to_owned(),
            generation: state.plugin_generation(plugin_id),
        })
    }

    fn cached(&self, plugin_id: &str, asset: &str) -> Result<Option<PluginImageAsset>> {
        let key = AssetKey {
            plugin_id: plugin_id.to_owned(),
            asset: asset.to_owned(),
        };
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件图片缓存锁已损坏: {error}"))?;
        let Some(entry) = state.entries.get(&key) else {
            return Ok(None);
        };
        let png = entry.png.clone();
        let width = entry.width;
        let height = entry.height;
        let clock = state.next_clock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.last_used = clock;
        }
        Ok(Some(PluginImageAsset {
            plugin_id: key.plugin_id,
            asset: key.asset,
            png,
            width,
            height,
        }))
    }

    fn source_match(
        &self,
        ticket: &PluginImageLoadTicket,
        key: &AssetKey,
        source: &[u8],
    ) -> Result<Option<PluginImageAsset>> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件图片缓存锁已损坏: {error}"))?;
        if state.plugin_generation(&ticket.plugin_id) != ticket.generation {
            bail!("插件图片在读取期间已失效，拒绝复用旧缓存");
        }
        let Some(entry) = state.entries.get(key) else {
            return Ok(None);
        };
        if entry.source.as_ref() != source {
            return Ok(None);
        }
        let png = entry.png.clone();
        let width = entry.width;
        let height = entry.height;
        let clock = state.next_clock();
        if let Some(entry) = state.entries.get_mut(key) {
            entry.last_used = clock;
        }
        Ok(Some(PluginImageAsset {
            plugin_id: key.plugin_id.clone(),
            asset: key.asset.clone(),
            png,
            width,
            height,
        }))
    }

    fn publish(
        &self,
        ticket: PluginImageLoadTicket,
        key: AssetKey,
        source: Arc<[u8]>,
        png: Arc<[u8]>,
        width: u32,
        height: u32,
    ) -> Result<PluginImageAsset> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件图片缓存锁已损坏: {error}"))?;
        if state.plugin_generation(&ticket.plugin_id) != ticket.generation {
            bail!("插件图片在解码期间已失效，拒绝发布旧图片");
        }
        if let Some(existing) = state.entries.remove(&key) {
            state.retained_bytes = state
                .retained_bytes
                .saturating_sub(existing.retained_bytes());
        }
        let last_used = state.next_clock();
        let entry = CachedImage {
            source,
            png: png.clone(),
            width,
            height,
            last_used,
        };
        state.retained_bytes = state.retained_bytes.saturating_add(entry.retained_bytes());
        state.entries.insert(key.clone(), entry);
        trim_cache(&mut state, Some(&key));
        Ok(PluginImageAsset {
            plugin_id: key.plugin_id,
            asset: key.asset,
            png,
            width,
            height,
        })
    }

    fn invalidate_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件图片缓存锁已损坏: {error}"))?;
        let generation = state
            .plugin_generations
            .entry(plugin_id.to_owned())
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        let victims = state
            .entries
            .iter()
            .filter(|(key, _)| key.plugin_id == plugin_id)
            .map(|(key, entry)| (key.clone(), entry.retained_bytes()))
            .collect::<Vec<_>>();
        for (key, bytes) in &victims {
            state.entries.remove(key);
            state.retained_bytes = state.retained_bytes.saturating_sub(*bytes);
        }
        Ok(victims.len())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginImageLoadFailure {
    pub asset: String,
    pub error: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginImagePreloadReport {
    pub requested: usize,
    pub ready: usize,
    pub failures: Vec<PluginImageLoadFailure>,
}

/// Read one already-normalized image from the Host cache. This performs only bounded in-memory cache
/// work: no package-manager state lock, filesystem I/O, image decoding or guest execution occurs on
/// the GPUI render path. Page visibility/access is checked by the management façade before render.
pub fn cached_image(plugin_id: &str, asset: &str) -> Result<Option<PluginImageAsset>> {
    validate_relative_asset_path(asset)?;
    PLUGIN_IMAGE_CACHE
        .get_or_init(PluginImageCache::default)
        .cached(plugin_id, asset)
}

/// Load, decode and normalize one package image on a controller/worker path.
///
/// This function performs filesystem I/O and decoding and therefore must never be called from GPUI
/// paint or the realtime audio callback. Callers can use `cached_image` from render after preload.
pub fn load_image(plugin_id: &str, asset: &str) -> Result<PluginImageAsset> {
    validate_relative_asset_path(asset)?;
    let manager = package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))?;
    if !manager.is_enabled(plugin_id) {
        bail!("插件已禁用，拒绝读取图片 asset: {plugin_id}");
    }

    let cache = PLUGIN_IMAGE_CACHE.get_or_init(PluginImageCache::default);
    let ticket = cache.begin_load(plugin_id)?;
    let catalog = PluginCatalog::discover(manager.plugin_root().to_path_buf());
    let plugin = catalog
        .plugin(plugin_id)
        .ok_or_else(|| anyhow!("插件包已不存在: {plugin_id}"))?;
    let canonical_package = fs::canonicalize(&plugin.package_dir)
        .with_context(|| format!("规范化插件目录失败: {}", plugin.package_dir.display()))?;
    let requested = canonical_package.join(asset);
    let symlink_metadata = fs::symlink_metadata(&requested)
        .with_context(|| format!("读取插件图片 asset metadata 失败: {}", requested.display()))?;
    if !symlink_metadata.file_type().is_file() || symlink_metadata.file_type().is_symlink() {
        bail!("插件图片 asset 必须是包内普通文件，且不能是符号链接");
    }

    let canonical_asset = fs::canonicalize(&requested)
        .with_context(|| format!("规范化插件图片 asset 失败: {}", requested.display()))?;
    if !canonical_asset.starts_with(&canonical_package) {
        bail!("插件图片 asset 逃逸插件目录，已拒绝");
    }

    let file = File::open(&canonical_asset)
        .with_context(|| format!("打开插件图片 asset 失败: {}", canonical_asset.display()))?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "读取已打开图片 asset metadata 失败: {}",
            canonical_asset.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES as u64 {
        bail!("插件图片 asset 不是普通文件或超过 {MAX_SOURCE_BYTES} bytes Host 上限");
    }
    let mut source = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_SOURCE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut source)
        .context("读取插件图片 asset 失败")?;
    if source.len() > MAX_SOURCE_BYTES {
        bail!("插件图片 asset 在读取过程中超过大小限制");
    }

    let key = AssetKey {
        plugin_id: plugin_id.to_owned(),
        asset: asset.to_owned(),
    };
    if let Some(cached) = cache.source_match(&ticket, &key, &source)? {
        return Ok(cached);
    }

    let (png, width, height) = normalize_image(&source)?;
    if !manager.is_enabled(plugin_id) {
        bail!("插件在图片解码期间已禁用，拒绝发布图片");
    }
    cache.publish(
        ticket,
        key,
        Arc::<[u8]>::from(source),
        Arc::<[u8]>::from(png),
        width,
        height,
    )
}

pub fn preload_page_images(
    plugin_id: &str,
    page: &UiPageModel,
) -> Result<PluginImagePreloadReport> {
    let mut assets = HashSet::new();
    collect_image_assets(&page.root, &mut assets)?;
    if assets.len() > MAX_PAGE_IMAGES {
        bail!("插件单页面图片数量超过 {MAX_PAGE_IMAGES} Host 上限");
    }

    let mut report = PluginImagePreloadReport {
        requested: assets.len(),
        ..PluginImagePreloadReport::default()
    };
    for asset in assets {
        match load_image(plugin_id, &asset) {
            Ok(_) => report.ready = report.ready.saturating_add(1),
            Err(error) => report.failures.push(PluginImageLoadFailure {
                asset,
                error: format!("{error:#}"),
            }),
        }
    }
    Ok(report)
}

pub fn invalidate_plugin(plugin_id: &str) -> Result<usize> {
    PLUGIN_IMAGE_CACHE
        .get_or_init(PluginImageCache::default)
        .invalidate_plugin(plugin_id)
}

fn normalize_image(source: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let cursor = Cursor::new(source);
    let mut reader = ImageReader::new(cursor)
        .with_guessed_format()
        .context("识别插件图片格式失败")?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow!("无法识别插件图片格式"))?;
    if !matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::WebP
            | ImageFormat::Gif
            | ImageFormat::Bmp
            | ImageFormat::Ico
            | ImageFormat::Tiff
    ) {
        bail!("插件图片格式不在 Host 允许列表: {format:?}");
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let image = reader.decode().context("解码插件图片失败")?;
    let image = if image.width() > MAX_OUTPUT_DIMENSION || image.height() > MAX_OUTPUT_DIMENSION {
        image.thumbnail(MAX_OUTPUT_DIMENSION, MAX_OUTPUT_DIMENSION)
    } else {
        image
    };
    let width = image.width();
    let height = image.height();
    let mut output = Cursor::new(Vec::new());
    image
        .write_to(&mut output, ImageFormat::Png)
        .context("归一化插件图片为 PNG 失败")?;
    let output = output.into_inner();
    if output.len() > MAX_NORMALIZED_BYTES {
        bail!("归一化插件图片超过 {MAX_NORMALIZED_BYTES} bytes Host 上限");
    }
    Ok((output, width, height))
}

fn collect_image_assets(node: &UiNode, assets: &mut HashSet<String>) -> Result<()> {
    match node {
        UiNode::Image { asset, .. } => {
            validate_relative_asset_path(asset)?;
            assets.insert(asset.clone());
        }
        UiNode::Column { children }
        | UiNode::Row { children }
        | UiNode::Card { children }
        | UiNode::List { children }
        | UiNode::Section { children, .. } => {
            for child in children {
                collect_image_assets(child, assets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn trim_cache(state: &mut AssetCacheState, protected: Option<&AssetKey>) {
    while state.entries.len() > MAX_CACHE_ENTRIES || state.retained_bytes > MAX_CACHE_BYTES {
        let victim = state
            .entries
            .iter()
            .filter(|(key, _)| protected != Some(*key))
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, entry)| (key.clone(), entry.retained_bytes()));
        let Some((key, bytes)) = victim else {
            break;
        };
        state.entries.remove(&key);
        state.retained_bytes = state.retained_bytes.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_paths_are_rejected() {
        assert!(validate_relative_asset_path("images/cover.png").is_ok());
        assert!(validate_relative_asset_path("../cover.png").is_err());
    }

    #[test]
    fn invalidation_rejects_in_flight_publish() {
        let cache = PluginImageCache::default();
        let ticket = cache.begin_load("plugin.test").expect("ticket");
        cache.invalidate_plugin("plugin.test").expect("invalidate");
        let key = AssetKey {
            plugin_id: "plugin.test".into(),
            asset: "cover.png".into(),
        };
        assert!(
            cache
                .publish(
                    ticket,
                    key,
                    Arc::<[u8]>::from(&b"source"[..]),
                    Arc::<[u8]>::from(&b"png"[..]),
                    1,
                    1,
                )
                .is_err()
        );
    }

    #[test]
    fn cache_reads_do_not_accept_different_source_bytes() {
        let cache = PluginImageCache::default();
        let ticket = cache.begin_load("plugin.test").expect("ticket");
        let key = AssetKey {
            plugin_id: "plugin.test".into(),
            asset: "cover.png".into(),
        };
        cache
            .publish(
                ticket.clone(),
                key.clone(),
                Arc::<[u8]>::from(&b"source-a"[..]),
                Arc::<[u8]>::from(&b"png"[..]),
                1,
                1,
            )
            .expect("publish");
        assert!(
            cache
                .source_match(&ticket, &key, b"source-a")
                .expect("match")
                .is_some()
        );
        assert!(
            cache
                .source_match(&ticket, &key, b"source-b")
                .expect("mismatch")
                .is_none()
        );
    }
}
