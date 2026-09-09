use crate::{MAX_DEBUG_SOURCES, SourceActivity, SpatialDebugSnapshot};

/// Fixed-size scene publication payload. Geometry and authored-channel energy are captured from the
/// same `SpatialEngine` render, so UI consumers never have to join unrelated global "latest" data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialDebugFrame {
    pub scene: SpatialDebugSnapshot,
    pub source_activity: [SourceActivity; MAX_DEBUG_SOURCES],
}

impl SpatialDebugFrame {
    pub const fn new(scene: SpatialDebugSnapshot) -> Self {
        Self {
            scene,
            source_activity: [SourceActivity { peak: 0.0, rms: 0.0 }; MAX_DEBUG_SOURCES],
        }
    }
}
