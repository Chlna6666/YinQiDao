use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{lyrics::LyricsDocument, model::Track};

use super::{ProviderKind, ProviderMatch, choose_best};

const SEARCH_URL: &str = "https://u.y.qq.com/cgi-bin/musicu.fcg";
const LYRIC_URL: &str = "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg";

#[derive(Serialize)]
struct SearchRequest {
    comm: SearchComm,
    #[serde(rename = "music.search.SearchCgiService.DoSearchForQQMusicDesktop")]
    search: SearchBody,
}

#[derive(Default, Serialize)]
struct SearchComm {
    #[serde(rename = "tmeAppID")]
    tme_app_id: &'static str,
    ct: &'static str,
    cv: &'static str,
    nettype: &'static str,
    #[serde(rename = "tmeLoginType")]
    tme_login_type: &'static str,
}

#[derive(Serialize)]
struct SearchBody {
    module: &'static str,
    method: &'static str,
    param: SearchParams,
}

#[derive(Serialize)]
struct SearchParams {
    num_per_page: u8,
    page_num: u8,
    remoteplace: &'static str,
    search_type: u8,
    query: String,
    grp: u8,
    searchid: String,
    nqc_flag: u8,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(rename = "music.search.SearchCgiService.DoSearchForQQMusicDesktop")]
    search: SearchResponseBody,
}

#[derive(Default, Deserialize)]
struct SearchResponseBody {
    #[serde(default)]
    data: SearchData,
}

#[derive(Default, Deserialize)]
struct SearchData {
    #[serde(default)]
    body: SearchResponseBodyInner,
}

#[derive(Default, Deserialize)]
struct SearchResponseBodyInner {
    #[serde(default)]
    song: SongList,
}

#[derive(Default, Deserialize)]
struct SongList {
    #[serde(default)]
    list: Vec<Song>,
}

#[derive(Default, Deserialize)]
struct Song {
    mid: String,
    title: String,
    #[serde(default)]
    singer: Vec<Singer>,
    album: Album,
    #[serde(rename = "time_public")]
    time_public: String,
    #[serde(default)]
    interval: u64,
}

#[derive(Default, Deserialize)]
struct Singer {
    name: String,
}

#[derive(Default, Deserialize)]
struct Album {
    mid: String,
    title: String,
}

#[derive(Default, Deserialize)]
struct LyricsResponse {
    code: i32,
    lyric: String,
    trans: String,
}

pub async fn search(client: &Client, track: &Track) -> Result<Option<ProviderMatch>> {
    let keyword = format!("{} {}", track.artist, track.title);
    let request = SearchRequest {
        comm: SearchComm {
            tme_app_id: "qqmusic",
            ct: "6",
            cv: "80600",
            nettype: "2",
            tme_login_type: "2",
        },
        search: SearchBody {
            module: "music.search.SearchCgiService",
            method: "DoSearchForQQMusicDesktop",
            param: SearchParams {
                num_per_page: 10,
                page_num: 1,
                remoteplace: "txt.mac.search",
                search_type: 0,
                query: keyword,
                grp: 1,
                searchid: format!(
                    "yinqidao-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |duration| duration.as_nanos())
                ),
                nqc_flag: 0,
            },
        },
    };
    let response = client
        .post(SEARCH_URL)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("Referer", "https://y.qq.com")
        .json(&request)
        .send()
        .await
        .context("QQ音乐搜索请求失败")?
        .error_for_status()
        .context("QQ音乐搜索返回错误状态")?
        .json::<SearchResponse>()
        .await
        .context("解析 QQ 音乐搜索结果失败")?;
    let candidates = response
        .search
        .data
        .body
        .song
        .list
        .into_iter()
        .map(|song| ProviderMatch {
            provider: ProviderKind::QqMusic,
            source_id: song.mid,
            title: song.title,
            artist: song
                .singer
                .into_iter()
                .map(|singer| singer.name)
                .collect::<Vec<_>>()
                .join("、"),
            album: song.album.title,
            duration_ms: Some(song.interval.saturating_mul(1_000)),
            release_date: (!song.time_public.trim().is_empty()).then_some(song.time_public),
            cover_url: (!song.album.mid.is_empty()).then(|| {
                format!(
                    "https://y.qq.com/music/photo_new/T002R800x800M000{}.jpg",
                    song.album.mid
                )
            }),
            lyric_url: None,
            score: 0,
        })
        .collect();
    Ok(choose_best(track, candidates))
}

pub async fn lyrics(client: &Client, matched: &ProviderMatch) -> Result<Option<LyricsDocument>> {
    let response = client
        .get(LYRIC_URL)
        .query(&[
            ("g_tk", "5381"),
            ("format", "json"),
            ("inCharset", "utf-8"),
            ("outCharset", "utf-8"),
            ("notice", "0"),
            ("platform", "h5"),
            ("needNewCode", "1"),
            ("ct", "121"),
            ("cv", "0"),
            ("songmid", matched.source_id.as_str()),
        ])
        .header("Referer", "https://y.qq.com")
        .send()
        .await
        .context("QQ音乐歌词请求失败")?
        .error_for_status()
        .context("QQ音乐歌词返回错误状态")?
        .json::<LyricsResponse>()
        .await
        .context("解析 QQ 音乐歌词失败")?;
    if response.code != 0 {
        return Ok(None);
    }
    let original = decode_lyric(&response.lyric);
    let translation = decode_lyric(&response.trans);
    if original.trim().is_empty() && translation.trim().is_empty() {
        return Ok(None);
    }
    let synced = (!original.trim().is_empty()).then_some(original.clone());
    let translation = (!translation.trim().is_empty()).then_some(translation);
    Ok(Some(LyricsDocument::from_sources(
        None,
        synced,
        translation,
        "QQ音乐",
    )))
}

fn decode_lyric(value: &str) -> String {
    STANDARD
        .decode(value)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .filter(|decoded| decoded.contains('['))
        .unwrap_or_else(|| value.to_owned())
}
