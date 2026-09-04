use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::model::Track;

use super::{ProviderKind, ProviderMatch, choose_best};

#[derive(Default, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    tracks: Tracks,
}

#[derive(Default, Deserialize)]
struct Tracks {
    #[serde(default)]
    items: Vec<SpotifyTrack>,
}

#[derive(Default, Deserialize)]
struct SpotifyTrack {
    id: String,
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    album: Album,
    #[serde(default)]
    duration_ms: Option<u64>,
}

#[derive(Default, Deserialize)]
struct Artist {
    name: String,
}

#[derive(Default, Deserialize)]
struct Album {
    name: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    images: Vec<Image>,
}

#[derive(Default, Deserialize)]
struct Image {
    url: String,
}

pub async fn search(
    client: &Client,
    token: Option<&str>,
    track: &Track,
) -> Result<Option<ProviderMatch>> {
    let Some(token) = token else {
        return Ok(None);
    };
    let keyword = format!("{} {}", track.artist, track.title);
    let response = client
        .get("https://api.spotify.com/v1/search")
        .bearer_auth(token)
        .query(&[("q", keyword.as_str()), ("type", "track"), ("limit", "10")])
        .send()
        .await
        .context("Spotify 搜索请求失败")?
        .error_for_status()
        .context("Spotify 搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析 Spotify 搜索结果失败")?;
    let candidates = response
        .tracks
        .items
        .into_iter()
        .map(|track| ProviderMatch {
            provider: ProviderKind::Spotify,
            source_id: track.id,
            title: track.name,
            artist: track
                .artists
                .into_iter()
                .map(|artist| artist.name)
                .collect::<Vec<_>>()
                .join("、"),
            album: track.album.name,
            duration_ms: track.duration_ms,
            release_date: track.album.release_date,
            cover_url: track.album.images.into_iter().next().map(|image| image.url),
            lyric_url: None,
            score: 0,
        })
        .collect();
    Ok(choose_best(track, candidates))
}
