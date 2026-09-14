use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    thread,
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow, bail};

use super::lifecycle::PluginInstanceKey;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const CACHE_BUCKET_PREFIX: &str = "component-v1-";

static PLUGIN_GC: OnceLock<Arc<PluginGcController>> = OnceLock::new();

/// Host-owned resource collection policy.
///
/// Guest components never receive this policy and cannot request, postpone or cancel a collection.
/// The Wasmtime adapter must obtain warm pools from `PluginGcController::new_instance_pool` instead
/// of exposing a guest-visible `gc()` operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginGcPolicy {
    /// Low-frequency Host sweep cadence. The sweep runs on a dedicated standard thread, never on
    /// GPUI and never on the realtime audio callback.
    pub sweep_interval: Duration,
    /// Warm Store/Instance bundles older than this are dropped automatically.
    pub warm_instance_idle_ttl: Duration,
    /// Host target for compiled Component idle retention. The Wasmtime adapter maps its in-process
    /// compiled cache to this value once generated bindings land.
    pub compiled_component_idle_ttl: Duration,
    /// Per plugin/provider route warm-instance bound.
    pub max_warm_instances_per_route: usize,
    /// Process-wide warm-instance bound across all plugins/providers.
    pub max_warm_instances_total: usize,
    /// Process-wide in-memory compiled Component target.
    pub max_compiled_components: usize,
    /// Host-owned serialized Wasmtime cache budget.
    pub max_disk_cache_bytes: u64,
    /// Buckets older than this are eligible for deletion regardless of total cache pressure.
    pub disk_cache_max_age: Duration,
}

impl Default for PluginGcPolicy {
    fn default() -> Self {
        Self {
            sweep_interval: Duration::from_secs(30),
            warm_instance_idle_ttl: Duration::from_secs(2 * 60),
            compiled_component_idle_ttl: Duration::from_secs(10 * 60),
            max_warm_instances_per_route: 2,
            max_warm_instances_total: 32,
            max_compiled_components: 16,
            max_disk_cache_bytes: 512 * MIB,
            disk_cache_max_age: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

impl PluginGcPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.sweep_interval < Duration::from_secs(5)
            || self.sweep_interval > Duration::from_secs(60 * 60)
        {
            bail!("插件 GC sweep interval 必须在 5 秒..=1 小时");
        }
        if self.warm_instance_idle_ttl < self.sweep_interval {
            bail!("插件 warm instance idle TTL 不能短于 GC sweep interval");
        }
        if self.compiled_component_idle_ttl < self.warm_instance_idle_ttl {
            bail!("compiled Component idle TTL 不能短于 warm instance TTL");
        }
        if self.max_warm_instances_per_route == 0 || self.max_warm_instances_per_route > 16 {
            bail!("插件单 route warm instance 上限必须在 1..=16");
        }
        if self.max_warm_instances_total < self.max_warm_instances_per_route
            || self.max_warm_instances_total > 256
        {
            bail!("插件全局 warm instance 上限非法");
        }
        if self.max_compiled_components == 0 || self.max_compiled_components > 128 {
            bail!("插件 compiled Component 内存缓存上限必须在 1..=128");
        }
        if self.max_disk_cache_bytes < 16 * MIB || self.max_disk_cache_bytes > 4 * GIB {
            bail!("插件 compiled disk cache 预算必须在 16 MiB..=4 GiB");
        }
        if self.disk_cache_max_age < Duration::from_secs(60 * 60) {
            bail!("插件 compiled disk cache 保留期至少为 1 小时");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PluginGcStats {
    pub released_memory_resources: usize,
    pub removed_disk_buckets: usize,
    pub disk_bytes_before: u64,
    pub disk_bytes_after: u64,
}

trait PluginGcTarget: Send + Sync {
    fn collect_idle(&self) -> Result<usize>;
}

#[derive(Debug)]
struct PooledInstance<I> {
    instance: I,
    last_used: Instant,
    sequence: u64,
}

#[derive(Debug)]
struct InstancePoolState<I> {
    available: HashMap<PluginInstanceKey, Vec<PooledInstance<I>>>,
    sequence: u64,
}

impl<I> Default for InstancePoolState<I> {
    fn default() -> Self {
        Self {
            available: HashMap::new(),
            sequence: 0,
        }
    }
}

impl<I> InstancePoolState<I> {
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn len(&self) -> usize {
        self.available.values().map(Vec::len).sum()
    }
}

/// Host-owned warm runtime pool with automatic idle collection and process-wide LRU pressure trim.
///
/// `I` will be the private Wasmtime Store/Instance bundle. The guest never observes this pool and
/// never controls retention. Dropping a pooled value is the collection operation.
pub struct HostOwnedInstancePool<I> {
    policy: PluginGcPolicy,
    state: Mutex<InstancePoolState<I>>,
}

impl<I> std::fmt::Debug for HostOwnedInstancePool<I> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pooled = self
            .state
            .lock()
            .map(|state| state.len())
            .unwrap_or_default();
        formatter
            .debug_struct("HostOwnedInstancePool")
            .field("pooled_instances", &pooled)
            .field(
                "max_warm_instances_total",
                &self.policy.max_warm_instances_total,
            )
            .finish()
    }
}

impl<I> HostOwnedInstancePool<I> {
    fn new(policy: PluginGcPolicy) -> Self {
        Self {
            policy,
            state: Mutex::new(InstancePoolState::default()),
        }
    }

    pub fn checkout(&self, key: &PluginInstanceKey) -> Result<Option<I>> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        collect_idle_locked(&mut state, self.policy.warm_instance_idle_ttl);
        let instance = state
            .available
            .get_mut(key)
            .and_then(Vec::pop)
            .map(|pooled| pooled.instance);
        if state.available.get(key).is_some_and(Vec::is_empty) {
            state.available.remove(key);
        }
        Ok(instance)
    }

    /// Returns `false` when the route is already at its warm capacity. The caller simply drops the
    /// instance in that case; no guest callback is involved in the decision.
    pub fn checkin(&self, key: PluginInstanceKey, instance: I) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        collect_idle_locked(&mut state, self.policy.warm_instance_idle_ttl);
        if state
            .available
            .get(&key)
            .is_some_and(|instances| instances.len() >= self.policy.max_warm_instances_per_route)
        {
            return Ok(false);
        }
        let sequence = state.next_sequence();
        state.available.entry(key).or_default().push(PooledInstance {
            instance,
            last_used: Instant::now(),
            sequence,
        });
        trim_total_lru(&mut state, self.policy.max_warm_instances_total);
        Ok(true)
    }

    pub fn invalidate_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        let before = state.len();
        state.available.retain(|key, _| key.plugin_id != plugin_id);
        Ok(before.saturating_sub(state.len()))
    }

    pub fn invalidate_component(&self, compiled_cache_key: &str) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        let before = state.len();
        state
            .available
            .retain(|key, _| key.compiled_cache_key != compiled_cache_key);
        Ok(before.saturating_sub(state.len()))
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        let removed = state.len();
        state.available.clear();
        Ok(removed)
    }

    pub fn pooled_len(&self) -> Result<usize> {
        self.state
            .lock()
            .map(|state| state.len())
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))
    }
}

impl<I> PluginGcTarget for HostOwnedInstancePool<I>
where
    I: Send + 'static,
{
    fn collect_idle(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 Host GC instance pool 锁已损坏: {error}"))?;
        Ok(collect_idle_locked(
            &mut state,
            self.policy.warm_instance_idle_ttl,
        ))
    }
}

fn collect_idle_locked<I>(state: &mut InstancePoolState<I>, idle_ttl: Duration) -> usize {
    let now = Instant::now();
    let before = state.len();
    state.available.retain(|_, instances| {
        instances.retain(|instance| now.duration_since(instance.last_used) < idle_ttl);
        !instances.is_empty()
    });
    before.saturating_sub(state.len())
}

fn trim_total_lru<I>(state: &mut InstancePoolState<I>, max_total: usize) {
    while state.len() > max_total {
        let victim = state
            .available
            .iter()
            .filter_map(|(key, instances)| {
                instances
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, instance)| instance.sequence)
                    .map(|(index, instance)| (key.clone(), index, instance.sequence))
            })
            .min_by_key(|(_, _, sequence)| *sequence);
        let Some((key, index, _)) = victim else {
            break;
        };
        if let Some(instances) = state.available.get_mut(&key) {
            instances.swap_remove(index);
            if instances.is_empty() {
                state.available.remove(&key);
            }
        }
    }
}

#[derive(Debug)]
pub struct PluginGcController {
    cache_root: PathBuf,
    policy: PluginGcPolicy,
    targets: Mutex<Vec<Weak<dyn PluginGcTarget>>>,
}

impl PluginGcController {
    pub fn new(cache_root: PathBuf, policy: PluginGcPolicy) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            cache_root,
            policy,
            targets: Mutex::new(Vec::new()),
        })
    }

    pub fn policy(&self) -> &PluginGcPolicy {
        &self.policy
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Create and register a Host-owned pool. This is the only warm-pool construction path the
    /// Wasmtime adapter should use.
    pub fn new_instance_pool<I>(self: &Arc<Self>) -> Result<Arc<HostOwnedInstancePool<I>>>
    where
        I: Send + 'static,
    {
        let pool = Arc::new(HostOwnedInstancePool::new(self.policy.clone()));
        let target: Arc<dyn PluginGcTarget> = pool.clone();
        self.targets
            .lock()
            .map_err(|error| anyhow!("插件 GC target registry 锁已损坏: {error}"))?
            .push(Arc::downgrade(&target));
        Ok(pool)
    }

    pub fn sweep_once(&self) -> Result<PluginGcStats> {
        let targets = {
            let mut registered = self
                .targets
                .lock()
                .map_err(|error| anyhow!("插件 GC target registry 锁已损坏: {error}"))?;
            let live = registered.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            registered.retain(|target| target.strong_count() > 0);
            live
        };

        let mut stats = PluginGcStats::default();
        for target in targets {
            stats.released_memory_resources = stats
                .released_memory_resources
                .saturating_add(target.collect_idle()?);
        }
        let disk = collect_disk_cache(&self.cache_root, &self.policy)?;
        stats.removed_disk_buckets = disk.removed_disk_buckets;
        stats.disk_bytes_before = disk.disk_bytes_before;
        stats.disk_bytes_after = disk.disk_bytes_after;
        Ok(stats)
    }

    fn start_background(self: &Arc<Self>) -> Result<()> {
        let weak = Arc::downgrade(self);
        let interval = self.policy.sweep_interval;
        thread::Builder::new()
            .name("yinqidao-plugin-gc".into())
            .spawn(move || loop {
                thread::sleep(interval);
                let Some(controller) = weak.upgrade() else {
                    break;
                };
                match controller.sweep_once() {
                    Ok(stats)
                        if stats.released_memory_resources > 0 || stats.removed_disk_buckets > 0 =>
                    {
                        tracing::debug!(
                            released_memory_resources = stats.released_memory_resources,
                            removed_disk_buckets = stats.removed_disk_buckets,
                            disk_bytes_before = stats.disk_bytes_before,
                            disk_bytes_after = stats.disk_bytes_after,
                            "Host 插件 GC 完成资源回收"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "Host 插件 GC sweep 失败"),
                }
            })
            .context("启动插件 Host GC 线程失败")?;
        Ok(())
    }
}

fn collect_disk_cache(cache_root: &Path, policy: &PluginGcPolicy) -> Result<PluginGcStats> {
    fs::create_dir_all(cache_root)
        .with_context(|| format!("创建插件 compiled cache root 失败: {}", cache_root.display()))?;
    let canonical_root = fs::canonicalize(cache_root)
        .with_context(|| format!("规范化插件 compiled cache root 失败: {}", cache_root.display()))?;

    #[derive(Debug)]
    struct Bucket {
        path: PathBuf,
        bytes: u64,
        modified: SystemTime,
        expired: bool,
    }

    let now = SystemTime::now();
    let mut buckets = Vec::new();
    for entry in fs::read_dir(&canonical_root).context("读取插件 compiled cache root 失败")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(%error, "读取插件 compiled cache entry 失败");
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(CACHE_BUCKET_PREFIX) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let canonical = match fs::canonicalize(entry.path()) {
            Ok(path) if path.starts_with(&canonical_root) => path,
            _ => continue,
        };

        let mut bytes = 0_u64;
        let mut modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let children = match fs::read_dir(&canonical) {
            Ok(children) => children,
            Err(_) => continue,
        };
        for child in children.flatten() {
            let child_meta = match fs::symlink_metadata(child.path()) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if !child_meta.file_type().is_file() {
                continue;
            }
            bytes = bytes.saturating_add(child_meta.len());
            if let Ok(child_modified) = child_meta.modified()
                && child_modified > modified
            {
                modified = child_modified;
            }
        }
        let expired = now
            .duration_since(modified)
            .map(|age| age >= policy.disk_cache_max_age)
            .unwrap_or(false);
        buckets.push(Bucket {
            path: canonical,
            bytes,
            modified,
            expired,
        });
    }

    let disk_bytes_before = buckets.iter().map(|bucket| bucket.bytes).sum::<u64>();
    let mut removed_disk_buckets = 0usize;
    let mut remaining_bytes = disk_bytes_before;

    for bucket in buckets.iter().filter(|bucket| bucket.expired) {
        if safe_remove_bucket(&canonical_root, &bucket.path)? {
            removed_disk_buckets = removed_disk_buckets.saturating_add(1);
            remaining_bytes = remaining_bytes.saturating_sub(bucket.bytes);
        }
    }

    if remaining_bytes > policy.max_disk_cache_bytes {
        buckets.sort_by_key(|bucket| bucket.modified);
        for bucket in &buckets {
            if remaining_bytes <= policy.max_disk_cache_bytes {
                break;
            }
            if !bucket.path.exists() {
                continue;
            }
            if safe_remove_bucket(&canonical_root, &bucket.path)? {
                removed_disk_buckets = removed_disk_buckets.saturating_add(1);
                remaining_bytes = remaining_bytes.saturating_sub(bucket.bytes);
            }
        }
    }

    Ok(PluginGcStats {
        released_memory_resources: 0,
        removed_disk_buckets,
        disk_bytes_before,
        disk_bytes_after: remaining_bytes,
    })
}

fn safe_remove_bucket(cache_root: &Path, bucket: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(bucket) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("读取插件 cache bucket metadata 失败"),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    let canonical = fs::canonicalize(bucket)
        .with_context(|| format!("规范化插件 cache bucket 失败: {}", bucket.display()))?;
    if !canonical.starts_with(cache_root) {
        bail!("拒绝删除 compiled cache root 外目录");
    }
    let Some(name) = canonical.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    if !name.starts_with(CACHE_BUCKET_PREFIX) {
        return Ok(false);
    }
    fs::remove_dir_all(&canonical)
        .with_context(|| format!("删除插件 cache bucket 失败: {}", canonical.display()))?;
    Ok(true)
}

pub fn initialize(cache_root: PathBuf, policy: PluginGcPolicy) -> Result<Arc<PluginGcController>> {
    if let Some(existing) = PLUGIN_GC.get() {
        return Ok(existing.clone());
    }
    let controller = Arc::new(PluginGcController::new(cache_root, policy)?);
    controller.start_background()?;
    let _ = PLUGIN_GC.set(controller.clone());
    Ok(PLUGIN_GC.get().cloned().unwrap_or(controller))
}

pub fn global() -> Option<Arc<PluginGcController>> {
    PLUGIN_GC.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(plugin: &str, route: &str, component: &str) -> PluginInstanceKey {
        PluginInstanceKey {
            plugin_id: plugin.into(),
            provider_id: Some(route.into()),
            compiled_cache_key: component.into(),
        }
    }

    #[test]
    fn pool_enforces_global_and_per_route_limits_without_guest_gc() {
        let policy = PluginGcPolicy {
            max_warm_instances_per_route: 2,
            max_warm_instances_total: 2,
            ..PluginGcPolicy::default()
        };
        let controller = Arc::new(PluginGcController::new("cache".into(), policy).expect("gc"));
        let pool = controller.new_instance_pool::<u32>().expect("pool");
        assert!(pool.checkin(key("a", "qq", "c1"), 1).expect("checkin"));
        assert!(pool.checkin(key("a", "qq", "c1"), 2).expect("checkin"));
        assert!(!pool.checkin(key("a", "qq", "c1"), 3).expect("route full"));
        assert!(pool.checkin(key("b", "ne", "c2"), 4).expect("global trim"));
        assert_eq!(pool.pooled_len().expect("len"), 2);
    }

    #[test]
    fn invalidating_plugin_drops_only_its_host_resources() {
        let controller = Arc::new(
            PluginGcController::new("cache".into(), PluginGcPolicy::default()).expect("gc"),
        );
        let pool = controller.new_instance_pool::<u32>().expect("pool");
        pool.checkin(key("a", "qq", "c1"), 1).expect("checkin");
        pool.checkin(key("b", "qq", "c2"), 2).expect("checkin");
        assert_eq!(pool.invalidate_plugin("a").expect("invalidate"), 1);
        assert_eq!(pool.pooled_len().expect("len"), 1);
    }
}
