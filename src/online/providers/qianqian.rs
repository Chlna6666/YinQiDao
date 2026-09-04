use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use md5::{Digest, Md5};
use reqwest::Client;
use serde::Deserialize;

use crate::{lyrics::LyricsDocument, model::Track};

use super::{ProviderKind, ProviderMatch, choose_best};

const SEARCH_URL: &str = "https://music.91q.com/v1/search";
const SECRET: &str = "0b50b02fd0d73a9c4c8c3a781c30845f";

#[derive(Default, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: SearchData,
}

#[derive(Default, Deserialize)]
struct SearchData {
    #[serde(rename = "typeTrack", default)]
    tracks: Vec<Song>,
}

#[derive(Default, Deserialize)]
struct Song {
    #[serde(rename = "TSID", default)]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    artist: Vec<Artist>,
    #[serde(rename = "albumTitle", default)]
    album: String,
    #[serde(default)]
    pic: Option<String>,
    #[serde(default)]
    lyric: Option<String>,
}

#[derive(Default, Deserialize)]
struct Artist {
    name: String,
}

pub async fn search(client: &Client, track: &Track) -> Result<Option<ProviderMatch>> {
    let keyword = format!("{} {}", track.artist, track.title);
    let mut params = BTreeMap::from([
        ("appid", "16073360".to_owned()),
        ("pageNo", "1".to_owned()),
        ("pageSize", "10".to_owned()),
        ("type", "1".to_owned()),
        ("word", keyword),
    ]);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    params.insert("timestamp", timestamp.to_string());
    let mut sign_input = params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    sign_input.push_str(SECRET);
    let mut hasher = Md5::new();
    hasher.update(sign_input.as_bytes());
    params.insert("sign", format!("{:x}", hasher.finalize()));

    let response = client
        .get(SEARCH_URL)
        .query(&params)
        .header("Referer", "https://music.91q.com/player")
        .send()
        .await
        .context("千千音乐搜索请求失败")?
        .error_for_status()
        .context("千千音乐搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析千千音乐搜索结果失败")?;
    let candidates = response
        .data
        .tracks
        .into_iter()
        .map(|song| ProviderMatch {
            provider: ProviderKind::Qianqian,
            source_id: song.id,
            title: song.title,
            artist: song
                .artist
                .into_iter()
                .map(|artist| artist.name)
                .collect::<Vec<_>>()
                .join("、"),
            album: song.album,
            duration_ms: None,
            release_date: None,
            cover_url: song.pic,
            lyric_url: song.lyric,
            score: 0,
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
        .header("Referer", "https://music.91q.com/player")
        .send()
        .await
        .context("千千音乐歌词请求失败")?
        .error_for_status()
        .context("千千音乐歌词返回错误状态")?
        .text()
        .await
        .context("读取千千音乐歌词失败")?;
    if text.trim().is_empty() {
        Ok(None)
    } else {
        Ok(Some(LyricsDocument::from_sources(
            None,
            Some(text),
            None,
            "千千音乐",
        )))
    }
}
