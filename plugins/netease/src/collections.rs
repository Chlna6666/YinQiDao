use serde_json::{Value, json};

use crate::{
    api, protocol,
    bindings::yinqidao::music_plugin::types,
};

const PROVIDER_ID: &str = "netease";
const MAX_COLLECTION_PAGE_LIMIT: u16 = 200;
const MAX_COLLECTION_RECOMMENDATION_LIMIT: u16 = 100;

pub fn media_collections(
    provider_id: &str,
    account_id: &str,
    kind: types::MediaCollectionKind,
    offset: u32,
    limit: u16,
) -> Result<Vec<types::MediaCollection>, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    validate_page_limit(limit)?;

    match kind {
        types::MediaCollectionKind::Playlist => playlist_collections(account_id, offset, limit),
        types::MediaCollectionKind::Album => {
            let json = protocol::weapi(
                account_id,
                "/api/album/sublist",
                json!({ "limit": limit, "offset": offset, "total": true }),
            )?;
            parse_collection_array(
                json.get("data").or_else(|| json.get("albums")),
                types::MediaCollectionKind::Album,
                parse_album_collection,
            )
        }
        types::MediaCollectionKind::Artist => {
            let json = protocol::weapi(
                account_id,
                "/api/artist/sublist",
                json!({ "limit": limit, "offset": offset, "total": true }),
            )?;
            parse_collection_array(
                json.get("data").or_else(|| json.get("artists")),
                types::MediaCollectionKind::Artist,
                parse_artist_collection,
            )
        }
        types::MediaCollectionKind::Video => {
            let json = protocol::weapi(
                account_id,
                "/api/cloudvideo/allvideo/sublist",
                json!({ "limit": limit, "offset": offset, "total": true }),
            )?;
            parse_collection_array(
                json.get("data").or_else(|| json.get("videos")),
                types::MediaCollectionKind::Video,
                parse_video_collection,
            )
        }
    }
}

pub fn set_media_saved(
    provider_id: &str,
    account_id: &str,
    collection: &types::MediaCollectionRef,
    saved: bool,
) -> Result<bool, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if collection.provider_id != PROVIDER_ID {
        return Err("媒体收藏 source provider 与网易云 route 不匹配".into());
    }
    let id = numeric_source_id(&collection.source_id)?;

    let response = match collection.kind {
        types::MediaCollectionKind::Playlist => protocol::eapi(
            account_id,
            if saved {
                "/api/playlist/subscribe"
            } else {
                "/api/playlist/unsubscribe"
            },
            json!({ "id": id.to_string() }),
        )?,
        types::MediaCollectionKind::Album => protocol::weapi(
            account_id,
            if saved { "/api/album/sub" } else { "/api/album/unsub" },
            json!({ "id": id.to_string() }),
        )?,
        types::MediaCollectionKind::Artist => protocol::weapi(
            account_id,
            if saved { "/api/artist/sub" } else { "/api/artist/unsub" },
            json!({
                "artistId": id.to_string(),
                "artistIds": format!("[{id}]"),
            }),
        )?,
        types::MediaCollectionKind::Video => protocol::weapi(
            account_id,
            if saved { "/api/mv/sub" } else { "/api/mv/unsub" },
            json!({
                "mvId": id.to_string(),
                "mvIds": format!("[\"{id}\"]"),
            }),
        )?,
    };
    Ok(api_code_success(&response))
}

pub fn collection_recommendations(
    provider_id: &str,
    account_id: &str,
    request: &types::CollectionRecommendationRequest,
) -> Result<Vec<types::CollectionRecommendationItem>, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    if request.limit == 0 || request.limit > MAX_COLLECTION_RECOMMENDATION_LIMIT {
        return Err(format!(
            "collection recommendation limit 必须位于 1..={MAX_COLLECTION_RECOMMENDATION_LIMIT}"
        ));
    }
    if !matches!(request.kind, types::MediaCollectionKind::Playlist) {
        return Err("网易云当前仅实现 playlist collection recommendation".into());
    }
    if !matches!(request.surface, types::CollectionRecommendationSurface::Daily) {
        return Err("网易云当前仅实现 daily playlist recommendation".into());
    }

    let json = protocol::weapi(
        account_id,
        "/api/v1/discovery/recommend/resource",
        json!({}),
    )?;
    let mut items = json
        .get("recommend")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(parse_recommended_playlist)
        .collect::<Result<Vec<_>, _>>()?;

    items.retain(|item| {
        !request.exclude.iter().any(|excluded| {
            excluded.provider_id == PROVIDER_ID
                && same_collection_kind(&excluded.kind, &item.collection.source.kind)
                && excluded.source_id == item.collection.source.source_id
        })
    });
    items.truncate(usize::from(request.limit));
    Ok(items)
}

pub fn user_profile(
    provider_id: &str,
    account_id: &str,
) -> Result<types::UserProfile, String> {
    ensure_provider(provider_id)?;
    validate_account_id(account_id)?;
    let path = format!("/api/v1/user/detail/{account_id}");
    let json = protocol::weapi(account_id, &path, json!({}))?;
    let profile = json
        .get("profile")
        .filter(|value| value.is_object())
        .ok_or_else(|| "网易云用户资料缺少 profile".to_string())?;
    let display_name = value_string(profile, &["nickname"])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "网易云用户资料缺少 nickname".to_string())?;

    Ok(types::UserProfile {
        provider_id: PROVIDER_ID.into(),
        account_id: account_id.into(),
        display_name,
        avatar_url: value_string(profile, &["avatarUrl"]).map(force_https),
        bio: value_string(profile, &["signature"]).filter(|value| !value.trim().is_empty()),
        level: value_u32(&json, &["level"]),
        follower_count: value_u32(profile, &["followeds"]),
        following_count: value_u32(profile, &["follows"]),
        playlist_count: value_u32(profile, &["playlistCount"]),
        listen_count: value_u64(&json, &["listenSongs"]),
    })
}

fn playlist_collections(
    account_id: &str,
    offset: u32,
    limit: u16,
) -> Result<Vec<types::MediaCollection>, String> {
    let playlists = api::playlists(PROVIDER_ID, account_id)?;
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(playlists.len());
    let end = start
        .saturating_add(usize::from(limit))
        .min(playlists.len());
    Ok(playlists[start..end]
        .iter()
        .map(|playlist| types::MediaCollection {
            source: types::MediaCollectionRef {
                provider_id: PROVIDER_ID.into(),
                kind: types::MediaCollectionKind::Playlist,
                source_id: playlist.source_id.clone(),
            },
            title: playlist.name.clone(),
            subtitle: None,
            artwork_url: playlist.cover_url.clone(),
            item_count: playlist.track_count,
            editable: playlist.editable,
            saved: Some(true),
        })
        .collect())
}

fn parse_collection_array(
    value: Option<&Value>,
    _kind: types::MediaCollectionKind,
    parser: fn(&Value) -> Result<types::MediaCollection, String>,
) -> Result<Vec<types::MediaCollection>, String> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(parser)
        .collect()
}

fn parse_album_collection(value: &Value) -> Result<types::MediaCollection, String> {
    let id = value_id_string(value, &["id"])
        .ok_or_else(|| "网易云收藏专辑缺少 id".to_string())?;
    let title = required_name(value, "网易云收藏专辑")?;
    let subtitle = value
        .get("artists")
        .and_then(Value::as_array)
        .map(|artists| join_names(artists, "name"))
        .filter(|value| !value.is_empty());
    Ok(types::MediaCollection {
        source: types::MediaCollectionRef {
            provider_id: PROVIDER_ID.into(),
            kind: types::MediaCollectionKind::Album,
            source_id: id,
        },
        title,
        subtitle,
        artwork_url: value_string(value, &["picUrl"]).map(force_https),
        item_count: value_u32(value, &["size"]),
        editable: false,
        saved: Some(true),
    })
}

fn parse_artist_collection(value: &Value) -> Result<types::MediaCollection, String> {
    let id = value_id_string(value, &["id"])
        .ok_or_else(|| "网易云收藏歌手缺少 id".to_string())?;
    let title = required_name(value, "网易云收藏歌手")?;
    let subtitle = value
        .get("alias")
        .and_then(Value::as_array)
        .map(|aliases| {
            aliases
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .filter(|value| !value.is_empty());
    Ok(types::MediaCollection {
        source: types::MediaCollectionRef {
            provider_id: PROVIDER_ID.into(),
            kind: types::MediaCollectionKind::Artist,
            source_id: id,
        },
        title,
        subtitle,
        artwork_url: value_string(value, &["picUrl"])
            .or_else(|| value_string(value, &["img1v1Url"]))
            .map(force_https),
        item_count: value_u32(value, &["albumSize"]),
        editable: false,
        saved: Some(true),
    })
}

fn parse_video_collection(value: &Value) -> Result<types::MediaCollection, String> {
    let id = value_id_string(value, &["vid"])
        .or_else(|| value_id_string(value, &["id"]))
        .or_else(|| value_id_string(value, &["mvId"]))
        .ok_or_else(|| "网易云收藏视频缺少 source id".to_string())?;
    let title = value_string(value, &["title"])
        .or_else(|| value_string(value, &["name"]))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "网易云收藏视频缺少标题".to_string())?;
    let subtitle = value
        .get("creator")
        .and_then(Value::as_array)
        .map(|creators| join_names(creators, "userName"))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            value
                .get("artists")
                .and_then(Value::as_array)
                .map(|artists| join_names(artists, "name"))
                .filter(|value| !value.is_empty())
        });
    Ok(types::MediaCollection {
        source: types::MediaCollectionRef {
            provider_id: PROVIDER_ID.into(),
            kind: types::MediaCollectionKind::Video,
            source_id: id,
        },
        title,
        subtitle,
        artwork_url: value_string(value, &["coverUrl"])
            .or_else(|| value_string(value, &["cover"]))
            .or_else(|| value_string(value, &["imgurl"]))
            .map(force_https),
        item_count: None,
        editable: false,
        saved: Some(true),
    })
}

fn parse_recommended_playlist(
    value: &Value,
) -> Result<types::CollectionRecommendationItem, String> {
    let source_id = value_id_string(value, &["id"])
        .ok_or_else(|| "网易云每日推荐歌单缺少 id".to_string())?;
    let title = required_name(value, "网易云每日推荐歌单")?;
    let subtitle = value
        .get("creator")
        .and_then(|creator| value_string(creator, &["nickname"]))
        .filter(|value| !value.trim().is_empty());
    let reason = value_string(value, &["copywriter"])
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("网易云每日推荐歌单".into()));
    Ok(types::CollectionRecommendationItem {
        collection: types::MediaCollection {
            source: types::MediaCollectionRef {
                provider_id: PROVIDER_ID.into(),
                kind: types::MediaCollectionKind::Playlist,
                source_id,
            },
            title,
            subtitle,
            artwork_url: value_string(value, &["picUrl"])
                .or_else(|| value_string(value, &["coverImgUrl"]))
                .map(force_https),
            item_count: value_u32(value, &["trackCount"]),
            editable: false,
            saved: value_bool(value, &["subscribed"]),
        },
        score: None,
        reason,
    })
}

fn required_name(value: &Value, context: &str) -> Result<String, String> {
    value_string(value, &["name"])
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{context}缺少 name"))
}

fn join_names(values: &[Value], key: &str) -> String {
    values
        .iter()
        .filter_map(|value| value.get(key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn same_collection_kind(
    left: &types::MediaCollectionKind,
    right: &types::MediaCollectionKind,
) -> bool {
    matches!(
        (left, right),
        (
            types::MediaCollectionKind::Playlist,
            types::MediaCollectionKind::Playlist
        ) | (
            types::MediaCollectionKind::Album,
            types::MediaCollectionKind::Album
        ) | (
            types::MediaCollectionKind::Artist,
            types::MediaCollectionKind::Artist
        ) | (
            types::MediaCollectionKind::Video,
            types::MediaCollectionKind::Video
        )
    )
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

fn validate_page_limit(limit: u16) -> Result<(), String> {
    if limit == 0 || limit > MAX_COLLECTION_PAGE_LIMIT {
        return Err(format!(
            "media collection limit 必须位于 1..={MAX_COLLECTION_PAGE_LIMIT}"
        ));
    }
    Ok(())
}

fn numeric_source_id(source_id: &str) -> Result<u64, String> {
    if source_id.is_empty()
        || source_id.len() > 32
        || !source_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("网易云 media collection source id 非法".into());
    }
    source_id
        .parse::<u64>()
        .map_err(|_| "网易云 media collection source id 超出数值范围".into())
}

fn value_id_string(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| current.as_u64().map(|value| value.to_string()))
        .or_else(|| {
            current
                .as_i64()
                .filter(|value| *value >= 0)
                .map(|value| value.to_string())
        })
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

fn value_u32(value: &Value, path: &[&str]) -> Option<u32> {
    value_u64(value, path).and_then(|value| u32::try_from(value).ok())
}

fn value_bool(value: &Value, path: &[&str]) -> Option<bool> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_bool()
}

fn api_code_success(json: &Value) -> bool {
    json.get("code")
        .and_then(Value::as_i64)
        .is_none_or(|code| (200..300).contains(&code))
}

fn force_https(url: String) -> String {
    if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        url
    }
}
