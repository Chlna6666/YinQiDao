#![allow(dead_code)]

//! 歌词 / 封面 / 播放地址。

use serde_json::{Map, Value};

use crate::api::{PROVIDER_ID, parse_remote_track, session_for, value_string, value_u64};
use crate::bindings::yinqidao::music_plugin::types;
use crate::protocol::{self, Session};

use super::api::ApiResult;

// ---------------------------------------------------------------------------
// 歌词
// ---------------------------------------------------------------------------

pub fn lyrics(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> ApiResult<Option<types::LyricDocument>> {
    ensure_source(provider_id, track)?;
    let now_ms = host::now_ms();
    let session = session_for(account_id, now_ms)?;
    let mid = &track.source_id;
    let url = format!(
        "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg?songmid={mid}&pcachetime={now_ms}&g_tk=5381&loginUin=0&hostUin=0&inCharset=utf8&outCharset=utf-8&notice=0&platform=yqq&needNewCode=0"
    );
    let value = protocol::fcgi_get(&session, account_id, &url)?;
    let retcode = value.get("retcode").and_then(Value::as_i64).unwrap_or(-1);
    if retcode != 0 {
        return Ok(None);
    }
    let original = value
        .get("lyric")
        .and_then(Value::as_str)
        .and_then(decode_base64_utf8);
    let Some(original) = original.filter(|text| !text.trim().is_empty()) else {
        return Ok(None);
    };
    let translated = value
        .get("trans")
        .and_then(Value::as_str)
        .and_then(decode_base64_utf8)
        .unwrap_or_default();
    let lines = merge_lrc(&original, &translated);
    Ok(Some(types::LyricDocument {
        source: "qqmusic".into(),
        plain: Some(original),
        lines,
    }))
}

fn decode_base64_utf8(encoded: &str) -> Option<String> {
    let bytes = crate::crypto::base64_decode(encoded)?;
    String::from_utf8(bytes).ok()
}

// ---------------------------------------------------------------------------
// 封面
// ---------------------------------------------------------------------------

pub fn artwork(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> ApiResult<Option<types::ArtworkDescriptor>> {
    ensure_source(provider_id, track)?;
    let session = session_for(account_id, host::now_ms())?;
    let Some(album_mid) = song_album_mid(&session, &track.source_id)? else {
        return Ok(None);
    };
    Ok(Some(types::ArtworkDescriptor {
        url: format!("https://y.gtimg.cn/music/photo_new/T002R500x500M000{album_mid}.jpg"),
        headers: Vec::new(),
        expires_at_ms: None,
    }))
}

fn song_album_mid(session: &Session, mid: &str) -> ApiResult<Option<String>> {
    let mut param = Map::new();
    param.insert("song_mid".into(), Value::from(mid));
    let object = protocol::musicu(
        session,
        None,
        &[(
            "detail",
            "music.pf_song_detail_svr",
            "get_song_detail_yqq",
            Value::Object(param),
        )],
        false,
    )?;
    Ok(object
        .pointer("/detail/data/track_info/album/mid")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

// ---------------------------------------------------------------------------
// 播放地址（vkey）
// ---------------------------------------------------------------------------

/// 音质 → 文件名前缀候选（按探测优先级排列）。
/// 游客稳定可用 M500；M800/F000 需要登录/绿钻，探测式请求一次拿全。
fn quality_candidates(quality: Option<&str>) -> &'static [(&'static str, &'static str)] {
    let requested = quality.unwrap_or("").trim().to_ascii_lowercase();
    match requested.as_str() {
        "low" | "standard" | "128k" => &[("M500", "mp3")],
        "high" | "higher" | "320k" | "hq" => &[("M800", "mp3"), ("M500", "mp3")],
        "lossless" | "flac" | "sq" | "hires" => {
            &[("F000", "flac"), ("M800", "mp3"), ("M500", "mp3")]
        }
        _ => &[("M800", "mp3"), ("M500", "mp3")],
    }
}

pub fn stream(
    provider_id: &str,
    account_id: &str,
    request: &types::StreamRequest,
) -> ApiResult<types::StreamDescriptor> {
    ensure_source(provider_id, &request.track)?;
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;
    let mid = request.track.source_id.clone();

    // 播放文件名 = 前缀 + songmid + mediaMid + 后缀；mediaMid 缺省时以 songmid 兜底。
    let media_mid = song_media_mid(&session, &mid)?.unwrap_or_else(|| mid.clone());
    let candidates = quality_candidates(request.quality.as_deref());
    let filenames: Vec<String> = candidates
        .iter()
        .map(|(prefix, extension)| format!("{prefix}{mid}{media_mid}.{extension}"))
        .collect();

    let mut param = Map::new();
    param.insert(
        "filename".into(),
        Value::Array(filenames.iter().cloned().map(Value::from).collect()),
    );
    param.insert("guid".into(), Value::from(session.guid.clone()));
    param.insert(
        "songmid".into(),
        Value::Array(vec![Value::from(mid.clone())]),
    );
    param.insert("songtype".into(), Value::Array(vec![Value::from(0)]));
    param.insert("uin".into(), Value::from(session.uin.clone()));
    param.insert("loginflag".into(), Value::from(1));
    param.insert("platform".into(), Value::from("20"));

    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "vkey",
            "vkey.GetVkeyServer",
            "CgiGetVkey",
            Value::Object(param),
        )],
        false,
    )?;

    let sip = object
        .pointer("/vkey/data/sip")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let midurlinfo = object
        .pointer("/vkey/data/midurlinfo")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .cloned()
        .unwrap_or(Value::Null);
    let purl = value_string(&midurlinfo, &["purl"]).unwrap_or_default();
    if purl.is_empty() {
        let result = value_u64(&midurlinfo, &["result"]).unwrap_or(0);
        return Err(match result {
            104_003 => "QQ音乐当前账号/版权无该音质播放权限（可尝试降低音质或登录绿钻账号）".into(),
            104_004 => "QQ音乐 vkey 获取失败".into(),
            104_013 => "QQ音乐播放设备受限".into(),
            other => format!("QQ音乐未返回播放地址（result={other}）"),
        });
    }
    // 优先 https 前缀：过滤明文 ws 前缀，回退 isure 域。
    let base = sip
        .iter()
        .filter_map(Value::as_str)
        .find(|sip| sip.starts_with("https://"))
        .or_else(|| {
            sip.iter()
                .filter_map(Value::as_str)
                .find(|sip| !sip.starts_with("http://ws"))
        })
        .unwrap_or("https://isure.stream.qqmusic.qq.com/");
    let url = if base.ends_with('/') {
        format!("{base}{purl}")
    } else {
        format!("{base}/{purl}")
    };

    let bitrate = filenames
        .iter()
        .position(|filename| purl.ends_with(filename.as_str()))
        .or_else(|| {
            purl.find(&mid)
                .map(|index| match purl.as_bytes().get(index.saturating_sub(1)) {
                    Some(b'F') => 999_000,
                    Some(b'M') if index > 0 && purl.as_bytes()[index - 1] == b'8' => 320_000,
                    _ => 128_000,
                })
        });

    Ok(types::StreamDescriptor {
        url,
        headers: vec![types::KeyValue {
            key: "Referer".into(),
            value: "https://y.qq.com/".into(),
        }],
        codec: Some("mp3".into()),
        bitrate: bitrate.map(|value| u32::try_from(value).unwrap_or(128_000)),
        sample_rate: None,
        channels: None,
        expires_at_ms: None,
    })
}

fn song_media_mid(session: &Session, mid: &str) -> ApiResult<Option<String>> {
    let mut param = Map::new();
    param.insert("song_mid".into(), Value::from(mid));
    let object = protocol::musicu(
        session,
        None,
        &[(
            "detail",
            "music.pf_song_detail_svr",
            "get_song_detail_yqq",
            Value::Object(param),
        )],
        false,
    )?;
    Ok(object
        .pointer("/detail/data/track_info/file/mediaMid")
        .and_then(Value::as_str)
        .map(str::to_owned))
}

// ---------------------------------------------------------------------------
// 共享
// ---------------------------------------------------------------------------

pub(crate) fn ensure_source(provider_id: &str, source: &types::SourceTrackRef) -> ApiResult<()> {
    if provider_id != PROVIDER_ID {
        return Err(format!("unknown provider id: {provider_id}"));
    }
    if source.provider_id != PROVIDER_ID {
        return Err("source provider 与 QQ音乐 route 不匹配".into());
    }
    Ok(())
}

/// features.rs 入口：校验 provider 并构造会话后走猜你喜欢电台流。
pub(crate) fn radio_tracks_for(
    provider_id: &str,
    account_id: &str,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    if provider_id != PROVIDER_ID {
        return Err(format!("unknown provider id: {provider_id}"));
    }
    let _ = crate::api::require_stored_account(account_id)?;
    let session = session_for(Some(account_id), host::now_ms())?;
    radio_tracks(&session, Some(account_id), limit)
}

/// 猜你喜欢（匿名可用的推荐流），供 features.rs 复用。
pub(crate) fn radio_tracks(
    session: &Session,
    account_id: Option<&str>,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    let mut param = Map::new();
    param.insert("id".into(), Value::from(99));
    param.insert("num".into(), Value::from(u32::from(limit)));
    param.insert("from".into(), Value::from(0));
    param.insert("scene".into(), Value::from(0));
    param.insert("song_ids".into(), Value::Array(Vec::new()));
    let object = protocol::musicu(
        session,
        account_id,
        &[(
            "radio",
            "music.radioProxy.MbTrackRadioSvr",
            "get_radio_track",
            Value::Object(param),
        )],
        false,
    )?;
    let songs = object
        .pointer("/radio/data/tracks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    songs
        .iter()
        .take(usize::from(limit))
        .map(parse_remote_track)
        .collect()
}

/// 新歌速递（匿名可用）。
pub(crate) fn new_songs(limit: u16) -> ApiResult<Vec<types::RemoteTrack>> {
    let mut param = Map::new();
    param.insert("type".into(), Value::from(5));
    let object = protocol::musicu(
        &Session::guest(host::now_ms()),
        None,
        &[(
            "newsong",
            "newsong.NewSongServer",
            "get_new_song_info",
            Value::Object(param),
        )],
        false,
    )?;
    let songs = object
        .pointer("/newsong/data/songlist")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(songs
        .iter()
        .take(usize::from(limit))
        .filter_map(|value| parse_remote_track(value).ok())
        .collect())
}

use crate::bindings::yinqidao::music_plugin::host;

// ---------------------------------------------------------------------------
// LRC 解析（与网易云插件同构；逐字 QRC 解密留待 ag-1/3DES 版本）
// ---------------------------------------------------------------------------

fn merge_lrc(original: &str, translated: &str) -> Vec<types::LyricLine> {
    let mut lines = std::collections::BTreeMap::<u64, String>::new();
    for (timestamp, text) in parse_lrc(original) {
        lines.entry(timestamp).or_insert(text);
    }
    let translations = parse_lrc(translated)
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_parser_supports_multiple_timestamps_and_translation_merge() {
        let lines = merge_lrc(
            "[00:01.2]hello
[00:03]world",
            "[00:01.2]你好",
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[0].translation.as_deref(), Some("你好"));
        assert_eq!(lines[1].text, "world");
        assert!(lines[1].translation.is_none());
    }

    #[test]
    fn quality_candidates_cover_tiers() {
        assert_eq!(quality_candidates(Some("128k")), &[("M500", "mp3")]);
        assert_eq!(quality_candidates(Some("lossless"))[0], ("F000", "flac"));
        assert_eq!(
            quality_candidates(None),
            &[("M800", "mp3"), ("M500", "mp3")]
        );
    }
}
