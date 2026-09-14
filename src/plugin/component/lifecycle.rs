use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex},
};

use anyhow::{Result, anyhow};

use super::registry::PluginComponentSnapshot;

#[derive(Debug)]
enum CompileEntryState<C> {
    Compiling,
    Ready(Arc<C>),
}

#[derive(Debug)]
struct CompileEntry<C> {
    plugin_id: String,
    plugin_generation: u64,
    global_generation: u64,
    last_used: u64,
    state: CompileEntryState<C>,
}

#[derive(Debug)]
struct CompileCacheState<C> {
    entries: HashMap<String, CompileEntry<C>>,
    plugin_generations: HashMap<String, u64>,
    global_generation: u64,
    clock: u64,
}

impl<C> Default for CompileCacheState<C> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            plugin_generations: HashMap::new(),
            global_generation: 0,
            clock: 0,
        }
    }
}

impl<C> CompileCacheState<C> {
    fn plugin_generation(&self, plugin_id: &str) -> u64 {
        self.plugin_generations.get(plugin_id).copied().unwrap_or(0)
    }

    fn next_clock(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }
}

/// In-process lazy cache for a runtime-specific compiled Component object.
///
/// `C` is deliberately generic so Wasmtime types remain private to `plugin::component`. The future
/// Wasmtime adapter will use `C = wasmtime::component::Component`, while compilation/deserialization
/// policy stays outside OnlineServices and the Host security layer.
///
/// Expensive compilation never runs while the cache mutex is held. Concurrent requests for the same
/// immutable snapshot wait for the current compiler instead of compiling the same Component twice.
/// Failed compilation does not poison the key: the placeholder is removed and a later call may retry.
pub struct LazyCompiledComponentCache<C> {
    max_ready_entries: usize,
    state: Mutex<CompileCacheState<C>>,
    wake: Condvar,
}

impl<C> std::fmt::Debug for LazyCompiledComponentCache<C> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ready_entries = self
            .state
            .lock()
            .map(|state| {
                state
                    .entries
                    .values()
                    .filter(|entry| matches!(&entry.state, CompileEntryState::Ready(_)))
                    .count()
            })
            .unwrap_or_default();
        formatter
            .debug_struct("LazyCompiledComponentCache")
            .field("max_ready_entries", &self.max_ready_entries)
            .field("ready_entries", &ready_entries)
            .finish()
    }
}

impl<C> LazyCompiledComponentCache<C>
where
    C: Send + Sync + 'static,
{
    pub fn new(max_ready_entries: usize) -> Self {
        Self {
            max_ready_entries: max_ready_entries.max(1),
            state: Mutex::new(CompileCacheState::default()),
            wake: Condvar::new(),
        }
    }

    /// Return a compiled object for `snapshot`, compiling it exactly once for concurrent callers.
    ///
    /// The compile closure receives the already validated immutable byte snapshot. A Wasmtime
    /// implementation must compile or deserialize from those bytes/cache paths rather than reopening
    /// the package path, preserving the registry's validation-to-use boundary.
    pub fn get_or_try_compile<F>(
        &self,
        snapshot: &PluginComponentSnapshot,
        compile: F,
    ) -> Result<Arc<C>>
    where
        F: FnOnce(&PluginComponentSnapshot) -> Result<C>,
    {
        let cache_key = snapshot.compiled_cache_key.clone();
        let plugin_id = snapshot.plugin_id.clone();
        let mut compile = Some(compile);

        loop {
            let mut state = self
                .state
                .lock()
                .map_err(|error| anyhow!("插件 compiled Component cache 锁已损坏: {error}"))?;

            if let Some(entry) = state.entries.get(&cache_key) {
                match &entry.state {
                    CompileEntryState::Ready(component) => {
                        let component = component.clone();
                        let clock = state.next_clock();
                        if let Some(entry) = state.entries.get_mut(&cache_key) {
                            entry.last_used = clock;
                        }
                        return Ok(component);
                    }
                    CompileEntryState::Compiling => {
                        state = self.wake.wait(state).map_err(|error| {
                            anyhow!("插件 compiled Component cache 等待锁已损坏: {error}")
                        })?;
                        drop(state);
                        continue;
                    }
                }
            }

            let plugin_generation = state.plugin_generation(&plugin_id);
            let global_generation = state.global_generation;
            let clock = state.next_clock();
            state.entries.insert(
                cache_key.clone(),
                CompileEntry {
                    plugin_id: plugin_id.clone(),
                    plugin_generation,
                    global_generation,
                    last_used: clock,
                    state: CompileEntryState::Compiling,
                },
            );
            drop(state);

            let compile_result = compile
                .take()
                .expect("compile closure is consumed only by the owning compiler")
                (snapshot)
                .map(Arc::new);

            let mut state = self
                .state
                .lock()
                .map_err(|error| anyhow!("插件 compiled Component cache 锁已损坏: {error}"))?;
            let generation_still_current = state.global_generation == global_generation
                && state.plugin_generation(&plugin_id) == plugin_generation;
            let owns_placeholder = state.entries.get(&cache_key).is_some_and(|entry| {
                entry.plugin_id == plugin_id
                    && entry.plugin_generation == plugin_generation
                    && entry.global_generation == global_generation
                    && matches!(&entry.state, CompileEntryState::Compiling)
            });

            if !generation_still_current || !owns_placeholder {
                if owns_placeholder {
                    state.entries.remove(&cache_key);
                }
                self.wake.notify_all();
                return Err(anyhow!("插件 Component 在编译期间已失效，拒绝发布旧编译结果"));
            }

            match compile_result {
                Ok(component) => {
                    let clock = state.next_clock();
                    state.entries.insert(
                        cache_key.clone(),
                        CompileEntry {
                            plugin_id,
                            plugin_generation,
                            global_generation,
                            last_used: clock,
                            state: CompileEntryState::Ready(component.clone()),
                        },
                    );
                    evict_ready_lru(&mut state, self.max_ready_entries, &cache_key);
                    self.wake.notify_all();
                    return Ok(component);
                }
                Err(error) => {
                    state.entries.remove(&cache_key);
                    self.wake.notify_all();
                    return Err(error);
                }
            }
        }
    }

    /// Invalidate all compiled objects for one plugin. In-flight compilation observes the generation
    /// change and cannot publish a stale result after this method returns.
    pub fn invalidate_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 compiled Component cache 锁已损坏: {error}"))?;
        let generation = state
            .plugin_generations
            .entry(plugin_id.to_owned())
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        let before = state.entries.len();
        state.entries.retain(|_, entry| entry.plugin_id != plugin_id);
        let removed = before.saturating_sub(state.entries.len());
        self.wake.notify_all();
        Ok(removed)
    }

    /// Clear every in-process compiled object. The global generation prevents an old in-flight
    /// compiler from repopulating the cache after the clear.
    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 compiled Component cache 锁已损坏: {error}"))?;
        state.global_generation = state.global_generation.wrapping_add(1);
        let removed = state.entries.len();
        state.entries.clear();
        self.wake.notify_all();
        Ok(removed)
    }

    pub fn ready_len(&self) -> Result<usize> {
        let state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 compiled Component cache 锁已损坏: {error}"))?;
        Ok(state
            .entries
            .values()
            .filter(|entry| matches!(&entry.state, CompileEntryState::Ready(_)))
            .count())
    }
}

fn evict_ready_lru<C>(
    state: &mut CompileCacheState<C>,
    max_ready_entries: usize,
    protected_key: &str,
) {
    loop {
        let ready_count = state
            .entries
            .values()
            .filter(|entry| matches!(&entry.state, CompileEntryState::Ready(_)))
            .count();
        if ready_count <= max_ready_entries {
            break;
        }

        let victim = state
            .entries
            .iter()
            .filter(|(key, entry)| {
                key.as_str() != protected_key
                    && matches!(&entry.state, CompileEntryState::Ready(_))
            })
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            break;
        };
        state.entries.remove(&victim);
    }
}

/// Identity of a warm runtime instance. The compiled cache key is part of the identity so an
/// instance created from an old Component can never be checked out for a newly installed version.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginInstanceKey {
    pub plugin_id: String,
    pub provider_id: Option<String>,
    pub compiled_cache_key: String,
}

impl PluginInstanceKey {
    pub fn plugin(snapshot: &PluginComponentSnapshot) -> Self {
        Self {
            plugin_id: snapshot.plugin_id.clone(),
            provider_id: None,
            compiled_cache_key: snapshot.compiled_cache_key.clone(),
        }
    }

    pub fn provider(snapshot: &PluginComponentSnapshot, provider_id: impl Into<String>) -> Self {
        Self {
            plugin_id: snapshot.plugin_id.clone(),
            provider_id: Some(provider_id.into()),
            compiled_cache_key: snapshot.compiled_cache_key.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInstanceTicket {
    key: PluginInstanceKey,
    plugin_generation: u64,
    global_generation: u64,
}

#[derive(Debug)]
struct InstancePoolState<I> {
    available: HashMap<PluginInstanceKey, Vec<I>>,
    plugin_generations: HashMap<String, u64>,
    global_generation: u64,
}

impl<I> Default for InstancePoolState<I> {
    fn default() -> Self {
        Self {
            available: HashMap::new(),
            plugin_generations: HashMap::new(),
            global_generation: 0,
        }
    }
}

impl<I> InstancePoolState<I> {
    fn plugin_generation(&self, plugin_id: &str) -> u64 {
        self.plugin_generations.get(plugin_id).copied().unwrap_or(0)
    }
}

/// Small warm-instance pool used by the future Wasmtime adapter.
///
/// A checked-out instance carries a generation ticket. Plugin update/uninstall invalidation makes
/// every older ticket stale, so an in-flight call cannot reinsert an obsolete Store/Instance bundle
/// after the pool was cleared. No lock is held while guest code runs.
pub struct WarmComponentInstancePool<I> {
    max_per_route: usize,
    state: Mutex<InstancePoolState<I>>,
}

impl<I> std::fmt::Debug for WarmComponentInstancePool<I> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pooled = self
            .state
            .lock()
            .map(|state| state.available.values().map(Vec::len).sum::<usize>())
            .unwrap_or_default();
        formatter
            .debug_struct("WarmComponentInstancePool")
            .field("max_per_route", &self.max_per_route)
            .field("pooled_instances", &pooled)
            .finish()
    }
}

impl<I> WarmComponentInstancePool<I> {
    pub fn new(max_per_route: usize) -> Self {
        Self {
            max_per_route: max_per_route.max(1),
            state: Mutex::new(InstancePoolState::default()),
        }
    }

    pub fn checkout(&self, key: PluginInstanceKey) -> Result<(PluginInstanceTicket, Option<I>)> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 warm instance pool 锁已损坏: {error}"))?;
        let plugin_generation = state.plugin_generation(&key.plugin_id);
        let global_generation = state.global_generation;
        let instance = state.available.get_mut(&key).and_then(Vec::pop);
        if state.available.get(&key).is_some_and(Vec::is_empty) {
            state.available.remove(&key);
        }
        Ok((
            PluginInstanceTicket {
                key,
                plugin_generation,
                global_generation,
            },
            instance,
        ))
    }

    /// Return a warm instance. `false` means the instance must be dropped because the pool was
    /// invalidated while it was checked out or the per-route capacity is already full.
    pub fn checkin(&self, ticket: PluginInstanceTicket, instance: I) -> Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 warm instance pool 锁已损坏: {error}"))?;
        if state.global_generation != ticket.global_generation
            || state.plugin_generation(&ticket.key.plugin_id) != ticket.plugin_generation
        {
            return Ok(false);
        }
        let instances = state.available.entry(ticket.key).or_default();
        if instances.len() >= self.max_per_route {
            return Ok(false);
        }
        instances.push(instance);
        Ok(true)
    }

    pub fn invalidate_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 warm instance pool 锁已损坏: {error}"))?;
        let generation = state
            .plugin_generations
            .entry(plugin_id.to_owned())
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        let mut removed = 0usize;
        state.available.retain(|key, instances| {
            if key.plugin_id == plugin_id {
                removed = removed.saturating_add(instances.len());
                false
            } else {
                true
            }
        });
        Ok(removed)
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 warm instance pool 锁已损坏: {error}"))?;
        state.global_generation = state.global_generation.wrapping_add(1);
        let removed = state.available.values().map(Vec::len).sum();
        state.available.clear();
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn snapshot(plugin_id: &str, key: &str) -> PluginComponentSnapshot {
        PluginComponentSnapshot {
            plugin_id: plugin_id.into(),
            plugin_version: "0.1.0".into(),
            source_path: "plugin.wasm".into(),
            bytes: Arc::<[u8]>::from([0_u8, 97, 115, 109]),
            locator_digest_hex: "test".into(),
            compiled_cache_key: key.into(),
            cache_dir: "cache".into(),
            source_verifier_path: "cache/source.wasm".into(),
            compiled_cache_path: "cache/component.cwasm".into(),
        }
    }

    #[test]
    fn repeated_snapshot_compiles_once() {
        let cache = LazyCompiledComponentCache::new(4);
        let snapshot = snapshot("plugin.a", "component-a");
        let calls = AtomicUsize::new(0);
        let first = cache
            .get_or_try_compile(&snapshot, |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, anyhow::Error>(String::from("compiled"))
            })
            .expect("compile");
        let second = cache
            .get_or_try_compile(&snapshot, |_| {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, anyhow::Error>(String::from("should-not-run"))
            })
            .expect("cached");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn compile_failure_does_not_poison_key() {
        let cache = LazyCompiledComponentCache::new(2);
        let snapshot = snapshot("plugin.a", "component-a");
        assert!(cache.get_or_try_compile(&snapshot, |_| Err(anyhow!("boom"))).is_err());
        assert_eq!(cache.ready_len().expect("ready len"), 0);
        assert_eq!(
            cache
                .get_or_try_compile(&snapshot, |_| Ok::<_, anyhow::Error>(7_u32))
                .expect("retry")
                .as_ref(),
            &7
        );
    }

    #[test]
    fn stale_instance_ticket_cannot_repopulate_after_invalidation() {
        let pool = WarmComponentInstancePool::new(2);
        let key = PluginInstanceKey::provider(&snapshot("plugin.a", "component-a"), "netease");
        let (ticket, instance) = pool.checkout(key.clone()).expect("checkout");
        assert!(instance.is_none());
        pool.invalidate_plugin("plugin.a").expect("invalidate");
        assert!(!pool.checkin(ticket, 42_u32).expect("checkin"));

        let (ticket, instance) = pool.checkout(key).expect("checkout after invalidate");
        assert!(instance.is_none());
        assert!(pool.checkin(ticket, 7_u32).expect("fresh checkin"));
    }
}
