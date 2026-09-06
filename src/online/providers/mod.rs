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

const CANDIDATE_FLOOR_SCORE: i32 = 35;
const AUTO_MATCH_SCORE: i32 = 80;
const STRONG_AUTO_MATCH_SCORE: i32 = 92;
const AUTO_MATCH_GAP: i32 = 8;

const VERSION_LIVE: u16 = 1 << 0;
const VERSION_REMIX: u16 = 1 << 1;
const VERSION_REMASTER: u16 = 1 << 2;
const VERSION_ACOUSTIC: u16 = 1 << 3;
const VERSION_INSTRUMENTAL: u16 = 1 << 4;
const VERSION_KARAOKE: u16 = 1 << 5;
const VERSION_SPEED: u16 = 1 << 6;
const VERSION_DEMO: u16 = 1 << 7;
const VERSION_EDIT: u16 = 1 << 8;
const VERSION_MONO: u16 = 1 << 9;
const VERSION_STEREO: u16 = 1 << 10;
const VERSION_COVER: u16 = 1 << 11;

const VERSION_TERMS: &[(&str, u16)] = &[
    ("live", VERSION_LIVE),
    ("现场", VERSION_LIVE),
    ("演唱会", VERSION_LIVE),
    ("concert", VERSION_LIVE),
    ("remix", VERSION_REMIX),
    ("mix", VERSION_REMIX),
    ("混音", VERSION_REMIX),
    ("remaster", VERSION_REMASTER),
    ("重制", VERSION_REMASTER),
    ("acoustic", VERSION_ACOUSTIC),
    ("unplugged", VERSION_ACOUSTIC),
    ("不插电", VERSION_ACOUSTIC),
    ("instrumental", VERSION_INSTRUMENTAL),
    ("伴奏", VERSION_INSTRUMENTAL),
    ("纯音乐", VERSION_INSTRUMENTAL),
    ("karaoke", VERSION_KARAOKE),
    ("卡拉ok", VERSION_KARAOKE),
    ("spedup", VERSION_SPEED),
    ("slowed", VERSION_SPEED),
    ("加速", VERSION_SPEED),
    ("慢速", VERSION_SPEED),
    ("demo", VERSION_DEMO),
    ("试听", VERSION_DEMO),
    ("radioedit", VERSION_EDIT),
    ("edit", VERSION_EDIT),
    ("剪辑版", VERSION_EDIT),
    ("mono", VERSION_MONO),
    ("单声道", VERSION_MONO),
    ("stereo", VERSION_STEREO),
    ("立体声", VERSION_STEREO),
    ("cover", VERSION_COVER),
    ("翻唱", VERSION_COVER),
];

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

/// Provider-local filtering is deliberately permissive. Cross-provider confidence and ambiguity
/// are decided only after all providers have had a chance to contribute a candidate.
fn choose_best(track: &Track, mut candidates: Vec<ProviderMatch>) -> Option<ProviderMatch> {
    candidates.iter_mut().for_each(|candidate| {
        candidate.score = match_score(track, candidate);
    });
    candidates
        .into_iter()
        .max_by_key(|candidate| candidate.score)
        .filter(|candidate| candidate.score >= CANDIDATE_FLOOR_SCORE)
}

/// Rank the best candidate from each provider as one candidate set. A result is accepted when it
/// is strong enough and either clearly leads the runner-up or multiple providers independently
/// agree that they refer to the same recording/version.
pub(super) fn choose_global_best(
    track: &Track,
    mut candidates: Vec<ProviderMatch>,
) -> Option<ProviderMatch> {
    for candidate in &mut candidates {
        candidate.score = match_score(track, candidate);
    }
    candidates.sort_by(|left, right| right.score.cmp(&left.score));

    let best = candidates.first()?.clone();
    if best.score < AUTO_MATCH_SCORE {
        return None;
    }

    if let Some(second) = candidates.get(1) {
        let gap = best.score.saturating_sub(second.score);
        let providers_agree = same_recording_identity(&best, second);
        if !providers_agree && best.score < STRONG_AUTO_MATCH_SCORE && gap < AUTO_MATCH_GAP {
            tracing::debug!(
                best_provider = best.provider.name(),
                best_score = best.score,
                second_provider = second.provider.name(),
                second_score = second.score,
                gap,
                "在线候选过于接近，拒绝自动绑定"
            );
            return None;
        }
    }

    Some(best)
}

fn match_score(track: &Track, candidate: &ProviderMatch) -> i32 {
    let local_title = normalized_title_base(&track.title);
    let remote_title = normalized_title_base(&candidate.title);
    let title_similarity = normalized_similarity(&local_title, &remote_title);
    if title_similarity < 40 {
        return 0;
    }

    let title_score = title_similarity * 40 / 100;
    let artist_score = if is_unknown_artist(&track.artist) {
        18
    } else {
        artist_similarity(&track.artist, &candidate.artist) * 25 / 100
    };
    let album_score = if is_unknown_album(&track.album) {
        6
    } else if is_unknown_album(&candidate.album) {
        0
    } else {
        text_similarity(&track.album, &candidate.album) * 10 / 100
    };
    let duration_score = candidate.duration_ms.map_or(0, |duration| {
        let delta = duration.abs_diff(track.duration_ms);
        if delta <= 1_500 {
            20
        } else if delta <= 3_000 {
            16
        } else if delta <= 6_000 {
            8
        } else if delta <= 10_000 {
            0
        } else if delta <= 15_000 {
            -10
        } else {
            -25
        }
    });
    let year_score = match (
        track.year,
        candidate.release_date.as_deref().and_then(parse_year),
    ) {
        (Some(local), Some(remote)) if local == remote => 5,
        (Some(local), Some(remote)) if local.abs_diff(remote) <= 1 => 2,
        (Some(_), Some(_)) => -3,
        (None, _) => 2,
        _ => 0,
    };
    let version_score = version_compatibility_score(&track.title, &candidate.title);

    (title_score + artist_score + album_score + duration_score + year_score + version_score)
        .clamp(0, 100)
}

fn version_compatibility_score(expected: &str, actual: &str) -> i32 {
    let expected = version_flags(expected);
    let actual = version_flags(actual);
    if expected == actual {
        return if expected == 0 { 0 } else { 4 };
    }
    match (expected == 0, actual == 0) {
        (true, false) => -28,
        (false, true) => -18,
        (false, false) => -32,
        (true, true) => 0,
    }
}

fn version_flags(value: &str) -> u16 {
    let normalized = normalize_text(value);
    let mut flags = 0;
    for &(term, flag) in VERSION_TERMS {
        if normalized.contains(term) {
            flags |= flag;
        }
    }
    flags
}

fn normalized_title_base(value: &str) -> String {
    let mut normalized = normalize_text(value);
    for &(term, _) in VERSION_TERMS {
        normalized = normalized.replace(term, "");
    }
    normalized
}

fn same_recording_identity(left: &ProviderMatch, right: &ProviderMatch) -> bool {
    if version_flags(&left.title) != version_flags(&right.title) {
        return false;
    }
    if normalized_similarity(
        &normalized_title_base(&left.title),
        &normalized_title_base(&right.title),
    ) < 95
    {
        return false;
    }
    if artist_similarity(&left.artist, &right.artist) < 90 {
        return false;
    }
    if let (Some(left_duration), Some(right_duration)) = (left.duration_ms, right.duration_ms)
        && left_duration.abs_diff(right_duration) > 4_000
    {
        return false;
    }
    if !is_unknown_album(&left.album)
        && !is_unknown_album(&right.album)
        && text_similarity(&left.album, &right.album) < 60
    {
        return false;
    }
    true
}

fn artist_similarity(expected: &str, actual: &str) -> i32 {
    let expected_names = split_artist_names(expected);
    let actual_names = split_artist_names(actual);
    if expected_names.is_empty() || actual_names.is_empty() {
        return text_similarity(expected, actual);
    }
    if expected_names == actual_names {
        return 100;
    }

    let common = expected_names
        .iter()
        .filter(|name| actual_names.contains(name))
        .count();
    let set_score = ((2 * common * 100) / (expected_names.len() + actual_names.len())) as i32;
    set_score.max(text_similarity(expected, actual))
}

fn split_artist_names(value: &str) -> Vec<String> {
    let mut value = value.to_lowercase();
    for separator in [
        " featuring ", " feat. ", " feat ", " ft. ", " ft ", "、", "/", "&", "，", ",", ";",
        "；", " + ", " x ", " × ",
    ] {
        value = value.replace(separator, "|");
    }
    let mut names = value
        .split('|')
        .map(normalize_text)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn text_similarity(expected: &str, actual: &str) -> i32 {
    let expected = normalize_text(expected);
    let actual = normalize_text(actual);
    normalized_similarity(&expected, &actual)
}

fn normalized_similarity(expected: &str, actual: &str) -> i32 {
    if expected.is_empty() || actual.is_empty() {
        return 0;
    }
    if expected == actual {
        return 100;
    }

    let expected_len = expected.chars().count();
    let actual_len = actual.chars().count();
    let shorter = expected_len.min(actual_len);
    let longer = expected_len.max(actual_len).max(1);
    let containment = if expected.contains(actual) || actual.contains(expected) {
        70 + ((shorter * 25) / longer) as i32
    } else {
        0
    };

    containment.max(edit_similarity(expected, actual))
}

fn edit_similarity(expected: &str, actual: &str) -> i32 {
    let left = expected.chars().collect::<Vec<_>>();
    let right = actual.chars().collect::<Vec<_>>();
    let max_len = left.len().max(right.len());
    if max_len == 0 {
        return 100;
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = usize::from(left_char != right_char);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    let distance = previous[right.len()];
    (((max_len - distance) * 100) / max_len) as i32
}

fn normalize_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_unknown_artist(value: &str) -> bool {
    matches!(
        normalize_text(value).as_str(),
        "" | "未知艺术家" | "unknownartist" | "unknown"
    )
}

fn is_unknown_album(value: &str) -> bool {
    matches!(
        normalize_text(value).as_str(),
        "" | "未知专辑" | "unknownalbum" | "unknown"
    )
}

fn parse_year(value: &str) -> Option<i32> {
    value.get(..4)?.parse().ok()
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

    fn track(title: &str, artist: &str, album: &str, duration_ms: u64) -> Track {
        Track {
            id: 1,
            path: "song.mp3".into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            year: Some(2020),
            genre: None,
            duration_ms,
            codec: "MP3".into(),
            sample_rate: 44_100,
            channels: 2,
            artwork_key: None,
        }
    }

    fn candidate(
        provider: ProviderKind,
        title: &str,
        artist: &str,
        album: &str,
        duration_ms: u64,
    ) -> ProviderMatch {
        ProviderMatch {
            provider,
            source_id: provider.key().into(),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration_ms: Some(duration_ms),
            release_date: Some("2020-01-01".into()),
            cover_url: None,
            lyric_url: None,
            score: 0,
        }
    }

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
    fn exact_metadata_beats_wrong_version() {
        let track = track("晴天", "周杰伦", "叶惠美", 269_000);
        let exact = candidate(
            ProviderKind::QqMusic,
            "晴天",
            "周杰伦",
            "叶惠美",
            269_300,
        );
        let live = candidate(
            ProviderKind::Netease,
            "晴天 (Live)",
            "周杰伦",
            "演唱会现场",
            269_100,
        );
        assert!(match_score(&track, &exact) > match_score(&track, &live) + 15);
    }

    #[test]
    fn artist_separators_are_treated_as_the_same_credit_set() {
        assert_eq!(artist_similarity("周杰伦、费玉清", "周杰伦 / 费玉清"), 100);
    }

    #[test]
    fn equivalent_cross_provider_candidates_are_not_marked_ambiguous() {
        let track = track("Hello World", "Artist", "Album", 180_000);
        let first = candidate(
            ProviderKind::Netease,
            "Hello World",
            "Artist",
            "Album",
            180_000,
        );
        let second = candidate(
            ProviderKind::QqMusic,
            "Hello World",
            "Artist",
            "Album",
            180_800,
        );
        assert!(choose_global_best(&track, vec![first, second]).is_some());
    }

    #[test]
    fn distant_metadata_is_rejected() {
        let track = track("Hello World", "Artist", "Album", 180_000);
        let wrong = candidate(
            ProviderKind::Netease,
            "Completely Different Song",
            "Another Singer",
            "Other Album",
            230_000,
        );
        assert!(choose_global_best(&track, vec![wrong]).is_none());
    }

    #[test]
    fn unix_millis_maps_to_iso_date() {
        assert_eq!(
            unix_millis_to_date(1_577_836_800_000).as_deref(),
            Some("2020-01-01")
        );
    }
}
