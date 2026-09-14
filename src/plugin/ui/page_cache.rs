use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Result, anyhow, bail};

use super::{
    manifest::validate_local_id,
    schema::{UiPageModel, UiSchemaLimits, validate_page_model},
};

static PLUGIN_UI_PAGE_CACHE: OnceLock<Arc<PluginUiPageCache>> = OnceLock::new();

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginUiPageKey {
    pub plugin_id: String,
    pub page_id: String,
}

impl PluginUiPageKey {
    pub fn new(plugin_id: impl Into<String>, page_id: impl Into<String>) -> Result<Self> {
        let key = Self {
            plugin_id: plugin_id.into(),
            page_id: page_id.into(),
        };
        validate_local_id(&key.plugin_id, "plugin id")?;
        validate_local_id(&key.page_id, "page id")?;
        Ok(key)
    }
}

#[derive(Clone, Debug)]
pub struct PluginUiPageLoadTicket {
    key: PluginUiPageKey,
    plugin_generation: u64,
    global_generation: u64,
    /// Exact page revision observed when the async operation started. `None` means the page must
    /// still be absent when the result is published; it is not a wildcard.
    expected_revision: Option<u64>,
}

impl PluginUiPageLoadTicket {
    pub fn key(&self) -> &PluginUiPageKey {
        &self.key
    }
}

#[derive(Clone, Debug)]
pub struct PluginUiPageSnapshot {
    pub key: PluginUiPageKey,
    pub revision: u64,
    pub model: Arc<UiPageModel>,
}

#[derive(Clone, Debug)]
struct PageEntry {
    revision: u64,
    last_used: u64,
    model: Arc<UiPageModel>,
}

#[derive(Debug, Default)]
struct PageCacheState {
    entries: HashMap<PluginUiPageKey, PageEntry>,
    plugin_generations: HashMap<String, u64>,
    global_generation: u64,
    clock: u64,
    revision_clock: u64,
    observable_revision: u64,
}

impl PageCacheState {
    fn next_clock(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn next_revision(&mut self) -> u64 {
        self.revision_clock = self.revision_clock.wrapping_add(1).max(1);
        self.revision_clock
    }

    fn bump_observable_revision(&mut self) -> u64 {
        self.observable_revision = self.observable_revision.wrapping_add(1).max(1);
        self.observable_revision
    }

    fn plugin_generation(&self, plugin_id: &str) -> u64 {
        self.plugin_generations.get(plugin_id).copied().unwrap_or(0)
    }
}

/// Host-owned immutable page-model cache.
///
/// Guest code never controls cache capacity, eviction, revisions or invalidation. Async runtime code
/// obtains a generation/revision ticket before calling the guest and can publish only while that
/// state is still current. Plugin update/uninstall prevents obsolete results from reappearing, and
/// slower initial loads or UI events cannot overwrite newer snapshots. GPUI paint reads only
/// validated `Arc<UiPageModel>` snapshots.
#[derive(Debug)]
pub struct PluginUiPageCache {
    max_entries: usize,
    limits: UiSchemaLimits,
    state: Mutex<PageCacheState>,
}

impl PluginUiPageCache {
    pub fn new(max_entries: usize, limits: UiSchemaLimits) -> Result<Self> {
        if max_entries == 0 {
            bail!("插件 UI page cache 容量不能为 0");
        }
        Ok(Self {
            max_entries,
            limits,
            state: Mutex::new(PageCacheState::default()),
        })
    }

    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Monotonic-ish Host change token for retained UI invalidation.
    ///
    /// It changes only when visible plugin UI state can change: publish, plugin invalidation or a
    /// global clear. Cache reads/LRU touches intentionally do not change it, so GPUI can include this
    /// value in a retained render key without creating a repaint loop.
    pub fn observable_revision(&self) -> Result<u64> {
        Ok(self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?
            .observable_revision)
    }

    pub fn begin_load(&self, plugin_id: &str, page_id: &str) -> Result<PluginUiPageLoadTicket> {
        let key = PluginUiPageKey::new(plugin_id, page_id)?;
        let state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        let expected_revision = state.entries.get(&key).map(|entry| entry.revision);
        Ok(PluginUiPageLoadTicket {
            plugin_generation: state.plugin_generation(plugin_id),
            global_generation: state.global_generation,
            expected_revision,
            key,
        })
    }

    pub fn begin_update(
        &self,
        plugin_id: &str,
        page_id: &str,
        expected_revision: u64,
    ) -> Result<PluginUiPageLoadTicket> {
        let key = PluginUiPageKey::new(plugin_id, page_id)?;
        let state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        let actual_revision = state.entries.get(&key).map(|entry| entry.revision);
        if actual_revision != Some(expected_revision) {
            bail!(
                "插件 UI page revision 已变化，拒绝从旧快照发起事件: expected={expected_revision}, actual={actual_revision:?}"
            );
        }
        Ok(PluginUiPageLoadTicket {
            plugin_generation: state.plugin_generation(plugin_id),
            global_generation: state.global_generation,
            expected_revision: Some(expected_revision),
            key,
        })
    }

    pub fn publish(
        &self,
        ticket: PluginUiPageLoadTicket,
        model: UiPageModel,
    ) -> Result<PluginUiPageSnapshot> {
        // Validation is CPU-only and can walk up to the configured node limit; never hold the cache
        // mutex while doing it.
        validate_page_model(&model, &self.limits)?;
        let model = Arc::new(model);

        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        if state.global_generation != ticket.global_generation
            || state.plugin_generation(&ticket.key.plugin_id) != ticket.plugin_generation
        {
            bail!("插件 UI page 在加载期间已失效，拒绝发布旧页面模型");
        }
        let actual_revision = state.entries.get(&ticket.key).map(|entry| entry.revision);
        if actual_revision != ticket.expected_revision {
            bail!(
                "插件 UI page 在异步执行期间已被更新，拒绝乱序覆盖: expected={:?}, actual={actual_revision:?}",
                ticket.expected_revision
            );
        }

        let revision = state.next_revision();
        let last_used = state.next_clock();
        state.entries.insert(
            ticket.key.clone(),
            PageEntry {
                revision,
                last_used,
                model: model.clone(),
            },
        );
        evict_lru(&mut state, self.max_entries, Some(&ticket.key));
        state.bump_observable_revision();
        Ok(PluginUiPageSnapshot {
            key: ticket.key,
            revision,
            model,
        })
    }

    pub fn get(&self, plugin_id: &str, page_id: &str) -> Result<Option<PluginUiPageSnapshot>> {
        let key = PluginUiPageKey::new(plugin_id, page_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        let Some(entry) = state.entries.get(&key) else {
            return Ok(None);
        };
        let revision = entry.revision;
        let model = entry.model.clone();
        let clock = state.next_clock();
        if let Some(entry) = state.entries.get_mut(&key) {
            entry.last_used = clock;
        }
        Ok(Some(PluginUiPageSnapshot {
            key,
            revision,
            model,
        }))
    }

    pub fn invalidate_page(&self, plugin_id: &str, page_id: &str) -> Result<bool> {
        let key = PluginUiPageKey::new(plugin_id, page_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        let removed = state.entries.remove(&key).is_some();
        if removed {
            state.bump_observable_revision();
        }
        Ok(removed)
    }

    pub fn invalidate_plugin(&self, plugin_id: &str) -> Result<usize> {
        validate_local_id(plugin_id, "plugin id")?;
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        let generation = state
            .plugin_generations
            .entry(plugin_id.to_owned())
            .or_insert(0);
        *generation = generation.wrapping_add(1);
        let before = state.entries.len();
        state.entries.retain(|key, _| key.plugin_id != plugin_id);
        let removed = before.saturating_sub(state.entries.len());
        // Generation changes make in-flight results obsolete even when this plugin had no cached
        // page yet, so retained UI still needs an observable invalidation edge.
        state.bump_observable_revision();
        Ok(removed)
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        state.global_generation = state.global_generation.wrapping_add(1);
        let removed = state.entries.len();
        state.entries.clear();
        state.bump_observable_revision();
        Ok(removed)
    }

    pub fn len(&self) -> Result<usize> {
        Ok(self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?
            .entries
            .len())
    }
}

fn evict_lru(
    state: &mut PageCacheState,
    max_entries: usize,
    protected: Option<&PluginUiPageKey>,
) {
    while state.entries.len() > max_entries {
        let victim = state
            .entries
            .iter()
            .filter(|(key, _)| protected != Some(*key))
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone());
        let Some(victim) = victim else {
            break;
        };
        state.entries.remove(&victim);
    }
}

pub fn initialize(max_entries: usize) -> Result<Arc<PluginUiPageCache>> {
    if let Some(cache) = PLUGIN_UI_PAGE_CACHE.get() {
        return Ok(cache.clone());
    }
    let cache = Arc::new(PluginUiPageCache::new(
        max_entries,
        UiSchemaLimits::default(),
    )?);
    let _ = PLUGIN_UI_PAGE_CACHE.set(cache.clone());
    Ok(PLUGIN_UI_PAGE_CACHE.get().cloned().unwrap_or(cache))
}

pub fn global() -> Option<Arc<PluginUiPageCache>> {
    PLUGIN_UI_PAGE_CACHE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::ui::schema::UiNode;

    fn page(text: &str) -> UiPageModel {
        UiPageModel {
            root: UiNode::Text { text: text.into() },
        }
    }

    #[test]
    fn stale_load_cannot_publish_after_plugin_invalidation() {
        let cache = PluginUiPageCache::new(4, UiSchemaLimits::default()).expect("cache");
        let ticket = cache.begin_load("plugin.demo", "home").expect("ticket");
        cache.invalidate_plugin("plugin.demo").expect("invalidate");
        assert!(cache.publish(ticket, page("stale")).is_err());
        assert!(cache.get("plugin.demo", "home").expect("get").is_none());
    }

    #[test]
    fn competing_initial_loads_use_compare_and_swap() {
        let cache = PluginUiPageCache::new(4, UiSchemaLimits::default()).expect("cache");
        let slow = cache.begin_load("plugin.demo", "home").expect("slow");
        let fast = cache.begin_load("plugin.demo", "home").expect("fast");
        let fast = cache.publish(fast, page("fast")).expect("publish fast");
        assert!(cache.publish(slow, page("slow")).is_err());
        let current = cache.get("plugin.demo", "home").expect("get").expect("page");
        assert_eq!(current.revision, fast.revision);
        assert_eq!(current.model.root, UiNode::Text { text: "fast".into() });
    }

    #[test]
    fn stale_event_cannot_overwrite_newer_revision() {
        let cache = PluginUiPageCache::new(4, UiSchemaLimits::default()).expect("cache");
        let initial = cache.begin_load("plugin.demo", "home").expect("initial");
        let initial = cache.publish(initial, page("initial")).expect("publish initial");
        let slow = cache
            .begin_update("plugin.demo", "home", initial.revision)
            .expect("slow event");
        let fast = cache
            .begin_update("plugin.demo", "home", initial.revision)
            .expect("fast event");
        let fast = cache.publish(fast, page("fast")).expect("publish fast");
        assert!(cache.publish(slow, page("slow")).is_err());
        let current = cache.get("plugin.demo", "home").expect("get").expect("page");
        assert_eq!(current.revision, fast.revision);
        assert_eq!(current.model.root, UiNode::Text { text: "fast".into() });
    }

    #[test]
    fn observable_revision_changes_only_for_visible_state_changes() {
        let cache = PluginUiPageCache::new(4, UiSchemaLimits::default()).expect("cache");
        assert_eq!(cache.observable_revision().expect("revision"), 0);

        let ticket = cache.begin_load("plugin.demo", "home").expect("ticket");
        cache.publish(ticket, page("first")).expect("publish");
        let after_publish = cache.observable_revision().expect("published revision");
        assert!(after_publish > 0);

        let _ = cache.get("plugin.demo", "home").expect("get");
        assert_eq!(cache.observable_revision().expect("LRU revision"), after_publish);

        cache.invalidate_plugin("plugin.demo").expect("invalidate");
        assert_ne!(cache.observable_revision().expect("invalidated revision"), after_publish);
    }

    #[test]
    fn lru_capacity_is_host_owned() {
        let cache = PluginUiPageCache::new(1, UiSchemaLimits::default()).expect("cache");
        let first = cache.begin_load("plugin.demo", "first").expect("first");
        cache.publish(first, page("first")).expect("publish first");
        let second = cache.begin_load("plugin.demo", "second").expect("second");
        cache.publish(second, page("second")).expect("publish second");
        assert!(cache.get("plugin.demo", "first").expect("first get").is_none());
        assert!(cache.get("plugin.demo", "second").expect("second get").is_some());
    }
}
