#![allow(dead_code)]

//! 能力级特性分发：推荐流、播放上报。

use crate::api::{PROVIDER_ID, require_stored_account, session_for, song_numeric_id};
use crate::bindings::yinqidao::music_plugin::{host, types};
use crate::{music, protocol};
use serde_json::json;

use super::api::ApiResult;

pub fn recommendations(
    provider_id: &str,
    account_id: &str,
    request: &types::RecommendationRequest,
) -> ApiResult<Vec<types::RecommendationItem>> {
    ensure_provider(provider_id)?;
    let _ = require_stored_account(account_id)?;
    let limit = request.limit;
    let items = match request.surface {
        // 猜你喜欢 / 每日混音：QQ 侧都是匿名可用的电台流（登录后内容由服务端个性化）。
        types::RecommendationSurface::Home
        | types::RecommendationSurface::DailyMix
        | types::RecommendationSurface::ContinueListening => {
            music::radio_tracks_for(provider_id, account_id, limit)?
        }
        // 新歌速递（匿名可用）。
        types::RecommendationSurface::Discovery => music::new_songs(limit)?,
        // 相似歌曲：需要数字 ID，第一版经“猜你喜欢”兜底。
        types::RecommendationSurface::SimilarTrack | types::RecommendationSurface::ArtistRadio => {
            music::radio_tracks_for(provider_id, account_id, limit)?
        }
    };
    Ok(items
        .into_iter()
        .map(|track| types::RecommendationItem {
            track,
            score: None,
            reason: None,
        })
        .collect())
}

/// Change only this source account's liked songs, never a local-file favorite.
/// The official favorite directory is dirId=201 (not an ordinary user playlist).
pub fn set_liked(
    provider_id: &str,
    account_id: &str,
    track: &types::SourceTrackRef,
    liked: bool,
) -> ApiResult<bool> {
    ensure_provider(provider_id)?;
    if track.provider_id != PROVIDER_ID {
        return Err("QQ音乐收藏来源不属于该平台账号".into());
    }
    if track.source_id.is_empty() || track.source_id.len() > 128 || track.source_id.contains('\0') {
        return Err("QQ音乐收藏歌曲 MID 非法".into());
    }
    let _ = require_stored_account(account_id)?;
    let now_ms = host::now_ms();
    let session = session_for(Some(account_id), now_ms)?;
    let song_id = song_numeric_id(&session, &track.source_id)?;
    let response = protocol::musics_ag1_write(
        &session,
        account_id,
        "like",
        "music.musicasset.PlaylistDetailWrite",
        if liked { "AddSonglist" } else { "DelSonglist" },
        json!({ "dirId": 201, "v_songInfo": [{ "songId": song_id, "songType": 0 }] }),
        now_ms,
    )?;
    Ok(protocol::ag1_write_success(&response, "like"))
}

pub fn report_playback(
    provider_id: &str,
    _account_id: &str,
    _signal: &types::PlaybackSignal,
) -> ApiResult<bool> {
    // 收听上报为可选能力：未声明 PlaybackEvents，宿主不会调用；兜底显式拒绝。
    Err(unsupported(provider_id, "playback_events"))
}

fn unsupported(provider_id: &str, operation: &str) -> String {
    if provider_id == PROVIDER_ID {
        format!("QQ音乐插件未声明或未实现 {operation} capability")
    } else {
        format!("unknown provider id: {provider_id}")
    }
}

fn ensure_provider(provider_id: &str) -> ApiResult<()> {
    if provider_id == PROVIDER_ID {
        Ok(())
    } else {
        Err(format!("unknown provider id: {provider_id}"))
    }
}
