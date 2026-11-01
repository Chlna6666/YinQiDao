#![allow(dead_code)]

//! 通用媒体集合（歌单/专辑/歌手）与用户资料。

use serde_json::{Map, Value};

use crate::api::{PROVIDER_ID, session_for, value_string, value_u64};
use crate::bindings::yinqidao::music_plugin::{host, types};
use crate::protocol::{self, Session};

use super::api::{
    ApiResult, ensure_provider, parse_remote_track, require_stored_account, validate_account_id,
};

// ---------------------------------------------------------------------------
// 歌单改名 / 删除
// ---------------------------------------------------------------------------

pub fn playlist_rename(
    provider_id: &str,
    account_id: &str,
    playlist_id: &str,
    name: &str,
) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let name = name.trim();
    if name.is_empty() || name.len() > 512 || name.contains('\0') {
        return Err("歌单名称为空或超过大小限制".into());
    }
    let session = session_for(Some(account_id), host::now_ms())?;
    let tid = playlist_id
        .parse::<u64>()
        .map_err(|_| "歌单 id 非法".to_string())?;
    // EditPlaylist 要求全字段 + mask:95（官网形状），先取 dirinfo 补齐字段。
    let dirinfo = diss_dirinfo(&session, account_id, tid)?;
    let dir_id = value_u64(&dirinfo, &["dirid"]).ok_or_else(|| "歌单详情缺少 dirId".to_string())?;

    let mut param = Map::new();
    param.insert("dirId".into(), Value::from(dir_id));
    param.insert("tid".into(), Value::from(tid));
    param.insert(
        "dirName".into(),
        Value::from(value_string(&dirinfo, &["diss_name"]).unwrap_or_else(|| name.to_owned())),
    );
    param.insert(
        "dirDesc".into(),
        Value::from(value_string(&dirinfo, &["desc"]).unwrap_or_default()),
    );
    param.insert(
        "dirPicUrl".into(),
        Value::from(value_string(&dirinfo, &["logo"]).unwrap_or_default()),
    );
    param.insert("taglist".into(), Value::from(""));
    param.insert("dirShow".into(), Value::from(2));
    param.insert("mask".into(), Value::from(95));
    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "edit",
            "music.musicasset.PlaylistBaseWrite",
            "EditPlaylist",
            Value::Object(param),
        )],
        false,
    )?;
    Ok(protocol::nested_write_success(
        object.get("edit").unwrap_or(&Value::Null),
    ))
}

pub fn playlist_delete(provider_id: &str, account_id: &str, playlist_id: &str) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let session = session_for(Some(account_id), host::now_ms())?;
    let tid = playlist_id
        .parse::<u64>()
        .map_err(|_| "歌单 id 非法".to_string())?;
    let dirinfo = diss_dirinfo(&session, account_id, tid)?;
    let dir_id = value_u64(&dirinfo, &["dirid"]).ok_or_else(|| "歌单详情缺少 dirId".to_string())?;
    let mut param = Map::new();
    param.insert("dirId".into(), Value::from(dir_id));
    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "delete",
            "music.musicasset.PlaylistBaseWrite",
            "DelPlaylist",
            Value::Object(param),
        )],
        false,
    )?;
    Ok(protocol::nested_write_success(
        object.get("delete").unwrap_or(&Value::Null),
    ))
}

fn diss_dirinfo(session: &Session, account_id: &str, tid: u64) -> ApiResult<Value> {
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
        .pointer("/diss/data/dirinfo")
        .cloned()
        .ok_or_else(|| "QQ音乐歌单详情缺少 dirinfo".into())
}

// ---------------------------------------------------------------------------
// 通用媒体集合
// ---------------------------------------------------------------------------

pub fn media_collections(
    provider_id: &str,
    account_id: &str,
    kind: types::MediaCollectionKind,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::MediaCollection>> {
    ensure_provider(provider_id)?;
    match kind {
        types::MediaCollectionKind::Playlist => {
            playlists_page(provider_id, account_id, offset, limit)
        }
        // QQ 音乐没有“我的专辑/歌手”概念：集合详情可经搜索/推荐入口进入，
        // 但“集合列表”语义不成立，返回空页（与宿主空态约定一致）。
        _ => Ok(Vec::new()),
    }
}

fn playlists_page(
    _provider_id: &str,
    account_id: &str,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::MediaCollection>> {
    let account = require_stored_account(account_id)?;
    let session = session_for(Some(account_id), host::now_ms())?;
    let g_tk = crate::crypto::g_tk(None);
    let mut output = Vec::new();

    let created_url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_user_created_diss?hostuin={}&hostUin=0&sin={}&size={}&g_tk={g_tk}&loginUin=0&format=json&platform=yqq.json&needNewCode=0",
        account.account_id, offset, limit
    );
    if let Ok(created) = protocol::fcgi_get(&session, Some(account_id), &created_url)
        && let Some(items) = first_array(&created, &["/data/disslist"])
    {
        for value in items {
            if let Some(collection) = parse_diss_collection(value, true) {
                output.push(collection);
            }
        }
    }

    let fav_url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_get_profile_order_asset.fcg?ct=20&cid=205360956&userid={}&reqtype=3&sin={}&ein={}&g_tk={g_tk}&loginUin=0&format=json&platform=yqq.json&needNewCode=0",
        account.account_id,
        offset,
        offset.saturating_add(u32::from(limit))
    );
    if let Ok(fav) = protocol::fcgi_get(&session, Some(account_id), &fav_url)
        && let Some(items) = first_array(&fav, &["/data/getplaylist"])
    {
        for value in items {
            if let Some(collection) = parse_diss_collection(value, false) {
                output.push(collection);
            }
        }
    }
    Ok(output
        .into_iter()
        .skip(usize::try_from(offset).unwrap_or(0))
        .take(usize::from(limit))
        .collect())
}

fn parse_diss_collection(value: &Value, editable: bool) -> Option<types::MediaCollection> {
    let source_id = value_u64(value, &["tid"])
        .or_else(|| value_u64(value, &["disstid"]))
        .or_else(|| value_u64(value, &["dirid"]))?;
    let title = value_string(value, &["diss_name"])
        .or_else(|| value_string(value, &["dissname"]))
        .or_else(|| value_string(value, &["title"]))
        .filter(|title| !title.trim().is_empty())?;
    Some(types::MediaCollection {
        source: types::MediaCollectionRef {
            provider_id: PROVIDER_ID.into(),
            kind: types::MediaCollectionKind::Playlist,
            source_id: source_id.to_string(),
        },
        title,
        subtitle: None,
        artwork_url: value_string(value, &["logo"])
            .or_else(|| value_string(value, &["imgurl"]))
            .or_else(|| value_string(value, &["picurl"])),
        item_count: value_u64(value, &["song_cnt"])
            .or_else(|| value_u64(value, &["songnum"]))
            .and_then(|count| u32::try_from(count).ok()),
        editable,
        saved: Some(!editable),
    })
}

pub fn media_collection_detail(
    provider_id: &str,
    account_id: Option<&str>,
    collection: &types::MediaCollectionRef,
) -> ApiResult<types::MediaCollection> {
    ensure_provider(provider_id)?;
    match collection.kind {
        types::MediaCollectionKind::Playlist => {
            let session = session_for(account_id, host::now_ms())?;
            let disstid = collection
                .source_id
                .parse::<u64>()
                .map_err(|_| "歌单 id 非法".to_string())?;
            let dirinfo = diss_dirinfo(&session, account_id.unwrap_or(""), disstid)?;
            let title = value_string(&dirinfo, &["diss_name"])
                .or_else(|| value_string(&dirinfo, &["dissname"]))
                .unwrap_or_else(|| "未命名歌单".into());
            Ok(types::MediaCollection {
                source: collection.clone(),
                title,
                subtitle: value_string(&dirinfo, &["creator", "name"]),
                artwork_url: value_string(&dirinfo, &["logo"])
                    .or_else(|| value_string(&dirinfo, &["imgurl"])),
                item_count: value_u64(&dirinfo, &["song_cnt"])
                    .and_then(|count| u32::try_from(count).ok()),
                editable: false,
                saved: None,
            })
        }
        types::MediaCollectionKind::Album => {
            let session = session_for(account_id, host::now_ms())?;
            let mut param = Map::new();
            param.insert("albumMId".into(), Value::from(collection.source_id.clone()));
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "album",
                    "music.musichallAlbum.AlbumInfoServer",
                    "GetAlbumDetail",
                    Value::Object(param),
                )],
                false,
            )?;
            let basic = object
                .pointer("/album/data/basicInfo")
                .cloned()
                .unwrap_or(Value::Null);
            Ok(types::MediaCollection {
                source: collection.clone(),
                title: value_string(&basic, &["name"]).unwrap_or_else(|| "未命名专辑".into()),
                subtitle: basic
                    .get("company")
                    .and_then(|company| value_string(company, &["name"])),
                artwork_url: value_string(&basic, &["cover"]).or_else(|| {
                    Some(format!(
                        "https://y.gtimg.cn/music/photo_new/T002R500x500M000{}.jpg",
                        collection.source_id
                    ))
                }),
                item_count: None,
                editable: false,
                saved: None,
            })
        }
        types::MediaCollectionKind::Artist => {
            let session = session_for(account_id, host::now_ms())?;
            let mut param = Map::new();
            param.insert(
                "singerMid".into(),
                Value::from(collection.source_id.clone()),
            );
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "singer",
                    "music.web_singer_info_svr",
                    "get_singer_detail_info",
                    Value::Object(param),
                )],
                false,
            )?;
            let brief = object
                .pointer("/singer/data")
                .cloned()
                .unwrap_or(Value::Null);
            Ok(types::MediaCollection {
                source: collection.clone(),
                title: value_string(&brief, &["singer_name"]).unwrap_or_else(|| "未知歌手".into()),
                subtitle: value_string(&brief, &["singer_brief"]),
                artwork_url: Some(format!(
                    "https://y.gtimg.cn/music/photo_new/T001R500x500M000{}.jpg",
                    collection.source_id
                )),
                item_count: value_u64(&brief, &["musicSize"])
                    .and_then(|count| u32::try_from(count).ok()),
                editable: false,
                saved: None,
            })
        }
        types::MediaCollectionKind::Video => Err("QQ音乐插件暂不支持视频集合".into()),
    }
}

pub fn collection_tracks(
    provider_id: &str,
    account_id: Option<&str>,
    collection: &types::MediaCollectionRef,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::RemoteTrack>> {
    ensure_provider(provider_id)?;
    if limit == 0 || limit > 300 {
        return Err("collection limit 必须位于 1..=300".into());
    }
    let session = session_for(account_id, host::now_ms())?;
    match collection.kind {
        types::MediaCollectionKind::Playlist => {
            let disstid = collection
                .source_id
                .parse::<u64>()
                .map_err(|_| "歌单 id 非法".to_string())?;
            let mut param = Map::new();
            param.insert("disstid".into(), Value::from(disstid));
            param.insert("dirid".into(), Value::from(0));
            param.insert("tag".into(), Value::Bool(false));
            param.insert("song_begin".into(), Value::from(offset));
            param.insert("song_num".into(), Value::from(u32::from(limit)));
            param.insert("userinfo".into(), Value::Bool(false));
            param.insert("orderlist".into(), Value::Bool(false));
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "diss",
                    "music.srfDissInfo.DissInfo",
                    "CgiGetDiss",
                    Value::Object(param),
                )],
                false,
            )?;
            let songs = object
                .pointer("/diss/data/songlist")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            songs
                .iter()
                .take(usize::from(limit))
                .map(parse_remote_track)
                .collect()
        }
        types::MediaCollectionKind::Album => {
            let mut param = Map::new();
            param.insert("albumMid".into(), Value::from(collection.source_id.clone()));
            param.insert("begin".into(), Value::from(offset));
            param.insert("count".into(), Value::from(u32::from(limit)));
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "album",
                    "music.musichallAlbum.AlbumSongList",
                    "GetAlbumSongList",
                    Value::Object(param),
                )],
                false,
            )?;
            let songs = object
                .pointer("/album/data/songList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(songs
                .iter()
                .filter_map(|item| item.get("songInfo").or(Some(item)))
                .filter_map(|value| parse_remote_track(value).ok())
                .take(usize::from(limit))
                .collect())
        }
        types::MediaCollectionKind::Artist => {
            let mut param = Map::new();
            param.insert(
                "singerMid".into(),
                Value::from(collection.source_id.clone()),
            );
            param.insert("begin".into(), Value::from(offset));
            param.insert("count".into(), Value::from(u32::from(limit)));
            param.insert("order".into(), Value::from(5));
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "singer",
                    "musichall.song_list_server",
                    "GetSingerSongList",
                    Value::Object(param),
                )],
                false,
            )?;
            let songs = object
                .pointer("/singer/data/songList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(songs
                .iter()
                .filter_map(|item| item.get("songInfo").or(Some(item)))
                .filter_map(|value| parse_remote_track(value).ok())
                .take(usize::from(limit))
                .collect())
        }
        types::MediaCollectionKind::Video => Err("QQ音乐插件暂不支持视频集合".into()),
    }
}

pub fn collection_items(
    provider_id: &str,
    account_id: Option<&str>,
    collection: &types::MediaCollectionRef,
    offset: u32,
    limit: u16,
) -> ApiResult<Vec<types::MediaCollection>> {
    ensure_provider(provider_id)?;
    match collection.kind {
        // 歌手 → 专辑列表（歌手页“全部专辑”入口）
        types::MediaCollectionKind::Artist => {
            let session = session_for(account_id, host::now_ms())?;
            let mut param = Map::new();
            param.insert(
                "singerMid".into(),
                Value::from(collection.source_id.clone()),
            );
            param.insert("begin".into(), Value::from(offset));
            param.insert("count".into(), Value::from(u32::from(limit)));
            param.insert("order".into(), Value::from(1));
            let object = protocol::musicu(
                &session,
                account_id,
                &[(
                    "albums",
                    "music.musichallAlbum.AlbumListServer",
                    "GetAlbumList",
                    Value::Object(param),
                )],
                false,
            )?;
            let albums = object
                .pointer("/albums/data/albumList")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Ok(albums
                .iter()
                .filter_map(|album| {
                    let mid = value_string(album, &["mid"])
                        .or_else(|| value_string(album, &["albumMid"]))?;
                    let basic = album.get("basicInfo").cloned().unwrap_or(album.clone());
                    Some(types::MediaCollection {
                        source: types::MediaCollectionRef {
                            provider_id: PROVIDER_ID.into(),
                            kind: types::MediaCollectionKind::Album,
                            source_id: mid,
                        },
                        title: value_string(&basic, &["name"])
                            .unwrap_or_else(|| "未命名专辑".into()),
                        subtitle: None,
                        artwork_url: value_string(&basic, &["cover"]).or_else(|| {
                            Some(format!(
                                "https://y.gtimg.cn/music/photo_new/T002R500x500M000{}.jpg",
                                value_string(album, &["mid"]).unwrap_or_default()
                            ))
                        }),
                        item_count: value_u64(&basic, &["albumDuration"])
                            .and_then(|count| u32::try_from(count).ok()),
                        editable: false,
                        saved: None,
                    })
                })
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

pub fn set_media_saved(
    _provider_id: &str,
    _account_id: &str,
    _collection: &types::MediaCollectionRef,
    _saved: bool,
) -> ApiResult<bool> {
    // 收藏歌单/专辑的写接口依赖 ag-1 加密通道（与红心同批实现）。
    Err("QQ音乐插件暂不支持收藏集合写入（ag-1 加密通道待后续版本）".to_string())
}

pub fn collection_recommendations(
    provider_id: &str,
    account_id: &str,
    request: &types::CollectionRecommendationRequest,
) -> ApiResult<Vec<types::CollectionRecommendationItem>> {
    ensure_provider(provider_id)?;
    let _ = require_stored_account(account_id)?;
    // 推荐歌单广场（匿名可用）；分页游标 From = 页码 * Size。
    let size = u32::from(request.limit).max(20);
    let from = match request.surface {
        types::CollectionRecommendationSurface::Home => 0,
        _ => 0,
    };
    let session = session_for(Some(account_id), host::now_ms())?;
    let mut param = Map::new();
    param.insert("From".into(), Value::from(from));
    param.insert("Size".into(), Value::from(size));
    let object = protocol::musicu(
        &session,
        Some(account_id),
        &[(
            "square",
            "music.playlist.PlaylistSquare",
            "GetRecommendFeed",
            Value::Object(param),
        )],
        false,
    )?;
    let list = object
        .pointer("/square/data/List")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|item| {
            let basic = item.pointer("/Playlist/basic")?;
            let tid = value_u64(basic, &["tid"])?;
            Some(types::CollectionRecommendationItem {
                collection: types::MediaCollection {
                    source: types::MediaCollectionRef {
                        provider_id: PROVIDER_ID.into(),
                        kind: types::MediaCollectionKind::Playlist,
                        source_id: tid.to_string(),
                    },
                    title: value_string(basic, &["title"]).unwrap_or_else(|| "未命名歌单".into()),
                    subtitle: item
                        .pointer("/Playlist/creator/nick")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    artwork_url: item
                        .pointer("/Playlist/cover/medium_url")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    item_count: value_u64(basic, &["song_cnt"])
                        .and_then(|count| u32::try_from(count).ok()),
                    editable: false,
                    saved: None,
                },
                score: None,
                reason: None,
            })
        })
        .collect())
}

pub fn user_profile(provider_id: &str, account_id: &str) -> ApiResult<types::UserProfile> {
    ensure_provider(provider_id)?;
    let account = require_stored_account(account_id)?;
    let session = session_for(Some(account_id), host::now_ms())?;
    let qm_keyst = session.qm_keyst.clone();
    let g_tk = crate::crypto::g_tk(qm_keyst.as_deref());
    let uin = &account.account_id;
    let url = format!(
        "https://c.y.qq.com/rsc/fcgi-bin/fcg_get_profile_homepage.fcg?g_tk={g_tk}&format=json&cid=205360838&needNewCode=0&loginUin={uin}&hostUin=0&userid={uin}&reqfrom=1"
    );
    let homepage = protocol::fcgi_get(&session, Some(account_id), &url)?;
    let data = homepage.get("data").cloned().unwrap_or(Value::Null);
    Ok(types::UserProfile {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.into(),
        display_name: value_string(&data, &["nickname"])
            .unwrap_or_else(|| account.display_name.clone()),
        avatar_url: value_string(&data, &["logo"]).or_else(|| account.avatar_url.clone()),
        bio: None,
        level: value_u64(&data, &["level"]).and_then(|level| u32::try_from(level).ok()),
        follower_count: value_u64(&data, &["fansnum"]).and_then(|count| u32::try_from(count).ok()),
        following_count: value_u64(&data, &["follownum"])
            .and_then(|count| u32::try_from(count).ok()),
        playlist_count: value_u64(&data, &["playlist_count"])
            .and_then(|count| u32::try_from(count).ok()),
        listen_count: value_u64(&data, &["listen_total"]),
    })
}

fn first_array<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a Vec<Value>> {
    for pointer in pointers {
        if let Some(array) = value.pointer(pointer).and_then(Value::as_array) {
            return Some(array);
        }
    }
    None
}
