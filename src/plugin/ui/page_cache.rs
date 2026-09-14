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

    fn plugin_generation(&self, plugin_id: &str) -> u64 {
        self.plugin_generations.get(plugin_id).copied().unwrap_or(0)
    }
}

/// Host-owned immutable page-model cache.
///
/// Guest code never controls cache capacity, eviction, revisions or invalidation. Async runtime code
/// obtains a generation ticket before calling the guest and can publish only while that generation
/// is still current. Plugin update/uninstall therefore prevents an old in-flight page result from
/// reappearing after invalidation. GPUI paint reads only validated `Arc<UiPageModel>` snapshots.
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

    pub fn begin_load(&self, plugin_id: &str, page_id: &str) -> Result<PluginUiPageLoadTicket> {
        let key = PluginUiPageKey::new(plugin_id, page_id)?;
        let state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        Ok(PluginUiPageLoadTicket {
            plugin_generation: state.plugin_generation(plugin_id),
            global_generation: state.global_generation,
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
        Ok(state.entries.remove(&key).is_some())
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
        Ok(before.saturating_sub(state.entries.len()))
    }

    pub fn clear(&self) -> Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|error| anyhow!("插件 UI page cache 锁已损坏: {error}"))?;
        state.global_generation = state.global_generation.wrapping_add(1);
        let removed = state.entries.len();
        state.entries.clear();
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
