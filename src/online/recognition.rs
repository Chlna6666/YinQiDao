use anyhow::Result;
use image::GenericImageView;

use crate::{
    audio::{AudioFingerprint, CHROMAPRINT_ALGORITHM},
    model::Track,
    plugin::{
        abi::{RecognitionRequest, RemoteTrack, RoutingPolicy},
        frontend::{self as plugin_frontend, plugin_lyrics_to_player},
    },
};

use super::{EnrichmentResult, MetadataMatch, OnlineServices};

const MIN_PLUGIN_RECOGNITION_CONFIDENCE: f32 = 0.70;
const MAX_RECOGNITION_DURATION_DELTA_MS: u64 = 15_000;
const MIN_COVER_BYTES: usize = 512;
const MIN_COVER_DIMENSION: u32 = 64;

impl OnlineServices {
    /// Cheap planning probe. This never invokes guest code and lets the caller avoid decoding audio
    /// solely for plugins when no authenticated Recognition route is currently eligible.
    pub(super) fn authenticated_plugin_recognition_available(&self) -> bool {
        let Some(frontend) = plugin_frontend::global() else {
            return false;
        };
        match frontend.has_authenticated_recognition_route(&RoutingPolicy::default()) {
            Ok(available) => available,
            Err(error) => {
                tracing::debug!(%error, "authenticated plugin Recognition 路由规划失败");
                false
            }
        }
    }

    /// Resolve an audio fingerprint through authenticated plugins before AcoustID/local fallback.
    /// The same Host-computed Chromaprint payload can subsequently be reused by AcoustID, so this
    /// path never asks the decoder to process the track a second time.
    pub(super) async fn enrich_from_authenticated_recognition(
        &self,
        track: &Track,
        fingerprint: &AudioFingerprint,
        fetch_lyrics: bool,
        fetch_artwork: bool,
    ) -> Result<Option<EnrichmentResult>> {
        let Some(frontend) = plugin_frontend::global() else {
            return Ok(None);
        };
        let request = RecognitionRequest {
            algorithm: CHROMAPRINT_ALGORITHM.into(),
            fingerprint: fingerprint.compressed.clone(),
            duration_ms: track.duration_ms,
        };
        let recognized = match frontend
            .recognize(&request, &RoutingPolicy::default())
            .await
        {
            Ok(recognized) => recognized,
            Err(error) => {
                tracing::debug!(%error, "authenticated plugin Recognition 调用失败，继续识曲兜底链");
                return Ok(None);
            }
        };
        if !recognized.client_ready {
            return Ok(None);
        }
        for failure in &recognized.failures {
            tracing::debug!(
                plugin = %failure.route.plugin_id,
                provider = %failure.route.provider_id,
                account = %failure.route.account_id,
                error = %failure.error,
                "authenticated plugin Recognition route 失败，尝试下一账号"
            );
        }
        let (Some(recognition), Some(route)) = (recognized.value, recognized.route) else {
            return Ok(None);
        };
        if recognition
            .confidence
            .is_some_and(|confidence| confidence < MIN_PLUGIN_RECOGNITION_CONFIDENCE)
        {
            tracing::debug!(
                plugin = %route.plugin_id,
                provider = %route.provider_id,
                confidence = recognition.confidence.unwrap_or_default(),
                "authenticated plugin Recognition 置信度不足，继续识曲兜底链"
            );
            return Ok(None);
        }

        let remote = recognition.track;
        if !recognition_duration_is_compatible(track, &remote) {
            tracing::warn!(
                plugin = %route.plugin_id,
                provider = %route.provider_id,
                local_duration_ms = track.duration_ms,
                remote_duration_ms = remote.duration_ms.unwrap_or_default(),
                "authenticated plugin Recognition 时长明显不匹配，已拒绝"
            );
            return Ok(None);
        }

        let artist = if remote.artists.is_empty() {
            track.artist.clone()
        } else {
            remote.artists.join(" / ")
        };
        let album = if remote.album.trim().is_empty() {
            track.album.clone()
        } else {
            remote.album.clone()
        };
        let metadata = MetadataMatch {
            title: remote.title.clone(),
            artist,
            album,
            recording_mbid: format!(
                "plugin-recognition:{}:{}:{}",
                route.plugin_id, route.provider_id, remote.source.source_id
            ),
            release_mbid: None,
            source: Some(format!("插件识曲 · {}", route.provider_id)),
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
                            "识曲后 authenticated plugin 歌词调用失败，尝试下一账号"
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
                                    "识曲后插件歌词结构非法，回退其他歌词来源"
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
                        "识曲后 authenticated plugin 歌词规划失败"
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
                .is_none_or(|lyrics| !lyrics.timed_lines().iter().any(|line| !line.words.is_empty()))
                && let Some(word_timed) = self
                    .fetch_word_timed_lyrics_for_track(&identity_track)
                    .await
            {
                lyrics = Some(word_timed);
            }
            if lyrics
                .as_ref()
                .is_none_or(|lyrics| !lyrics.has_translation())
                && let Some(translated) = self
                    .fetch_translated_lyrics_for_track(&identity_track)
                    .await
            {
                if lyrics.is_none() {
                    lyrics = Some(translated);
                }
            }
            if lyrics.is_none() {
                lyrics = self.fetch_lyrics(Some(&metadata), &identity_track).await?;
            }
        }

        let (mut artwork, mut artwork_key) = if fetch_artwork {
            match frontend.artwork_for_route(&route, &remote.source).await {
                Ok(result) => {
                    for failure in &result.failures {
                        tracing::debug!(
                            plugin = %failure.route.plugin_id,
                            provider = %failure.route.provider_id,
                            account = %failure.route.account_id,
                            error = %failure.error,
                            "识曲后 authenticated plugin 封面调用失败，尝试下一账号"
                        );
                    }
                    match result.value {
                        Some(bytes) => match validate_recognition_cover_bytes(bytes).await {
                            Some(bytes) => (
                                Some(bytes),
                                Some(format!(
                                    "plugin-recognition:{}:{}:{}",
                                    route.plugin_id, route.provider_id, remote.source.source_id
                                )),
                            ),
                            None => {
                                tracing::warn!(
                                    plugin = %route.plugin_id,
                                    provider = %route.provider_id,
                                    "识曲后插件封面数据无效，回退 MusicBrainz/CAA"
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
                        "识曲后 authenticated plugin 封面规划失败"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        if fetch_artwork
            && artwork.is_none()
            && let Ok(Some(musicbrainz)) = self.search_recording(&identity_track).await
            && let Some(release_mbid) = musicbrainz.release_mbid.as_deref()
            && let Ok(Some(bytes)) = self.fetch_cover(release_mbid).await
            && let Some(bytes) = validate_recognition_cover_bytes(bytes).await
        {
            artwork = Some(bytes);
            artwork_key = Some(format!("caa:{release_mbid}"));
        }

        tracing::debug!(
            plugin = %route.plugin_id,
            provider = %route.provider_id,
            account = %route.account_id,
            confidence = recognition.confidence.unwrap_or_default(),
            title = %metadata.title,
            artist = %metadata.artist,
            "采用 authenticated plugin 音频识曲结果"
        );

        Ok(Some(EnrichmentResult {
            metadata: Some(metadata),
            lyrics,
            artwork,
            artwork_key,
        }))
    }
}

fn recognition_duration_is_compatible(local: &Track, remote: &RemoteTrack) -> bool {
    remote.duration_ms.is_none_or(|duration_ms| {
        local.duration_ms == 0
            || local.duration_ms.abs_diff(duration_ms) <= MAX_RECOGNITION_DURATION_DELTA_MS
    })
}

async fn validate_recognition_cover_bytes(bytes: Vec<u8>) -> Option<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        model::TrackData,
        plugin::abi::SourceTrackRef,
    };

    fn local_track(duration_ms: u64) -> Track {
        Track::new(TrackData {
            id: 1,
            path: PathBuf::from("track.flac"),
            title: "Track".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            year: None,
            genre: None,
            duration_ms,
            codec: "flac".into(),
            sample_rate: 48_000,
            channels: 2,
            artwork_key: None,
        })
    }

    #[test]
    fn recognition_duration_rejects_obvious_wrong_song() {
        let track = local_track(180_000);
        let close = RemoteTrack {
            source: SourceTrackRef {
                provider_id: "qqmusic".into(),
                source_id: "a".into(),
            },
            title: "Track".into(),
            duration_ms: Some(186_000),
            ..RemoteTrack::default()
        };
        let far = RemoteTrack {
            duration_ms: Some(220_000),
            ..close.clone()
        };
        assert!(recognition_duration_is_compatible(&track, &close));
        assert!(!recognition_duration_is_compatible(&track, &far));
    }
}
