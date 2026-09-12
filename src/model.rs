use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
    sync::Arc,
};

use serde::{Deserialize, Serialize};

pub type TrackId = i64;

/// Cheap-to-clone handle to immutable-by-default track metadata.
///
/// Playback snapshots, preload requests, engine registration, enrichment tasks and UI projections
/// frequently retain the same logical track concurrently. Keeping the payload behind `Arc` makes
/// those clones O(1); rare metadata edits use `DerefMut`/`Arc::make_mut` for copy-on-write updates.
#[derive(Clone, Debug)]
pub struct Track(Arc<TrackData>);

#[derive(Clone, Debug)]
pub struct TrackData {
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

impl Track {
    pub fn new(data: TrackData) -> Self {
        Self(Arc::new(data))
    }
}

impl Deref for Track {
    type Target = TrackData;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl DerefMut for Track {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpatialMotionMode {
    #[default]
    Static,
    Orbit8d,
    Orbit360,
    Pendulum,
    FrontBack,
    Planetary,
    NearEar,
    Helix,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualBedMode {
    Off,
    #[default]
    Auto,
    Surround5_1,
    Surround7_1,
    Surround5_1_2,
    Surround5_1_4,
    Surround7_1_2,
    Surround7_1_4,
}

/// User-confirmed speaker layout for a decoded multichannel stream whose codec/container exposes
/// no reliable speaker metadata. This is deliberately independent from `VirtualBedMode`: declaring
/// what an existing N-channel source means is not the same operation as synthesizing a speaker bed
/// from mono/stereo programme.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLayoutOverride {
    #[default]
    None,
    Surround5_1,
    Surround7_1,
    Surround5_1_2,
    Surround5_1_4,
    Surround7_1_2,
    Surround7_1_4,
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
    /// Static externalization/envelopment amount independent of trajectory motion.
    pub immersive_3d: f32,
    /// Internal virtual speaker bed synthesized from mono/stereo programme only.
    pub virtual_bed: VirtualBedMode,
    /// Explicit speaker semantics for metadata-less/discrete multichannel PCM. Reliable
    /// codec/container metadata always wins; the selected layout is accepted only when its exact
    /// speaker count matches the decoded PCM channel count.
    pub source_layout_override: SourceLayoutOverride,
    /// Dynamic source trajectory used by moving spherical scenes, including lower-hemisphere Helix.
    pub motion_mode: SpatialMotionMode,
    /// Orbit cycles per second. Normal UI range is roughly 0.02..0.30 Hz.
    pub motion_speed_hz: f32,
    /// Normalized virtual orbit radius around the listener.
    pub motion_radius: f32,
    /// How strongly the moving virtual source is mixed into the processed signal.
    pub motion_intensity: f32,
    /// Reverse the orbital direction without changing the preset geometry.
    pub clockwise: bool,
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
            virtual_bed: VirtualBedMode::Auto,
            source_layout_override: SourceLayoutOverride::None,
            motion_mode: SpatialMotionMode::Static,
            motion_speed_hz: 0.08,
            motion_radius: 0.65,
            motion_intensity: 0.0,
            clockwise: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct SmartAudioSettings {
    /// Automatically choose EQ/spatial parameters from track metadata on track changes.
    pub enabled: bool,
    /// 0 keeps the manual baseline, 1 applies the complete automatically selected profile.
    pub intensity: f32,
}

impl Default for SmartAudioSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.85,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionMode {
    #[default]
    Direct,
    FadeOutIn,
    Crossfade,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TrackTransitionSettings {
    /// Global switch. Disabled always behaves like Direct.
    pub enabled: bool,
    pub mode: TransitionMode,
    /// Total transition duration. Crossfade overlaps both tracks for this amount of time.
    pub duration_ms: u64,
    /// Analyze the beginning of a preloaded next track and start at a stable musical onset.
    pub smart_cue: bool,
    /// Never skip more than this much audio even if the detected onset is later.
    pub max_smart_cue_ms: u64,
    /// Manual Next/Previous remain immediate unless explicitly enabled.
    pub apply_to_manual_skip: bool,
}

impl Default for TrackTransitionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: TransitionMode::Crossfade,
            duration_ms: 3_500,
            smart_cue: true,
            max_smart_cue_ms: 3_500,
            apply_to_manual_skip: false,
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
