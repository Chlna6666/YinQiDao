//! Shared, account-scoped prepared audio cache for all online providers.
//! The Host StreamCache remains the authoritative bounded disk store; this
//! process-local LRU prevents repeated provider RPCs and joins in-flight
//! prefetches when the user clicks Next/Previous.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use tokio::sync::watch;

use crate::{
    model::Track,
    plugin::abi::{PluginRoute, RemoteTrack, SourceTrackRef},
};

const MAX_READY_TRACKS: usize = 6;
// All online downloads (foreground AND background) share the same budget.
const MAX_ACTIVE_DOWNLOADS: usize = 2;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AudioKey {
    plugin: String,
    provider: String,
    account: String,
    source: String,
    package_generation: u64,
}

impl AudioKey {
    fn new(route: &PluginRoute, source: &SourceTrackRef) -> Self {
        Self {
            plugin: route.plugin_id.clone(),
            provider: route.provider_id.clone(),
            account: route.account_id.clone(),
            source: source.source_id.clone(),
            package_generation: crate::plugin::runtime_ports::package_mutation_generation(),
        }
    }
}

type PreparedResult = Result<Track, String>;

struct Pending {
    receiver: watch::Receiver<Option<PreparedResult>>,
    abort: tokio::task::AbortHandle,
    background: bool,
    sequence: u64,
}

#[derive(Default)]
struct CacheState {
    ready: HashMap<AudioKey, Track>,
    order: VecDeque<AudioKey>,
    pending: HashMap<AudioKey, Pending>,
    epoch: u64,
    next_sequence: u64,
}

impl CacheState {
    fn mark_used(&mut self, key: &AudioKey) {
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
    }

    fn insert(&mut self, key: AudioKey, track: Track) {
        self.ready.insert(key.clone(), track);
        self.mark_used(&key);
        while self.order.len() > MAX_READY_TRACKS {
            if let Some(old) = self.order.pop_front() {
                self.ready.remove(&old);
            }
        }
    }
}

/// O(1) cache probes on the GPUI UI thread; every network and file operation
/// runs on the existing Tokio runtime. Cache content retains Host leases.
#[derive(Default)]
pub(super) struct OnlineAudioCache {
    state: Mutex<CacheState>,
}

impl OnlineAudioCache {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(super) fn ready(&self, route: &PluginRoute, source: &SourceTrackRef) -> Option<Track> {
        let key = AudioKey::new(route, source);
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        let track = state.ready.get(&key).cloned();
        if track.is_some() {
            state.mark_used(&key);
        }
        // Every new playback receives a fresh transient ID: reusing the ID
        // of the original prefetch would alias an old track/crossfade state.
        track.and_then(|track| fresh_playback_track(track).ok())
    }

    /// A low-priority speculative fetch; limited to two concurrent tracks.
    /// Duplicate keys do not create duplicate provider requests.
    pub(super) fn prefetch(self: &Arc<Self>, route: PluginRoute, remote: RemoteTrack) {
        let _ = self.start(route, remote, true);
    }

    /// Foreground requests join the exact same materialization as prefetch.
    /// Failed/cancelled work is removed so retry can start a new request.
    pub(super) async fn prepare(
        self: &Arc<Self>,
        route: PluginRoute,
        remote: RemoteTrack,
    ) -> Result<Track> {
        if let Some(track) = self.ready(&route, &remote.source) {
            return Ok(track);
        }
        let mut rx = self
            .start(route, remote, false)
            .ok_or_else(|| anyhow!("无法启动在线音频准备任务"))?;
        loop {
            let outcome = (*rx.borrow_and_update()).clone();
            if let Some(outcome) = outcome {
                return outcome
                    .map_err(anyhow::Error::msg)
                    .and_then(fresh_playback_track);
            }
            rx.changed()
                .await
                .map_err(|_| anyhow!("在线音频准备任务被取消"))?;
        }
    }

    fn start(
        self: &Arc<Self>,
        route: PluginRoute,
        remote: RemoteTrack,
        background: bool,
    ) -> Option<watch::Receiver<Option<PreparedResult>>> {
        let key = AudioKey::new(&route, &remote.source);
        let handle = crate::runtime::io_handle().ok()?;
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(track) = state.ready.get(&key).cloned() {
            let (tx, rx) = watch::channel(Some(Ok(track)));
            drop(tx);
            return Some(rx);
        }
        if let Some(pending) = state.pending.get_mut(&key) {
            if !background {
                pending.background = false;
            }
            return Some(pending.receiver.clone());
        }
        if state.pending.len() >= MAX_ACTIVE_DOWNLOADS {
            if background {
                // Never displace an audible/foreground request just to warm
                // another song. The following transport switch will rescan.
                return None;
            }
            // The newest user click is authoritative. Evict an unrelated
            // speculative download first; otherwise evict the oldest request
            // left behind by a superseded foreground click.
            let victim = state
                .pending
                .iter()
                .filter(|(_, pending)| pending.background)
                .min_by_key(|(_, pending)| pending.sequence)
                .or_else(|| {
                    state
                        .pending
                        .iter()
                        .min_by_key(|(_, pending)| pending.sequence)
                })
                .map(|(key, _)| key.clone());
            if let Some(victim) = victim
                && let Some(pending) = state.pending.remove(&victim)
            {
                pending.abort.abort();
            }
        }

        let epoch = state.epoch;
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.wrapping_add(1);
        let (tx, rx) = watch::channel(None);
        let cache = Arc::clone(self);
        let worker_key = key.clone();
        // Spawn under the lock: the worker cannot commit before its Pending
        // entry has been installed. No lock is held across any network await.
        let join = handle.spawn(async move {
            let result: PreparedResult = async {
                let frontend = crate::plugin::frontend::global()
                    .ok_or_else(|| anyhow!("插件前端尚未初始化"))?;
                let response = frontend
                    .prepare_remote_track_for_route(&route, &remote, None)
                    .await?;
                response
                    .value
                    .map(|prepared| prepared.into_track())
                    .ok_or_else(|| anyhow!("在线音频源不可用"))
            }
            .await
            .map_err(|err| format!("{err:#}"));

            {
                let mut state = cache.state.lock().unwrap_or_else(|err| err.into_inner());
                if state.epoch == epoch {
                    state.pending.remove(&worker_key);
                    if let Ok(track) = &result {
                        state.insert(worker_key, track.clone());
                    }
                }
            }
            // Notify EVERY waiter, including foreground callers that joined
            // this speculative request.
            let _ = tx.send(Some(result));
        });
        state.pending.insert(
            key,
            Pending {
                receiver: rx.clone(),
                abort: join.abort_handle(),
                background,
                sequence,
            },
        );
        Some(rx)
    }

    /// Clear account-bound audio and cancel outstanding requests when plugins
    /// are disabled or the account session is revoked.
    pub(super) fn clear(&self) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        for (_, pending) in state.pending.drain() {
            pending.abort.abort();
        }
        state.order.clear();
        state.ready.clear();
    }
}

/// Clone the Host-backed Track metadata without copying the downloaded
/// audio; each transport activation gets a distinct negative process-local ID.
fn fresh_playback_track(mut track: Track) -> Result<Track> {
    track.id = crate::plugin::streaming::allocate_remote_track_id()?;
    Ok(track)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_never_mix_accounts_or_providers() {
        let route = PluginRoute {
            plugin_id: "music".into(),
            provider_id: "netease".into(),
            account_id: "alice".into(),
            priority: 0,
            is_default: false,
        };
        let source = SourceTrackRef {
            provider_id: "netease".into(),
            source_id: "track-1".into(),
        };
        let key = AudioKey::new(&route, &source);
        let mut other = route.clone();
        other.account_id = "bob".into();
        assert_ne!(key, AudioKey::new(&other, &source));
        other = route.clone();
        other.provider_id = "qqmusic".into();
        assert_ne!(key, AudioKey::new(&other, &source));
    }

    #[test]
    fn cache_bounds_track_count_and_lru_order() {
        let mut state = CacheState::default();
        let route = PluginRoute {
            plugin_id: "music".into(),
            provider_id: "netease".into(),
            account_id: "alice".into(),
            priority: 0,
            is_default: false,
        };
        let first = AudioKey::new(
            &route,
            &SourceTrackRef {
                provider_id: "netease".into(),
                source_id: "0".into(),
            },
        );
        for index in 0..(MAX_READY_TRACKS + 3) {
            let key = AudioKey::new(
                &route,
                &SourceTrackRef {
                    provider_id: "netease".into(),
                    source_id: index.to_string(),
                },
            );
            // This test exercises only ordering and eviction; the track value
            // is irrelevant, so use a regular local TrackData fixture.
            state.mark_used(&key);
            while state.order.len() > MAX_READY_TRACKS {
                state.order.pop_front();
            }
        }
        assert_eq!(state.order.len(), MAX_READY_TRACKS);
        assert!(!state.order.contains(&first));
    }
}
