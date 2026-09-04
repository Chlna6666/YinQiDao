use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Client;
use serde::Deserialize;

use crate::{lyrics::LyricsDocument, model::Track};

use super::{ProviderKind, ProviderMatch, choose_best};

const SEARCH_URL: &str = "https://mobilecdn.kugou.com/api/v3/search/song";
const LYRIC_SEARCH_URL: &str = "https://lyrics.kugou.com/search";
const LYRIC_DOWNLOAD_URL: &str = "https://lyrics.kugou.com/download";

#[derive(Default, Deserialize)]
struct SearchResponse {
    status: i32,
    #[serde(default)]
    data: SearchData,
}

#[derive(Default, Deserialize)]
struct SearchData {
    #[serde(default)]
    info: Vec<Song>,
}

#[derive(Default, Deserialize)]
struct Song {
    #[serde(default)]
    hash: String,
    #[serde(default)]
    songname: String,
    #[serde(default)]
    singername: String,
    #[serde(default)]
    album_name: String,
    #[serde(default)]
    timelen: u64,
    #[serde(default)]
    trans_param: TransParam,
}

#[derive(Default, Deserialize)]
struct TransParam {
    #[serde(default)]
    union_cover: String,
}

#[derive(Default, Deserialize)]
struct LyricsSearchResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
}

#[derive(Default, Deserialize)]
struct Candidate {
    id: serde_json::Value,
    #[serde(default)]
    accesskey: String,
}

#[derive(Default, Deserialize)]
struct LyricsDownloadResponse {
    #[serde(default)]
    content: String,
}

pub async fn search(client: &Client, track: &Track) -> Result<Option<ProviderMatch>> {
    let keyword = format!("{} {}", track.artist, track.title);
    let response = client
        .get(SEARCH_URL)
        .query(&[
            ("format", "json"),
            ("keyword", keyword.as_str()),
            ("showtype", "1"),
            ("page", "1"),
            ("pagesize", "10"),
        ])
        .send()
        .await
        .context("酷狗音乐搜索请求失败")?
        .error_for_status()
        .context("酷狗音乐搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析酷狗音乐搜索结果失败")?;
    if response.status != 1 {
        return Ok(None);
    }
    let candidates = response
        .data
        .info
        .into_iter()
        .map(|song| {
            let cover_url = (!song.trans_param.union_cover.is_empty())
                .then(|| song.trans_param.union_cover.replace("{size}", "800"));
            ProviderMatch {
                provider: ProviderKind::Kugou,
                source_id: song.hash,
                title: song.songname,
                artist: song.singername,
                album: song.album_name,
                duration_ms: Some(song.timelen),
                release_date: None,
                cover_url,
                lyric_url: None,
                score: 0,
            }
        })
        .collect();
    Ok(choose_best(track, candidates))
}

pub async fn lyrics(client: &Client, matched: &ProviderMatch) -> Result<Option<LyricsDocument>> {
    let response = client
        .get(LYRIC_SEARCH_URL)
        .query(&[
            ("keyword", ""),
            ("duration", "99999"),
            ("hash", matched.source_id.as_str()),
        ])
        .send()
        .await
        .context("酷狗歌词搜索请求失败")?
        .error_for_status()
        .context("酷狗歌词搜索返回错误状态")?
        .json::<LyricsSearchResponse>()
        .await
        .context("解析酷狗歌词搜索结果失败")?;
    let Some(candidate) = response.candidates.into_iter().next() else {
        return Ok(None);
    };
    let Some(id) = candidate
        .id
        .as_str()
        .map(str::to_owned)
        .or_else(|| candidate.id.as_u64().map(|id| id.to_string()))
    else {
        return Ok(None);
    };
    let content = client
        .get(LYRIC_DOWNLOAD_URL)
        .query(&[
            ("ver", "1"),
            ("client", "pc"),
            ("id", id.as_str()),
            ("accesskey", candidate.accesskey.as_str()),
            ("fmt", "lrc"),
            ("charset", "utf8"),
        ])
        .send()
        .await
        .context("酷狗歌词下载请求失败")?
        .error_for_status()
        .context("酷狗歌词下载返回错误状态")?
        .json::<LyricsDownloadResponse>()
        .await
        .context("解析酷狗歌词下载结果失败")?;
    let bytes = STANDARD
        .decode(content.content)
        .context("解码酷狗歌词失败")?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(LyricsDocument::from_sources(
            None,
            Some(text),
            None,
            "酷狗音乐",
        )))
    }
}
