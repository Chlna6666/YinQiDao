use std::{
    collections::HashMap,
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
static RESERVED_BYTES: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

fn pinned_buckets() -> &'static Mutex<HashMap<PathBuf, usize>> {
    PINNED_BUCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn reserved_bytes() -> &'static Mutex<HashMap<PathBuf, u64>> {
    RESERVED_BYTES.get_or_init(|| Mutex::new(HashMap::new()))
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

/// In-process reservation for the worst-case bytes one active materialization may still create.
///
/// The reservation is intentionally held by both the async materializer and its blocking writer.
/// If the async future is cancelled, the writer therefore keeps the reservation until it has
/// observed the closed channel and removed its temp directory. No reservation bookkeeping runs on
/// the realtime audio callback.
#[derive(Debug)]
pub struct PluginStreamCacheReservation {
    root: PathBuf,
    bytes: u64,
}

impl Drop for PluginStreamCacheReservation {
    fn drop(&mut self) {
        let Ok(mut reserved) = reserved_bytes().lock() else {
            return;
        };
        let Some(current) = reserved.get_mut(&self.root) else {
            return;
        };
        if *current <= self.bytes {
            reserved.remove(&self.root);
        } else {
            *current -= self.bytes;
        }
    }
}

pub fn pin_materialized_path(
    root: &Path,
    materialized_path: &Path,
) -> Result<Arc<PluginStreamCacheLease>> {
    let bucket = materialized_path
        .parent()
        .ok_or_else(|| anyhow!("插件 stream cache materialized path 缺少 bucket"))?;
    if bucket.parent() != Some(root) || !is_locator_bucket(bucket) {
        bail!("插件 stream cache materialized path 不属于 Host cache root");
    }

    // Hold the same mutex used by GC from the first filesystem validation through pin publication.
    // Without this critical section GC could snapshot an unpinned bucket, then delete it after this
    // function validated the path but before the pin reached the registry.
    let mut pins = pinned_buckets()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache pin registry 锁已损坏: {error}"))?;
    let metadata = fs::symlink_metadata(bucket).with_context(|| {
        format!(
            "读取插件 stream cache bucket metadata 失败: {}",
            bucket.display()
        )
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
    *pins.entry(bucket.clone()).or_insert(0) += 1;
    drop(pins);
    Ok(Arc::new(PluginStreamCacheLease { bucket }))
}

/// Reserve worst-case disk headroom for one active stream download.
///
/// Reservation and the pre-download GC run while holding the same root-scoped accounting lock, so
/// concurrent materializations cannot both observe the same free capacity. Finalized cache bytes
/// plus every active reservation are kept at or below `max_cache_bytes`. If pinned playback buckets
/// prevent enough eviction, the new download is rejected before a temp audio file is created.
pub fn reserve_download_capacity(
    root: &Path,
    bytes: u64,
    policy: PluginStreamCacheGcPolicy,
) -> Result<Arc<PluginStreamCacheReservation>> {
    if bytes == 0 {
        bail!("插件 stream cache 磁盘预留必须大于 0 bytes");
    }
    let policy = policy.normalized();
    if bytes > policy.max_cache_bytes {
        bail!(
            "插件 stream cache 单次预留 {} bytes 超过总预算 {} bytes",
            bytes,
            policy.max_cache_bytes
        );
    }

    let root = root.to_path_buf();
    let mut reserved = reserved_bytes()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache reservation registry 锁已损坏: {error}"))?;
    let current_reserved = reserved.get(&root).copied().unwrap_or(0);

    // A reservation exists for every in-process writer before it can create its temp directory and
    // remains alive until cancellation cleanup finishes. Therefore, when this root has zero active
    // reservations, every `.tmp-*` entry is necessarily orphaned (including crash leftovers from a
    // previous process). Remove them synchronously before calculating new headroom instead of waiting
    // up to the normal 24 h stale-temp TTL and allowing unaccounted bytes to break the hard budget.
    if current_reserved == 0 {
        let removed = cleanup_orphan_temp_entries(&root)?;
        if removed > 0 {
            tracing::info!(removed, "已清理崩溃/异常退出遗留的插件 stream 临时目录");
        }
    }

    let total_reserved = current_reserved
        .checked_add(bytes)
        .ok_or_else(|| anyhow!("插件 stream cache reservation 长度溢出"))?;
    if total_reserved > policy.max_cache_bytes {
        bail!(
            "插件 stream cache 活跃下载预留超过总预算: reserved={total_reserved}, max={}",
            policy.max_cache_bytes
        );
    }

    let finalized_limit = policy.max_cache_bytes - total_reserved;
    let stats = prune(
        &root,
        PluginStreamCacheGcPolicy {
            max_cache_bytes: finalized_limit,
            target_cache_bytes: policy.target_cache_bytes.min(finalized_limit),
            cache_ttl: policy.cache_ttl,
            stale_temp_ttl: policy.stale_temp_ttl,
        },
    )?;
    if stats.cache_bytes_after > finalized_limit {
        bail!(
            "插件 stream cache 无法为下载预留磁盘空间: finalized={}, limit={}, pinned={}",
            stats.cache_bytes_after,
            finalized_limit,
            stats.pinned_buckets
        );
    }

    reserved.insert(root.clone(), total_reserved);
    Ok(Arc::new(PluginStreamCacheReservation { root, bytes }))
}

/// Run normal GC without consuming capacity already promised to active temp downloads.
///
/// Callers should release their own reservation after their temp directory has been atomically
/// committed, then use this helper for post-commit cleanup. Reservations belonging to other active
/// downloads continue to reduce the finalized-cache budget during the scan.
pub fn prune_preserving_reservations(
    root: &Path,
    policy: PluginStreamCacheGcPolicy,
) -> Result<PluginStreamCacheGcStats> {
    let policy = policy.normalized();
    let reserved = reserved_bytes()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache reservation registry 锁已损坏: {error}"))?;
    let active_reserved = reserved.get(root).copied().unwrap_or(0);
    let finalized_limit = policy.max_cache_bytes.saturating_sub(active_reserved);
    prune(
        root,
        PluginStreamCacheGcPolicy {
            max_cache_bytes: finalized_limit,
            target_cache_bytes: policy.target_cache_bytes.min(finalized_limit),
            cache_ttl: policy.cache_ttl,
            stale_temp_ttl: policy.stale_temp_ttl,
        },
    )
}

pub fn prune(root: &Path, policy: PluginStreamCacheGcPolicy) -> Result<PluginStreamCacheGcStats> {
    let policy = policy.normalized();
    ensure_plain_cache_root(root)?;
    // Keep the live pin registry locked for the whole scan/deletion transaction. Pin publication
    // uses this same mutex, eliminating the stale-snapshot TOCTOU that could otherwise delete a
    // bucket immediately after a decoder-facing materialization acquired its lease.
    let pins = pinned_buckets()
        .lock()
        .map_err(|error| anyhow!("插件 stream cache pin registry 锁已损坏: {error}"))?;
    let mut stats = PluginStreamCacheGcStats {
        pinned_buckets: pins.values().filter(|count| **count > 0).count(),
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
            if !pin_is_active(&pins, &path)
                && path_age(&path).is_some_and(|age| age > policy.stale_temp_ttl)
            {
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
            if !pin_is_active(&pins, &path) {
                remove_direct_entry(&path, true)?;
                stats.removed_invalid_entries += 1;
            }
            continue;
        };
        stats.cache_bytes_before = stats.cache_bytes_before.saturating_add(bucket.bytes);
        if !pin_is_active(&pins, &path)
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
            if pin_is_active(&pins, &bucket.path) {
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
            fs::create_dir_all(root)
                .with_context(|| format!("创建插件 stream cache root 失败: {}", root.display()))?;
            let metadata = fs::symlink_metadata(root)
                .with_context(|| format!("复核插件 stream cache root 失败: {}", root.display()))?;
            if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
                bail!("插件 stream cache root 创建后不是普通目录");
            }
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "读取插件 stream cache root metadata 失败: {}",
                    root.display()
                )
            });
        }
    }
    Ok(())
}

/// Remove Host-owned temp namespace entries only when the caller has already proven there are no
/// active in-process reservations for this root. Symlinks are unlinked rather than followed, and
/// unrelated directories are never touched.
fn cleanup_orphan_temp_entries(root: &Path) -> Result<usize> {
    ensure_plain_cache_root(root)?;
    let mut removed = 0usize;
    for entry in fs::read_dir(root)
        .with_context(|| format!("读取插件 stream cache root 失败: {}", root.display()))?
    {
        let entry = entry.context("读取插件 stream cache 临时目录项失败")?;
        let path = entry.path();
        if !is_temp_dir(&path) {
            continue;
        }
        let file_type = entry
            .file_type()
            .with_context(|| format!("读取插件 stream cache 临时项类型失败: {}", path.display()))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            remove_direct_entry(&path, true)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            remove_direct_entry(&path, false)?;
        } else {
            bail!(
                "插件 stream cache 临时命名空间存在不支持的文件系统对象: {}",
                path.display()
            );
        }
        removed += 1;
    }
    Ok(removed)
}

fn inspect_bucket(path: &Path) -> Result<Option<CacheBucket>> {
    let mut bytes = 0u64;
    let mut modified = fs::symlink_metadata(path)
        .with_context(|| {
            format!(
                "读取插件 stream cache bucket metadata 失败: {}",
                path.display()
            )
        })?
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
            format!(
                "读取插件 stream cache bucket 项类型失败: {}",
                child_path.display()
            )
        })?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Ok(None);
        }
        let metadata = child.metadata().with_context(|| {
            format!(
                "读取插件 stream cache 文件 metadata 失败: {}",
                child_path.display()
            )
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

fn pin_is_active(pins: &HashMap<PathBuf, usize>, path: &Path) -> bool {
    pins.get(path).is_some_and(|count| *count > 0)
}

fn is_locator_bucket(path: &Path) -> bool {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
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
        assert!(is_locator_bucket(Path::new(
            "0123456789abcdef0123456789abcdef"
        )));
        assert!(!is_locator_bucket(Path::new(
            "0123456789ABCDEF0123456789ABCDEF"
        )));
        assert!(!is_locator_bucket(Path::new(
            "../0123456789abcdef0123456789abcdef"
        )));
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
    fn download_reservations_cannot_overcommit_cache_budget() {
        let root = temp_root("reserve");
        fs::create_dir_all(&root).expect("root");
        let policy = PluginStreamCacheGcPolicy {
            max_cache_bytes: 10,
            target_cache_bytes: 10,
            cache_ttl: Duration::from_secs(60 * 60),
            stale_temp_ttl: Duration::from_secs(60 * 60),
        };

        let first = reserve_download_capacity(&root, 6, policy).expect("first reservation");
        assert!(reserve_download_capacity(&root, 5, policy).is_err());
        drop(first);
        let second = reserve_download_capacity(&root, 10, policy).expect("full reservation");
        drop(second);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn first_reservation_removes_orphan_temp_entries() {
        let root = temp_root("orphan-temp");
        fs::create_dir_all(&root).expect("root");
        let orphan = root.join(".0123456789abcdef0123456789abcdef.tmp-dead-process");
        fs::create_dir_all(&orphan).expect("orphan temp");
        fs::write(orphan.join("audio.media"), vec![0u8; 32]).expect("partial audio");
        let policy = PluginStreamCacheGcPolicy {
            max_cache_bytes: 128,
            target_cache_bytes: 128,
            cache_ttl: Duration::from_secs(60 * 60),
            stale_temp_ttl: Duration::from_secs(60 * 60),
        };

        let reservation = reserve_download_capacity(&root, 16, policy).expect("reservation");
        assert!(!orphan.exists());
        drop(reservation);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn active_reservation_preserves_peer_temp_entry() {
        let root = temp_root("active-temp");
        fs::create_dir_all(&root).expect("root");
        let policy = PluginStreamCacheGcPolicy {
            max_cache_bytes: 128,
            target_cache_bytes: 128,
            cache_ttl: Duration::from_secs(60 * 60),
            stale_temp_ttl: Duration::from_secs(60 * 60),
        };
        let first = reserve_download_capacity(&root, 16, policy).expect("first reservation");
        let active = root.join(".0123456789abcdef0123456789abcdef.tmp-active");
        fs::create_dir_all(&active).expect("active temp");
        fs::write(active.join("audio.media"), vec![0u8; 8]).expect("active partial audio");

        let second = reserve_download_capacity(&root, 16, policy).expect("second reservation");
        assert!(active.is_dir());
        drop(second);
        drop(first);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pinned_cache_can_block_a_new_download_reservation() {
        let root = temp_root("reserve-pinned");
        fs::create_dir_all(&root).expect("root");
        let bucket = create_bucket(&root, "44444444444444444444444444444444", 8);
        let lease = pin_materialized_path(&root, &bucket.join("audio.flac")).expect("lease");
        let policy = PluginStreamCacheGcPolicy {
            max_cache_bytes: 12,
            target_cache_bytes: 12,
            cache_ttl: Duration::from_secs(60 * 60),
            stale_temp_ttl: Duration::from_secs(60 * 60),
        };

        assert!(reserve_download_capacity(&root, 4, policy).is_err());
        drop(lease);
        let reservation = reserve_download_capacity(&root, 4, policy).expect("reservation");
        assert!(!bucket.exists());
        drop(reservation);

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
