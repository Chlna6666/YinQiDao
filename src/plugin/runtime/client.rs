use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow};

use super::abi::{
    ArtworkDescriptor, AuthChallenge, AuthMethod, AuthPollResult, KeyValue, PlaybackSignal,
    PlaylistDescriptor, PluginLyricDocument, PluginManifest, ProviderAccount, RecognitionRequest,
    RecognitionResult, RecommendationItem, RecommendationRequest, RemoteTrack, SourceTrackRef,
    StreamDescriptor, StreamRequest, TrackQuery,
};

/// Future returned by the runtime-neutral provider client boundary.
///
/// Calls run only on ordinary async/worker paths. The realtime audio callback must never await or
/// invoke this interface.
pub type PluginClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Runtime-neutral semantic mirror of the WIT `provider` exports.
///
/// The future Wasmtime Component adapter owns Store/instance details and implements this trait. The
/// rest of YinQiDao depends only on these stable Host types, keeping generated binding types out of
/// OnlineServices, account/session management and UI code. `plugin_id` is always selected by the
/// Host; it is not a guest-controlled WIT argument.
pub trait PluginProviderClient: Send + Sync {
    fn manifest<'a>(&'a self, plugin_id: &'a str) -> PluginClientFuture<'a, PluginManifest>;

    fn accounts<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
    ) -> PluginClientFuture<'a, Vec<ProviderAccount>>;

    fn auth_begin<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        method: AuthMethod,
    ) -> PluginClientFuture<'a, AuthChallenge>;

    fn auth_poll<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
    ) -> PluginClientFuture<'a, AuthPollResult>;

    fn auth_submit<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
        values: &'a [KeyValue],
    ) -> PluginClientFuture<'a, AuthPollResult>;

    fn auth_cancel<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        challenge_id: &'a str,
    ) -> PluginClientFuture<'a, bool>;

    fn logout<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
    ) -> PluginClientFuture<'a, bool>;

    fn search<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        query: &'a str,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>>;

    fn resolve_track<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        query: &'a TrackQuery,
    ) -> PluginClientFuture<'a, Option<RemoteTrack>>;

    fn lyrics<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        track: &'a SourceTrackRef,
    ) -> PluginClientFuture<'a, Option<PluginLyricDocument>>;

    fn artwork<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        track: &'a SourceTrackRef,
    ) -> PluginClientFuture<'a, Option<ArtworkDescriptor>>;

    fn stream<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        request: &'a StreamRequest,
    ) -> PluginClientFuture<'a, StreamDescriptor>;

    fn playlists<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
    ) -> PluginClientFuture<'a, Vec<PlaylistDescriptor>>;

    fn playlist_tracks<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>>;

    fn playlist_create<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        name: &'a str,
    ) -> PluginClientFuture<'a, PlaylistDescriptor>;

    fn playlist_add<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        tracks: &'a [SourceTrackRef],
    ) -> PluginClientFuture<'a, bool>;

    fn playlist_remove<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        playlist_id: &'a str,
        tracks: &'a [SourceTrackRef],
    ) -> PluginClientFuture<'a, bool>;

    fn cloud_library<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>>;

    fn liked_tracks<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        offset: u32,
        limit: u16,
    ) -> PluginClientFuture<'a, Vec<RemoteTrack>>;

    fn set_liked<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        track: &'a SourceTrackRef,
        liked: bool,
    ) -> PluginClientFuture<'a, bool>;

    fn recommendations<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        request: &'a RecommendationRequest,
    ) -> PluginClientFuture<'a, Vec<RecommendationItem>>;

    fn recognize<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: Option<&'a str>,
        request: &'a RecognitionRequest,
    ) -> PluginClientFuture<'a, Option<RecognitionResult>>;

    fn report_playback<'a>(
        &'a self,
        plugin_id: &'a str,
        provider_id: &'a str,
        account_id: &'a str,
        signal: &'a PlaybackSignal,
    ) -> PluginClientFuture<'a, bool>;
}

/// Process-wide provider client slot.
///
/// Readers clone the current `Arc` before invoking it, so a Component-runtime reload can replace
/// the adapter without invalidating in-flight calls. During a coordinated Provider/UI port swap,
/// reads fail closed so callers cannot observe a half-swapped runtime adapter.
#[derive(Default)]
pub struct PluginClientRegistry {
    client: RwLock<Option<Arc<dyn PluginProviderClient>>>,
}

impl std::fmt::Debug for PluginClientRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginClientRegistry")
            .field("ready", &self.is_ready().unwrap_or(false))
            .finish()
    }
}

impl PluginClientRegistry {
    pub fn client(&self) -> Result<Option<Arc<dyn PluginProviderClient>>> {
        if super::runtime_ports::is_swapping() {
            return Err(anyhow!("插件 Component runtime 正在切换，Provider 调用暂不可用"));
        }
        Ok(self
            .client
            .read()
            .map_err(|error| anyhow!("插件 Provider client registry 锁已损坏: {error}"))?
            .clone())
    }

    pub fn is_ready(&self) -> Result<bool> {
        if super::runtime_ports::is_swapping() {
            return Ok(false);
        }
        Ok(self
            .client
            .read()
            .map_err(|error| anyhow!("插件 Provider client registry 锁已损坏: {error}"))?
            .is_some())
    }

    pub(super) fn install(
        &self,
        client: Arc<dyn PluginProviderClient>,
    ) -> Result<Option<Arc<dyn PluginProviderClient>>> {
        Ok(self
            .client
            .write()
            .map_err(|error| anyhow!("插件 Provider client registry 锁已损坏: {error}"))?
            .replace(client))
    }

    pub(super) fn clear(&self) -> Result<Option<Arc<dyn PluginProviderClient>>> {
        Ok(self
            .client
            .write()
            .map_err(|error| anyhow!("插件 Provider client registry 锁已损坏: {error}"))?
            .take())
    }
}

static PLUGIN_CLIENTS: OnceLock<Arc<PluginClientRegistry>> = OnceLock::new();

pub fn initialize() -> Arc<PluginClientRegistry> {
    PLUGIN_CLIENTS
        .get_or_init(|| Arc::new(PluginClientRegistry::default()))
        .clone()
}

pub fn global() -> Option<Arc<PluginClientRegistry>> {
    PLUGIN_CLIENTS.get().cloned()
}
