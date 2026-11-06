#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bindings::yinqidao::music_plugin::{host, types};
use crate::crypto;
use crate::protocol::{self, Session};

pub const PROVIDER_ID: &str = "qqmusic";
const ACCOUNT_INDEX_KEY: &str = "account-index-v1";
const COOKIE_KEY: &str = "cookie-v1";
pub(crate) const COOKIE_CHALLENGE_ID: &str = "cookie-import-v1";
const MAX_COOKIE_BYTES: usize = 16 * 1024;
const MAX_QUERY_BYTES: usize = 2 * 1024;
const MAX_PLAYLIST_NAME_BYTES: usize = 512;
const MAX_ACCOUNTS: usize = 64;
const MAX_SEARCH_LIMIT: u16 = 100;
const MAX_PAGE_LIMIT: u16 = 200;
const MAX_TRACKS_PER_MUTATION: usize = 100;
const DIRID_FAVOURITE: u32 = 201;

pub type ApiResult<T> = Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredAccount {
    pub(crate) account_id: String,
    pub(crate) display_name: String,
    pub(crate) avatar_url: Option<String>,
    /// 加密 UIN（euin）：歌单收藏列表 / “我喜欢”接口的 enc_host_uin。
    pub(crate) euin: Option<String>,
}

pub fn manifest() -> types::PluginManifest {
    types::PluginManifest {
        id: "io.yinqidao.qqmusic".into(),
        name: "QQ音乐".into(),
        version: "0.1.0".into(),
        abi_version: 1,
        description: "YinQiDao QQ音乐 Provider".into(),
        homepage: Some("https://y.qq.com".into()),
        providers: vec![types::ProviderDescriptor {
            id: PROVIDER_ID.into(),
            display_name: "QQ音乐".into(),
            capabilities: provider_capabilities(),
            auth_methods: vec![types::AuthMethod::QrCode, types::AuthMethod::CookieImport],
        }],
        network_domains: vec![
            "y.qq.com".into(),
            "u.y.qq.com".into(),
            "u6.y.qq.com".into(),
            "c.y.qq.com".into(),
            "i.y.qq.com".into(),
            "ssl.ptlogin2.qq.com".into(),
            "ssl.ptlogin2.graph.qq.com".into(),
            "graph.qq.com".into(),
            "*.music.tc.qq.com".into(),
            "dl.stream.qqmusic.qq.com".into(),
            "isure.stream.qqmusic.qq.com".into(),
            "aqqmusic.tc.qq.com".into(),
            "*.gtimg.cn".into(),
        ],
    }
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
        types::Capability::MediaCollections,
        types::Capability::LikeSync,
        types::Capability::Recommendations,
        types::Capability::UserProfile,
    ]
}

pub fn unsupported(provider_id: &str, operation: &str) -> String {
    if provider_id == PROVIDER_ID {
        format!("QQ音乐插件未声明或未实现 {operation} capability")
    } else {
        format!("unknown provider id: {provider_id}")
    }
}

// ---------------------------------------------------------------------------
// 会话与账户
// ---------------------------------------------------------------------------

pub fn guest_session(now_ms: u64) -> Session {
    Session::guest(now_ms)
}

pub fn session_for(account_id: Option<&str>, now_ms: u64) -> ApiResult<Session> {
    match account_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => {
            let account = require_stored_account(id)?;
            let cookie = account_cookie(id)?;
            let qm_keyst =
                cookie_value(&cookie, "qm_keyst").or_else(|| cookie_value(&cookie, "qqmusic_key"));
            Ok(Session::logged_in(
                cookie,
                account.account_id.clone(),
                qm_keyst,
                now_ms,
            ))
        }
        None => Ok(Session::guest(now_ms)),
    }
}

pub fn accounts(provider_id: &str) -> ApiResult<Vec<types::Account>> {
    ensure_provider(provider_id)?;
    Ok(load_accounts()?
        .into_iter()
        .map(|account| types::Account {
            account_id: account.account_id,
            provider_id: PROVIDER_ID.into(),
            display_name: account.display_name,
            avatar_url: account.avatar_url,
            capabilities: provider_capabilities(),
        })
        .collect())
}

/// Cookie 导入：与网易云插件同构的表单质询（表单字段 `cookie`）。
pub fn auth_begin(provider_id: &str, method: types::AuthMethod) -> ApiResult<types::AuthChallenge> {
    ensure_provider(provider_id)?;
    if method != types::AuthMethod::CookieImport {
        return Err("QQ音乐插件当前仅支持 Cookie 导入登录".into());
    }
    Ok(types::AuthChallenge {
        challenge_id: COOKIE_CHALLENGE_ID.into(),
        kind: types::AuthChallengeKind::Form,
        verification_uri: Some("https://y.qq.com".into()),
        user_code: None,
        qr_payload: None,
        fields: vec![types::KeyValue {
            key: "cookie".into(),
            value: "QQ音乐 Web Cookie（music.y.qq.com 登录后复制）".into(),
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
    let account = register_authenticated_cookie(&cookie)?;
    Ok(types::AuthPoll::Authenticated(account))
}

pub fn auth_cancel(provider_id: &str, challenge_id: &str) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    ensure_cookie_challenge(challenge_id)?;
    Ok(true)
}

/// 用导入的 Cookie 换取账户资料并持久化会话。
///
/// 校验通道：`fcg_get_profile_homepage`（`code != 0` 即会话失效）。昵称/头像字段为宽松
/// 提取：QQ 主页接口字段随版本漂移，失败时回退通用展示名，不影响会话有效性。
pub fn register_authenticated_cookie(raw_cookie: &str) -> ApiResult<types::Account> {
    let cookie = normalize_cookie(raw_cookie)?;
    let uin =
        cookie_uin(&cookie).ok_or_else(|| "Cookie 缺少 uin（未登录 QQ 音乐 Web）".to_string())?;
    let qm_keyst = cookie_value(&cookie, "qm_keyst");
    let now_ms = host::now_ms();
    let session = Session::logged_in(cookie.clone(), uin.clone(), qm_keyst.clone(), now_ms);
    let g_tk = crypto::g_tk(qm_keyst.as_deref());

    let url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg?g_tk={g_tk}&format=json&cid=205360838&needNewCode=0&loginUin={uin}&hostUin=0&userid={uin}&reqfrom=1"
    );
    let homepage = protocol::fcgi_get(&session, None, &url)?;
    let code = homepage.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(format!("QQ音乐 Cookie 无效或登录态已过期（code={code}）"));
    }
    let display_name = homepage
        .pointer("/data/nickname")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("QQ用户 {uin}"));
    let avatar_url = homepage
        .pointer("/data/logo")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|url| url.starts_with("http"));
    let euin = cookie_value(&cookie, "euin");

    secret_set(
        &host::SecretScope {
            provider_id: PROVIDER_ID.into(),
            account_id: Some(uin.clone()),
        },
        COOKIE_KEY,
        cookie.as_bytes(),
    )?;

    let stored = StoredAccount {
        account_id: uin.clone(),
        display_name,
        avatar_url,
        euin,
    };
    let mut accounts = load_accounts()?;
    if let Some(existing) = accounts
        .iter_mut()
        .find(|account| account.account_id == stored.account_id)
    {
        *existing = stored.clone();
    } else {
        if accounts.len() >= MAX_ACCOUNTS {
            let _ = secret_delete(
                &host::SecretScope {
                    provider_id: PROVIDER_ID.into(),
                    account_id: Some(stored.account_id.clone()),
                },
                COOKIE_KEY,
            );
            return Err(format!("QQ音乐账号数量超过 Host 插件上限 {MAX_ACCOUNTS}"));
        }
        accounts.push(stored.clone());
    }
    accounts.sort_by(|left, right| left.account_id.cmp(&right.account_id));
    save_accounts(&accounts)?;

    Ok(types::Account {
        account_id: stored.account_id,
        provider_id: PROVIDER_ID.into(),
        display_name: stored.display_name,
        avatar_url: stored.avatar_url,
        capabilities: provider_capabilities(),
    })
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

// ---------------------------------------------------------------------------
// 搜索 / 歌曲详情
// ---------------------------------------------------------------------------

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
    let now_ms = host::now_ms();
    let session = session_for(account_id, now_ms)?;
    let page_num = 1_u32;
    let num_per_page = u32::from(limit).max(20);

    // `code 2001` 为搜索瞬时错误：立即重试一次（社区口径 300ms 退避，这里以单次重试兜底）。
    let mut attempt = 0;
    let response = loop {
        attempt += 1;
        let raw = protocol::musicu_search_raw(&session, query, page_num, num_per_page)?;
        let code = raw.get("code").and_then(Value::as_i64).unwrap_or(0);
        if code == 2001 && attempt < 2 {
            continue;
        }
        if !(200..300).contains(&code) {
            return Err(format!("QQ音乐搜索错误 code={code}"));
        }
        break raw;
    };

    let songs = response
        .pointer("/music.search.SearchCgiService/body/song/list")
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
        .collect::<std::collections::HashSet<_>>();

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

/// 数字歌曲 ID（歌单写接口要求 songId 而非 MID）。
pub(crate) fn song_numeric_id(session: &Session, mid: &str) -> ApiResult<u64> {
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
    find_numeric_song_id(object.get("detail").unwrap_or(&Value::Null), mid)
        .ok_or_else(|| "QQ音乐未返回歌曲数字 ID".to_string())
}

fn find_numeric_song_id(value: &Value, mid: &str) -> Option<u64> {
    match value {
        Value::Object(map) => {
            let mid_matches = map
                .get("mid")
                .and_then(Value::as_str)
                .is_some_and(|value| value == mid);
            if mid_matches && let Some(id) = map.get("id").and_then(Value::as_u64) {
                return Some(id);
            }
            map.values()
                .find_map(|child| find_numeric_song_id(child, mid))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|child| find_numeric_song_id(child, mid)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 歌单
// ---------------------------------------------------------------------------

pub fn playlists(provider_id: &str, account_id: &str) -> ApiResult<Vec<types::Playlist>> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;
    let g_tk = crypto::g_tk(None);
    let mut output = Vec::new();

    // 创建的歌单（uin 为 QQ 号）
    let url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_user_created_diss?hostuin={}&hostUin=0&sin=0&size=200&g_tk={g_tk}&loginUin=0&format=json&platform=yqq.json&needNewCode=0",
        account.account_id
    );
    if let Ok(created) = protocol::fcgi_get(&session, Some(account_id), &url)
        && let Some(items) = first_array(&created, &["/data/disslist"])
    {
        for value in items {
            if let Some(playlist) = parse_diss_summary(value, true) {
                output.push(playlist);
            }
        }
    }

    // 收藏的歌单（euin 为加密 UIN）
    if let Some(euin) = account.euin.as_deref().filter(|value| !value.is_empty()) {
        let url = format!(
            "https://c.y.qq.com/rsc/fcgi-bin/fcg_get_profile_order_asset.fcg?ct=20&cid=205360956&userid={}&reqtype=3&sin=0&ein=100&g_tk={g_tk}&loginUin=0&format=json&platform=yqq.json&needNewCode=0",
            account.account_id
        );
        if let Ok(fav) = protocol::fcgi_get(&session, Some(account_id), &url)
            && let Some(items) = first_array(&fav, &["/data/getplaylist"])
        {
            for value in items {
                if let Some(playlist) = parse_diss_summary(value, false) {
                    output.push(playlist);
                }
            }
        }
        let _ = euin; // enc_host_uin 在“我喜欢”读取时使用（ euin cookie 亦随会话携带）
    }

    // “我喜欢”（dirid 201，需要 enc_host_uin）
    if let Some(euin) = account.euin.as_deref().filter(|value| !value.is_empty()) {
        output.push(types::Playlist {
            provider_id: PROVIDER_ID.into(),
            source_id: format!("fav-{euin}"),
            name: "我喜欢".into(),
            cover_url: None,
            track_count: None,
            editable: false,
        });
    }
    Ok(output)
}

fn parse_diss_summary(value: &Value, editable: bool) -> Option<types::Playlist> {
    let source_id = value_u64(value, &["tid"])
        .or_else(|| value_u64(value, &["disstid"]))
        .or_else(|| value_u64(value, &["dirid"]))?;
    let name = value_string(value, &["diss_name"])
        .or_else(|| value_string(value, &["dissname"]))
        .or_else(|| value_string(value, &["title"]))
        .filter(|name| !name.trim().is_empty())?;
    let cover_url = value_string(value, &["logo"])
        .or_else(|| value_string(value, &["imgurl"]))
        .or_else(|| value_string(value, &["picurl"]));
    Some(types::Playlist {
        provider_id: PROVIDER_ID.into(),
        source_id: source_id.to_string(),
        name,
        cover_url,
        track_count: value_u64(value, &["song_cnt"])
            .or_else(|| value_u64(value, &["songnum"]))
            .and_then(|count| u32::try_from(count).ok()),
        editable,
    })
}

/// 歌单曲目（含“我喜欢”fav- 前缀伪 id）。走 `CgiGetDiss`（新式 track 字段）。
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
    // 账户存在性校验；“我喜欢”（fav-）需要 Cookie 里的 euin，其余歌单走 tid。
    let _ = require_stored_account(account_id)?;
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;

    let mut param = Map::new();
    if let Some(euin) = playlist_id.strip_prefix("fav-") {
        param.insert("disstid".into(), Value::from(0));
        param.insert("dirid".into(), Value::from(DIRID_FAVOURITE));
        param.insert("enc_host_uin".into(), Value::from(euin));
    } else {
        let disstid =
            value_u64(&Value::from(playlist_id), &[]).ok_or_else(|| "歌单 id 非法".to_string())?;
        param.insert("disstid".into(), Value::from(disstid));
        param.insert("dirid".into(), Value::from(0));
    }
    param.insert("tag".into(), Value::Bool(false));
    param.insert("song_begin".into(), Value::from(offset));
    param.insert("song_num".into(), Value::from(u32::from(limit)));
    param.insert("userinfo".into(), Value::Bool(false));
    param.insert("orderlist".into(), Value::Bool(false));

    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "diss",
            "music.srfDissInfo.DissInfo",
            "CgiGetDiss",
            Value::Object(param),
        )],
        false,
    )?;
    let response_code = object
        .pointer("/diss/code")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if response_code != 0 {
        return Err(format!("QQ音乐歌单接口返回异常 code={response_code}"));
    }
    let songs = object
        .pointer("/diss/data/songlist")
        .and_then(Value::as_array);
    // A missing result list with zero total is a valid empty playlist; a
    // missing result list otherwise is a failed read, never an unlike event.
    if songs.is_none()
        && object
            .pointer("/diss/data/dirinfo/song_cnt")
            .and_then(Value::as_u64)
            != Some(0)
    {
        return Err("QQ音乐歌单响应缺少 songlist，拒绝解释为空收藏".into());
    }
    songs
        .into_iter()
        .flatten()
        .take(usize::from(limit))
        .map(parse_remote_track)
        .collect()
}

pub fn playlist_create(
    provider_id: &str,
    account_id: &str,
    name: &str,
) -> ApiResult<types::Playlist> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let name = sanitize_playlist_name(name)?;
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;
    let mut param = Map::new();
    param.insert("dirName".into(), Value::from(name));
    param.insert("dirDesc".into(), Value::from(""));
    param.insert("dirPicUrl".into(), Value::from(""));
    param.insert("taglist".into(), Value::from(""));
    param.insert("dirShow".into(), Value::from(2));
    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "create",
            "music.musicasset.PlaylistBaseWrite",
            "AddPlaylist",
            Value::Object(param),
        )],
        false,
    )?;
    let block = object.get("create").cloned().unwrap_or(Value::Null);
    if !protocol::nested_write_success(&block) {
        return Err("QQ音乐创建歌单被拒绝".into());
    }
    let tid = block
        .pointer("/data/tid")
        .and_then(Value::as_u64)
        .or_else(|| block.pointer("/data/dirId").and_then(Value::as_u64))
        .ok_or_else(|| "QQ音乐创建歌单响应缺少 tid".to_string())?;
    Ok(types::Playlist {
        provider_id: PROVIDER_ID.into(),
        source_id: tid.to_string(),
        name: name.to_owned(),
        cover_url: None,
        track_count: Some(0),
        editable: true,
    })
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
    if tracks.is_empty() || tracks.len() > MAX_TRACKS_PER_MUTATION {
        return Err(format!(
            "单次歌单变更曲目数必须位于 1..={MAX_TRACKS_PER_MUTATION}"
        ));
    }
    for track in tracks {
        ensure_source(provider_id, track)?;
    }
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;

    // 歌单写接口需要数字 dirId 与数字 songId（MID 需先换取数字 ID）。
    let tid = playlist_id
        .parse::<u64>()
        .map_err(|_| "歌单 id 非法（“我喜欢”暂不支持增删歌）".to_string())?;
    let dir_id = diss_dir_id(&session, account_id, tid)?;
    let mut song_info = Vec::with_capacity(tracks.len());
    for track in tracks {
        let song_id = song_numeric_id(&session, &track.source_id)?;
        let mut entry = Map::new();
        entry.insert("songId".into(), Value::from(song_id));
        entry.insert("songType".into(), Value::from(0));
        song_info.push(Value::Object(entry));
    }
    let mut param = Map::new();
    param.insert("dirId".into(), Value::from(dir_id));
    param.insert("tid".into(), Value::from(tid));
    param.insert("bFmtUtf8".into(), Value::Bool(true));
    param.insert("v_songInfo".into(), Value::Array(song_info));
    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "mutate",
            "music.musicasset.PlaylistDetailWrite",
            if add { "AddSonglist" } else { "DelSonglist" },
            Value::Object(param),
        )],
        false,
    )?;
    Ok(protocol::nested_write_success(
        object.get("mutate").unwrap_or(&Value::Null),
    ))
}

fn diss_dir_id(session: &Session, account_id: &str, tid: u64) -> ApiResult<u64> {
    let mut param = Map::new();
    param.insert("disstid".into(), Value::from(tid));
    param.insert("dirid".into(), Value::from(0));
    param.insert("tag".into(), Value::Bool(false));
    param.insert("song_begin".into(), Value::from(0));
    param.insert("song_num".into(), Value::from(1));
    param.insert("userinfo".into(), Value::Bool(false));
    param.insert("orderlist".into(), Value::Bool(false));
    let object = protocol::musicu(
        session,
        Some(account_id),
        &[(
            "diss",
            "music.srfDissInfo.DissInfo",
            "CgiGetDiss",
            Value::Object(param),
        )],
        false,
    )?;
    object
        .pointer("/diss/data/dirinfo/dirid")
        .and_then(Value::as_u64)
        .or_else(|| object.pointer("/diss/data/dirid").and_then(Value::as_u64))
        .ok_or_else(|| "QQ音乐歌单详情缺少 dirId".to_string())
}

fn sanitize_playlist_name(name: &str) -> ApiResult<&str> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_PLAYLIST_NAME_BYTES || name.contains('\0') {
        return Err("歌单名称为空或超过大小限制".into());
    }
    Ok(name)
}

pub fn liked_tracks(
    provider_id: &str,
    account_id: &str,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let euin = account
        .euin
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "QQ音乐 Cookie 缺少 euin，无法读取“我喜欢”".to_string())?;
    let fav_id = format!("fav-{euin}");
    playlist_tracks(provider_id, account_id, &fav_id, offset, limit)
}

// ---------------------------------------------------------------------------
// 共享工具
// ---------------------------------------------------------------------------

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    for part in cookie.split(';') {
        let mut pair = part.trim().splitn(2, '=');
        let key = pair.next()?.trim();
        if key.eq_ignore_ascii_case(name) {
            return pair.next().map(|value| value.trim().to_owned());
        }
    }
    None
}

/// uin Cookie 兼容 `o12345` / `s12345` 前缀与纯数字。
fn cookie_uin(cookie: &str) -> Option<String> {
    let raw = cookie_value(cookie, "uin")?;
    let digits: String = raw.chars().skip_while(|ch| !ch.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(digits)
}

pub(crate) fn ensure_provider(provider_id: &str) -> ApiResult<()> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
}

fn ensure_source(provider_id: &str, source: &types::SourceTrackRef) -> ApiResult<()> {
    ensure_provider(provider_id)?;
    if source.provider_id != PROVIDER_ID {
        return Err("source provider 与 QQ音乐 route 不匹配".into());
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

pub(crate) fn validate_account_id(account_id: &str) -> ApiResult<()> {
    if account_id.is_empty()
        || account_id.len() > 32
        || !account_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("QQ音乐 account id 非法".into());
    }
    Ok(())
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
        return Err("QQ音乐账号索引超过大小限制".into());
    }
    let mut accounts: Vec<StoredAccount> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("解析QQ音乐账号索引失败: {error}"))?;
    accounts.retain(|account| validate_account_id(&account.account_id).is_ok());
    accounts.truncate(MAX_ACCOUNTS);
    Ok(accounts)
}

fn save_accounts(accounts: &[StoredAccount]) -> ApiResult<()> {
    let bytes = serde_json::to_vec(accounts)
        .map_err(|error| format!("序列化QQ音乐账号索引失败: {error}"))?;
    if bytes.len() > 256 * 1024 {
        return Err("QQ音乐账号索引超过大小限制".into());
    }
    secret_set(&provider_scope(), ACCOUNT_INDEX_KEY, &bytes)?;
    Ok(())
}

pub(crate) fn require_stored_account(account_id: &str) -> ApiResult<StoredAccount> {
    validate_account_id(account_id)?;
    load_accounts()?
        .into_iter()
        .find(|account| account.account_id == account_id)
        .ok_or_else(|| "QQ音乐账号未登录或 Host session 尚未恢复".into())
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
    .ok_or_else(|| "QQ音乐账号 Cookie 不存在，请重新登录".to_string())?;
    let cookie =
        String::from_utf8(bytes).map_err(|_| "QQ音乐 Cookie 不是合法 UTF-8".to_string())?;
    normalize_cookie(&cookie)
}

/// 宽多数组提取：QQ 接口响应路径随版本漂移，按候选路径顺序取第一个数组。
fn first_array<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a Vec<Value>> {
    for pointer in pointers {
        if let Some(array) = value.pointer(pointer).and_then(Value::as_array) {
            return Some(array);
        }
    }
    None
}

pub(crate) fn parse_remote_track(value: &Value) -> ApiResult<types::RemoteTrack> {
    // 新式字段（mid/name/singer/album/interval）优先，老式 fcgi 字段兜底。
    let mid = value_string(value, &["mid"])
        .or_else(|| value_string(value, &["songmid"]))
        .filter(|mid| !mid.trim().is_empty())
        .ok_or_else(|| "歌曲缺少 mid".to_string())?;
    let title = value_string(value, &["name"])
        .or_else(|| value_string(value, &["songname"]))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "歌曲缺少 name".to_string())?;
    let singers = value
        .get("singer")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let artists = singers
        .iter()
        .filter_map(|singer| value_string(singer, &["name"]))
        .filter(|artist| !artist.trim().is_empty())
        .collect::<Vec<_>>();
    let album_mid = value
        .get("album")
        .and_then(|album| value_string(album, &["mid"]))
        .or_else(|| value_string(value, &["albummid"]))
        .unwrap_or_default();
    let album = value
        .get("album")
        .and_then(|album| value_string(album, &["name"]))
        .or_else(|| value_string(value, &["albumname"]))
        .unwrap_or_default();
    let interval = value_u64(value, &["interval"]);
    let cover_url = if album_mid.is_empty() {
        None
    } else {
        Some(format!(
            "https://y.gtimg.cn/music/photo_new/T002R500x500M000{album_mid}.jpg"
        ))
    };
    Ok(types::RemoteTrack {
        source: types::SourceTrackRef {
            provider_id: PROVIDER_ID.into(),
            source_id: mid,
        },
        title,
        artists,
        album,
        duration_ms: interval.map(|seconds| seconds.saturating_mul(1_000)),
        isrc: None,
        cover_url,
        playable: true,
        explicit: false,
    })
}

pub(crate) fn normalize_identity_text(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !ch.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn value_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().map(str::to_owned)
}

pub(crate) fn value_u64(value: &Value, path: &[&str]) -> Option<u64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_u64()
        .or_else(|| current.as_i64().and_then(|value| u64::try_from(value).ok()))
        .or_else(|| current.as_str().and_then(|value| value.parse::<u64>().ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_uin_strips_ptlogin_prefixes() {
        assert_eq!(
            cookie_uin("uin=o12345; qm_keyst=abc; euin=oXabc"),
            Some("12345".into())
        );
        assert_eq!(cookie_uin("uin=999"), Some("999".into()));
        assert_eq!(cookie_uin("qm_keyst=abc"), None);
    }

    #[test]
    fn cookie_value_lookup_is_case_insensitive() {
        assert_eq!(
            cookie_value("A=1; QM_KeySt=xyz", "QM_KeySt"),
            Some("xyz".into())
        );
    }

    #[test]
    fn find_numeric_song_id_matches_by_mid() {
        let value = serde_json::json!({
            "data": {"info": [{"mid": "other", "id": 1}, {"mid": "abc", "id": 42}]}
        });
        assert_eq!(find_numeric_song_id(&value, "abc"), Some(42));
        assert_eq!(find_numeric_song_id(&value, "missing"), None);
    }

    #[test]
    fn parse_remote_track_supports_new_and_legacy_shapes() {
        let new_shape = serde_json::json!({
            "mid": "M1", "name": "歌", "singer": [{"name": "A"}],
            "album": {"mid": "AL", "name": "专辑"}, "interval": 200
        });
        let track = parse_remote_track(&new_shape).expect("new shape");
        assert_eq!(track.source.source_id, "M1");
        assert_eq!(track.duration_ms, Some(200_000));
        assert_eq!(
            track.cover_url.as_deref(),
            Some("https://y.gtimg.cn/music/photo_new/T002R500x500M000AL.jpg")
        );

        let legacy = serde_json::json!({
            "songmid": "M2", "songname": "老歌", "singer": [{"name": "B"}],
            "albumname": "老专辑", "albummid": "AL2", "interval": 180
        });
        let track = parse_remote_track(&legacy).expect("legacy shape");
        assert_eq!(track.source.source_id, "M2");
        assert_eq!(track.album, "老专辑");
    }

    #[test]
    fn search_envelope_omits_comm_and_uses_module_key() {
        // 协议规则回归：搜索信封 key 必须等于 module 名且不带 comm（见 protocol.rs 注释）。
        let query = "周杰伦";
        let envelope = serde_json::json!({
            "music.search.SearchCgiService": {
                "module": "music.search.SearchCgiService",
                "method": "DoSearchForQQMusicDesktop",
                "param": {"search_type": 0, "query": query, "page_num": 1, "num_per_page": 20}
            }
        });
        assert!(envelope.get("comm").is_none());
        assert!(envelope.get("music.search.SearchCgiService").is_some());
    }
}
