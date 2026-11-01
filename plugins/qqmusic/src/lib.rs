#![allow(unsafe_code)]

mod api;
mod auth;
mod collections;
mod crypto;
mod features;
mod music;
mod protocol;

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
struct QqMusicPlugin;

#[cfg(target_arch = "wasm32")]
impl provider::Guest for QqMusicPlugin {
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
        auth::auth_begin(&provider_id, method)
    }

    fn poll_auth(provider_id: String, challenge_id: String) -> Result<types::AuthPoll, String> {
        auth::auth_poll(&provider_id, &challenge_id)
    }

    fn auth_submit(
        provider_id: String,
        challenge_id: String,
        values: Vec<types::KeyValue>,
    ) -> Result<types::AuthPoll, String> {
        auth::auth_submit(&provider_id, &challenge_id, values)
    }

    fn auth_cancel(provider_id: String, challenge_id: String) -> Result<bool, String> {
        auth::auth_cancel(&provider_id, &challenge_id)
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
        music::lyrics(&provider_id, account_id.as_deref(), &track)
    }

    fn artwork(
        provider_id: String,
        account_id: Option<String>,
        track: types::SourceTrackRef,
    ) -> Result<Option<types::ArtworkDescriptor>, String> {
        music::artwork(&provider_id, account_id.as_deref(), &track)
    }

    fn stream(
        provider_id: String,
        account_id: String,
        request: types::StreamRequest,
    ) -> Result<types::StreamDescriptor, String> {
        music::stream(&provider_id, &account_id, &request)
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

    fn playlist_rename(
        provider_id: String,
        account_id: String,
        playlist_id: String,
        name: String,
    ) -> Result<bool, String> {
        collections::playlist_rename(&provider_id, &account_id, &playlist_id, &name)
    }

    fn playlist_delete(
        provider_id: String,
        account_id: String,
        playlist_id: String,
    ) -> Result<bool, String> {
        collections::playlist_delete(&provider_id, &account_id, &playlist_id)
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

    fn media_collections(
        provider_id: String,
        account_id: String,
        kind: types::MediaCollectionKind,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::MediaCollection>, String> {
        collections::media_collections(&provider_id, &account_id, kind, offset, limit)
    }

    fn media_collection_detail(
        provider_id: String,
        account_id: Option<String>,
        collection: types::MediaCollectionRef,
    ) -> Result<types::MediaCollection, String> {
        collections::media_collection_detail(&provider_id, account_id.as_deref(), &collection)
    }

    fn collection_tracks(
        provider_id: String,
        account_id: Option<String>,
        collection: types::MediaCollectionRef,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        collections::collection_tracks(
            &provider_id,
            account_id.as_deref(),
            &collection,
            offset,
            limit,
        )
    }

    fn collection_items(
        provider_id: String,
        account_id: Option<String>,
        collection: types::MediaCollectionRef,
        offset: u32,
        limit: u16,
    ) -> Result<Vec<types::MediaCollection>, String> {
        collections::collection_items(
            &provider_id,
            account_id.as_deref(),
            &collection,
            offset,
            limit,
        )
    }

    fn set_media_saved(
        provider_id: String,
        account_id: String,
        collection: types::MediaCollectionRef,
        saved: bool,
    ) -> Result<bool, String> {
        collections::set_media_saved(&provider_id, &account_id, &collection, saved)
    }

    fn collection_recommendations(
        provider_id: String,
        account_id: String,
        request: types::CollectionRecommendationRequest,
    ) -> Result<Vec<types::CollectionRecommendationItem>, String> {
        collections::collection_recommendations(&provider_id, &account_id, &request)
    }

    fn get_user_profile(
        provider_id: String,
        account_id: String,
    ) -> Result<types::UserProfile, String> {
        collections::user_profile(&provider_id, &account_id)
    }

    fn cloud_library(
        provider_id: String,
        _account_id: String,
        _offset: u32,
        _limit: u16,
    ) -> Result<Vec<types::RemoteTrack>, String> {
        Err(api::unsupported(&provider_id, "cloud_library"))
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
        features::set_liked(&provider_id, &account_id, &track, liked)
    }

    fn recommendations(
        provider_id: String,
        account_id: String,
        request: types::RecommendationRequest,
    ) -> Result<Vec<types::RecommendationItem>, String> {
        features::recommendations(&provider_id, &account_id, &request)
    }

    fn recognize(
        provider_id: String,
        _account_id: Option<String>,
        _request: types::RecognitionRequest,
    ) -> Result<Option<types::RecognitionResult>, String> {
        Err(api::unsupported(&provider_id, "recognition"))
    }

    fn report_playback(
        provider_id: String,
        account_id: String,
        signal: types::PlaybackSignal,
    ) -> Result<bool, String> {
        features::report_playback(&provider_id, &account_id, &signal)
    }
}

#[cfg(target_arch = "wasm32")]
impl ui::Guest for QqMusicPlugin {
    fn load_page(_page_id: String) -> Result<ui::UiPage, String> {
        Err("QQ音乐插件没有声明自定义 UI page".into())
    }

    fn handle_event(_page_id: String, _event: ui::UiEvent) -> Result<ui::UiResponse, String> {
        Err("QQ音乐插件没有声明自定义 UI event".into())
    }

    fn invoke_command(
        _command_id: String,
        _context: ui::CommandContext,
    ) -> Result<ui::CommandResponse, String> {
        Err("QQ音乐插件没有声明自定义 command".into())
    }
}

#[cfg(target_arch = "wasm32")]
bindings::export!(QqMusicPlugin with_types_in bindings);
