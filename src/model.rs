use std::{path::PathBuf, sync::Arc};

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
#[serde(default)]
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
#[serde(default)]
pub struct SpatialSettings {
    pub enabled: bool,
    /// Mid/side stereo width. 0 keeps the image narrow, 1 enables the maximum safe widening.
    pub width: f32,
    /// Early-reflection/decorrelation depth.
    pub depth: f32,
    /// Perceptual listener distance. Also drives high-frequency air absorption.
    pub distance: f32,
    /// Wet/dry amount of the spatial processor.
    pub mix: f32,
    /// Controlled inter-channel feed for headphone compatibility and center stability.
    pub crossfeed: f32,
    /// Early-room reflection size/amount.
    pub room_size: f32,
    /// Interaural delay/decorrelation amount used by the 3D stage.
    pub immersive_3d: f32,
}

impl Default for SpatialSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            width: 0.5,
            depth: 0.35,
            distance: 0.2,
            mix: 0.5,
            crossfeed: 0.08,
            room_size: 0.15,
            immersive_3d: 0.10,
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
    pub queue: Arc<Vec<TrackId>>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub error: Option<String>,
}
