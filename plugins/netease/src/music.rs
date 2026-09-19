use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use crate::{bindings::yinqidao::music_plugin::types, protocol};

const PROVIDER_ID: &str = "netease";
const MAX_QUERY_BYTES: usize = 2 * 1024;
const MAX_SEARCH_LIMIT: u16 = 100;
const MAX_DETAIL_IDS: usize = 200;

pub fn search(
    provider_id: &str,
    account_id: Option<&str>,
    query: &str,
    limit: u16,
) -> Result<Vec<types::RemoteTrack>, String> {
    ensure_provider(provider_id)?;
    let query = query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES || query.contains('\0') {
        return Err("搜索关键字为空或超过大小限制".into());
    }
    if limit == 0 || limit > MAX_SEARCH_LIMIT {
        return Err(format!("搜索 limit 必须位于 1..={MAX_SEARCH_LIMIT}"));
    }

    let response = request_eapi(
        account_id,
        "/api/cloudsearch/pc",
        json!({
            "s": query,
            "type": 1,
            "limit": limit,
            "offset": 0,
            "total": true,
        }),
    )?;
    Ok(response
        .pointer("/result/songs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(usize::from(limit))
        .filter_map(|s| parse_remote_track(s).ok())
        .collect())
}

pub fn resolve_track(
    provider_id: &str,
    account_id: Option<&str>,
    query: &types::TrackQuery,
) -> Result<Option<types::RemoteTrack>, String> {
    ensure_provider(provider_id)?;
    let title = query.title.trim();
    if title.is_empty() {
        return Ok(None);
    }
    let mut terms = title.to_owned();
    if let Some(artist) = query.artists.first().map(String::as_str).map(str::trim)
        && !artist.is_empty()
    {
        terms.push(' ');
        terms.push_str(artist);
    }

    let candidates = search(provider_id, account_id, &terms, 20)?;
    let normalized_title = normalize_identity_text(title);
    let normalized_artists = query
        .artists
        .iter()
        .map(|artist| normalize_identity_text(artist))
        .filter(|artist| !artist.is_empty())
        .collect::<HashSet<_>>();

    let mut best: Option<(u8, types::RemoteTrack)> = None;
    for track in candidates {
        let mut score = 0_u8;
        if normalize_identity_text(&track.title) == normalized_title {
            score = score.saturating_add(4);
        }
        if !normalized_artists.is_empty()
            && track
                .artists
                .iter()
                .map(|artist| normalize_identity_text(artist))
                .any(|artist| normalized_artists.contains(&artist))
        {
            score = score.saturating_add(2);
        }
        if let (Some(expected), Some(actual)) = (query.duration_ms, track.duration_ms)
            && expected.abs_diff(actual) <= 3_000
        {
            score = score.saturating_add(1);
        }
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, track));
        }
    }
    Ok(best
        .filter(|(score, _)| *score >= 4)
        .map(|(_, track)| track))
}

pub fn lyrics(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> Result<Option<types::LyricDocument>, String> {
    ensure_source(provider_id, track)?;
    let id = numeric_source_id(&track.source_id)?;
    let response = request_eapi(
        account_id,
        "/api/song/lyric/v1",
        json!({
            "id": id.to_string(),
            "cp": false,
            "tv": 0,
            "lv": 0,
            "rv": 0,
            "kv": 0,
            "yv": 0,
            "ytv": 0,
            "yrv": 0,
        }),
    )?;

    let original = response.pointer("/lrc/lyric").and_then(Value::as_str);
    let word_by_word = response
        .pointer("/yrc/lyric")
        .and_then(Value::as_str)
        .or_else(|| response.pointer("/klyric/lyric").and_then(Value::as_str));
    if original.is_none() && word_by_word.is_none() {
        return Ok(None);
    }
    let translated = response
        .pointer("/tlyric/lyric")
        .and_then(Value::as_str)
        .unwrap_or("");

    let lines = word_by_word
        .map(|value| merge_yrc(value, translated))
        .filter(|lines| !lines.is_empty())
        .unwrap_or_else(|| merge_lrc(original.unwrap_or(""), translated));
    let plain = original
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);

    Ok(Some(types::LyricDocument {
        source: "netease".into(),
        plain,
        lines,
    }))
}

pub fn artwork(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> Result<Option<types::ArtworkDescriptor>, String> {
    ensure_source(provider_id, track)?;
    let songs = song_detail(account_id, &[numeric_source_id(&track.source_id)?])?;
    let Some(song) = songs.first() else {
        return Ok(None);
    };
    let url = song
        .get("al")
        .or_else(|| song.get("album"))
        .and_then(|album| value_string(album, &["picUrl"]));
    Ok(url.map(|url| types::ArtworkDescriptor {
        url: force_https(url),
        headers: Vec::new(),
        expires_at_ms: None,
    }))
}

pub fn stream(
    provider_id: &str,
    account_id: &str,
    request: &types::StreamRequest,
) -> Result<types::StreamDescriptor, String> {
    ensure_source(provider_id, &request.track)?;
    validate_account_id(account_id)?;
    let id = numeric_source_id(&request.track.source_id)?;
    let level = quality_level(request.quality.as_deref());
    let mut data = json!({
        "ids": format!("[{id}]"),
        "level": level,
        "encodeType": "flac",
    });
    if level == "sky" {
        data["immerseType"] = Value::String("c51".into());
    }
    // 优先尝试网易云官方接口获取音频流
    if let Ok(response) = protocol::eapi(account_id, "/api/song/enhance/player/url/v1", data) {
        let item = response
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first());
        if let Some(item) = item {
            let url = value_string(item, &["url"]).filter(|url| !url.is_empty());
            if let Some(url) = url {
                return Ok(types::StreamDescriptor {
                    url: force_https(url),
                    headers: vec![types::KeyValue {
                        key: "Referer".into(),
                        value: "https://music.163.com/".into(),
                    }],
                    codec: value_string(item, &["type"])
                        .map(|value| value.to_ascii_lowercase())
                        .filter(|value| !value.is_empty()),
                    bitrate: value_u64(item, &["br"]).and_then(|value| u32::try_from(value).ok()),
                    sample_rate: value_u64(item, &["sr"]).and_then(|value| u32::try_from(value).ok()),
                    channels: value_u64(item, &["channels"])
                        .or_else(|| value_u64(item, &["channel"]))
                        .and_then(|value| u16::try_from(value).ok()),
                    expires_at_ms: None,
                });
            }
        }
    }

    // 官方接口未返回可用播放地址（歌曲为灰色/版权受限/VIP限制），自动启动音源解灰
    let (title, artists, duration_ms) = {
        let songs = song_detail(Some(account_id), &[id]).unwrap_or_default();
        if let Some(song) = songs.first() {
            let name = value_string(song, &["name"]).unwrap_or_default();
            let arts = song
                .get("ar")
                .or_else(|| song.get("artists"))
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter_map(|v| value_string(v, &["name"])).collect())
                .unwrap_or_default();
            let dur = value_u64(song, &["dt"]).or_else(|| value_u64(song, &["duration"]));
            (name, arts, dur)
        } else {
            (String::new(), Vec::new(), None)
        }
    };

    crate::unblock::resolve_stream(id, &title, &artists, duration_ms)
}

fn request_eapi(account_id: Option<&str>, path: &str, data: Value) -> Result<Value, String> {
    match account_id {
        Some(account_id) => protocol::eapi(account_id, path, data),
        None => protocol::eapi_anonymous(path, data),
    }
}

fn request_weapi(account_id: Option<&str>, path: &str, data: Value) -> Result<Value, String> {
    match account_id {
        Some(account_id) => protocol::weapi(account_id, path, data),
        None => protocol::weapi_anonymous(path, data),
    }
}

pub fn song_detail(account_id: Option<&str>, ids: &[u64]) -> Result<Vec<Value>, String> {
    if ids.is_empty() || ids.len() > MAX_DETAIL_IDS {
        return Err("歌曲详情批量 id 数量非法".into());
    }
    let c = ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>();
    let c = serde_json::to_string(&c)
        .map_err(|error| format!("序列化网易云歌曲详情请求失败: {error}"))?;
    let response = request_weapi(account_id, "/api/v3/song/detail", json!({ "c": c }))?;
    Ok(response
        .get("songs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub fn parse_remote_track(value: &Value) -> Result<types::RemoteTrack, String> {
    let id = value_u64(value, &["id"]).ok_or_else(|| "歌曲缺少 id".to_string())?;
    let title = value_string(value, &["name"])
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "歌曲缺少 name".to_string())?;

    let mut artists = Vec::new();
    if let Some(arr) = value.get("ar").or_else(|| value.get("artists")).and_then(Value::as_array) {
        for a in arr {
            if let Some(name) = a.get("name").and_then(Value::as_str) {
                if !name.is_empty() {
                    artists.push(name.to_string());
                }
            } else if let Some(name) = a.as_str() {
                if !name.is_empty() {
                    artists.push(name.to_string());
                }
            }
        }
    }

    let album_value = value.get("al").or_else(|| value.get("album"));
    let album = album_value
        .and_then(|alb| alb.get("name"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_default();

    let cover_url = album_value
        .and_then(|alb| alb.get("picUrl").or_else(|| alb.get("coverImgUrl")))
        .and_then(Value::as_str)
        .or_else(|| value.get("picUrl").and_then(Value::as_str))
        .map(str::to_string)
        .map(force_https);

    let duration_ms = value_u64(value, &["dt"]).or_else(|| value_u64(value, &["duration"]));
    let isrc = value_string(value, &["isrc"]);

    Ok(types::RemoteTrack {
        source: types::SourceTrackRef {
            provider_id: PROVIDER_ID.into(),
            source_id: id.to_string(),
        },
        title,
        artists,
        album,
        duration_ms,
        isrc,
        cover_url,
        playable: true,
        explicit: false,
    })
}

fn merge_yrc(original: &str, translated: &str) -> Vec<types::LyricLine> {
    let translations = parse_lrc(translated)
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    original
        .lines()
        .filter_map(parse_yrc_line)
        .map(
            |(timestamp_ms, _duration_ms, text, words)| types::LyricLine {
                timestamp_ms,
                text,
                translation: translations
                    .get(&timestamp_ms)
                    .filter(|value| !value.trim().is_empty())
                    .cloned(),
                words,
            },
        )
        .collect()
}

fn parse_yrc_line(raw: &str) -> Option<(u64, u64, String, Vec<types::LyricWord>)> {
    let raw = raw.trim();
    let after_open = raw.strip_prefix('[')?;
    let header_end = after_open.find(']')?;
    let mut header = after_open[..header_end].split(',');
    let timestamp_ms = header.next()?.trim().parse::<u64>().ok()?;
    let duration_ms = header.next()?.trim().parse::<u64>().ok()?;
    let mut rest = &after_open[header_end + 1..];
    let mut words = Vec::new();
    let mut text = String::new();

    while let Some(after_word_open) = rest.strip_prefix('(') {
        let metadata_end = after_word_open.find(')')?;
        let mut metadata = after_word_open[..metadata_end].split(',');
        let word_timestamp = metadata.next()?.trim().parse::<u64>().ok()?;
        let word_duration = metadata.next()?.trim().parse::<u64>().ok()?;
        let after_metadata = &after_word_open[metadata_end + 1..];
        let next = after_metadata.find('(').unwrap_or(after_metadata.len());
        let word_text = &after_metadata[..next];
        text.push_str(word_text);
        words.push(types::LyricWord {
            timestamp_ms: word_timestamp,
            duration_ms: Some(word_duration),
            text: word_text.to_owned(),
        });
        rest = &after_metadata[next..];
    }

    if words.is_empty() {
        text = rest.trim().to_owned();
    }
    Some((timestamp_ms, duration_ms, text, words))
}

fn merge_lrc(original: &str, translated: &str) -> Vec<types::LyricLine> {
    let mut lines = BTreeMap::<u64, String>::new();
    for (timestamp, text) in parse_lrc(original) {
        lines.entry(timestamp).or_insert(text);
    }
    let translations = parse_lrc(translated)
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    lines
        .into_iter()
        .map(|(timestamp_ms, text)| types::LyricLine {
            timestamp_ms,
            text,
            translation: translations
                .get(&timestamp_ms)
                .filter(|translation| !translation.trim().is_empty())
                .cloned(),
            words: Vec::new(),
        })
        .collect()
}

fn parse_lrc(input: &str) -> Vec<(u64, String)> {
    let mut output = Vec::new();
    for raw_line in input.lines() {
        let mut rest = raw_line.trim();
        let mut stamps = Vec::new();
        while let Some(after_open) = rest.strip_prefix('[') {
            let Some(end) = after_open.find(']') else {
                break;
            };
            let stamp = &after_open[..end];
            let Some(timestamp) = parse_lrc_timestamp(stamp) else {
                break;
            };
            stamps.push(timestamp);
            rest = &after_open[end + 1..];
        }
        let text = rest.trim().to_owned();
        for stamp in stamps {
            output.push((stamp, text.clone()));
        }
    }
    output.sort_by_key(|(timestamp, _)| *timestamp);
    output
}

fn parse_lrc_timestamp(value: &str) -> Option<u64> {
    let (minutes, seconds) = value.split_once(':')?;
    let minutes = minutes.trim().parse::<u64>().ok()?;
    let seconds = seconds.trim();
    let (whole_seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let whole_seconds = whole_seconds.parse::<u64>().ok()?;
    if whole_seconds >= 60 {
        return None;
    }
    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()?.saturating_mul(100),
        2 => fraction.parse::<u64>().ok()?.saturating_mul(10),
        _ => fraction.get(..3)?.parse::<u64>().ok()?,
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(whole_seconds.checked_mul(1_000)?)?
        .checked_add(millis)
}

fn quality_level(quality: Option<&str>) -> &'static str {
    match quality.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" | "standard" | "128k" => "standard",
        "higher" | "192k" => "higher",
        "high" | "exhigh" | "320k" => "exhigh",
        "lossless" => "lossless",
        "hires" => "hires",
        "jyeffect" => "jyeffect",
        "sky" => "sky",
        "dolby" => "dolby",
        _ => "exhigh",
    }
}

fn normalize_identity_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

fn ensure_provider(provider_id: &str) -> Result<(), String> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
}

fn ensure_source(provider_id: &str, source: &types::SourceTrackRef) -> Result<(), String> {
    ensure_provider(provider_id)?;
    if source.provider_id != PROVIDER_ID {
        return Err("source provider 与网易云 route 不匹配".into());
    }
    Ok(())
}

fn validate_account_id(account_id: &str) -> Result<(), String> {
    if account_id.is_empty()
        || account_id.len() > 32
        || !account_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("网易云 account id 非法".into());
    }
    Ok(())
}

fn numeric_source_id(source_id: &str) -> Result<u64, String> {
    if source_id.is_empty()
        || source_id.len() > 32
        || !source_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("网易云 source id 非法".into());
    }
    source_id
        .parse::<u64>()
        .map_err(|_| "网易云 source id 超出数值范围".into())
}

fn value_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_owned)
}

fn value_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_u64()
        .or_else(|| current.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| current.as_str().and_then(|value| value.parse::<u64>().ok()))
}

fn force_https(url: String) -> String {
    if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        url
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yrc_parser_preserves_word_timing() {
        let (_, _, text, words) =
            parse_yrc_line("[1000,900](1000,300,0)你(1300,200,0)好(1500,400,0)世界").unwrap();
        assert_eq!(text, "你好世界");
        assert_eq!(words.len(), 3);
        assert_eq!(words[0].timestamp_ms, 1000);
        assert_eq!(words[2].duration_ms, Some(400));
    }

    #[test]
    fn quality_aliases_map_to_v1_levels() {
        assert_eq!(quality_level(Some("320k")), "exhigh");
        assert_eq!(quality_level(Some("lossless")), "lossless");
        assert_eq!(quality_level(Some("sky")), "sky");
    }
}
