//! Read-through provider-owned favorite state; never merge remote songs into
//! the player's local-file favorites. A read is authoritative only if every
//! requested page succeeded and the first page stayed stable.
use std::collections::HashSet;

use crate::{
    plugin::abi::{PluginRoute, RemoteTrack},
    settings::{AppConfig, SavedLikedSource, SavedOnlineLikedTrack},
};

pub(super) type LikeScope = (String, String, String);

pub(super) fn scope(route: &PluginRoute) -> LikeScope {
    (
        route.plugin_id.clone(),
        route.provider_id.clone(),
        route.account_id.clone(),
    )
}

pub(super) fn key(route: &PluginRoute, id: &str) -> SavedLikedSource {
    SavedLikedSource {
        plugin_id: route.plugin_id.clone(),
        provider_id: route.provider_id.clone(),
        account_id: route.account_id.clone(),
        source_id: id.to_owned(),
    }
}

fn matches(route: &PluginRoute, other: &PluginRoute) -> bool {
    scope(route) == scope(other)
}

fn matches_key(route: &PluginRoute, other: &SavedLikedSource) -> bool {
    route.plugin_id == other.plugin_id
        && route.provider_id == other.provider_id
        && route.account_id == other.account_id
}

/// Replace the cached *provider account* state only after a full successful
/// remote read. The local music file favorites are intentionally untouched.
pub(super) fn replace_verified(
    config: &mut AppConfig,
    route: &PluginRoute,
    tracks: Vec<RemoteTrack>,
) -> (usize, usize) {
    let incoming: HashSet<_> = tracks
        .iter()
        .map(|track| track.source.source_id.clone())
        .collect();
    let previous: HashSet<_> = config
        .online_liked_sources
        .iter()
        .filter(|item| matches_key(route, item))
        .map(|item| item.source_id.clone())
        .collect();
    let added = incoming.difference(&previous).count();
    let removed = previous.difference(&incoming).count();
    config
        .online_liked_sources
        .retain(|item| !matches_key(route, item));
    config
        .online_liked_tracks
        .retain(|item| !matches(route, &item.route));
    for track in tracks {
        if config
            .online_liked_sources
            .insert(key(route, &track.source.source_id))
        {
            config.online_liked_tracks.push(SavedOnlineLikedTrack {
                route: route.clone(),
                track,
            });
        }
    }
    (added, removed)
}

/// Provider read pagination uses a 200-track page. If any page fails, the
/// entire read fails: a partial list must never be interpreted as an unlike.
pub(super) async fn read_complete(route: &PluginRoute) -> anyhow::Result<Vec<RemoteTrack>> {
    use anyhow::{anyhow, bail};
    const PAGE: u16 = 200;
    const MAX_PAGES: usize = 26;

    let frontend = crate::plugin::frontend::global().ok_or_else(|| anyhow!("插件前端未初始化"))?;
    for attempt in 0..2 {
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        let mut head = Vec::new();
        let mut complete = false;
        for page in 0..MAX_PAGES {
            let offset = page as u32 * u32::from(PAGE);
            let batch = frontend.liked_tracks_for_route(route, offset, PAGE).await?;
            if page == 0 {
                head = batch
                    .iter()
                    .map(|track| track.source.source_id.clone())
                    .collect();
            }
            let len = batch.len();
            for track in batch {
                if !seen.insert(track.source.source_id.clone()) {
                    bail!("平台收藏分页发生重复，可能正被另一设备修改");
                }
                all.push(track);
            }
            if len < usize::from(PAGE) {
                complete = true;
                break;
            }
        }
        if !complete {
            bail!("平台收藏数量超过完整读取上限，未改动缓存");
        }
        let second_head = frontend.liked_tracks_for_route(route, 0, PAGE).await?;
        let ids: Vec<_> = second_head
            .iter()
            .map(|track| track.source.source_id.clone())
            .collect();
        if head == ids {
            return Ok(all);
        }
        if attempt == 1 {
            bail!("平台收藏同步期间发生变化，请重新刷新");
        }
    }
    Err(anyhow!("平台收藏完整读取未完成"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(account: &str) -> PluginRoute {
        PluginRoute {
            plugin_id: "p".into(),
            provider_id: "net".into(),
            account_id: account.into(),
            priority: 0,
            is_default: true,
        }
    }

    fn song(id: &str) -> RemoteTrack {
        RemoteTrack {
            source: crate::plugin::abi::SourceTrackRef {
                provider_id: "net".into(),
                source_id: id.into(),
            },
            title: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn remote_refresh_only_modifies_exact_account_cache() {
        let mut c = AppConfig::default();
        c.local_liked_tracks.insert(42);
        c.local_liked_paths.insert("music/song.flac".into());
        replace_verified(&mut c, &route("alice"), vec![song("a"), song("b")]);
        replace_verified(&mut c, &route("bob"), vec![song("z")]);
        let (added, removed) =
            replace_verified(&mut c, &route("alice"), vec![song("b"), song("c")]);
        assert_eq!((added, removed), (1, 1));
        assert!(!c.online_liked_sources.contains(&key(&route("alice"), "a")));
        assert!(c.online_liked_sources.contains(&key(&route("alice"), "c")));
        assert!(c.online_liked_sources.contains(&key(&route("bob"), "z")));
        assert_eq!(c.local_liked_tracks.len(), 1);
        assert_eq!(c.local_liked_paths.len(), 1);
    }

    #[test]
    fn repeated_provider_read_is_idempotent() {
        let mut c = AppConfig::default();
        replace_verified(&mut c, &route("alice"), vec![song("a")]);
        assert_eq!(
            replace_verified(&mut c, &route("alice"), vec![song("a")]),
            (0, 0)
        );
        assert_eq!(c.online_liked_tracks.len(), 1);
    }
}
