use std::time::Duration;

use anyhow::Result;

use crate::{lyrics::LyricsDocument, model::Track};

use super::{EnrichmentResult, MetadataMatch, OnlineServices, SpotifyTokenResponse, providers};

impl OnlineServices {
    pub(super) async fn enrich_from_providers(
        &self,
        track: &Track,
        fetch_lyrics: bool,
    ) -> Result<Option<EnrichmentResult>> {
        let mut spotify_token = None;
        for provider in providers::ProviderKind::priority_order() {
            if provider == providers::ProviderKind::Spotify && spotify_token.is_none() {
                spotify_token = self.spotify_token().await;
            }
            let matched = match providers::search(
                &self.client,
                provider,
                track,
                &self.netease_base_url,
                spotify_token.as_deref(),
            )
            .await
            {
                Ok(matched) => matched,
                Err(error) => {
                    tracing::debug!(provider = provider.name(), %error, "音乐平台搜索失败，尝试下一个平台");
                    continue;
                }
            };
            let Some(matched) = matched else { continue };
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
                            provider = provider.name(),
                            %error,
                            "首选音乐平台歌词读取失败，尝试中文字幕兜底"
                        );
                        None
                    }
                };

                match provider_lyrics {
                    Some(lyrics) if lyrics.has_translation() => Some(lyrics),
                    Some(lyrics) => self
                        .fetch_translated_lyrics_fallback(track, Some(matched.provider))
                        .await
                        .or(Some(lyrics)),
                    None => {
                        if let Some(translated) = self
                            .fetch_translated_lyrics_fallback(track, Some(matched.provider))
                            .await
                        {
                            Some(translated)
                        } else {
                            self.fetch_lyrics(Some(&metadata), track).await?
                        }
                    }
                }
            } else {
                None
            };
            let artwork =
                match providers::download_cover(&self.client, matched.cover_url.as_deref()).await {
                    Ok(artwork) => artwork,
                    Err(error) => {
                        tracing::debug!(provider = provider.name(), %error, "音乐平台封面读取失败");
                        None
                    }
                };
            return Ok(Some(EnrichmentResult {
                metadata: Some(metadata),
                lyrics,
                artwork,
                artwork_key: Some(format!("{}:{}", matched.provider.key(), matched.source_id)),
            }));
        }
        Ok(None)
    }

    /// Upgrade an already-cached monolingual lyric without refetching metadata or artwork.
    /// This remains entirely on the async enrichment path and never blocks GPUI.
    pub(crate) async fn fetch_translated_lyrics_for_track(
        &self,
        track: &Track,
    ) -> Option<LyricsDocument> {
        self.fetch_translated_lyrics_fallback(track, None).await
    }

    /// Prefer a lyric document that carries a synchronized translation without changing the
    /// metadata provider selected for the track. Netease and QQ expose explicit translated lyric
    /// tracks; keep this lookup in the background enrichment path so it never blocks GPUI.
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
                    tracing::debug!(
                        provider = provider.name(),
                        "使用备用平台补全同步中文字幕"
                    );
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
