use anyhow::{Result, anyhow};
use gpui::Context;
use gpui_tokio::Tokio;

use crate::{
    lyrics::LyricsDocument,
    online::{EnrichmentResult, OnlineServices},
};

use super::shell::MusicApp;

struct EnrichmentOutcome {
    result: EnrichmentResult,
    artwork_key: Option<String>,
    artwork: Option<PreparedArtwork>,
    cached_lyrics: Option<LyricsDocument>,
}

struct PreparedArtwork {
    png: Vec<u8>,
    blurred_png: Option<Vec<u8>>,
    palette: Option<crate::artwork::ArtworkPalette>,
}

impl MusicApp {
    pub(crate) fn request_current_enrichment(&mut self, cx: &mut Context<Self>) {
        let Some(track) = self.snapshot.current_track.clone() else {
            return;
        };
        if self.enrichment_done.contains(&track.id) || !self.enrichment_loading.insert(track.id) {
            return;
        }
        let Some(library) = self.library.clone() else {
            self.enrichment_loading.remove(&track.id);
            return;
        };
        let cache = self.artwork_cache.clone();
        let metadata_enabled = self.config.online_metadata;
        let lyrics_enabled = self.config.online_lyrics;
        let api_key = self
            .config
            .acoustid_api_key
            .clone()
            .or_else(|| std::env::var("YINQIDAO_ACOUSTID_API_KEY").ok());
        let track_id = track.id;
        let task = Tokio::spawn_result(cx, async move {
            let lookup_library = library.clone();
            let lookup_track = track.clone();
            let stored =
                tokio::task::spawn_blocking(move || lookup_library.enrichment(&lookup_track))
                    .await
                    .map_err(|_| anyhow!("读取歌词缓存任务异常退出"))??;
            if stored.checked_online || (!metadata_enabled && !lyrics_enabled) {
                return Ok(EnrichmentOutcome {
                    result: EnrichmentResult::default(),
                    artwork_key: None,
                    artwork: None,
                    cached_lyrics: stored.lyrics,
                });
            }

            let services = OnlineServices::new(api_key)?;
            let mut result = services.enrich(&track, lyrics_enabled).await?;
            if !metadata_enabled {
                result.metadata = None;
            }
            let artwork_key = result.artwork_key.take().or_else(|| {
                result
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.release_mbid.as_ref())
                    .zip(result.artwork.as_ref())
                    .map(|(release, _)| format!("musicbrainz:{release}"))
            });
            let raw_artwork = result.artwork.take();
            let artwork = match (cache, artwork_key.clone(), raw_artwork) {
                (Some(cache), Some(key), Some(bytes)) => {
                    let artwork = tokio::task::spawn_blocking(move || cache.store(&key, &bytes))
                        .await
                        .map_err(|_| anyhow!("在线封面缓存任务异常退出"))??;
                    Some(PreparedArtwork {
                        png: artwork.png,
                        blurred_png: Some(artwork.blurred_png),
                        palette: Some(artwork.palette),
                    })
                }
                (_, _, Some(bytes)) => {
                    let artwork = tokio::task::spawn_blocking(move || {
                        let Ok(image) = image::load_from_memory(&bytes) else {
                            return PreparedArtwork {
                                png: bytes,
                                blurred_png: None,
                                palette: None,
                            };
                        };
                        let blurred_png = crate::artwork::generate_blurred_artwork(&image).ok();
                        let palette = Some(crate::artwork::extract_palette(&image));
                        PreparedArtwork {
                            png: bytes,
                            blurred_png,
                            palette,
                        }
                    })
                    .await
                    .map_err(|_| anyhow!("在线封面处理任务异常退出"))?;
                    Some(artwork)
                }
                _ => None,
            };
            let persist_library = library.clone();
            // 封面所有权只交给 PreparedArtwork；持久化结果仅复制元数据与歌词，避免复制 PNG。
            let persist_result = result.clone();
            let persist_key = artwork_key.clone();
            tokio::task::spawn_blocking(move || {
                persist_library.apply_enrichment(track_id, &persist_result, persist_key.as_deref())
            })
            .await
            .map_err(|_| anyhow!("保存联网识别结果任务异常退出"))??;
            Ok(EnrichmentOutcome {
                result,
                artwork_key,
                artwork,
                cached_lyrics: stored.lyrics,
            })
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                this.enrichment_loading.remove(&track_id);
                this.enrichment_done.insert(track_id);
                match outcome {
                    Ok(outcome) => {
                        if let Some(lyrics) = outcome.result.lyrics.or(outcome.cached_lyrics) {
                            this.cache_lyrics(track_id, lyrics);
                        }
                        if let Some(artwork) = outcome.artwork {
                            this.set_artwork_parts(
                                track_id,
                                artwork.png,
                                artwork.blurred_png,
                                artwork.palette,
                            );
                            this.artwork_missing.remove(&track_id);
                        }

                        // Enrichment only changes one logical track. Re-querying the entire library
                        // here used to make the GPUI callback clone every Track again before
                        // register_tracks, producing a visible hitch on large libraries while a song
                        // was already playing. Patch the one in-memory Track and refresh exactly one
                        // engine registry entry instead.
                        let mut updated_track = None;
                        if let Some(track) = this.tracks.iter_mut().find(|track| track.id == track_id)
                        {
                            if let Some(metadata) = outcome.result.metadata.as_ref() {
                                track.title.clone_from(&metadata.title);
                                track.artist.clone_from(&metadata.artist);
                                track.album.clone_from(&metadata.album);
                            }
                            if let Some(artwork_key) = outcome.artwork_key.as_ref() {
                                track.artwork_key = Some(artwork_key.clone());
                            }
                            updated_track = Some(track.clone());
                        }

                        if let Some(track) = updated_track {
                            if let Some(current) = this
                                .snapshot
                                .current_track
                                .as_mut()
                                .filter(|current| current.id == track_id)
                            {
                                current.clone_from(&track);
                            }
                            if let Some(engine) = &this.engine {
                                engine.register_tracks(std::iter::once(track));
                            }
                        }

                        if let Some(metadata) = outcome.result.metadata.as_ref() {
                            let source = metadata.source.as_deref().unwrap_or("在线服务");
                            this.status = format!("已从{source}更新歌曲信息");
                        } else if outcome.artwork_key.is_some() {
                            this.status = "在线封面已更新".into();
                        } else if this.lyrics.contains_key(&track_id) {
                            this.status = "歌词已就绪".into();
                        } else {
                            this.status = "未找到可靠的联网匹配或歌词".into();
                        }
                    }
                    Err(error) => this.status = format!("联网识别失败：{error:#}"),
                }
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    pub(crate) fn retry_current_enrichment(&mut self, cx: &mut Context<Self>) {
        if let Some(track) = &self.snapshot.current_track {
            self.enrichment_done.remove(&track.id);
        }
        self.status = "正在重新识别当前歌曲…".into();
        self.request_current_enrichment(cx);
        cx.notify();
    }

    pub(crate) fn toggle_online_metadata(&mut self, cx: &mut Context<Self>) {
        self.config.online_metadata = !self.config.online_metadata;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_online_lyrics(&mut self, cx: &mut Context<Self>) {
        self.config.online_lyrics = !self.config.online_lyrics;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn edit_acoustid_key(&mut self, cx: &mut Context<Self>) {
        self.acoustid_key_active = true;
        self.config.acoustid_api_key.get_or_insert_default();
        self.status = "请输入 AcoustID 应用 API key，按 Enter 保存，Esc 取消编辑".into();
        cx.notify();
    }

    pub(crate) fn update_acoustid_key(&mut self, key: &str, cx: &mut Context<Self>) {
        match key {
            "enter" => {
                self.acoustid_key_active = false;
                if self
                    .config
                    .acoustid_api_key
                    .as_ref()
                    .is_some_and(|key| key.trim().is_empty())
                {
                    self.config.acoustid_api_key = None;
                }
                self.save_config();
                self.status = "AcoustID 配置已保存".into();
            }
            "escape" => {
                self.acoustid_key_active = false;
                self.status = "已结束 AcoustID 密钥编辑".into();
            }
            "backspace" => {
                if let Some(value) = &mut self.config.acoustid_api_key {
                    value.pop();
                }
            }
            value if value.len() == 1 && !value.chars().next().is_some_and(char::is_control) => {
                self.config
                    .acoustid_api_key
                    .get_or_insert_default()
                    .push_str(value);
            }
            _ => {}
        }
        cx.notify();
    }
}
