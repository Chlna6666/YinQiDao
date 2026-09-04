use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

use crate::{lyrics::LyricsDocument, model::Track};

use super::{ProviderKind, ProviderMatch, choose_best};

const SEARCH_URL: &str = "https://pd.musicapp.migu.cn/MIGUM2.0/v1.0/content/search_all.do";

#[derive(Default, Deserialize)]
struct SearchResponse {
    #[serde(rename = "songResultData", default)]
    songs: SongResult,
}

#[derive(Default, Deserialize)]
struct SongResult {
    #[serde(default)]
    result: Vec<Song>,
}

#[derive(Default, Deserialize)]
struct Song {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    singers: Vec<Singer>,
    #[serde(default)]
    albums: Vec<Album>,
    #[serde(rename = "imgItems", default)]
    images: Vec<Image>,
    #[serde(rename = "lyricUrl", default)]
    lyric_url: String,
    #[serde(rename = "trcUrl", default)]
    trc_url: String,
    #[serde(rename = "contentId", default)]
    content_id: String,
    #[serde(rename = "rateFormats", default)]
    rate_formats: Vec<RateFormat>,
}

#[derive(Default, Deserialize)]
struct Singer {
    name: String,
}

#[derive(Default, Deserialize)]
struct Album {
    name: String,
}

#[derive(Default, Deserialize)]
struct Image {
    img: String,
}

#[derive(Default, Deserialize)]
struct RateFormat {
    #[serde(rename = "duration", default)]
    duration_ms: Option<u64>,
}

pub async fn search(client: &Client, track: &Track) -> Result<Option<ProviderMatch>> {
    let keyword = format!("{} {}", track.artist, track.title);
    let response = client
        .get(SEARCH_URL)
        .query(&[
            ("ua", "Android_migu"),
            ("version", "5.0.1"),
            ("text", keyword.as_str()),
            ("pageNo", "1"),
            ("pageSize", "10"),
            ("searchSwitch", r#"{"song":1,"album":0,"singer":0,"tagSong":0,"mvSong":0,"songlist":0,"bestShow":1}"#),
        ])
        .header("Referer", "https://music.migu.cn/")
        .send()
        .await
        .context("咪咕音乐搜索请求失败")?
        .error_for_status()
        .context("咪咕音乐搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析咪咕音乐搜索结果失败")?;
    let candidates = response
        .songs
        .result
        .into_iter()
        .map(|song| {
            let album = song
                .albums
                .into_iter()
                .next()
                .map_or_else(String::new, |album| album.name);
            let cover_url = song.images.into_iter().next().map(|image| image.img);
            let lyric_url = non_empty(song.lyric_url).or_else(|| non_empty(song.trc_url));
            let source_id = non_empty(song.content_id)
                .or_else(|| non_empty(song.id))
                .unwrap_or_default();
            let duration_ms = song
                .rate_formats
                .into_iter()
                .find_map(|rate| rate.duration_ms);
            ProviderMatch {
                provider: ProviderKind::Migu,
                source_id,
                title: song.name,
                artist: song
                    .singers
                    .into_iter()
                    .map(|singer| singer.name)
                    .collect::<Vec<_>>()
                    .join("、"),
                album,
                duration_ms,
                release_date: None,
                cover_url,
                lyric_url,
                score: 0,
            }
        })
        .collect();
    Ok(choose_best(track, candidates))
}

pub async fn lyrics(client: &Client, matched: &ProviderMatch) -> Result<Option<LyricsDocument>> {
    let Some(url) = matched.lyric_url.as_deref() else {
        return Ok(None);
    };
    let text = client
        .get(url)
        .header("Referer", "https://music.migu.cn/")
        .send()
        .await
        .context("咪咕音乐歌词请求失败")?
        .error_for_status()
        .context("咪咕音乐歌词返回错误状态")?
        .text()
        .await
        .context("读取咪咕音乐歌词失败")?;
    if text.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(LyricsDocument::from_sources(
            None,
            Some(text),
            None,
            "咪咕音乐",
        )))
    }
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}
