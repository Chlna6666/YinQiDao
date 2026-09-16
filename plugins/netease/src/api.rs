use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bindings::yinqidao::music_plugin::{host, types};

const PROVIDER_ID: &str = "netease";
const ACCOUNT_INDEX_KEY: &str = "account-index-v1";
const COOKIE_KEY: &str = "cookie-v1";
const COOKIE_CHALLENGE_ID: &str = "cookie-import-v1";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36";
const API_BASE: &str = "https://music.163.com";
const MAX_HTTP_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_COOKIE_BYTES: usize = 16 * 1024;
const MAX_QUERY_BYTES: usize = 2 * 1024;
const MAX_PLAYLIST_NAME_BYTES: usize = 512;
const MAX_ACCOUNTS: usize = 64;
const MAX_SEARCH_LIMIT: u16 = 100;
const MAX_PAGE_LIMIT: u16 = 200;
const MAX_TRACK_IDS_PER_MUTATION: usize = 200;

pub type ApiResult<T> = Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredAccount {
    account_id: String,
    display_name: String,
    avatar_url: Option<String>,
    uid: u64,
}

pub fn manifest() -> types::PluginManifest {
    types::PluginManifest {
        id: "io.yinqidao.netease".into(),
        name: "网易云音乐".into(),
        version: "0.1.0".into(),
        abi_version: 1,
        description: "YinQiDao 网易云音乐 Provider".into(),
        homepage: Some("https://music.163.com".into()),
        providers: vec![types::ProviderDescriptor {
            id: PROVIDER_ID.into(),
            display_name: "网易云音乐".into(),
            capabilities: provider_capabilities(),
            auth_methods: vec![types::AuthMethod::CookieImport],
        }],
        network_domains: vec![
            "music.163.com".into(),
            "interface.music.163.com".into(),
            "*.music.126.net".into(),
        ],
    }
}

pub fn accounts(provider_id: &str) -> ApiResult<Vec<types::Account>> {
    ensure_provider(provider_id)?;
    let accounts = load_accounts()?;
    Ok(accounts.into_iter().map(account_to_wit).collect())
}

pub fn auth_begin(provider_id: &str, method: types::AuthMethod) -> ApiResult<types::AuthChallenge> {
    ensure_provider(provider_id)?;
    if method != types::AuthMethod::CookieImport {
        return Err("网易云插件当前仅支持 Cookie 导入登录".into());
    }
    Ok(types::AuthChallenge {
        challenge_id: COOKIE_CHALLENGE_ID.into(),
        kind: types::AuthChallengeKind::Form,
        verification_uri: Some("https://music.163.com".into()),
        user_code: None,
        qr_payload: None,
        fields: vec![types::KeyValue {
            key: "cookie".into(),
            value: "网易云音乐 Web Cookie".into(),
        }],
        expires_at_ms: None,
    })
}

pub fn auth_poll(provider_id: &str, challenge_id: &str) -> ApiResult<types::AuthPoll> {
    ensure_provider(provider_id)?;
    ensure_cookie_challenge(challenge_id)?;
    Ok(types::AuthPoll::Pending)
}

pub fn auth_submit(
    provider_id: &str,
    challenge_id: &str,
    values: Vec<types::KeyValue>,
) -> ApiResult<types::AuthPoll> {
    ensure_provider(provider_id)?;
    ensure_cookie_challenge(challenge_id)?;
    let cookie = values
        .into_iter()
        .find(|entry| entry.key == "cookie")
        .map(|entry| entry.value)
        .ok_or_else(|| "Cookie 登录表单缺少 cookie 字段".to_string())?;
    let cookie = normalize_cookie(&cookie)?;

    let account_json = request_json_with_cookie(
        None,
        "GET",
        "/api/nuser/account/get",
        None,
        Some(&cookie),
    )?;
    let profile = account_json
        .get("profile")
        .filter(|profile| !profile.is_null())
        .ok_or_else(|| "网易云 Cookie 无效或登录态已过期".to_string())?;
    let uid = value_u64(profile, &["userId"])
        .ok_or_else(|| "网易云账号信息缺少 userId".to_string())?;
    let display_name = value_string(profile, &["nickname"])
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("网易云用户 {uid}"));
    let avatar_url = value_string(profile, &["avatarUrl"]);
    let account_id = uid.to_string();

    secret_set(
        &host::SecretScope {
            provider_id: PROVIDER_ID.into(),
            account_id: Some(account_id.clone()),
        },
        COOKIE_KEY,
        cookie.as_bytes(),
    )?;

    let mut accounts = load_accounts()?;
    let stored = StoredAccount {
        account_id: account_id.clone(),
        display_name,
        avatar_url,
        uid,
    };
    if let Some(existing) = accounts
        .iter_mut()
        .find(|account| account.account_id == account_id)
    {
        *existing = stored.clone();
    } else {
        if accounts.len() >= MAX_ACCOUNTS {
            let _ = secret_delete(
                &host::SecretScope {
                    provider_id: PROVIDER_ID.into(),
                    account_id: Some(account_id),
                },
                COOKIE_KEY,
            );
            return Err(format!("网易云账号数量超过 Host 插件上限 {MAX_ACCOUNTS}"));
        }
        accounts.push(stored.clone());
    }
    accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    save_accounts(&accounts)?;

    Ok(types::AuthPoll::Authenticated(account_to_wit(stored)))
}

pub fn auth_cancel(provider_id: &str, challenge_id: &str) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    ensure_cookie_challenge(challenge_id)?;
    Ok(true)
}

pub fn logout(provider_id: &str, account_id: &str) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let scope = host::SecretScope {
        provider_id: PROVIDER_ID.into(),
        account_id: Some(account_id.into()),
    };
    let removed_secret = secret_delete(&scope, COOKIE_KEY)?;
    let mut accounts = load_accounts()?;
    let old_len = accounts.len();
    accounts.retain(|account| account.account_id != account_id);
    let removed_index = accounts.len() != old_len;
    if removed_index {
        save_accounts(&accounts)?;
    }
    Ok(removed_secret || removed_index)
}

pub fn search(
    provider_id: &str,
    account_id: Option<&str>,
    query: &str,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    ensure_provider(provider_id)?;
    let query = query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err("搜索关键字为空或超过大小限制".into());
    }
    if limit == 0 || limit > MAX_SEARCH_LIMIT {
        return Err(format!("搜索 limit 必须位于 1..={MAX_SEARCH_LIMIT}"));
    }
    let body = form_encode(&[
        ("s", query.to_owned()),
        ("type", "1".into()),
        ("offset", "0".into()),
        ("limit", limit.to_string()),
    ]);
    let json = request_json(account_id, "POST", "/api/search/get/web", Some(body))?;
    let songs = json
        .pointer("/result/songs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    songs
        .iter()
        .take(usize::from(limit))
        .map(parse_remote_track)
        .collect()
}

pub fn resolve_track(
    provider_id: &str,
    account_id: Option<&str>,
    query: &types::TrackQuery,
) -> ApiResult<Option<types::RemoteTrack>> {
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
        if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
            best = Some((score, track));
        }
    }
    Ok(best.filter(|(score, _)| *score >= 4).map(|(_, track)| track))
}

pub fn lyrics(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> ApiResult<Option<types::LyricDocument>> {
    ensure_source(provider_id, track)?;
    let id = numeric_source_id(&track.source_id)?;
    let path = format!("/api/song/lyric?id={id}&lv=-1&kv=-1&tv=-1");
    let json = request_json(account_id, "GET", &path, None)?;
    let original = json.pointer("/lrc/lyric").and_then(Value::as_str);
    let translated = json.pointer("/tlyric/lyric").and_then(Value::as_str);
    let Some(original) = original else {
        return Ok(None);
    };
    let lines = merge_lrc(original, translated.unwrap_or(""));
    Ok(Some(types::LyricDocument {
        source: "netease".into(),
        plain: if original.trim().is_empty() {
            None
        } else {
            Some(original.to_owned())
        },
        lines,
    }))
}

pub fn artwork(
    provider_id: &str,
    account_id: Option<&str>,
    track: &types::SourceTrackRef,
) -> ApiResult<Option<types::ArtworkDescriptor>> {
    ensure_source(provider_id, track)?;
    let detail = song_detail(account_id, &[numeric_source_id(&track.source_id)?])?;
    let Some(song) = detail.first() else {
        return Ok(None);
    };
    let url = value_string(song, &["al", "picUrl"])
        .or_else(|| value_string(song, &["album", "picUrl"]));
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
) -> ApiResult<types::StreamDescriptor> {
    ensure_source(provider_id, &request.track)?;
    validate_account_id(account_id)?;
    let id = numeric_source_id(&request.track.source_id)?;
    let bitrate = quality_bitrate(request.quality.as_deref());
    let path = format!("/api/song/enhance/player/url?ids=%5B{id}%5D&br={bitrate}");
    let json = request_json(Some(account_id), "GET", &path, None)?;
    let item = json
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| "网易云未返回播放地址".to_string())?;
    let url = value_string(item, &["url"])
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "网易云当前账号/版权状态下无可用播放地址".to_string())?;
    let actual_bitrate = value_u64(item, &["br"])
        .and_then(|value| u32::try_from(value).ok())
        .or(Some(bitrate));
    let codec = value_string(item, &["type"])
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty());

    Ok(types::StreamDescriptor {
        url: force_https(url),
        headers: vec![types::KeyValue {
            key: "Referer".into(),
            value: "https://music.163.com/".into(),
        }],
        codec,
        bitrate: actual_bitrate,
        sample_rate: None,
        channels: None,
        expires_at_ms: None,
    })
}

pub fn playlists(provider_id: &str, account_id: &str) -> ApiResult<Vec<types::Playlist>> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let path = format!("/api/user/playlist/?uid={}&limit=1000&offset=0", account.uid);
    let json = request_json(Some(account_id), "GET", &path, None)?;
    let values = json
        .get("playlist")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(values
        .iter()
        .filter_map(|value| parse_playlist(value, account.uid))
        .collect())
}

pub fn playlist_tracks(
    provider_id: &str,
    account_id: &str,
    playlist_id: &str,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if limit == 0 || limit > MAX_PAGE_LIMIT {
        return Err(format!("playlist limit 必须位于 1..={MAX_PAGE_LIMIT}"));
    }
    let playlist_id = numeric_source_id(playlist_id)?;
    let path = format!("/api/v6/playlist/detail?id={playlist_id}&n=100000&s=8");
    let json = request_json(Some(account_id), "GET", &path, None)?;
    let track_ids = json
        .pointer("/playlist/trackIds")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value_u64(value, &["id"]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(track_ids.len());
    let end = start.saturating_add(usize::from(limit)).min(track_ids.len());
    if start == end {
        return Ok(Vec::new());
    }
    song_detail(Some(account_id), &track_ids[start..end])?
        .iter()
        .map(parse_remote_track)
        .collect()
}

pub fn playlist_create(
    provider_id: &str,
    account_id: &str,
    name: &str,
) -> ApiResult<types::Playlist> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_PLAYLIST_NAME_BYTES || name.contains('\0') {
        return Err("歌单名称为空或超过大小限制".into());
    }
    let body = form_encode(&[("name", name.to_owned()), ("privacy", "0".into())]);
    let json = request_json(Some(account_id), "POST", "/api/playlist/create", Some(body))?;
    let playlist = json
        .get("playlist")
        .ok_or_else(|| "网易云创建歌单成功响应缺少 playlist".to_string())?;
    parse_playlist(playlist, account.uid).ok_or_else(|| "网易云返回的歌单信息非法".into())
}

pub fn playlist_mutate(
    provider_id: &str,
    account_id: &str,
    playlist_id: &str,
    tracks: &[types::SourceTrackRef],
    add: bool,
) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let playlist_id = numeric_source_id(playlist_id)?;
    if tracks.is_empty() || tracks.len() > MAX_TRACK_IDS_PER_MUTATION {
        return Err(format!(
            "单次歌单变更曲目数必须位于 1..={MAX_TRACK_IDS_PER_MUTATION}"
        ));
    }
    let mut ids = Vec::with_capacity(tracks.len());
    for track in tracks {
        ensure_source(provider_id, track)?;
        ids.push(numeric_source_id(&track.source_id)?);
    }
    let ids_json = serde_json::to_string(&ids).map_err(|error| error.to_string())?;
    let body = form_encode(&[
        ("op", if add { "add" } else { "del" }.into()),
        ("pid", playlist_id.to_string()),
        ("trackIds", ids_json.clone()),
        ("tracks", ids_json),
    ]);
    let json = request_json(
        Some(account_id),
        "POST",
        "/api/playlist/manipulate/tracks",
        Some(body),
    )?;
    Ok(api_code_success(&json))
}

pub fn liked_tracks(
    provider_id: &str,
    account_id: &str,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    let playlist_id = liked_playlist_id(provider_id, account_id)?
        .ok_or_else(|| "未找到“我喜欢的音乐”歌单".to_string())?;
    playlist_tracks(provider_id, account_id, &playlist_id.to_string(), offset, limit)
}

pub fn cloud_library(
    provider_id: &str,
    account_id: &str,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if limit == 0 || limit > MAX_PAGE_LIMIT {
        return Err(format!("cloud library limit 必须位于 1..={MAX_PAGE_LIMIT}"));
    }
    let body = form_encode(&[
        ("offset", offset.to_string()),
        ("limit", limit.to_string()),
    ]);
    let json = request_json(Some(account_id), "POST", "/api/v1/cloud/get", Some(body))?;
    let values = json
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    values
        .iter()
        .take(usize::from(limit))
        .map(|value| value.get("simpleSong").unwrap_or(value))
        .map(parse_remote_track)
        .collect()
}

pub fn set_liked(
    provider_id: &str,
    account_id: &str,
    track: &types::SourceTrackRef,
    liked: bool,
) -> ApiResult<bool> {
    ensure_source(provider_id, track)?;
    validate_account_id(account_id)?;
    let id = numeric_source_id(&track.source_id)?;
    let account = require_stored_account(account_id)?;
    let body = form_encode(&[
        ("trackId", id.to_string()),
        ("userid", account.uid.to_string()),
        ("like", liked.to_string()),
        ("time", "3".into()),
    ]);
    let json = request_json(Some(account_id), "POST", "/api/song/like", Some(body))?;
    Ok(api_code_success(&json))
}

pub fn unsupported(operation: &str) -> String {
    format!("网易云插件未声明或未实现 {operation} capability")
}

fn provider_capabilities() -> Vec<types::Capability> {
    vec![
        types::Capability::Authentication,
        types::Capability::Search,
        types::Capability::Metadata,
        types::Capability::Lyrics,
        types::Capability::Artwork,
        types::Capability::Streaming,
        types::Capability::Playlists,
        types::Capability::CloudLibrary,
        types::Capability::LikeSync,
    ]
}

fn account_to_wit(account: StoredAccount) -> types::Account {
    types::Account {
        account_id: account.account_id,
        provider_id: PROVIDER_ID.into(),
        display_name: account.display_name,
        avatar_url: account.avatar_url,
        capabilities: provider_capabilities(),
    }
}

fn ensure_provider(provider_id: &str) -> ApiResult<()> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
}

fn ensure_source(provider_id: &str, source: &types::SourceTrackRef) -> ApiResult<()> {
    ensure_provider(provider_id)?;
    if source.provider_id != PROVIDER_ID {
        return Err("source provider 与网易云 route 不匹配".into());
    }
    Ok(())
}

fn ensure_cookie_challenge(challenge_id: &str) -> ApiResult<()> {
    if challenge_id == COOKIE_CHALLENGE_ID {
        Ok(())
    } else {
        Err("未知或已过期的 Cookie 登录 challenge".into())
    }
}

fn validate_account_id(account_id: &str) -> ApiResult<()> {
    if account_id.is_empty()
        || account_id.len() > 32
        || !account_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("网易云 account id 非法".into());
    }
    Ok(())
}

fn numeric_source_id(source_id: &str) -> ApiResult<u64> {
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

fn normalize_cookie(raw: &str) -> ApiResult<String> {
    let cookie = raw.trim();
    if cookie.is_empty() || cookie.len() > MAX_COOKIE_BYTES {
        return Err("Cookie 为空或超过大小限制".into());
    }
    if cookie.contains('\r') || cookie.contains('\n') || cookie.contains('\0') {
        return Err("Cookie 包含非法控制字符".into());
    }
    Ok(cookie.to_owned())
}

fn provider_scope() -> host::SecretScope {
    host::SecretScope {
        provider_id: PROVIDER_ID.into(),
        account_id: None,
    }
}

fn secret_get(scope: &host::SecretScope, key: &str) -> ApiResult<Option<Vec<u8>>> {
    host::secret_get(scope, key).map_err(|error| format!("Host Secret 读取失败: {error}"))
}

fn secret_set(scope: &host::SecretScope, key: &str, value: &[u8]) -> ApiResult<bool> {
    host::secret_set(scope, key, value).map_err(|error| format!("Host Secret 写入失败: {error}"))
}

fn secret_delete(scope: &host::SecretScope, key: &str) -> ApiResult<bool> {
    host::secret_delete(scope, key).map_err(|error| format!("Host Secret 删除失败: {error}"))
}

fn load_accounts() -> ApiResult<Vec<StoredAccount>> {
    let Some(bytes) = secret_get(&provider_scope(), ACCOUNT_INDEX_KEY)? else {
        return Ok(Vec::new());
    };
    if bytes.len() > 256 * 1024 {
        return Err("网易云账号索引超过大小限制".into());
    }
    let mut accounts: Vec<StoredAccount> =
        serde_json::from_slice(&bytes).map_err(|error| format!("解析网易云账号索引失败: {error}"))?;
    accounts.retain(|account| validate_account_id(&account.account_id).is_ok());
    accounts.truncate(MAX_ACCOUNTS);
    Ok(accounts)
}

fn save_accounts(accounts: &[StoredAccount]) -> ApiResult<()> {
    let bytes = serde_json::to_vec(accounts)
        .map_err(|error| format!("序列化网易云账号索引失败: {error}"))?;
    if bytes.len() > 256 * 1024 {
        return Err("网易云账号索引超过大小限制".into());
    }
    secret_set(&provider_scope(), ACCOUNT_INDEX_KEY, &bytes)?;
    Ok(())
}

fn require_stored_account(account_id: &str) -> ApiResult<StoredAccount> {
    validate_account_id(account_id)?;
    load_accounts()?
        .into_iter()
        .find(|account| account.account_id == account_id)
        .ok_or_else(|| "网易云账号未登录或 Host session 尚未恢复".into())
}

fn account_cookie(account_id: &str) -> ApiResult<String> {
    validate_account_id(account_id)?;
    let bytes = secret_get(
        &host::SecretScope {
            provider_id: PROVIDER_ID.into(),
            account_id: Some(account_id.into()),
        },
        COOKIE_KEY,
    )?
    .ok_or_else(|| "网易云账号 Cookie 不存在，请重新登录".to_string())?;
    let cookie = String::from_utf8(bytes).map_err(|_| "网易云 Cookie 不是合法 UTF-8".to_string())?;
    normalize_cookie(&cookie)
}

fn request_json(
    account_id: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
) -> ApiResult<Value> {
    let cookie = account_id.map(account_cookie).transpose()?;
    request_json_with_cookie(account_id, method, path, body, cookie.as_deref())
}

fn request_json_with_cookie(
    account_id: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    cookie: Option<&str>,
) -> ApiResult<Value> {
    if !path.starts_with('/') || path.contains("\r") || path.contains("\n") {
        return Err("内部网易云 API path 非法".into());
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
    ];
    if method == "POST" {
        headers.push(types::KeyValue {
            key: "Content-Type".into(),
            value: "application/x-www-form-urlencoded; charset=UTF-8".into(),
        });
    }
    if let Some(cookie) = cookie {
        let cookie = if cookie.split(';').any(|part| part.trim().starts_with("os=")) {
            cookie.to_owned()
        } else {
            format!("{cookie}; os=pc")
        };
        headers.push(types::KeyValue {
            key: "Cookie".into(),
            value: cookie,
        });
    }

    let request = host::HttpRequestData {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.map(str::to_owned),
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

fn parse_remote_track(value: &Value) -> ApiResult<types::RemoteTrack> {
    let id = value_u64(value, &["id"]).ok_or_else(|| "歌曲缺少 id".to_string())?;
    let title = value_string(value, &["name"])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "歌曲缺少 name".to_string())?;
    let artists_value = value
        .get("ar")
        .or_else(|| value.get("artists"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let artists = artists_value
        .iter()
        .filter_map(|artist| value_string(artist, &["name"]))
        .filter(|artist| !artist.trim().is_empty())
        .collect::<Vec<_>>();
    let album = value
        .get("al")
        .or_else(|| value.get("album"))
        .and_then(|album| value_string(album, &["name"]))
        .unwrap_or_default();
    let duration_ms = value_u64(value, &["dt"]).or_else(|| value_u64(value, &["duration"]));
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
        duration_ms,
        isrc: value_string(value, &["isrc"]),
        cover_url,
        playable: true,
        explicit: false,
    })
}

fn song_detail(account_id: Option<&str>, ids: &[u64]) -> ApiResult<Vec<Value>> {
    if ids.is_empty() || ids.len() > MAX_PAGE_LIMIT as usize {
        return Err("歌曲详情批量 id 数量非法".into());
    }
    let ids = serde_json::to_string(ids).map_err(|error| error.to_string())?;
    let path = format!("/api/song/detail?ids={}", percent_encode(&ids));
    let json = request_json(account_id, "GET", &path, None)?;
    Ok(json
        .get("songs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

fn parse_playlist(value: &Value, uid: u64) -> Option<types::Playlist> {
    let source_id = value_u64(value, &["id"])?;
    let name = value_string(value, &["name"])?;
    let creator_uid = value
        .get("creator")
        .and_then(|creator| value_u64(creator, &["userId"]));
    Some(types::Playlist {
        provider_id: PROVIDER_ID.into(),
        source_id: source_id.to_string(),
        name,
        cover_url: value_string(value, &["coverImgUrl"]).map(force_https),
        track_count: value_u64(value, &["trackCount"]).and_then(|count| u32::try_from(count).ok()),
        editable: creator_uid == Some(uid),
    })
}

fn liked_playlist_id(provider_id: &str, account_id: &str) -> ApiResult<Option<u64>> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let path = format!("/api/user/playlist/?uid={}&limit=1000&offset=0", account.uid);
    let json = request_json(Some(account_id), "GET", &path, None)?;
    Ok(json
        .get("playlist")
        .and_then(Value::as_array)
        .and_then(|playlists| {
            playlists.iter().find_map(|playlist| {
                let special = value_u64(playlist, &["specialType"]);
                let creator = playlist
                    .get("creator")
                    .and_then(|creator| value_u64(creator, &["userId"]));
                if special == Some(5) && creator == Some(account.uid) {
                    value_u64(playlist, &["id"])
                } else {
                    None
                }
            })
        }))
}

fn api_code_success(json: &Value) -> bool {
    json.get("code")
        .and_then(Value::as_i64)
        .is_some_and(|code| (200..300).contains(&code))
}

fn quality_bitrate(quality: Option<&str>) -> u32 {
    match quality.unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "low" | "standard" | "128k" => 128_000,
        "higher" | "192k" => 192_000,
        "high" | "exhigh" | "320k" => 320_000,
        "lossless" | "hires" | "jyeffect" | "sky" | "dolby" => 999_000,
        _ => 320_000,
    }
}

fn normalize_identity_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
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

fn form_encode(fields: &[(&str, String)]) -> Vec<u8> {
    let mut body = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            body.push('&');
        }
        body.push_str(&percent_encode(key));
        body.push('=');
        body.push_str(&percent_encode(value));
    }
    body.into_bytes()
}

fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn merge_lrc(original: &str, translated: &str) -> Vec<types::LyricLine> {
    let mut lines = BTreeMap::<u64, String>::new();
    for (timestamp, text) in parse_lrc(original) {
        lines.entry(timestamp).or_insert(text);
    }
    let translations = parse_lrc(translated).into_iter().collect::<BTreeMap<_, _>>();
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
    fn percent_encoding_is_url_safe() {
        assert_eq!(percent_encode("A B&中文"), "A%20B%26%E4%B8%AD%E6%96%87");
    }

    #[test]
    fn lrc_parser_supports_multiple_timestamps_and_fraction_widths() {
        let lines = parse_lrc("[00:01.2][00:02.34]hello\n[01:03.456]world");
        assert_eq!(
            lines,
            vec![
                (1_200, "hello".into()),
                (2_340, "hello".into()),
                (63_456, "world".into())
            ]
        );
    }

    #[test]
    fn identity_text_ignores_spacing_case_and_ascii_punctuation() {
        assert_eq!(normalize_identity_text("Hello, World!"), "helloworld");
    }

    #[test]
    fn source_id_rejects_non_numeric_values() {
        assert!(numeric_source_id("123456").is_ok());
        assert!(numeric_source_id("../123").is_err());
        assert!(numeric_source_id("").is_err());
    }
}
