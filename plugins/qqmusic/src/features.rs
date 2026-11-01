#![allow(dead_code)]

//! 能力级特性分发：推荐流、播放上报。

use crate::api::{PROVIDER_ID, require_stored_account};
use crate::bindings::yinqidao::music_plugin::types;
use crate::music;

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

pub fn set_liked(
    provider_id: &str,
    _account_id: &str,
    _track: &types::SourceTrackRef,
    _liked: bool,
) -> ApiResult<bool> {
    // 红心写入依赖 zzc 签名 + ag-1 加密通道（明文 AddSonglist 必定 80105），
    // 能力清单未声明 LikeSync，宿主 UI 会隐藏对应入口。
    Err(unsupported(provider_id, "like_sync"))
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
