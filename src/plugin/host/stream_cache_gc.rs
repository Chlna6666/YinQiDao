use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};

const DEFAULT_MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const DEFAULT_TARGET_CACHE_BYTES: u64 = 6 * 1024 * 1024 * 1024;
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_STALE_TEMP_TTL: Duration = Duration::from_secs(24 * 60 * 60);

static PINNED_BUCKETS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

fn pinned_buckets() -> &'static Mutex<HashMap<PathBuf, usize>> {
    PINNED_BUCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Copy, Debug)]
pub struct PluginStreamCacheGcPolicy {
    pub max_cache_bytes: u64,
    pub target_cache_bytes: u64,
    pub cache_ttl: Duration,
    pub stale_temp_ttl: Duration,
}

impl Default for PluginStreamCacheGcPolicy {
    fn default() -> Self {
        Self {
            max_cache_bytes: DEFAULT_MAX_CACHE_BYTES,
            target_cache_bytes: DEFAULT_TARGET_CACHE_BYTES,
            cache_ttl: DEFAULT_CACHE_TTL,
            stale_temp_ttl: DEFAULT_STALE_TEMP_TTL,
        }
    }
}

impl PluginStreamCacheGcPolicy {
    fn normalized(mut self) -> Self {
        self.max_cache_bytes = self.max_cache_bytes.max(1);
        self.target_cache_bytes = self.target_cache_bytes.min(self.max_cache_bytes);
        self
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginStreamCacheGcStats {
    pub cache_bytes_before: u64,
    pub cache_bytes_after: u64,
    pub removed_expired_buckets: usize,
    pub removed_capacity_buckets: usize,
    pub removed_stale_temp_dirs: usize,
    pub removed_invalid_entries: usize,
    pub pinned_buckets: usize,
    pub over_budget_bytes: u64,
}

/// Control-plane lease protecting one finalized cache bucket from GC.
///
/// The lease is intentionally `Arc`-owned by callers. Cloning the `Arc` does not touch the pin
/// registry; the registry is updated only once when the first lease is created and once when its
/// final `Arc` is dropped. Never create/drop these leases from the realtime audio callback.
#[derive(Debug)]
pub struct PluginStreamCacheLease {
    bucket: PathBuf,
}

impl PluginStreamCacheLease {
    pub fn bucket(&self) -> &Path {
        &self.bucket
    }
}

impl Drop for PluginStreamCacheLease {
    fn drop(&mut self) {
        let Ok(mut pins) = pinned_buckets().lock() else {
            return;
        };
        let Some(count) = pins.get_mut(&self.bucket) else {
            return;
        };
        if *count <= 1 {
            pins.remove(&self.bucket);
        } else {
            *count -= 1;
        }
    }
}

pub fn pin_materialized_path(root: &Path, materialized_path: &Path) -> Result<Arc<PluginStreamCacheLease>> {
    let bucket = materialized_path
        .parent()
        .ok_or_else(|| anyhow!("插件 stream cache materialized path 缺少 bucket"))?;
    if bucket.parent() != Some(root) || !is_locator_bucket(bucket) {
        bail!("插件 stream cache materialized path 不属于 Host cache root");
    }

    let metadata = fs::symlink_metadata(bucket).with_context(|| {
        format!("读取插件 stream cache bucket metadata 失败: {}", bucket.display())
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!("插件 stream cache bucket 不是普通目录");
    }
    let file_metadata = fs::symlink_metadata(materialized_path).with_context(|| {
        format!(
            "读取插件 stream cache materialized file metadata 失败: {}",
            materialized_path.display()
        )
    })?;
    if !file_metadata.file_type().is_file() || file_metadata.file_type().is_symlink() {
        bail!("插件 stream cache materialized path 不是普通文件");
    }

    let bucket = bucket.to_path_buf();
    let mut pins = pinned_buckets()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache pin registry 锁已损坏: {error}"))?;
    *pins.entry(bucket.clone()).or_insert(0) += 1;
    drop(pins);
    Ok(Arc::new(PluginStreamCacheLease { bucket }))
}

pub fn prune(root: &Path, policy: PluginStreamCacheGcPolicy) -> Result<PluginStreamCacheGcStats> {
    let policy = policy.normalized();
    ensure_plain_cache_root(root)?;
    let pinned = pinned_snapshot()?;
    let mut stats = PluginStreamCacheGcStats {
        pinned_buckets: pinned.len(),
        ..PluginStreamCacheGcStats::default()
    };
    let mut buckets = Vec::new();

    for entry in fs::read_dir(root)
        .with_context(|| format!("读取插件 stream cache root 失败: {}", root.display()))?
    {
        let entry = entry.context("读取插件 stream cache 目录项失败")?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("读取插件 stream cache 目录项类型失败: {}", path.display()))?;

        if file_type.is_symlink() {
            remove_direct_entry(&path, false)?;
            stats.removed_invalid_entries += 1;
            continue;
        }
        if file_type.is_file() {
            remove_direct_entry(&path, false)?;
            stats.removed_invalid_entries += 1;
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }

        if is_temp_dir(&path) {
            if !pinned.contains(&path) && path_age(&path).is_some_and(|age| age > policy.stale_temp_ttl) {
                remove_direct_entry(&path, true)?;
                stats.removed_stale_temp_dirs += 1;
            }
            continue;
        }
        if !is_locator_bucket(&path) {
            // Unknown directories are not ours. Do not recursively delete user data just because it
            // happens to be under the configured cache root.
            continue;
        }

        let Some(bucket) = inspect_bucket(&path)? else {
            if !pinned.contains(&path) {
                remove_direct_entry(&path, true)?;
                stats.removed_invalid_entries += 1;
            }
            continue;
        };
        stats.cache_bytes_before = stats.cache_bytes_before.saturating_add(bucket.bytes);
        if !pinned.contains(&path)
            && bucket
                .modified
                .elapsed()
                .ok()
                .is_some_and(|age| age > policy.cache_ttl)
        {
            remove_direct_entry(&path, true)?;
            stats.removed_expired_buckets += 1;
            continue;
        }
        buckets.push(bucket);
    }

    let mut cache_bytes = buckets
        .iter()
        .fold(0u64, |total, bucket| total.saturating_add(bucket.bytes));
    if cache_bytes > policy.max_cache_bytes {
        buckets.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.path.cmp(&right.path))
        });
        for bucket in buckets {
            if cache_bytes <= policy.target_cache_bytes {
                break;
            }
            if pinned.contains(&bucket.path) {
                continue;
            }
            remove_direct_entry(&bucket.path, true)?;
            cache_bytes = cache_bytes.saturating_sub(bucket.bytes);
            stats.removed_capacity_buckets += 1;
        }
    }

    stats.cache_bytes_after = cache_bytes;
    stats.over_budget_bytes = cache_bytes.saturating_sub(policy.max_cache_bytes);
    Ok(stats)
}

#[derive(Clone, Debug)]
struct CacheBucket {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn ensure_plain_cache_root(root: &Path) -> Result<()> {
    match fs::symlink_metadata(root) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                bail!("插件 stream cache root 不是普通目录: {}", root.display());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(root).with_context(|| {
                format!("创建插件 stream cache root 失败: {}", root.display())
            })?;
            let metadata = fs::symlink_metadata(root).with_context(|| {
                format!("复核插件 stream cache root 失败: {}", root.display())
            })?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                bail!("插件 stream cache root 创建后不是普通目录");
            }
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("读取插件 stream cache root metadata 失败: {}", root.display())
            });
        }
    }
    Ok(())
}

fn inspect_bucket(path: &Path) -> Result<Option<CacheBucket>> {
    let mut bytes = 0u64;
    let mut modified = fs::symlink_metadata(path)
        .with_context(|| format!("读取插件 stream cache bucket metadata 失败: {}", path.display()))?
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut regular_files = 0usize;
    let mut has_identity = false;
    let mut has_audio = false;

    for child in fs::read_dir(path)
        .with_context(|| format!("读取插件 stream cache bucket 失败: {}", path.display()))?
    {
        let child = child.context("读取插件 stream cache bucket 目录项失败")?;
        let child_path = child.path();
        let file_type = child.file_type().with_context(|| {
            format!("读取插件 stream cache bucket 项类型失败: {}", child_path.display())
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Ok(None);
        }
        let metadata = child.metadata().with_context(|| {
            format!("读取插件 stream cache 文件 metadata 失败: {}", child_path.display())
        })?;
        bytes = bytes.saturating_add(metadata.len());
        modified = modified.max(metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        regular_files += 1;
        let name = child.file_name();
        if name == "identity.bin" {
            has_identity = true;
        } else if name.to_string_lossy().starts_with("audio.") {
            has_audio = true;
        } else {
            return Ok(None);
        }
    }

    if regular_files != 2 || !has_identity || !has_audio || bytes == 0 {
        return Ok(None);
    }
    Ok(Some(CacheBucket {
        path: path.to_path_buf(),
        bytes,
        modified,
    }))
}

fn pinned_snapshot() -> Result<HashSet<PathBuf>> {
    let pins = pinned_buckets()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache pin registry 锁已损坏: {error}"))?;
    Ok(pins
        .iter()
        .filter_map(|(path, count)| (*count > 0).then_some(path.clone()))
        .collect())
}

fn is_locator_bucket(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    name.len() == 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_temp_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.starts_with('.') && name.contains(".tmp-"))
}

fn path_age(path: &Path) -> Option<Duration> {
    fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
}

fn remove_direct_entry(path: &Path, directory: bool) -> Result<()> {
    if directory {
        fs::remove_dir_all(path)
            .with_context(|| format!("删除插件 stream cache 目录失败: {}", path.display()))
    } else {
        fs::remove_file(path)
            .with_context(|| format!("删除插件 stream cache 文件失败: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        env,
        sync::atomic::{AtomicU64, Ordering},
    };

    static TEST_NONCE: AtomicU64 = AtomicU64::new(1);

    fn temp_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "yinqidao-stream-gc-{name}-{}-{}",
            std::process::id(),
            TEST_NONCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn create_bucket(root: &Path, name: &str, audio_bytes: usize) -> PathBuf {
        let bucket = root.join(name);
        fs::create_dir_all(&bucket).expect("bucket");
        fs::write(bucket.join("identity.bin"), b"i").expect("identity");
        fs::write(bucket.join("audio.flac"), vec![0u8; audio_bytes]).expect("audio");
        bucket
    }

    #[test]
    fn locator_names_are_strict_lower_hex() {
        assert!(is_locator_bucket(Path::new("0123456789abcdef0123456789abcdef")));
        assert!(!is_locator_bucket(Path::new("0123456789ABCDEF0123456789ABCDEF")));
        assert!(!is_locator_bucket(Path::new("../0123456789abcdef0123456789abcdef")));
        assert!(!is_locator_bucket(Path::new("short")));
    }

    #[test]
    fn capacity_gc_never_evicts_pinned_bucket() {
        let root = temp_root("pin");
        fs::create_dir_all(&root).expect("root");
        let pinned_bucket = create_bucket(&root, "11111111111111111111111111111111", 4);
        let _other_a = create_bucket(&root, "22222222222222222222222222222222", 4);
        let _other_b = create_bucket(&root, "33333333333333333333333333333333", 4);
        let lease = pin_materialized_path(&root, &pinned_bucket.join("audio.flac")).expect("lease");

        let stats = prune(
            &root,
            PluginStreamCacheGcPolicy {
                max_cache_bytes: 10,
                target_cache_bytes: 5,
                cache_ttl: Duration::from_secs(60 * 60),
                stale_temp_ttl: Duration::from_secs(60 * 60),
            },
        )
        .expect("prune");
        assert!(pinned_bucket.is_dir());
        assert_eq!(lease.bucket(), pinned_bucket.as_path());
        assert!(stats.removed_capacity_buckets >= 2);
        assert!(stats.cache_bytes_after <= 5);

        drop(lease);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_bucket_is_removed_without_following_unknown_directories() {
        let root = temp_root("invalid");
        fs::create_dir_all(&root).expect("root");
        let malformed = root.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        fs::create_dir_all(&malformed).expect("malformed");
        fs::write(malformed.join("unexpected.bin"), b"x").expect("unexpected");
        let unknown = root.join("user-folder");
        fs::create_dir_all(&unknown).expect("unknown");
        fs::write(unknown.join("keep.txt"), b"keep").expect("keep");

        let stats = prune(&root, PluginStreamCacheGcPolicy::default()).expect("prune");
        assert!(!malformed.exists());
        assert!(unknown.join("keep.txt").is_file());
        assert_eq!(stats.removed_invalid_entries, 1);

        let _ = fs::remove_dir_all(&root);
    }
}
