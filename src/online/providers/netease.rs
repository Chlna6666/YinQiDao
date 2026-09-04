use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::{lyrics::LyricsDocument, model::Track};

use super::{ProviderKind, ProviderMatch, choose_best, unix_millis_to_date};

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    result: SearchResult,
}

#[derive(Default, Deserialize)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<Song>,
}

#[derive(Deserialize)]
struct Song {
    id: u64,
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    album: Album,
    #[serde(default)]
    duration: Option<u64>,
}

#[derive(Deserialize)]
struct Artist {
    name: String,
}

#[derive(Default, Deserialize)]
struct Album {
    name: String,
    #[serde(rename = "picUrl")]
    pic_url: Option<String>,
    #[serde(rename = "publishTime")]
    publish_time: Option<i64>,
}

#[derive(Deserialize)]
struct LyricsResponse {
    #[serde(default)]
    lrc: LyricsBody,
    #[serde(default)]
    tlyric: LyricsBody,
}

#[derive(Default, Deserialize)]
struct LyricsBody {
    #[serde(default)]
    lyric: String,
}

pub async fn search(
    client: &Client,
    base_url: &str,
    track: &Track,
) -> Result<Option<ProviderMatch>> {
    let keyword = format!("{} {}", track.artist, track.title);
    let response = client
        .get(format!("{}/search", base_url.trim_end_matches('/')))
        .query(&[("keywords", keyword.as_str()), ("limit", "10")])
        .send()
        .await
        .context("网易云搜索请求失败")?
        .error_for_status()
        .context("网易云搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析网易云搜索结果失败")?;
    let candidates = response
        .result
        .songs
        .into_iter()
        .map(|song| ProviderMatch {
            provider: ProviderKind::Netease,
            source_id: song.id.to_string(),
            title: song.name,
            artist: song
                .artists
                .into_iter()
                .map(|artist| artist.name)
                .collect::<Vec<_>>()
                .join("、"),
            album: song.album.name,
            duration_ms: song.duration,
            release_date: song.album.publish_time.and_then(format_publish_date),
            cover_url: song.album.pic_url,
            lyric_url: None,
            score: 0,
        })
        .collect();
    Ok(choose_best(track, candidates))
}

pub async fn lyrics(client: &Client, matched: &ProviderMatch) -> Result<Option<LyricsDocument>> {
    let response = client
        .get("https://music.163.com/api/song/lyric")
        .query(&[
            ("id", matched.source_id.as_str()),
            ("lv", "1"),
            ("kv", "1"),
            ("tv", "1"),
        ])
        .send()
        .await
        .context("网易云歌词请求失败")?
        .error_for_status()
        .context("网易云歌词返回错误状态")?
        .json::<LyricsResponse>()
        .await
        .context("解析网易云歌词失败")?;
    let original = non_empty(response.lrc.lyric);
    let translation = non_empty(response.tlyric.lyric);
    if original.is_none() && translation.is_none() {
        return Ok(None);
    }
    let synced = original.clone().or_else(|| translation.clone());
    let translated = original.as_ref().and(translation.as_ref()).cloned();
    Ok(Some(LyricsDocument::from_sources(
        None,
        synced,
        translated,
        "网易云音乐",
    )))
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn format_publish_date(timestamp_ms: i64) -> Option<String> {
    unix_millis_to_date(timestamp_ms)
}
