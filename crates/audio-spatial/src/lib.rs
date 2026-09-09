//! YinQiDao's self-owned low-latency spatial audio engine.
//!
//! The realtime path is intentionally sans-I/O: no file access, no locks, no thread creation and
//! no heap growth are permitted while rendering. Expensive state is allocated when the engine is
//! constructed; rendering consumes borrowed PCM and writes into caller-owned output buffers.

#![allow(clippy::manual_is_multiple_of)]

mod debug;
mod debug_activity;
mod debug_scene_activity;
mod delay;
mod engine;
mod environment;
mod error;
mod image_source;
mod late_field;
mod layout;
mod pinna;
mod pose;
mod renderer;
mod trajectory;

pub use debug::{
    MAX_DEBUG_REFLECTIONS, MAX_DEBUG_SOURCES, SpatialDebugReflection,
    SpatialDebugReflectionWall, SpatialDebugSnapshot, SpatialDebugSource, SpatialDebugSourceKind,
};
pub use debug_activity::{SourceActivity, analyze_interleaved_activity};
pub use debug_scene_activity::SpatialDebugFrame;
pub use engine::{DEFAULT_BLOCK_FRAMES, DEFAULT_MAX_SOURCES, EngineConfig, SpatialEngine};
pub use environment::EnvironmentSettings;
pub use error::SpatialError;
pub use late_field::{LateFieldTelemetry, late_field_telemetry};
pub use layout::{ChannelLayout, ChannelRole, SourceKind, Speaker, SpeakerLayout};
pub use pinna::{PinnaCueTelemetry, pinna_cue_telemetry};
pub use pose::{DEFAULT_HEAD_RADIUS_M, ListenerPose, RoomPose, SourcePose, Vec3};
pub use trajectory::{Trajectory, TrajectoryKind};
