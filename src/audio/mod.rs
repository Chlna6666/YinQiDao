mod command_queue;
mod decoder;
#[path = "dsp/mod.rs"]
mod dsp;
#[allow(dead_code)]
mod engine;
#[rustfmt::skip]
mod facade;
mod fingerprint;
mod smart_profile;
mod transition;

pub use dsp::{EqPreset, SpatialPreset, clamp_eq, clamp_spatial};
pub use engine::{OutputDeviceInfo, PlayerCommand, PlayerEvent};
pub use facade::AudioEngine;
pub use smart_profile::classify as classify_smart_audio;
pub(crate) use fingerprint::fingerprint_file;
