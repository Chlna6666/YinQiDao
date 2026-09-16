use std::collections::HashSet;

use serde_json::{Value, json};

use crate::{
    api, protocol,
    bindings::yinqidao::music_plugin::types,
};

const PROVIDER_ID: &str = "netease";
const MAX_RECOMMENDATION_LIMIT: u16 = 100;

pub fn recommendations(
    provider_id: &str,
    account_id: &str,
    request: &types::RecommendationRequest,
) -> Result<Vec<types::RecommendationItem>, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if request.limit == 0 || request.limit > MAX_RECOMMENDATION_LIMIT {
        return Err(format!(
            "recommendation limit 必须位于 1..={MAX_RECOMMENDATION_LIMIT}"
        ));
    }

    let (mut tracks, reason) = match request.surface.clone() {
        types::RecommendationSurface::Home | types::RecommendationSurface::Discovery => (
            personalized_new_music(account_id, request.limit)?,
            "网易云个性推荐",
        ),
        types::RecommendationSurface::DailyMix => {
            (daily_recommendations(account_id)?, "网易云每日推荐")
        }
        types::RecommendationSurface::SimilarTrack => (
            similar_tracks(account_id, request.seed.as_ref())?,
            "网易云相似歌曲",
        ),
        types::RecommendationSurface::ContinueListening => {
            (personal_fm(account_id)?, "网易云私人 FM")
        }
        types::RecommendationSurface::ArtistRadio => {
            return Err("网易云 Artist Radio 需要 artist-aware recommendation ABI，当前不会伪装为私人 FM".into());
        }
    };

    let excluded = request
        .exclude
        .iter()
        .filter(|source| source.provider_id == PROVIDER_ID)
        .map(|source| source.source_id.clone())
        .collect::<HashSet<_>>();
    tracks.retain(|track| !excluded.contains(&track.source.source_id));
    tracks.truncate(usize::from(request.limit));

    Ok(tracks
        .into_iter()
        .map(|track| types::RecommendationItem {
            track,
            score: None,
            reason: Some(reason.into()),
        })
        .collect())
}

pub fn set_liked(
    provider_id: &str,
    account_id: &str,
    track: &types::SourceTrackRef,
    liked: bool,
) -> Result<bool, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if track.provider_id != PROVIDER_ID {
        return Err("红心同步 source provider 与网易云 route 不匹配".into());
    }
    let song_id = numeric_source_id(&track.source_id)?;
    let json = protocol::weapi(
        account_id,
        "/api/song/like",
        json!({
            "trackId": song_id.to_string(),
            "userid": account_id,
            "like": liked,
        }),
    )?;
    Ok(api_code_success(&json))
}

pub fn report_playback(
    provider_id: &str,
    account_id: &str,
    signal: &types::PlaybackSignal,
) -> Result<bool, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;

    match signal.kind.clone() {
        types::PlaybackSignalKind::Liked | types::PlaybackSignalKind::Unliked => {
            let source = require_signal_source(signal)?;
            set_liked(
                provider_id,
                account_id,
                source,
                matches!(signal.kind, types::PlaybackSignalKind::Liked),
            )
        }
        types::PlaybackSignalKind::Disliked => {
            let source = require_signal_source(signal)?;
            fm_trash(account_id, source, signal.position_ms)
        }
        types::PlaybackSignalKind::Completed => {
            let source = require_signal_source(signal)?;
            scrobble(account_id, source, signal.duration_ms.max(signal.position_ms))
        }
        types::PlaybackSignalKind::Skipped => {
            if signal.position_ms < 30_000 {
                return Ok(false);
            }
            let source = require_signal_source(signal)?;
            scrobble(account_id, source, signal.position_ms)
        }
        types::PlaybackSignalKind::Started => Ok(false),
    }
}

fn require_signal_source(signal: &types::PlaybackSignal) -> Result<&types::SourceTrackRef, String> {
    let source = signal
        .source
        .as_ref()
        .ok_or_else(|| "网易云 playback event 缺少精确 source id".to_string())?;
    if source.provider_id != PROVIDER_ID {
        return Err("playback event source provider 与网易云 route 不匹配".into());
    }
    numeric_source_id(&source.source_id)?;
    Ok(source)
}

fn fm_trash(
    account_id: &str,
    source: &types::SourceTrackRef,
    position_ms: u64,
) -> Result<bool, String> {
    let song_id = numeric_source_id(&source.source_id)?;
    let seconds = (position_ms / 1_000).clamp(1, 86_400);
    let json = protocol::weapi(
        account_id,
        "/api/radio/trash/add",
        json!({
            "songId": song_id.to_string(),
            "alg": "RT",
            "time": seconds,
        }),
    )?;
    Ok(api_code_success(&json))
}

fn scrobble(
    account_id: &str,
    source: &types::SourceTrackRef,
    played_ms: u64,
) -> Result<bool, String> {
    let song_id = numeric_source_id(&source.source_id)?;
    let seconds = (played_ms / 1_000).min(86_400);
    let logs = json!([{
        "action": "play",
        "json": {
            "download": 0,
            "end": "playend",
            "id": song_id.to_string(),
            "sourceId": "",
            "time": seconds,
            "type": "song",
            "wifi": 0,
            "source": "list",
            "mainsite": 1,
            "content": ""
        }
    }]);
    let logs = serde_json::to_string(&logs)
        .map_err(|error| format!("序列化网易云听歌打卡失败: {error}"))?;
    let json = protocol::weapi(
        account_id,
        "/api/feedback/weblog",
        json!({ "logs": logs }),
    )?;
    Ok(api_code_success(&json))
}

fn daily_recommendations(account_id: &str) -> Result<Vec<types::RemoteTrack>, String> {
    let json = protocol::weapi(
        account_id,
        "/api/v3/discovery/recommend/songs",
        json!({}),
    )?;
    parse_track_array(json.pointer("/data/dailySongs"))
}

fn personalized_new_music(
    account_id: &str,
    limit: u16,
) -> Result<Vec<types::RemoteTrack>, String> {
    let json = protocol::weapi(
        account_id,
        "/api/personalized/newsong",
        json!({
            "type": "recommend",
            "limit": limit,
            "areaId": 0,
        }),
    )?;
    parse_track_array(json.get("result"))
}

fn personal_fm(account_id: &str) -> Result<Vec<types::RemoteTrack>, String> {
    let json = protocol::weapi(account_id, "/api/v1/radio/get", json!({}))?;
    parse_track_array(json.get("data"))
}

fn similar_tracks(
    account_id: &str,
    seed: Option<&types::TrackQuery>,
) -> Result<Vec<types::RemoteTrack>, String> {
    let seed = seed.ok_or_else(|| "相似歌曲推荐缺少 seed track".to_string())?;
    let resolved = api::resolve_track(PROVIDER_ID, Some(account_id), seed)?
        .ok_or_else(|| "无法在网易云解析相似歌曲 seed".to_string())?;
    let song_id = numeric_source_id(&resolved.source.source_id)?;
    let path = format!("/api/discovery/simiSong?songid={song_id}");
    let json = protocol::get(account_id, &path)?;
    parse_track_array(json.get("songs"))
}

fn parse_track_array(value: Option<&Value>) -> Result<Vec<types::RemoteTrack>, String> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|value| parse_remote_track(value.get("song").unwrap_or(value)))
        .collect()
}

fn parse_remote_track(value: &Value) -> Result<types::RemoteTrack, String> {
    let id = value_u64(value, &["id"]).ok_or_else(|| "推荐歌曲缺少 id".to_string())?;
    let title = value_string(value, &["name"])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "推荐歌曲缺少 name".to_string())?;
    let artists = value
        .get("ar")
        .or_else(|| value.get("artists"))
        .and_then(Value::as_array)
        .map(|artists| {
            artists
                .iter()
                .filter_map(|artist| value_string(artist, &["name"]))
                .filter(|artist| !artist.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let album = value
        .get("al")
        .or_else(|| value.get("album"))
        .and_then(|album| value_string(album, &["name"]))
        .unwrap_or_default();
    let cover_url = value
        .get("al")
        .or_else(|| value.get("album"))
        .and_then(|album| value_string(album, &["picUrl"]))
        .map(force_https);

    Ok(types::RemoteTrack {
        source: types::SourceTrackRef {
            provider_id: PROVIDER_ID.into(),
            source_id: id.to_string(),
        },
        title,
        artists,
        album,
        duration_ms: value_u64(value, &["dt"]).or_else(|| value_u64(value, &["duration"])),
        isrc: value_string(value, &["isrc"]),
        cover_url,
        playable: true,
        explicit: false,
    })
}

fn ensure_provider(provider_id: &str) -> Result<(), String> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
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

fn api_code_success(json: &Value) -> bool {
    json.get("code")
        .and_then(Value::as_i64)
        .is_none_or(|code| (200..300).contains(&code))
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
