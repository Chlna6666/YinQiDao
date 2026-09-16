use std::collections::HashSet;

use serde_json::Value;

use crate::{
    api,
    bindings::yinqidao::music_plugin::{host, types},
};

const PROVIDER_ID: &str = "netease";
const COOKIE_KEY: &str = "cookie-v1";
const API_BASE: &str = "https://music.163.com";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_COOKIE_BYTES: usize = 16 * 1024;
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

fn daily_recommendations(account_id: &str) -> Result<Vec<types::RemoteTrack>, String> {
    let json = request_json(
        account_id,
        "POST",
        "/api/v3/discovery/recommend/songs",
        None,
    )?;
    parse_track_array(json.pointer("/data/dailySongs"))
}

fn personalized_new_music(
    account_id: &str,
    limit: u16,
) -> Result<Vec<types::RemoteTrack>, String> {
    let body = format!("type=recommend&limit={limit}&areaId=0").into_bytes();
    let json = request_json(
        account_id,
        "POST",
        "/api/personalized/newsong",
        Some(body),
    )?;
    parse_track_array(json.get("result"))
}

fn personal_fm(account_id: &str) -> Result<Vec<types::RemoteTrack>, String> {
    let json = request_json(account_id, "POST", "/api/v1/radio/get", None)?;
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
    let json = request_json(account_id, "GET", &path, None)?;
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

fn request_json(
    account_id: &str,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<Value, String> {
    validate_account_id(account_id)?;
    if !path.starts_with('/') || path.contains('\r') || path.contains('\n') {
        return Err("内部网易云 API path 非法".into());
    }
    if !matches!(method, "GET" | "POST") {
        return Err("内部网易云 API method 非法".into());
    }

    let mut cookie = account_cookie(account_id)?;
    if !cookie
        .split(';')
        .any(|part| part.trim().starts_with("os="))
    {
        cookie.push_str("; os=pc");
    }

    let mut headers = vec![
        types::KeyValue {
            key: "User-Agent".into(),
            value: USER_AGENT.into(),
        },
        types::KeyValue {
            key: "Accept".into(),
            value: "application/json, text/plain, */*".into(),
        },
        types::KeyValue {
            key: "Referer".into(),
            value: "https://music.163.com/".into(),
        },
        types::KeyValue {
            key: "Origin".into(),
            value: "https://music.163.com".into(),
        },
        types::KeyValue {
            key: "Cookie".into(),
            value: cookie,
        },
    ];
    if method == "POST" {
        headers.push(types::KeyValue {
            key: "Content-Type".into(),
            value: "application/x-www-form-urlencoded; charset=UTF-8".into(),
        });
    }

    let request = host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: Some(account_id.into()),
        method: method.into(),
        url: format!("{API_BASE}{path}"),
        headers,
        body: body.unwrap_or_default(),
    };
    let response = host::http_request(&request)
        .map_err(|error| format!("网易云 Host HTTP 调用失败: {error}"))?;
    if response.body.len() > MAX_HTTP_BODY_BYTES {
        return Err(format!(
            "网易云 HTTP 响应超过插件上限 {MAX_HTTP_BODY_BYTES} bytes"
        ));
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("网易云 HTTP 状态异常: {}", response.status));
    }

    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| format!("网易云返回 JSON 解析失败: {error}"))?;
    if let Some(code) = value.get("code").and_then(Value::as_i64)
        && !(200..300).contains(&code)
    {
        let message = value
            .get("message")
            .or_else(|| value.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(format!("网易云 API 错误 code={code}: {message}"));
    }
    Ok(value)
}

fn account_cookie(account_id: &str) -> Result<String, String> {
    let scope = host::SecretScope {
        provider_id: PROVIDER_ID.into(),
        account_id: Some(account_id.into()),
    };
    let bytes = host::secret_get(&scope, COOKIE_KEY)
        .map_err(|error| format!("Host Secret 读取失败: {error}"))?
        .ok_or_else(|| "网易云账号 Cookie 不存在，请重新登录".to_string())?;
    if bytes.len() > MAX_COOKIE_BYTES {
        return Err("网易云 Cookie 超过大小限制".into());
    }
    let cookie = String::from_utf8(bytes).map_err(|_| "网易云 Cookie 不是合法 UTF-8".to_string())?;
    if cookie.contains('\r') || cookie.contains('\n') || cookie.contains('\0') {
        return Err("网易云 Cookie 包含非法控制字符".into());
    }
    Ok(cookie)
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
