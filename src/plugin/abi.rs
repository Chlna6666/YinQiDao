use serde::{Deserialize, Serialize};

/// Development host/component contract for YinQiDao music-provider plugins.
///
/// ABI-facing Rust records in this module mirror the WIT contract under `plugins/wit/`. Host-only
/// routing/account state is kept in separate types in the same module so Wasmtime adapters do not
/// accidentally expose local ids, persisted session state, priorities, or defaults to guests.
pub const PLUGIN_ABI_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    Authentication,
    Search,
    Metadata,
    Lyrics,
    Artwork,
    Streaming,
    Playlists,
    CloudLibrary,
    Recommendations,
    Recognition,
    UserProfile,
    LikeSync,
    PlaybackEvents,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    QrCode,
    BrowserOAuth,
    DeviceCode,
    CookieImport,
    CustomForm,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub abi_version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderDescriptor>,
    /// Outbound hosts requested by the component. The host still decides which entries are granted.
    #[serde(default)]
    pub network_domains: Vec<String>,
}

impl PluginManifest {
    pub fn supports_host_abi(&self) -> bool {
        self.abi_version == PLUGIN_ABI_VERSION
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthChallengeKind {
    QrCode,
    Browser,
    DeviceCode,
    Form,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthChallenge {
    pub challenge_id: String,
    pub kind: AuthChallengeKind,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default)]
    pub user_code: Option<String>,
    #[serde(default)]
    pub qr_payload: Option<String>,
    #[serde(default)]
    pub fields: Vec<KeyValue>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

/// Exact host-side semantic mirror of WIT `types.account`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderAccount {
    pub account_id: String,
    pub provider_id: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
}

/// Exact host-side semantic mirror of WIT `types.auth-poll`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthPollResult {
    Pending,
    Authenticated(ProviderAccount),
    Expired,
    Denied(String),
}

/// Persisted Host routing state. This is deliberately richer than WIT `types.account` and must not
/// be exported to a guest as-is.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    Authenticated,
    Expired,
    LoggedOut,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginAccount {
    pub plugin_id: String,
    pub provider_id: String,
    pub account_id: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    pub state: AccountState,
    #[serde(default)]
    pub capabilities: Vec<PluginCapability>,
    /// Higher values win when several accounts can satisfy the same single-provider request.
    #[serde(default)]
    pub priority: i32,
    /// Default is scoped to one provider, not to the whole application.
    #[serde(default)]
    pub is_default: bool,
}

impl PluginAccount {
    pub fn is_authenticated(&self) -> bool {
        self.state == AccountState::Authenticated
    }

    pub fn supports(&self, capability: PluginCapability) -> bool {
        self.is_authenticated() && self.capabilities.contains(&capability)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceTrackRef {
    pub provider_id: String,
    pub source_id: String,
}

/// Exact semantic mirror of WIT `types.track-query`. Local database ids and cross-provider source
/// mappings intentionally do not cross the guest boundary.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TrackQuery {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub musicbrainz_recording_id: Option<String>,
    #[serde(default)]
    pub fingerprint_id: Option<String>,
}

/// Host-side identity envelope used by future canonical-library work. This is not a WIT record.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TrackIdentity {
    #[serde(default)]
    pub local_track_id: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub musicbrainz_recording_id: Option<String>,
    #[serde(default)]
    pub fingerprint_id: Option<String>,
    #[serde(default)]
    pub sources: Vec<SourceTrackRef>,
}

impl From<&TrackIdentity> for TrackQuery {
    fn from(identity: &TrackIdentity) -> Self {
        Self {
            title: identity.title.clone(),
            artists: identity.artists.clone(),
            album: identity.album.clone(),
            duration_ms: identity.duration_ms,
            isrc: identity.isrc.clone(),
            musicbrainz_recording_id: identity.musicbrainz_recording_id.clone(),
            fingerprint_id: identity.fingerprint_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RemoteTrack {
    pub source: SourceTrackRef,
    pub title: String,
    #[serde(default)]
    pub artists: Vec<String>,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub isrc: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub playable: bool,
    #[serde(default)]
    pub explicit: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricWord {
    pub timestamp_ms: u64,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LyricLine {
    pub timestamp_ms: u64,
    pub text: String,
    #[serde(default)]
    pub translation: Option<String>,
    #[serde(default)]
    pub words: Vec<LyricWord>,
}

/// ABI lyric payload. Conversion into the player's richer `lyrics::LyricsDocument` remains a Host
/// responsibility so guest data never gains ownership of rendering/runtime objects.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginLyricDocument {
    pub source: String,
    #[serde(default)]
    pub plain: Option<String>,
    #[serde(default)]
    pub lines: Vec<LyricLine>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtworkDescriptor {
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamRequest {
    pub track: SourceTrackRef,
    #[serde(default)]
    pub quality: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StreamDescriptor {
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub bitrate: Option<u32>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub channels: Option<u16>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationSurface {
    Home,
    DailyMix,
    Discovery,
    SimilarTrack,
    ArtistRadio,
    ContinueListening,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecommendationRequest {
    pub surface: RecommendationSurface,
    #[serde(default)]
    pub seed: Option<TrackQuery>,
    #[serde(default = "default_recommendation_limit")]
    pub limit: u16,
    #[serde(default)]
    pub exclude: Vec<SourceTrackRef>,
}

fn default_recommendation_limit() -> u16 {
    30
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecommendationItem {
    pub track: RemoteTrack,
    #[serde(default)]
    pub score: Option<f32>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecognitionRequest {
    /// Stable algorithm name, for example `chromaprint-v1`.
    pub algorithm: String,
    /// Host-computed fingerprint. Raw decoder buffers stay outside the plugin boundary by default.
    pub fingerprint: Vec<u8>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RecognitionResult {
    pub track: RemoteTrack,
    #[serde(default)]
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlaylistDescriptor {
    pub provider_id: String,
    pub source_id: String,
    pub name: String,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub track_count: Option<u32>,
    #[serde(default)]
    pub editable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackSignalKind {
    Started,
    Completed,
    Skipped,
    Liked,
    Unliked,
    Disliked,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PlaybackSignal {
    pub kind: PlaybackSignalKind,
    pub track: TrackQuery,
    #[serde(default)]
    pub source: Option<SourceTrackRef>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceKind {
    Search,
    Metadata,
    Lyrics,
    Artwork,
    Streaming,
    Playlists,
    CloudLibrary,
    Recommendations,
    Recognition,
    UserProfile,
    LikeSync,
    PlaybackEvents,
}

impl ServiceKind {
    pub const fn capability(self) -> PluginCapability {
        match self {
            Self::Search => PluginCapability::Search,
            Self::Metadata => PluginCapability::Metadata,
            Self::Lyrics => PluginCapability::Lyrics,
            Self::Artwork => PluginCapability::Artwork,
            Self::Streaming => PluginCapability::Streaming,
            Self::Playlists => PluginCapability::Playlists,
            Self::CloudLibrary => PluginCapability::CloudLibrary,
            Self::Recommendations => PluginCapability::Recommendations,
            Self::Recognition => PluginCapability::Recognition,
            Self::UserProfile => PluginCapability::UserProfile,
            Self::LikeSync => PluginCapability::LikeSync,
            Self::PlaybackEvents => PluginCapability::PlaybackEvents,
        }
    }

    /// Aggregate surfaces keep every authenticated platform active at the same time. A user should
    /// never need to switch a global "current music service" just to see another account.
    pub const fn fan_out(self) -> bool {
        matches!(
            self,
            Self::Search | Self::Playlists | Self::CloudLibrary | Self::Recommendations
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginRoute {
    pub plugin_id: String,
    pub provider_id: String,
    pub account_id: String,
    pub priority: i32,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingPolicy {
    /// Authenticated plugin APIs are attempted before built-in anonymous provider code.
    pub authenticated_plugin_first: bool,
    /// Existing `src/online/providers` code remains a compatibility fallback during migration.
    pub allow_builtin_fallback: bool,
    /// Local-only mechanisms such as AcoustID may run only after plugin/built-in routes fail.
    pub allow_local_fallback: bool,
    /// Optional per-operation preference. It never logs other providers out.
    pub preferred_provider: Option<String>,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            authenticated_plugin_first: true,
            allow_builtin_fallback: true,
            allow_local_fallback: true,
            preferred_provider: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutePlan {
    pub service: ServiceKind,
    pub plugin_routes: Vec<PluginRoute>,
    pub authenticated_plugin_first: bool,
    pub allow_builtin_fallback: bool,
    pub allow_local_fallback: bool,
}

impl RoutePlan {
    pub fn has_authenticated_plugin(&self) -> bool {
        !self.plugin_routes.is_empty()
    }
}

/// Runtime-neutral account fusion and service router.
///
/// The future Wasmtime host registers sessions here after authentication. The router deliberately
/// stores all accounts concurrently and makes a routing decision per service call, which avoids the
/// traditional "switch NetEase/QQ account mode then rebuild the whole app" UX.
#[derive(Clone, Debug, Default)]
pub struct PluginServiceRouter {
    accounts: Vec<PluginAccount>,
}

impl PluginServiceRouter {
    pub fn upsert_account(&mut self, account: PluginAccount) {
        if let Some(existing) = self.accounts.iter_mut().find(|existing| {
            existing.plugin_id == account.plugin_id
                && existing.provider_id == account.provider_id
                && existing.account_id == account.account_id
        }) {
            *existing = account;
        } else {
            self.accounts.push(account);
        }
    }

    pub fn remove_account(&mut self, plugin_id: &str, provider_id: &str, account_id: &str) -> bool {
        let old_len = self.accounts.len();
        self.accounts.retain(|account| {
            account.plugin_id != plugin_id
                || account.provider_id != provider_id
                || account.account_id != account_id
        });
        self.accounts.len() != old_len
    }

    pub fn accounts(&self) -> &[PluginAccount] {
        &self.accounts
    }

    pub fn routes_for(
        &self,
        service: ServiceKind,
        preferred_provider: Option<&str>,
    ) -> Vec<PluginRoute> {
        let capability = service.capability();
        let mut routes = self
            .accounts
            .iter()
            .filter(|account| account.supports(capability))
            .map(|account| PluginRoute {
                plugin_id: account.plugin_id.clone(),
                provider_id: account.provider_id.clone(),
                account_id: account.account_id.clone(),
                priority: account.priority,
                is_default: account.is_default,
            })
            .collect::<Vec<_>>();

        routes.sort_by(|left, right| {
            let left_preferred =
                preferred_provider.is_some_and(|provider| provider == left.provider_id);
            let right_preferred =
                preferred_provider.is_some_and(|provider| provider == right.provider_id);
            right_preferred
                .cmp(&left_preferred)
                .then_with(|| right.is_default.cmp(&left.is_default))
                .then_with(|| right.priority.cmp(&left.priority))
                .then_with(|| left.provider_id.cmp(&right.provider_id))
                .then_with(|| left.account_id.cmp(&right.account_id))
        });

        if !service.fan_out() {
            routes.truncate(1);
        }
        routes
    }

    pub fn plan(&self, service: ServiceKind, policy: &RoutingPolicy) -> RoutePlan {
        RoutePlan {
            service,
            plugin_routes: self.routes_for(service, policy.preferred_provider.as_deref()),
            authenticated_plugin_first: policy.authenticated_plugin_first,
            allow_builtin_fallback: policy.allow_builtin_fallback,
            allow_local_fallback: policy.allow_local_fallback,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(
        provider_id: &str,
        account_id: &str,
        capabilities: &[PluginCapability],
        priority: i32,
    ) -> PluginAccount {
        PluginAccount {
            plugin_id: format!("plugin.{provider_id}"),
            provider_id: provider_id.into(),
            account_id: account_id.into(),
            display_name: account_id.into(),
            avatar_url: None,
            state: AccountState::Authenticated,
            capabilities: capabilities.to_vec(),
            priority,
            is_default: true,
        }
    }

    #[test]
    fn track_query_does_not_expose_host_only_identity_fields() {
        let identity = TrackIdentity {
            local_track_id: Some(42),
            title: "Track".into(),
            artists: vec!["Artist".into()],
            sources: vec![SourceTrackRef {
                provider_id: "qqmusic".into(),
                source_id: "mid".into(),
            }],
            ..TrackIdentity::default()
        };
        let query = TrackQuery::from(&identity);
        assert_eq!(query.title, "Track");
        assert_eq!(query.artists, vec!["Artist"]);
    }

    #[test]
    fn authenticated_accounts_are_fused_instead_of_switched() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account(
            "netease",
            "cloud-user",
            &[PluginCapability::Recommendations, PluginCapability::Search],
            10,
        ));
        router.upsert_account(account(
            "qqmusic",
            "qq-user",
            &[PluginCapability::Recommendations, PluginCapability::Search],
            5,
        ));

        let routes = router.routes_for(ServiceKind::Recommendations, None);
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].provider_id, "netease");
        assert_eq!(routes[1].provider_id, "qqmusic");
    }

    #[test]
    fn preferred_provider_is_operation_scoped_not_global() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account(
            "netease",
            "cloud-user",
            &[PluginCapability::Lyrics],
            100,
        ));
        router.upsert_account(account(
            "qqmusic",
            "qq-user",
            &[PluginCapability::Lyrics],
            1,
        ));

        let routes = router.routes_for(ServiceKind::Lyrics, Some("qqmusic"));
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].provider_id, "qqmusic");
        assert_eq!(router.accounts().len(), 2);
    }

    #[test]
    fn logged_out_account_never_suppresses_fallback() {
        let mut logged_out = account(
            "netease",
            "cloud-user",
            &[PluginCapability::Recognition],
            10,
        );
        logged_out.state = AccountState::LoggedOut;
        let mut router = PluginServiceRouter::default();
        router.upsert_account(logged_out);

        let plan = router.plan(ServiceKind::Recognition, &RoutingPolicy::default());
        assert!(!plan.has_authenticated_plugin());
        assert!(plan.allow_builtin_fallback);
        assert!(plan.allow_local_fallback);
    }

    #[test]
    fn plugin_first_recognition_uses_authenticated_api_before_local_fallback() {
        let mut router = PluginServiceRouter::default();
        router.upsert_account(account(
            "qqmusic",
            "qq-user",
            &[PluginCapability::Recognition],
            0,
        ));

        let plan = router.plan(ServiceKind::Recognition, &RoutingPolicy::default());
        assert!(plan.authenticated_plugin_first);
        assert_eq!(plan.plugin_routes.len(), 1);
        assert_eq!(plan.plugin_routes[0].provider_id, "qqmusic");
    }
}
