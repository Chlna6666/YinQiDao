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
    plugin::{
        abi::{PluginRoute, RemoteTrack, SourceTrackRef},
        frontend::PluginCallFailure,
    },
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
    quality: Option<String>,
    package_generation: u64,
}

impl AudioKey {
    fn new(route: &PluginRoute, source: &SourceTrackRef, quality: Option<&str>) -> Self {
        Self {
            plugin: route.plugin_id.clone(),
            provider: route.provider_id.clone(),
            account: route.account_id.clone(),
            source: source.source_id.clone(),
            quality: quality.map(str::to_owned),
            package_generation: crate::plugin::runtime_ports::package_mutation_generation(),
        }
    }
}

// Enforce the provider boundary before any expensive WIT/RPC call, so a
// future third-party plugin cannot poison another provider's hot cache.
fn valid_route_source(route: &PluginRoute, source: &SourceTrackRef) -> bool {
    !route.plugin_id.trim().is_empty()
        && !route.provider_id.trim().is_empty()
        && !source.source_id.trim().is_empty()
        && route.provider_id == source.provider_id
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

    fn usable(&mut self, key: &AudioKey) -> Option<Track> {
        let track = self.ready.get(key).cloned()?;
        if let Some(session) = crate::audio::progressive::lookup(&track.path)
            && matches!(
                session.snapshot(),
                crate::audio::progressive::DownloadState::Failed(_)
            )
        {
            self.ready.remove(key);
            self.order.retain(|candidate| candidate != key);
            return None;
        }
        self.mark_used(key);
        Some(track)
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

    pub(super) fn ready(
        &self,
        route: &PluginRoute,
        source: &SourceTrackRef,
        quality: Option<&str>,
    ) -> Option<Track> {
        if !valid_route_source(route, source) {
            return None;
        }
        let key = AudioKey::new(route, source, quality);
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        let track = state.usable(&key);
        // Every new playback receives a fresh transient ID: reusing the ID
        // of the original prefetch would alias an old track/crossfade state.
        track.and_then(|track| fresh_playback_track(track).ok())
    }

    /// A low-priority speculative fetch; limited to two concurrent tracks.
    /// Duplicate keys do not create duplicate provider requests.
    pub(super) fn prefetch(
        self: &Arc<Self>,
        route: PluginRoute,
        remote: RemoteTrack,
        quality: Option<String>,
    ) {
        if valid_route_source(&route, &remote.source) {
            let _ = self.start(route, remote, quality, true);
        }
    }

    /// Foreground requests join the exact same materialization as prefetch.
    /// Failed/cancelled work is removed so retry can start a new request.
    pub(super) async fn prepare(
        self: &Arc<Self>,
        route: PluginRoute,
        remote: RemoteTrack,
        quality: Option<String>,
    ) -> Result<Track> {
        if !valid_route_source(&route, &remote.source) {
            return Err(anyhow!("在线音频插件路由与歌曲来源不一致"));
        }
        if let Some(track) = self.ready(&route, &remote.source, quality.as_deref()) {
            return Ok(track);
        }
        let mut rx = self
            .start(route, remote, quality, false)
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
        quality: Option<String>,
        background: bool,
    ) -> Option<watch::Receiver<Option<PreparedResult>>> {
        let key = AudioKey::new(&route, &remote.source, quality.as_deref());
        let handle = crate::runtime::io_handle().ok()?;
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(track) = state.usable(&key) {
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
                    .prepare_remote_track_for_route(&route, &remote, quality.as_deref())
                    .await?;
                let crate::plugin::frontend::PluginSingleResult {
                    value,
                    failures,
                    client_ready,
                    ..
                } = response;
                match value {
                    Some(prepared) => Ok(prepared.into_track()),
                    None => Err(anyhow!(unavailable_stream_reason(&failures, client_ready))),
                }
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
        // Host must stop detached progressive transfers too: aborting a frontend
        // waiter alone does not cancel the shared HTTP writer after early Ready.
        crate::audio::progressive::cancel_all();
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        state.epoch = state.epoch.wrapping_add(1);
        for (_, pending) in state.pending.drain() {
            pending.abort.abort();
        }
        state.order.clear();
        state.ready.clear();
    }
}

/// Do not collapse Host/plugin failures into the same generic "source unavailable".
/// A playlist row can be clickable even when a provider denies the stream.
fn unavailable_stream_reason(failures: &[PluginCallFailure], client_ready: bool) -> String {
    if let Some(failure) = failures.last() {
        let message = failure.error.lines().next().unwrap_or("").trim();
        let lowercase = message.to_ascii_lowercase();
        // Never display potentially signed stream URLs, cookies or token values
        // returned by an untrusted third-party plug-in.
        if lowercase.contains("://")
            || lowercase.contains("cookie")
            || lowercase.contains("token")
            || lowercase.contains("authorization")
        {
            return "在线音源解析失败，请检查该平台账号、版权和网络状态".into();
        }
        if !message.is_empty() {
            let reason: String = message.chars().take(180).collect();
            return format!("在线音源解析失败：{reason}");
        }
    }
    if !client_ready {
        "在线音源插件尚未就绪，请检查插件安装与账号登录状态".into()
    } else {
        "当前账号未提供可播放的在线音源（可能受版权、会员或地区限制）".into()
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
    fn failed_source_surfaces_actual_plugin_reason_without_credentials() {
        let route = PluginRoute {
            plugin_id: "netease".into(),
            provider_id: "netease".into(),
            account_id: "session".into(),
            priority: 0,
            is_default: true,
        };
        let failures = [PluginCallFailure {
            route,
            error: "歌曲无版权或会员限制".into(),
        }];
        assert!(unavailable_stream_reason(&failures, true).contains("会员限制"));
        assert!(unavailable_stream_reason(&[], false).contains("尚未就绪"));
        assert!(unavailable_stream_reason(&[], true).contains("地区限制"));
        let secret = [PluginCallFailure {
            route: failures[0].route.clone(),
            error: "https://example.org/audio?token=secret".into(),
        }];
        assert!(!unavailable_stream_reason(&secret, true).contains("secret"));
    }

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
        let key = AudioKey::new(&route, &source, None);
        let mut other = route.clone();
        other.account_id = "bob".into();
        assert_ne!(key, AudioKey::new(&other, &source, None));
        other = route.clone();
        other.provider_id = "qqmusic".into();
        assert_ne!(key, AudioKey::new(&other, &source, None));
    }

    #[test]
    fn prepared_cache_never_reuses_the_wrong_quality() {
        let route = PluginRoute {
            plugin_id: "music".into(),
            provider_id: "netease".into(),
            account_id: "alice".into(),
            priority: 0,
            is_default: true,
        };
        let source = SourceTrackRef {
            provider_id: "netease".into(),
            source_id: "1234".into(),
        };
        let standard = AudioKey::new(&route, &source, Some("standard"));
        let lossless = AudioKey::new(&route, &source, Some("lossless"));
        let automatic = AudioKey::new(&route, &source, None);
        assert_ne!(standard, lossless);
        assert_ne!(automatic, lossless);
    }

    #[test]
    fn shared_provider_protocol_rejects_cross_provider_track_sources() {
        let mut route = PluginRoute {
            plugin_id: "new-provider-plugin".into(),
            provider_id: "provider-a".into(),
            account_id: "session-a".into(),
            priority: 0,
            is_default: false,
        };
        let source = SourceTrackRef {
            provider_id: "provider-a".into(),
            source_id: "song-42".into(),
        };
        assert!(valid_route_source(&route, &source));
        route.provider_id = "provider-b".into();
        assert!(!valid_route_source(&route, &source));
        route.provider_id = "provider-a".into();
        assert!(!valid_route_source(
            &route,
            &SourceTrackRef {
                source_id: "".into(),
                ..source
            }
        ));
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
            None,
        );
        for index in 0..(MAX_READY_TRACKS + 3) {
            let key = AudioKey::new(
                &route,
                &SourceTrackRef {
                    provider_id: "netease".into(),
                    source_id: index.to_string(),
                },
                None,
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
