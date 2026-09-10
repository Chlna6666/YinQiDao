mod avs3_backend;
mod command_queue;
mod debug;
mod decoder;
mod dsp;
#[allow(dead_code)]
mod engine;
#[rustfmt::skip]
mod facade;
mod fingerprint;
mod head_tracking;
mod head_tracking_opentrack;
mod smart_profile;
mod spatial_activity;
mod spatial_debug;
mod transition;

pub use debug::{
    AudioDebugMonitorMode, AudioDebugSnapshot, AudioDebugStage, audio_debug_enabled,
    audio_debug_latest_snapshot, set_audio_debug_enabled, set_audio_debug_monitor_mode,
};
pub use dsp::{EqPreset, SpatialPreset, clamp_eq, clamp_spatial};
pub use engine::{OutputDeviceInfo, PlayerCommand, PlayerEvent};
pub use facade::AudioEngine;
pub(crate) use fingerprint::fingerprint_file;
pub use head_tracking::{
    HeadTrackingBridge, HeadTrackingCalibration, HeadTrackingEulerPose, HeadTrackingProvider,
    ManualHeadTrackingProvider,
};
pub use head_tracking_opentrack::{
    OpenTrackUdpConfig, OpenTrackUdpProvider, OpenTrackUdpTransform,
};
pub use smart_profile::classify as classify_smart_audio;
pub use spatial_debug::spatial_debug_latest_snapshot;
pub use yinqidao_audio_spatial::{
    ListenerPose, Vec3, reset_runtime_listener_pose, set_runtime_listener_pose,
};

impl AudioEngine {
    /// Publish a high-rate listener/head pose directly to the spatial DSP without entering the
    /// bounded PlayerCommand mailbox. The spatial engines consume the latest coherent pose once per
    /// internal render block, so head tracking cannot queue stale orientation updates behind decoder
    /// or transport work.
    #[inline]
    pub fn set_listener_pose(&self, pose: ListenerPose) {
        set_runtime_listener_pose(pose);
    }

    /// Restore the identity listener pose through the same lock-free runtime channel.
    #[inline]
    pub fn reset_listener_pose(&self) {
        reset_runtime_listener_pose();
    }
}