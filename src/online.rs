use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::{audio::fingerprint_file, lyrics::LyricsDocument, model::Track};

mod provider_chain;
mod providers;

const USER_AGENT: &str = "YinQiDao/0.1.0 (https://github.com/Chlna6666)";

#[derive(Clone, Debug, Default)]
pub struct MetadataMatch {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub recording_mbid: String,
    pub release_mbid: Option<String>,
    pub source: Option<String>,
    pub release_date: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EnrichmentResult {
    pub metadata: Option<MetadataMatch>,
    pub lyrics: Option<LyricsDocument>,
    pub artwork: Option<Vec<u8>>,
    pub artwork_key: Option<String>,
}

#[derive(Clone)]
pub struct OnlineServices {
    client: Client,
    acoustid_api_key: Option<String>,
    musicbrainz_gate: Arc<Mutex<std::time::Instant>>,
    netease_base_url: String,
    spotify_client_id: Option<String>,
    spotify_client_secret: Option<String>,
}

impl OnlineServices {
    pub fn new(acoustid_api_key: Option<String>) -> Result<Self> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(25))
            .build()
            .context("创建联网元数据客户端失败")?;
        Ok(Self {
            client,
            acoustid_api_key: acoustid_api_key.filter(|key| !key.trim().is_empty()),
            musicbrainz_gate: Arc::new(Mutex::new(
                std::time::Instant::now() - Duration::from_secs(1),
            )),
            netease_base_url: std::env::var("YINQIDAO_NETEASE_API_BASE")
                .unwrap_or_else(|_| "https://api-music.imsyy.com".into()),
            spotify_client_id: std::env::var("YINQIDAO_SPOTIFY_CLIENT_ID").ok(),
            spotify_client_secret: std::env::var("YINQIDAO_SPOTIFY_CLIENT_SECRET").ok(),
        })
    }

    pub async fn enrich(&self, track: &Track, fetch_lyrics: bool) -> Result<EnrichmentResult> {
        if let Some(result) = self.enrich_from_providers(track, fetch_lyrics).await? {
            return Ok(result);
        }

        let recording_mbid = if let Some(key) = self.acoustid_api_key.clone() {
            let path = track.path.clone();
            let fingerprint = tokio::task::spawn_blocking(move || fingerprint_file(&path))
                .await
                .context("AcoustID 指纹任务异常退出")??;
            self.lookup_acoustid(&key, track.duration_ms, &fingerprint)
                .await?
        } else {
            None
        };

        let metadata = if let Some(recording_mbid) = recording_mbid {
            self.lookup_recording(&recording_mbid).await?
        } else {
            self.search_recording(track).await?
        };
        let identity = metadata.as_ref().map_or(track, |_| track);
        let lyrics = if fetch_lyrics {
            self.fetch_lyrics(metadata.as_ref(), identity).await?
        } else {
            None
        };
        let artwork = if let Some(release_mbid) = metadata
            .as_ref()
            .and_then(|metadata| metadata.release_mbid.as_deref())
        {
            self.fetch_cover(release_mbid).await?
        } else {
            None
        };
        Ok(EnrichmentResult {
            metadata,
            lyrics,
            artwork,
            artwork_key: None,
        })
    }

    async fn lookup_acoustid(
        &self,
        key: &str,
        duration_ms: u64,
        fingerprint: &str,
    ) -> Result<Option<String>> {
        let response = self
            .client
            .post("https://api.acoustid.org/v2/lookup")
            .form(&[
                ("client", key),
                ("format", "json"),
                ("meta", "recordingids"),
                ("duration", &(duration_ms / 1_000).max(1).to_string()),
                ("fingerprint", fingerprint),
            ])
            .send()
            .await
            .context("请求 AcoustID 失败")?
            .error_for_status()
            .context("AcoustID 返回错误状态")?
            .json::<AcoustIdResponse>()
            .await
            .context("解析 AcoustID 响应失败")?;
        if response.status != "ok" {
            bail!("AcoustID 识别失败");
        }
        Ok(response
            .results
            .into_iter()
            .filter(|result| result.score >= 0.7)
            .flat_map(|result| result.recordings)
            .map(|recording| recording.id)
            .next())
    }

    async fn search_recording(&self, track: &Track) -> Result<Option<MetadataMatch>> {
        let title = escape_query(&track.title);
        let mut query = format!("recording:\"{title}\"");
        if track.artist != "未知艺术家" {
            query.push_str(&format!(" AND artist:\"{}\"", escape_query(&track.artist)));
        }
        self.wait_for_musicbrainz().await;
        let response = self
            .client
            .get("https://musicbrainz.org/ws/2/recording")
            .query(&[
                ("query", query),
                ("fmt", "json".into()),
                ("limit", "5".into()),
            ])
            .send()
            .await
            .context("搜索 MusicBrainz 失败")?
            .error_for_status()
            .context("MusicBrainz 搜索返回错误状态")?
            .json::<RecordingSearchResponse>()
            .await
            .context("解析 MusicBrainz 搜索结果失败")?;
        Ok(response
            .recordings
            .into_iter()
            .find(|candidate| candidate.is_confident_match(track))
            .map(Recording::into_metadata))
    }

    async fn lookup_recording(&self, recording_mbid: &str) -> Result<Option<MetadataMatch>> {
        self.wait_for_musicbrainz().await;
        let url = format!("https://musicbrainz.org/ws/2/recording/{recording_mbid}");
        let response = self
            .client
            .get(url)
            .query(&[("inc", "artist-credits+releases"), ("fmt", "json")])
            .send()
            .await
            .context("查询 MusicBrainz 录音失败")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(
            response
                .error_for_status()
                .context("MusicBrainz 查询返回错误状态")?
                .json::<Recording>()
                .await
                .context("解析 MusicBrainz 录音信息失败")?
                .into_metadata(),
        ))
    }

    async fn fetch_lyrics(
        &self,
        metadata: Option<&MetadataMatch>,
        track: &Track,
    ) -> Result<Option<LyricsDocument>> {
        let title = metadata.map_or(track.title.as_str(), |value| value.title.as_str());
        let artist = metadata.map_or(track.artist.as_str(), |value| value.artist.as_str());
        if artist == "未知艺术家" || title.trim().is_empty() {
            return Ok(None);
        }
        let album = metadata.map_or(track.album.as_str(), |value| value.album.as_str());
        let duration_sec = (track.duration_ms / 1_000).clamp(1, 3_600);
        let duration_str = duration_sec.to_string();

        // 1. 尝试精准匹配
        let response = self
            .client
            .get("https://lrclib.net/api/get")
            .query(&[
                ("track_name", title),
                ("artist_name", artist),
                ("album_name", album),
                ("duration", duration_str.as_str()),
            ])
            .send()
            .await;

        if let Ok(res) = response
            && res.status().is_success()
            && let Ok(lyrics) = res.json::<LrcLibResponse>().await
        {
            return Ok(Some(lyrics_doc_from_response(lyrics)));
        }

        // 2. 精准匹配未命中时，自动回退到 LRCLIB 搜索接口 /api/search
        let search_query = format!("{title} {artist}");
        let search_res = self
            .client
            .get("https://lrclib.net/api/search")
            .query(&[("q", search_query.as_str())])
            .send()
            .await;

        if let Ok(res) = search_res
            && res.status().is_success()
            && let Ok(list) = res.json::<Vec<LrcLibResponse>>().await
        {
            // 优先选择包含同步歌词且时长偏差在 6 秒之内的候选项
            let target_sec = duration_sec as f64;
            let best = list
                .iter()
                .filter(|item| {
                    item.synced_lyrics.is_some() || item.plain_lyrics.is_some() || item.instrumental
                })
                .min_by(|a, b| {
                    let diff_a = a.duration.map_or(999.0, |d| (d - target_sec).abs());
                    let diff_b = b.duration.map_or(999.0, |d| (d - target_sec).abs());
                    diff_a
                        .partial_cmp(&diff_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

            if let Some(matched) = best {
                return Ok(Some(lyrics_doc_from_response(matched.clone())));
            }
        }

        Ok(None)
    }

    async fn fetch_cover(&self, release_mbid: &str) -> Result<Option<Vec<u8>>> {
        let url = format!("https://coverartarchive.org/release/{release_mbid}/front-500");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("请求在线封面失败")?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let bytes = response
            .error_for_status()
            .context("在线封面服务返回错误状态")?
            .bytes()
            .await
            .context("读取在线封面失败")?;
        Ok(Some(bytes.to_vec()))
    }

    async fn wait_for_musicbrainz(&self) {
        let mut previous = self.musicbrainz_gate.lock().await;
        let elapsed = previous.elapsed();
        if elapsed < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
        }
        *previous = std::time::Instant::now();
    }
}

fn escape_query(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Deserialize)]
struct AcoustIdResponse {
    status: String,
    #[serde(default)]
    results: Vec<AcoustIdResult>,
}

#[derive(Deserialize)]
struct AcoustIdResult {
    score: f32,
    #[serde(default)]
    recordings: Vec<AcoustIdRecording>,
}

#[derive(Deserialize)]
struct AcoustIdRecording {
    id: String,
}

#[derive(Deserialize)]
struct RecordingSearchResponse {
    #[serde(default)]
    recordings: Vec<Recording>,
}

#[derive(Deserialize)]
struct Recording {
    id: String,
    title: String,
    #[serde(default)]
    score: u8,
    length: Option<u64>,
    #[serde(rename = "artist-credit", default)]
    artist_credit: Vec<ArtistCredit>,
    #[serde(default)]
    releases: Vec<Release>,
}

impl Recording {
    fn is_confident_match(&self, track: &Track) -> bool {
        let duration_matches = self
            .length
            .is_none_or(|length| length.abs_diff(track.duration_ms) <= 5_000);
        self.score >= 90 && duration_matches
    }

    fn into_metadata(self) -> MetadataMatch {
        let release = self.releases.into_iter().next();
        MetadataMatch {
            title: self.title,
            artist: self
                .artist_credit
                .into_iter()
                .map(|credit| credit.name)
                .collect::<Vec<_>>()
                .join("、"),
            album: release
                .as_ref()
                .map_or_else(|| "未知专辑".into(), |release| release.title.clone()),
            recording_mbid: self.id,
            release_mbid: release.map(|release| release.id),
            source: Some("MusicBrainz".into()),
            release_date: None,
        }
    }
}

#[derive(Deserialize)]
struct ArtistCredit {
    name: String,
}

#[derive(Deserialize)]
struct Release {
    id: String,
    title: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibResponse {
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    duration: Option<f64>,
}

#[derive(Deserialize)]
struct SpotifyTokenResponse {
    access_token: String,
}

fn lyrics_doc_from_response(lyrics: LrcLibResponse) -> LyricsDocument {
    if lyrics.instrumental {
        LyricsDocument::from_sources(Some("纯音乐".into()), None, None, "LRCLIB")
    } else {
        LyricsDocument::from_sources(
            lyrics.plain_lyrics.filter(|value| !value.trim().is_empty()),
            lyrics
                .synced_lyrics
                .filter(|value| !value.trim().is_empty()),
            None,
            "LRCLIB",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musicbrainz_response_maps_artist_release_and_ids() {
        let response: RecordingSearchResponse = serde_json::from_str(
            r#"{"recordings":[{"id":"recording-id","title":"Song","score":99,"length":180000,"artist-credit":[{"name":"Artist"}],"releases":[{"id":"release-id","title":"Album"}]}]}"#,
        )
        .expect("response");
        let metadata = response
            .recordings
            .into_iter()
            .next()
            .unwrap()
            .into_metadata();
        assert_eq!(metadata.artist, "Artist");
        assert_eq!(metadata.album, "Album");
        assert_eq!(metadata.release_mbid.as_deref(), Some("release-id"));
    }
}
