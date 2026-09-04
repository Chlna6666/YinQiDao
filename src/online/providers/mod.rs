use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::Client;

use crate::model::Track;

mod kugou;
mod migu;
mod netease;
mod qianqian;
mod qqmusic;
mod spotify;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderKind {
    #[default]
    Netease,
    QqMusic,
    Spotify,
    Migu,
    Qianqian,
    Kugou,
}

impl ProviderKind {
    pub const fn priority_order() -> [Self; 6] {
        [
            Self::Netease,
            Self::QqMusic,
            Self::Spotify,
            Self::Migu,
            Self::Qianqian,
            Self::Kugou,
        ]
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Netease => "网易云音乐",
            Self::QqMusic => "QQ音乐",
            Self::Spotify => "Spotify",
            Self::Migu => "咪咕音乐",
            Self::Qianqian => "千千音乐",
            Self::Kugou => "酷狗音乐",
        }
    }

    pub const fn key(self) -> &'static str {
        match self {
            Self::Netease => "netease",
            Self::QqMusic => "qqmusic",
            Self::Spotify => "spotify",
            Self::Migu => "migu",
            Self::Qianqian => "qianqian",
            Self::Kugou => "kugou",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderMatch {
    pub provider: ProviderKind,
    pub source_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub release_date: Option<String>,
    pub cover_url: Option<String>,
    pub lyric_url: Option<String>,
    pub score: i32,
}

pub async fn search(
    client: &Client,
    provider: ProviderKind,
    track: &Track,
    netease_base_url: &str,
    spotify_token: Option<&str>,
) -> Result<Option<ProviderMatch>> {
    let request = async {
        match provider {
            ProviderKind::Netease => netease::search(client, netease_base_url, track).await,
            ProviderKind::QqMusic => qqmusic::search(client, track).await,
            ProviderKind::Spotify => spotify::search(client, spotify_token, track).await,
            ProviderKind::Migu => migu::search(client, track).await,
            ProviderKind::Qianqian => qianqian::search(client, track).await,
            ProviderKind::Kugou => kugou::search(client, track).await,
        }
    };
    tokio::time::timeout(Duration::from_secs(8), request)
        .await
        .map_err(|_| anyhow!("{}搜索超时", provider.name()))?
}

pub async fn lyrics(
    client: &Client,
    matched: &ProviderMatch,
) -> Result<Option<crate::lyrics::LyricsDocument>> {
    let provider = matched.provider;
    let request = async {
        match provider {
            ProviderKind::Netease => netease::lyrics(client, matched).await,
            ProviderKind::QqMusic => qqmusic::lyrics(client, matched).await,
            ProviderKind::Spotify => Ok(None),
            ProviderKind::Migu => migu::lyrics(client, matched).await,
            ProviderKind::Qianqian => qianqian::lyrics(client, matched).await,
            ProviderKind::Kugou => kugou::lyrics(client, matched).await,
        }
    };
    tokio::time::timeout(Duration::from_secs(8), request)
        .await
        .map_err(|_| anyhow!("{}歌词请求超时", provider.name()))?
}

pub async fn download_cover(client: &Client, url: Option<&str>) -> Result<Option<Vec<u8>>> {
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return Ok(None);
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        let response = client.get(url).send().await?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > 10 * 1024 * 1024)
        {
            bail!("在线封面超过 10 MiB，已拒绝读取");
        }
        Ok(Some(response.bytes().await?.to_vec()))
    })
    .await
    .map_err(|_| anyhow!("在线封面请求超时"))?
}

fn choose_best(track: &Track, mut candidates: Vec<ProviderMatch>) -> Option<ProviderMatch> {
    candidates.iter_mut().for_each(|candidate| {
        candidate.score = match_score(
            track,
            &candidate.title,
            &candidate.artist,
            candidate.duration_ms,
        );
    });
    candidates
        .into_iter()
        .max_by_key(|candidate| candidate.score)
        .filter(|candidate| candidate.score >= 45)
}

fn match_score(track: &Track, title: &str, artist: &str, duration_ms: Option<u64>) -> i32 {
    let title_score = text_score(&track.title, title);
    let artist_score = if track.artist == "未知艺术家" {
        0
    } else {
        text_score(&track.artist, artist)
    };
    let duration_score = duration_ms.map_or(0, |duration| {
        let delta = duration.abs_diff(track.duration_ms);
        if delta <= 2_000 {
            20
        } else if delta <= 6_000 {
            10
        } else if delta <= 15_000 {
            -10
        } else {
            -30
        }
    });
    (title_score + artist_score + duration_score).clamp(0, 100)
}

fn text_score(expected: &str, actual: &str) -> i32 {
    let expected = normalize_text(expected);
    let actual = normalize_text(actual);
    if expected.is_empty() || actual.is_empty() {
        return 0;
    }
    if expected == actual {
        return 60;
    }
    if actual.contains(&expected) || expected.contains(&actual) {
        return 40;
    }
    let common = expected
        .chars()
        .filter(|character| actual.contains(*character))
        .count();
    ((common * 30) / expected.chars().count().max(1)) as i32
}

fn normalize_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn unix_millis_to_date(timestamp_ms: i64) -> Option<String> {
    if timestamp_ms <= 0 {
        return None;
    }
    let days = timestamp_ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_priority_puts_primary_providers_first() {
        assert_eq!(
            ProviderKind::priority_order()[..3],
            [
                ProviderKind::Netease,
                ProviderKind::QqMusic,
                ProviderKind::Spotify,
            ]
        );
    }

    #[test]
    fn match_score_prefers_exact_metadata_and_duration() {
        let track = Track {
            id: 1,
            path: "song.mp3".into(),
            title: "Hello World".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            year: None,
            genre: None,
            duration_ms: 180_000,
            codec: "MP3".into(),
            sample_rate: 44_100,
            channels: 2,
            artwork_key: None,
        };
        assert!(match_score(&track, "Hello World", "Artist", Some(180_000)) > 90);
        assert!(match_score(&track, "Other", "Different", Some(180_000)) < 60);
    }

    #[test]
    fn unix_millis_maps_to_iso_date() {
        assert_eq!(
            unix_millis_to_date(1_577_836_800_000).as_deref(),
            Some("2020-01-01")
        );
    }
}
