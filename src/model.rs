use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type TrackId = i64;

#[derive(Clone, Debug)]
pub struct Track {
    pub id: TrackId,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub duration_ms: u64,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub artwork_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaybackState {
    #[default]
    Stopped,
    Loading,
    Playing,
    Paused,
    #[allow(dead_code)]
    Buffering,
    Error,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EqSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    pub bands_db: [f32; 10],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; 10],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SpatialSettings {
    pub enabled: bool,
    pub width: f32,
    pub depth: f32,
    pub distance: f32,
    pub mix: f32,
}

impl Default for SpatialSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            width: 0.5,
            depth: 0.35,
            distance: 0.2,
            mix: 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LibraryTab {
    #[default]
    Songs,
    Albums,
    Artists,
    Playlists,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AppPage {
    #[default]
    Home,
    Library,
    Player,
    Settings,
}

impl AppPage {
    pub fn pathname(self) -> &'static str {
        match self {
            Self::Home => "/",
            Self::Library => "/library",
            Self::Player => "/player",
            Self::Settings => "/settings",
        }
    }

    pub fn from_pathname(path: &str) -> Self {
        match path {
            "/player" => Self::Player,
            "/library" => Self::Library,
            "/settings" => Self::Settings,
            _ => Self::Home,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlayerSnapshot {
    pub state: PlaybackState,
    pub current_track: Option<Track>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub volume: f32,
    pub queue: Vec<TrackId>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub error: Option<String>,
}
