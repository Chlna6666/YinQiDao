use crate::image_source::room_half_extents;
use crate::{EnvironmentSettings, Vec3};

/// Return the exact half-extents of the fixed world-space rectangular room used by the realtime
/// image-source solver.
///
/// This is a read-only diagnostic API. The values remain in the engine's world coordinate frame;
/// GPU/debug callers must project them through the current `ListenerPose` instead of treating the
/// room as listener-local geometry. Keeping this helper beside the realtime solver prevents the
/// visualization from copying the room-size formula and drifting away from the acoustic path.
#[inline]
pub fn debug_room_half_extents(settings: EnvironmentSettings) -> Vec3 {
    let room = room_half_extents(settings);
    Vec3::new(room.width, room.height, room.depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_extents_follow_realtime_room_size_policy() {
        let compact = debug_room_half_extents(EnvironmentSettings {
            mix: 0.10,
            room_size: 0.0,
            damping: 0.45,
        });
        let large = debug_room_half_extents(EnvironmentSettings {
            mix: 0.10,
            room_size: 1.0,
            damping: 0.45,
        });

        assert!(compact.x > 0.0 && compact.y > 0.0 && compact.z > 0.0);
        assert!(large.x > compact.x);
        assert!(large.y > compact.y);
        assert!(large.z > compact.z);
        assert!(large.x.is_finite() && large.y.is_finite() && large.z.is_finite());
    }
}
