#[cfg(target_arch = "wasm32")]
mod api;
pub mod decoder;

#[cfg(target_arch = "wasm32")]
pub mod bindings {
    wit_bindgen::generate!({
        world: "music-plugin",
        path: "../wit",
        pub_export_macro: true,
    });
}

#[cfg(target_arch = "wasm32")]
use bindings::exports::yinqidao::music_plugin::{provider, ui};
#[cfg(target_arch = "wasm32")]
use bindings::yinqidao::music_plugin::types;

#[cfg(target_arch = "wasm32")]
struct NeteasePlugin;

#[cfg(target_arch = "wasm32")]
impl provider::Guest for NeteasePlugin {
    fn manifest() -> types::PluginManifest {
        api::manifest()
    }

    fn accounts(provider_id: String) -> Result<Vec<types::Account>, String> {
        api::accounts(&provider_id)
    }

    fn auth_begin(
        provider_id: String,
        method: types::AuthMethod,
    ) -> Result<types::AuthChallenge, String> {
        api::auth_begin(&provider_id, method)
    }

    fn auth_poll(provider_id: String, challenge_id: String) -> Result<types::AuthPoll, String> {
        api::auth_poll(&provider_id, &challenge_id)
    }

    fn auth_submit(
        provider_id: String,
        challenge_id: String,
        values: Vec<types::KeyValue>,
    ) -> Result<types::AuthPoll, String> {
        api::auth_submit(&provider_id, &challenge_id, values)
    }

    fn auth_cancel(provider_id: String, challenge_id: String) -> Result<bool, String> {
        api::auth_cancel(&provider_id, &challenge_id)
    }

    fn logout(provider_id: String, account_id: String) -> Result<bool, String> {
        api::logout(&provider_id, &account_id)
    }

    fn search(
        provider_id: String,
        account_id: Option<String>,
        query: String,
        limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        api::search(&provider_id, account_id.as_deref(), &query, limit)
    }

    fn resolve_track(
        provider_id: String,
        account_id: Option<String>,
        query: types::TrackQuery,
    ) -> Result<Option<types::RemoteTrack>, String> {
        api::resolve_track(&provider_id, account_id.as_deref(), &query)
    }

    fn lyrics(
        provider_id: String,
        account_id: Option<String>,
        track: types::SourceTrackRef,
    ) -> Result<Option<types::LyricDocument>, String> {
        api::lyrics(&provider_id, account_id.as_deref(), &track)
    }

    fn artwork(
        provider_id: String,
        account_id: Option<String>,
        track: types::SourceTrackRef,
    ) -> Result<Option<types::ArtworkDescriptor>, String> {
        api::artwork(&provider_id, account_id.as_deref(), &track)
    }

    fn stream(
        provider_id: String,
        account_id: String,
        request: types::StreamRequest,
    ) -> Result<types::StreamDescriptor, String> {
        api::stream(&provider_id, &account_id, &request)
    }

    fn playlists(provider_id: String, account_id: String) -> Result<Vec<types::Playlist>, String> {
        api::playlists(&provider_id, &account_id)
    }

    fn playlist_tracks(
        provider_id: String,
        account_id: String,
        playlist_id: String,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        api::playlist_tracks(&provider_id, &account_id, &playlist_id, offset, limit)
    }

    fn playlist_create(
        provider_id: String,
        account_id: String,
        name: String,
    ) -> Result<types::Playlist, String> {
        api::playlist_create(&provider_id, &account_id, &name)
    }

    fn playlist_add(
        provider_id: String,
        account_id: String,
        playlist_id: String,
        tracks: Vec<types::SourceTrackRef>,
    ) -> Result<bool, String> {
        api::playlist_mutate(&provider_id, &account_id, &playlist_id, &tracks, true)
    }

    fn playlist_remove(
        provider_id: String,
        account_id: String,
        playlist_id: String,
        tracks: Vec<types::SourceTrackRef>,
    ) -> Result<bool, String> {
        api::playlist_mutate(&provider_id, &account_id, &playlist_id, &tracks, false)
    }

    fn cloud_library(
        provider_id: String,
        account_id: String,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        api::cloud_library(&provider_id, &account_id, offset, limit)
    }

    fn liked_tracks(
        provider_id: String,
        account_id: String,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        api::liked_tracks(&provider_id, &account_id, offset, limit)
    }

    fn set_liked(
        provider_id: String,
        account_id: String,
        track: types::SourceTrackRef,
        liked: bool,
    ) -> Result<bool, String> {
        api::set_liked(&provider_id, &account_id, &track, liked)
    }

    fn recommendations(
        _provider_id: String,
        _account_id: String,
        _request: types::RecommendationRequest,
    ) -> Result<Vec<types::RecommendationItem>, String> {
        Err(api::unsupported("recommendations"))
    }

    fn recognize(
        _provider_id: String,
        _account_id: Option<String>,
        _request: types::RecognitionRequest,
    ) -> Result<Option<types::RecognitionResult>, String> {
        Err(api::unsupported("recognition"))
    }

    fn report_playback(
        _provider_id: String,
        _account_id: String,
        _signal: types::PlaybackSignal,
    ) -> Result<bool, String> {
        Err(api::unsupported("playback_events"))
    }
}

#[cfg(target_arch = "wasm32")]
impl ui::Guest for NeteasePlugin {
    fn load_page(_page_id: String) -> Result<ui::UiPage, String> {
        Err("网易云插件没有声明自定义 UI page".into())
    }

    fn handle_event(_page_id: String, _event: ui::UiEvent) -> Result<ui::UiResponse, String> {
        Err("网易云插件没有声明自定义 UI event".into())
    }

    fn invoke_command(
        _command_id: String,
        _context: ui::CommandContext,
    ) -> Result<ui::CommandResponse, String> {
        Err("网易云插件没有声明自定义 command".into())
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(NeteasePlugin with_types_in bindings);
