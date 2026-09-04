use std::{borrow::Cow, fs, path::Path, time::SystemTime};

use anyhow::{Context, Result};
use lofty::prelude::{Accessor, AudioFile, TaggedFileExt};

use crate::model::Track;

use super::normalize_path;

pub(super) fn read_track(path: &Path) -> Result<Track> {
    let tagged = lofty::read_from_path(path)
        .with_context(|| format!("读取音频元数据失败: {}", path.display()))?;
    let properties = tagged.properties();
    let filename = filename_metadata(path);
    let primary = tagged.primary_tag().or_else(|| tagged.first_tag());
    let title = primary
        .and_then(Accessor::title)
        .and_then(non_empty)
        .or_else(|| {
            tagged
                .tags()
                .iter()
                .find_map(|tag| tag.title().and_then(non_empty))
        })
        .or(filename.title)
        .unwrap_or_else(|| "未命名歌曲".into());
    let artist = primary
        .and_then(Accessor::artist)
        .and_then(non_empty)
        .or_else(|| {
            tagged
                .tags()
                .iter()
                .find_map(|tag| tag.artist().and_then(non_empty))
        })
        .or(filename.artist)
        .unwrap_or_else(|| "未知艺术家".into());
    let album = primary
        .and_then(Accessor::album)
        .and_then(non_empty)
        .or_else(|| {
            tagged
                .tags()
                .iter()
                .find_map(|tag| tag.album().and_then(non_empty))
        })
        .or(filename.album)
        .unwrap_or_else(|| "未知专辑".into());
    let year = primary
        .and_then(Accessor::year)
        .or_else(|| tagged.tags().iter().find_map(Accessor::year))
        .map(|year| year as i32);
    let genre = primary
        .and_then(Accessor::genre)
        .and_then(non_empty)
        .or_else(|| {
            tagged
                .tags()
                .iter()
                .find_map(|tag| tag.genre().and_then(non_empty))
        });
    let has_artwork = tagged.tags().iter().any(|tag| {
        tag.pictures()
            .iter()
            .any(|picture| !picture.data().is_empty())
    });
    let artwork_key = has_artwork.then(|| {
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map_or(0, timestamp_nanos);
        format!("{}:{modified}", normalize_path(path))
    });

    Ok(Track {
        id: 0,
        path: path.to_path_buf(),
        title,
        artist,
        album,
        year,
        genre,
        duration_ms: properties.duration().as_millis() as u64,
        codec: path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("unknown")
            .to_uppercase(),
        sample_rate: properties.sample_rate().unwrap_or_default(),
        channels: properties.channels().unwrap_or_default() as u16,
        artwork_key,
    })
}

struct FilenameMetadata {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
}

fn filename_metadata(path: &Path) -> FilenameMetadata {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or_default();
    let mut parts = stem.splitn(3, " - ").map(str::trim);
    let first = parts.next().filter(|value| !value.is_empty());
    let second = parts.next().filter(|value| !value.is_empty());
    let third = parts.next().filter(|value| !value.is_empty());
    match (first, second, third) {
        (Some(artist), Some(album), Some(title)) => FilenameMetadata {
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            album: Some(album.to_owned()),
        },
        (Some(artist), Some(title), None) => FilenameMetadata {
            title: Some(title.to_owned()),
            artist: Some(artist.to_owned()),
            album: None,
        },
        _ => FilenameMetadata {
            title: (!stem.is_empty()).then(|| stem.to_owned()),
            artist: None,
            album: None,
        },
    }
}

fn non_empty(value: Cow<'_, str>) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn timestamp_nanos(time: SystemTime) -> u128 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_metadata_fills_common_artist_album_title_names() {
        let metadata = filename_metadata(Path::new("Artist - Album - Title.flac"));
        assert_eq!(metadata.artist.as_deref(), Some("Artist"));
        assert_eq!(metadata.album.as_deref(), Some("Album"));
        assert_eq!(metadata.title.as_deref(), Some("Title"));

        let metadata = filename_metadata(Path::new("老人と海.flac"));
        assert_eq!(metadata.title.as_deref(), Some("老人と海"));
        assert!(metadata.artist.is_none());
        assert!(metadata.album.is_none());
    }
}
