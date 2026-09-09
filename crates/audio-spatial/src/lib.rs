//! YinQiDao's self-owned low-latency spatial audio engine.
//!
//! The realtime path is intentionally sans-I/O: no file access, no locks, no thread creation and
//! no heap growth are permitted while rendering. Expensive state is allocated when the engine is
//! constructed; rendering consumes borrowed PCM and writes into caller-owned output buffers.

#![allow(clippy::manual_is_multiple_of)]

mod debug;
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
pub use engine::{DEFAULT_BLOCK_FRAMES, DEFAULT_MAX_SOURCES, EngineConfig, SpatialEngine};
pub use environment::EnvironmentSettings;
pub use error::SpatialError;
pub use layout::{ChannelLayout, SourceKind, Speaker, SpeakerLayout};
pub use pose::{ListenerPose, SourcePose, Vec3};
pub use trajectory::{Trajectory, TrajectoryKind};
