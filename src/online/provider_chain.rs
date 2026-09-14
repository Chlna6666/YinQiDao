use std::time::Duration;

use anyhow::Result;
use image::GenericImageView;

use crate::{
    lyrics::LyricsDocument,
    model::Track,
    plugin::{
        abi::{RemoteTrack, RoutingPolicy, TrackQuery},
        frontend::{self as plugin_frontend, plugin_lyrics_to_player},
    },
};

use super::{EnrichmentResult, MetadataMatch, OnlineServices, SpotifyTokenResponse, providers};

const MIN_COVER_BYTES: usize = 512;
const MIN_COVER_DIMENSION: u32 = 64;
const COVER_TITLE_SIMILARITY: i32 = 90;
const COVER_ARTIST_SIMILARITY: i32 = 85;
const COVER_ALBUM_SIMILARITY: i32 = 60;
const COVER_DURATION_TOLERANCE_MS: u64 = 5_000;
const MAX_PLUGIN_TRACK_TEXT_BYTES: usize = 32 * 1024;
const MAX_PLUGIN_TRACK_ARTISTS: usize = 128;
const COVER_VERSION_TERMS: &[&str] = &[
    "live",
    "现场",
    "演唱会",
    "concert",
    "remix",
    "混音",
    "remaster",
    "重制",
    "acoustic",
    "unplugged",
    "不插电",
    "instrumental",
    "伴奏",
    "纯音乐",
    "karaoke",
    "卡拉ok",
    "spedup",
    "slowed",
    "加速",
    "慢速",
    "demo",
    "radioedit",
    "mono",
    "单声道",
    "stereo",
    "立体声",
    "cover",
    "翻唱",
];

struct AuthenticatedPluginEnrichment {
    result: EnrichmentResult,
    identity_track: Track,
}

impl OnlineServices {
    pub(super) async fn enrich_from_providers(
        &self,
        track: &Track,
        fetch_lyrics: bool,
        fetch_artwork: bool,
    ) -> Result<Option<EnrichmentResult>> {
        // Authenticated plugins are the first identity source. With no installed Wasmtime adapter,
        // or when every authenticated route is gated/unmatched, this returns None and the existing
        // built-in chain below is unchanged.
        let mut authenticated = self
            .enrich_from_authenticated_plugin(track, fetch_lyrics, fetch_artwork)
            .await?;
        if authenticated.as_ref().is_some_and(|enrichment| {
            !fetch_artwork || enrichment.result.artwork.is_some()
        }) {
            return Ok(authenticated.take().map(|enrichment| enrichment.result));
        }

        // Search every built-in provider before committing to an anonymous identity. When an
        // authenticated plugin already resolved the track but did not provide valid artwork, these
        // searches are used only for artwork; search with the resolved identity so stale local tags
        // cannot poison that fallback.
        let search_track = authenticated
            .as_ref()
            .map_or_else(|| track.clone(), |enrichment| enrichment.identity_track.clone());
        let spotify_token = self.spotify_token().await;
        let mut searches = Vec::with_capacity(providers::ProviderKind::priority_order().len());
        for provider in providers::ProviderKind::priority_order() {
            let client = self.client.clone();
            let track = search_track.clone();
            let netease_base_url = self.netease_base_url.clone();
            let spotify_token = spotify_token.clone();
            searches.push((
                provider,
                tokio::spawn(async move {
                    providers::search(
                        &client,
                        provider,
                        &track,
                        &netease_base_url,
                        spotify_token.as_deref(),
                    )
                    .await
                }),
            ));
        }

        let mut candidates = Vec::with_capacity(searches.len());
        for (provider, search) in searches {
            match search.await {
                Ok(Ok(Some(candidate))) => {
                    tracing::debug!(
                        provider = provider.name(),
                        score = candidate.score,
                        title = %candidate.title,
                        artist = %candidate.artist,
                        "在线平台提交候选"
                    );
                    candidates.push(candidate);
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    tracing::debug!(provider = provider.name(), %error, "音乐平台搜索失败");
                }
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "音乐平台搜索任务异常退出");
                }
            }
        }

        let matched = providers::choose_global_best(&search_track, candidates.clone());

        // Authenticated metadata remains the winner. Artwork is intentionally independent: an
        // authenticated descriptor is attempted first inside enrich_from_authenticated_plugin();
        // only when that path is unavailable/invalid do matching built-in candidates or CAA fill
        // the artwork field.
        if let Some(mut enrichment) = authenticated {
            if fetch_artwork && enrichment.result.artwork.is_none() {
                let (artwork, artwork_key) = if let Some(matched) = matched.as_ref() {
                    self.resolve_online_artwork(
                        &enrichment.identity_track,
                        matched,
                        &candidates,
                    )
                    .await
                } else {
                    self.resolve_musicbrainz_artwork(&enrichment.identity_track)
                        .await
                };
                enrichment.result.artwork = artwork;
                enrichment.result.artwork_key = artwork_key;
            }
            return Ok(Some(enrichment.result));
        }

        let Some(matched) = matched else {
            // Returning None intentionally hands control back to the AcoustID/MusicBrainz path.
            // Ambiguous platform results should never overwrite a stronger audio identity.
            return Ok(None);
        };
        let matched_provider = matched.provider;
        tracing::debug!(
            provider = matched_provider.name(),
            score = matched.score,
            title = %matched.title,
            artist = %matched.artist,
            "采用全局最佳在线候选"
        );

        let metadata = MetadataMatch {
            title: matched.title.clone(),
            artist: matched.artist.clone(),
            album: matched.album.clone(),
            recording_mbid: format!("{}:{}", matched.provider.key(), matched.source_id),
            release_mbid: None,
            source: Some(matched.provider.name().into()),
            release_date: matched.release_date.clone(),
        };
        let lyrics = if fetch_lyrics {
            let provider_lyrics = match providers::lyrics(&self.client, &matched).await {
                Ok(lyrics) => lyrics,
                Err(error) => {
                    tracing::debug!(
                        provider = matched_provider.name(),
                        %error,
                        "首选音乐平台歌词读取失败，尝试高质量歌词兜底"
                    );
                    None
                }
            };

            let mut selected = provider_lyrics;
            if selected
                .as_ref()
                .map_or(true, |lyrics| !lyrics_have_word_timing(lyrics))
                && let Some(word_timed) = self
                    .fetch_word_timed_lyrics_fallback(track, Some(matched.provider))
                    .await
            {
                selected = Some(match selected {
                    Some(primary) => prefer_richer_lyrics(primary, word_timed),
                    None => word_timed,
                });
            }

            if selected
                .as_ref()
                .map_or(true, |lyrics| !lyrics.has_translation())
                && let Some(translated) = self
                    .fetch_translated_lyrics_fallback(track, Some(matched.provider))
                    .await
            {
                selected = Some(match selected {
                    Some(primary) => prefer_richer_lyrics(primary, translated),
                    None => translated,
                });
            }

            if selected.is_some() {
                selected
            } else {
                self.fetch_lyrics(Some(&metadata), track).await?
            }
        } else {
            None
        };

        // Artwork is resolved independently from the metadata winner, but only after the local
        // artwork loader has explicitly requested an online fallback. This prevents ordinary
        // metadata/lyrics enrichment from replacing a local artwork_key in persistent storage.
        let (artwork, artwork_key) = if fetch_artwork {
            self.resolve_online_artwork(track, &matched, &candidates)
                .await
        } else {
            (None, None)
        };

        Ok(Some(EnrichmentResult {
            metadata: Some(metadata),
            lyrics,
            artwork,
            artwork_key,
        }))
    }

    async fn enrich_from_authenticated_plugin(
        &self,
        track: &Track,
        fetch_lyrics: bool,
        fetch_artwork: bool,
    ) -> Result<Option<AuthenticatedPluginEnrichment>> {
        let Some(frontend) = plugin_frontend::global() else {
            return Ok(None);
        };
        let query = local_track_query(track);
        let resolved = match frontend
            .resolve_track(&query, &RoutingPolicy::default())
            .await
        {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::debug!(%error, "authenticated plugin metadata 调用失败，回退匿名 provider");
                return Ok(None);
            }
        };
        if !resolved.client_ready {
            return Ok(None);
        }
        for failure in &resolved.failures {
            tracing::debug!(
                plugin = %failure.route.plugin_id,
                provider = %failure.route.provider_id,
                account = %failure.route.account_id,
                error = %failure.error,
                "authenticated plugin route 调用失败，尝试下一账号"
            );
        }
        let (Some(remote), Some(route)) = (resolved.value, resolved.route) else {
            return Ok(None);
        };
        if !plugin_remote_track_is_bounded(&remote)
            || remote.source.provider_id != route.provider_id
            || remote.source.source_id.trim().is_empty()
        {
            tracing::warn!(
                plugin = %route.plugin_id,
                provider = %route.provider_id,
                "authenticated plugin 返回非法或越界 track，已拒绝"
            );
            return Ok(None);
        }

        let artist = remote.artists.join(" / ");
        let confidence_candidate = providers::ProviderMatch {
            provider: providers::ProviderKind::Netease,
            source_id: remote.source.source_id.clone(),
            title: remote.title.clone(),
            artist: artist.clone(),
            album: remote.album.clone(),
            duration_ms: remote.duration_ms,
            release_date: None,
            // Never feed guest-provided artwork URLs into the anonymous reqwest path. Artwork from
            // plugin descriptors is downloaded only by Host-mediated HTTP.
            cover_url: None,
            lyric_url: None,
            score: 0,
        };
        let Some(scored) = providers::choose_global_best(track, vec![confidence_candidate]) else {
            tracing::debug!(
                plugin = %route.plugin_id,
                provider = %route.provider_id,
                title = %remote.title,
                artist = %artist,
                "authenticated plugin track 未通过 Host 身份置信度校验"
            );
            return Ok(None);
        };

        let metadata = MetadataMatch {
            title: remote.title.clone(),
            artist: if artist.trim().is_empty() {
                track.artist.clone()
            } else {
                artist
            },
            album: if remote.album.trim().is_empty() {
                track.album.clone()
            } else {
                remote.album.clone()
            },
            recording_mbid: format!(
                "plugin:{}:{}:{}",
                route.plugin_id, route.provider_id, remote.source.source_id
            ),
            release_mbid: None,
            source: Some(format!("插件 · {}", route.provider_id)),
            release_date: None,
        };

        let mut identity_track = track.clone();
        identity_track.title.clone_from(&metadata.title);
        identity_track.artist.clone_from(&metadata.artist);
        identity_track.album.clone_from(&metadata.album);

        let mut lyrics = if fetch_lyrics {
            match frontend.lyrics_for_route(&route, &remote.source).await {
                Ok(result) => {
                    for failure in &result.failures {
                        tracing::debug!(
                            plugin = %failure.route.plugin_id,
                            provider = %failure.route.provider_id,
                            account = %failure.route.account_id,
                            error = %failure.error,
                            "authenticated plugin 歌词调用失败，尝试下一账号"
                        );
                    }
                    match result.value {
                        Some(document) => match plugin_lyrics_to_player(
                            document,
                            &format!("插件 {}", route.provider_id),
                        ) {
                            Ok(lyrics) => lyrics,
                            Err(error) => {
                                tracing::warn!(
                                    plugin = %route.plugin_id,
                                    provider = %route.provider_id,
                                    %error,
                                    "authenticated plugin 歌词结构非法，回退其他歌词来源"
                                );
                                None
                            }
                        },
                        None => None,
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        plugin = %route.plugin_id,
                        provider = %route.provider_id,
                        %error,
                        "authenticated plugin 歌词规划失败，回退其他歌词来源"
                    );
                    None
                }
            }
        } else {
            None
        };

        if fetch_lyrics {
            if lyrics
                .as_ref()
                .map_or(true, |lyrics| !lyrics_have_word_timing(lyrics))
                && let Some(word_timed) = self
                    .fetch_word_timed_lyrics_fallback(&identity_track, None)
                    .await
            {
                lyrics = Some(match lyrics {
                    Some(primary) => prefer_richer_lyrics(primary, word_timed),
                    None => word_timed,
                });
            }
            if lyrics
                .as_ref()
                .map_or(true, |lyrics| !lyrics.has_translation())
                && let Some(translated) = self
                    .fetch_translated_lyrics_fallback(&identity_track, None)
                    .await
            {
                lyrics = Some(match lyrics {
                    Some(primary) => prefer_richer_lyrics(primary, translated),
                    None => translated,
                });
            }
            if lyrics.is_none() {
                lyrics = self.fetch_lyrics(Some(&metadata), &identity_track).await?;
            }
        }

        let (artwork, artwork_key) = if fetch_artwork {
            match frontend.artwork_for_route(&route, &remote.source).await {
                Ok(result) => {
                    for failure in &result.failures {
                        tracing::debug!(
                            plugin = %failure.route.plugin_id,
                            provider = %failure.route.provider_id,
                            account = %failure.route.account_id,
                            error = %failure.error,
                            "authenticated plugin 封面调用失败，尝试下一账号"
                        );
                    }
                    match result.value {
                        Some(bytes) => match validate_cover_bytes(bytes).await {
                            Some(bytes) => {
                                tracing::debug!(
                                    plugin = %route.plugin_id,
                                    provider = %route.provider_id,
                                    source_id = %remote.source.source_id,
                                    "采用 authenticated plugin 封面"
                                );
                                (
                                    Some(bytes),
                                    Some(format!(
                                        "plugin:{}:{}:{}",
                                        route.plugin_id,
                                        route.provider_id,
                                        remote.source.source_id
                                    )),
                                )
                            }
                            None => {
                                tracing::warn!(
                                    plugin = %route.plugin_id,
                                    provider = %route.provider_id,
                                    "authenticated plugin 封面数据无效，回退其他封面来源"
                                );
                                (None, None)
                            }
                        },
                        None => (None, None),
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        plugin = %route.plugin_id,
                        provider = %route.provider_id,
                        %error,
                        "authenticated plugin 封面规划失败，回退其他封面来源"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        tracing::debug!(
            plugin = %route.plugin_id,
            provider = %route.provider_id,
            account = %route.account_id,
            score = scored.score,
            title = %metadata.title,
            artist = %metadata.artist,
            "采用 authenticated plugin 元数据"
        );
        Ok(Some(AuthenticatedPluginEnrichment {
            result: EnrichmentResult {
                metadata: Some(metadata),
                lyrics,
                artwork,
                artwork_key,
            },
            identity_track,
        }))
    }

    async fn resolve_online_artwork(
        &self,
        track: &Track,
        matched: &providers::ProviderMatch,
        candidates: &[providers::ProviderMatch],
    ) -> (Option<Vec<u8>>, Option<String>) {
        let mut cover_candidates = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .cover_url
                    .as_deref()
                    .is_some_and(|url| !url.trim().is_empty())
                    && cover_identity_matches(matched, candidate)
            })
            .cloned()
            .collect::<Vec<_>>();

        cover_candidates.sort_by(|left, right| {
            cover_candidate_rank(track, matched, right)
                .cmp(&cover_candidate_rank(track, matched, left))
        });

        for candidate in cover_candidates {
            let provider = candidate.provider;
            match providers::download_cover(&self.client, candidate.cover_url.as_deref()).await {
                Ok(Some(bytes)) => {
                    if let Some(bytes) = validate_cover_bytes(bytes).await {
                        tracing::debug!(
                            provider = provider.name(),
                            source_id = %candidate.source_id,
                            "采用匹配身份的在线封面"
                        );
                        return (
                            Some(bytes),
                            Some(format!("{}:{}", provider.key(), candidate.source_id)),
                        );
                    }
                    tracing::debug!(
                        provider = provider.name(),
                        source_id = %candidate.source_id,
                        "在线封面数据无效，尝试下一个来源"
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(
                        provider = provider.name(),
                        source_id = %candidate.source_id,
                        %error,
                        "在线封面读取失败，尝试下一个来源"
                    );
                }
            }
        }

        let mut identity_track = track.clone();
        identity_track.title.clone_from(&matched.title);
        identity_track.artist.clone_from(&matched.artist);
        identity_track.album.clone_from(&matched.album);
        if let Some(year) = matched
            .release_date
            .as_deref()
            .and_then(|value| value.get(..4))
            .and_then(|value| value.parse::<i32>().ok())
        {
            identity_track.year = Some(year);
        }

        match self.search_recording(&identity_track).await {
            Ok(Some(musicbrainz))
                if albums_compatible(&matched.album, &musicbrainz.album)
                    && musicbrainz.release_mbid.is_some() =>
            {
                let release_mbid = musicbrainz.release_mbid.as_deref().unwrap_or_default();
                match self.fetch_cover(release_mbid).await {
                    Ok(Some(bytes)) => {
                        if let Some(bytes) = validate_cover_bytes(bytes).await {
                            tracing::debug!(
                                release_mbid,
                                "音乐平台无可用封面，使用匹配的 Cover Art Archive 封面"
                            );
                            return (Some(bytes), Some(format!("musicbrainz:{release_mbid}")));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(%error, release_mbid, "Cover Art Archive 封面回退失败");
                    }
                }
            }
            Ok(Some(musicbrainz)) => {
                tracing::debug!(
                    provider_album = %matched.album,
                    musicbrainz_album = %musicbrainz.album,
                    "MusicBrainz 专辑身份不一致，拒绝封面回退"
                );
            }
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "MusicBrainz 封面身份搜索失败");
            }
        }

        (None, None)
    }

    async fn resolve_musicbrainz_artwork(
        &self,
        identity_track: &Track,
    ) -> (Option<Vec<u8>>, Option<String>) {
        match self.search_recording(identity_track).await {
            Ok(Some(metadata)) => {
                let Some(release_mbid) = metadata.release_mbid.as_deref() else {
                    return (None, None);
                };
                match self.fetch_cover(release_mbid).await {
                    Ok(Some(bytes)) => {
                        if let Some(bytes) = validate_cover_bytes(bytes).await {
                            return (Some(bytes), Some(format!("musicbrainz:{release_mbid}")));
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::debug!(%error, release_mbid, "authenticated identity CAA 封面回退失败");
                    }
                }
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(%error, "authenticated identity MusicBrainz 封面搜索失败"),
        }
        (None, None)
    }

    /// Upgrade an already-cached monolingual lyric without refetching metadata or artwork.
    /// This remains entirely on the async enrichment path and never blocks GPUI.
    pub(crate) async fn fetch_translated_lyrics_for_track(
        &self,
        track: &Track,
    ) -> Option<LyricsDocument> {
        self.fetch_translated_lyrics_fallback(track, None).await
    }

    /// Upgrade an already-cached line-only lyric without changing metadata or artwork. This is
    /// intentionally separate from the normal metadata winner so a QQ/Spotify identity can still
    /// consume authored Netease YRC timing.
    pub(crate) async fn fetch_word_timed_lyrics_for_track(
        &self,
        track: &Track,
    ) -> Option<LyricsDocument> {
        self.fetch_word_timed_lyrics_fallback(track, None).await
    }

    async fn fetch_word_timed_lyrics_fallback(
        &self,
        track: &Track,
        exclude: Option<providers::ProviderKind>,
    ) -> Option<LyricsDocument> {
        for provider in [
            providers::ProviderKind::Netease,
            providers::ProviderKind::QqMusic,
        ] {
            if exclude == Some(provider) {
                continue;
            }

            let matched = match providers::search(
                &self.client,
                provider,
                track,
                &self.netease_base_url,
                None,
            )
            .await
            {
                Ok(Some(matched)) => matched,
                Ok(None) => continue,
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "逐字歌词候选搜索失败");
                    continue;
                }
            };

            match providers::lyrics(&self.client, &matched).await {
                Ok(Some(lyrics)) if lyrics_have_word_timing(&lyrics) => {
                    tracing::debug!(provider = provider.name(), "使用备用平台补全 authored 逐字时间");
                    return Some(lyrics);
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "逐字歌词候选读取失败");
                }
            }
        }
        None
    }

    async fn fetch_translated_lyrics_fallback(
        &self,
        track: &Track,
        exclude: Option<providers::ProviderKind>,
    ) -> Option<LyricsDocument> {
        for provider in [
            providers::ProviderKind::Netease,
            providers::ProviderKind::QqMusic,
        ] {
            if exclude == Some(provider) {
                continue;
            }

            let matched = match providers::search(
                &self.client,
                provider,
                track,
                &self.netease_base_url,
                None,
            )
            .await
            {
                Ok(Some(matched)) => matched,
                Ok(None) => continue,
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "中文字幕候选搜索失败");
                    continue;
                }
            };

            match providers::lyrics(&self.client, &matched).await {
                Ok(Some(lyrics)) if lyrics.has_translation() => {
                    tracing::debug!(provider = provider.name(), "使用备用平台补全同步中文字幕");
                    return Some(lyrics);
                }
                Ok(Some(_)) | Ok(None) => {}
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "中文字幕候选歌词读取失败");
                }
            }
        }
        None
    }

    async fn spotify_token(&self) -> Option<String> {
        let (Some(client_id), Some(client_secret)) = (
            self.spotify_client_id.as_deref(),
            self.spotify_client_secret.as_deref(),
        ) else {
            return None;
        };
        self.client
            .post("https://accounts.spotify.com/api/token")
            .timeout(Duration::from_secs(8))
            .basic_auth(client_id, Some(client_secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json::<SpotifyTokenResponse>()
            .await
            .ok()
            .map(|response| response.access_token)
    }
}

fn local_track_query(track: &Track) -> TrackQuery {
    let artists = if metadata_unknown_artist(&track.artist) {
        Vec::new()
    } else {
        vec![track.artist.clone()]
    };
    TrackQuery {
        title: track.title.clone(),
        artists,
        album: track.album.clone(),
        duration_ms: Some(track.duration_ms),
        isrc: None,
        musicbrainz_recording_id: None,
        fingerprint_id: None,
    }
}

fn plugin_remote_track_is_bounded(track: &RemoteTrack) -> bool {
    if track.artists.len() > MAX_PLUGIN_TRACK_ARTISTS {
        return false;
    }
    let mut bytes = track
        .source
        .provider_id
        .len()
        .saturating_add(track.source.source_id.len())
        .saturating_add(track.title.len())
        .saturating_add(track.album.len())
        .saturating_add(track.cover_url.as_ref().map_or(0, String::len));
    for artist in &track.artists {
        bytes = bytes.saturating_add(artist.len());
    }
    bytes <= MAX_PLUGIN_TRACK_TEXT_BYTES
}

fn lyrics_have_word_timing(lyrics: &LyricsDocument) -> bool {
    lyrics
        .timed_lines()
        .iter()
        .any(|line| !line.words.is_empty())
}

fn lyric_quality(lyrics: &LyricsDocument) -> u8 {
    u8::from(lyrics_have_word_timing(lyrics)) * 4
        + u8::from(lyrics.has_translation()) * 2
        + u8::from(!lyrics.timed_lines().is_empty())
}

fn attach_raw_translation(
    target: LyricsDocument,
    translation: Option<String>,
) -> LyricsDocument {
    if target.has_translation() {
        return target;
    }
    let Some(translation) = translation.filter(|value| !value.trim().is_empty()) else {
        return target;
    };
    LyricsDocument::from_sources(
        target.plain.clone(),
        target.synced.clone(),
        Some(translation),
        target.source.clone(),
    )
}

fn prefer_richer_lyrics(primary: LyricsDocument, candidate: LyricsDocument) -> LyricsDocument {
    let primary_translation = primary.translation.clone();
    let candidate_translation = candidate.translation.clone();
    let primary = attach_raw_translation(primary, candidate_translation);
    let candidate = attach_raw_translation(candidate, primary_translation);
    if lyric_quality(&candidate) > lyric_quality(&primary) {
        candidate
    } else {
        primary
    }
}

async fn validate_cover_bytes(bytes: Vec<u8>) -> Option<Vec<u8>> {
    if bytes.len() < MIN_COVER_BYTES {
        return None;
    }
    tokio::task::spawn_blocking(move || {
        let image = image::load_from_memory(&bytes).ok()?;
        let (width, height) = image.dimensions();
        (width >= MIN_COVER_DIMENSION && height >= MIN_COVER_DIMENSION).then_some(bytes)
    })
    .await
    .ok()
    .flatten()
}

fn cover_candidate_rank(
    track: &Track,
    matched: &providers::ProviderMatch,
    candidate: &providers::ProviderMatch,
) -> i32 {
    let album_score = if metadata_unknown(&track.album) {
        identity_similarity(&matched.album, &candidate.album) / 10
    } else {
        identity_similarity(&track.album, &candidate.album) / 5
    };
    let selected_provider_bonus = i32::from(candidate.provider == matched.provider) * 6;
    let provider_quality = match candidate.provider {
        providers::ProviderKind::Spotify => 10,
        providers::ProviderKind::QqMusic => 9,
        providers::ProviderKind::Netease => 8,
        providers::ProviderKind::Migu => 6,
        providers::ProviderKind::Kugou => 5,
        providers::ProviderKind::Qianqian => 4,
    };
    candidate.score + album_score + selected_provider_bonus + provider_quality
}

fn cover_identity_matches(
    matched: &providers::ProviderMatch,
    candidate: &providers::ProviderMatch,
) -> bool {
    if cover_version_flags(&matched.title) != cover_version_flags(&candidate.title) {
        return false;
    }
    if identity_similarity(&matched.title, &candidate.title) < COVER_TITLE_SIMILARITY {
        return false;
    }
    if identity_similarity(&matched.artist, &candidate.artist) < COVER_ARTIST_SIMILARITY {
        return false;
    }
    if let (Some(left), Some(right)) = (matched.duration_ms, candidate.duration_ms)
        && left.abs_diff(right) > COVER_DURATION_TOLERANCE_MS
    {
        return false;
    }
    albums_compatible(&matched.album, &candidate.album)
}

fn albums_compatible(expected: &str, actual: &str) -> bool {
    metadata_unknown(expected)
        || metadata_unknown(actual)
        || identity_similarity(expected, actual) >= COVER_ALBUM_SIMILARITY
}

fn cover_version_flags(value: &str) -> u32 {
    let normalized = normalize_identity(value);
    let mut flags = 0u32;
    for (index, &term) in COVER_VERSION_TERMS.iter().enumerate() {
        if normalized.contains(term) {
            flags |= 1u32 << index;
        }
    }
    flags
}

fn identity_similarity(left: &str, right: &str) -> i32 {
    let left = normalize_identity(left);
    let right = normalize_identity(right);
    if left.is_empty() || right.is_empty() {
        return 0;
    }
    if left == right {
        return 100;
    }

    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    let max_len = left_chars.len().max(right_chars.len());
    if max_len == 0 {
        return 100;
    }

    let shorter = left_chars.len().min(right_chars.len());
    let containment = if left.contains(&right) || right.contains(&left) {
        70 + ((shorter * 25) / max_len) as i32
    } else {
        0
    };

    let mut previous = (0..=right_chars.len()).collect::<Vec<_>>();
    let mut current = vec![0; right_chars.len() + 1];
    for (left_index, left_char) in left_chars.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right_chars.iter().enumerate() {
            let substitution = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    let distance = previous[right_chars.len()];
    let edit = (((max_len - distance) * 100) / max_len) as i32;
    containment.max(edit)
}

fn normalize_identity(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn metadata_unknown(value: &str) -> bool {
    matches!(
        normalize_identity(value).as_str(),
        "" | "未知专辑" | "unknownalbum" | "unknown"
    )
}

fn metadata_unknown_artist(value: &str) -> bool {
    matches!(
        normalize_identity(value).as_str(),
        "" | "未知艺术家" | "unknownartist" | "unknown"
    )
}
