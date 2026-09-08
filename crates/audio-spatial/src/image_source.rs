use crate::environment::{EARLY_REFLECTION_TAP_COUNT, ReflectionWall, SPEED_OF_SOUND_M_S};
use crate::{EnvironmentSettings, ListenerPose, SourcePose, Vec3};

pub(crate) const MAX_REFLECTION_DELAY_SECONDS: f32 = 0.080;
const WALL_INTERIOR_EPSILON_M: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RoomHalfExtents {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

/// Listener-local rectangular room used by the current music-oriented image-source renderer.
/// The geometry is shared by every source; source positions never resize the room independently.
pub(crate) fn room_half_extents(settings: EnvironmentSettings) -> RoomHalfExtents {
    let room = finite_or_zero(settings.room_size).clamp(0.0, 1.0);
    RoomHalfExtents {
        width: 1.65 + room * 3.00,
        height: 1.25 + room * 1.50,
        depth: 2.10 + room * 4.10,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SourceReflectionDescriptor {
    pub wall: ReflectionWall,
    pub image_position: Vec3,
    pub bounce_position: Vec3,
    pub path_length_meters: f32,
    pub excess_path_meters: f32,
    pub excess_delay_samples: f32,
    pub wall_reflectance: f32,
    pub damping_cutoff_hz: f32,
}

/// Solve all six first-order room reflections with the image-source method.
///
/// Direct programme audio intentionally has no absolute propagation delay. Reflections therefore
/// add only the *excess* source→wall→listener path delay, preserving low playback latency while
/// retaining the direct/reflected timing cue.
pub(crate) fn source_reflection_descriptors(
    sample_rate: f32,
    source: SourcePose,
    listener: ListenerPose,
    settings: EnvironmentSettings,
) -> [SourceReflectionDescriptor; EARLY_REFLECTION_TAP_COUNT] {
    let sample_rate = sample_rate.max(1.0);
    let settings = sanitize_settings(settings);
    let (right, up, forward) = listener.basis();
    let relative = source.position - listener.position;
    let source_local_raw = Vec3::new(
        finite_or_zero(relative.dot(right)),
        finite_or_zero(relative.dot(up)),
        finite_or_zero(relative.dot(forward)),
    );
    let room = room_half_extents(settings);
    let source_local = Vec3::new(
        source_local_raw.x.clamp(
            -room.width + WALL_INTERIOR_EPSILON_M,
            room.width - WALL_INTERIOR_EPSILON_M,
        ),
        source_local_raw.y.clamp(
            -room.height + WALL_INTERIOR_EPSILON_M,
            room.height - WALL_INTERIOR_EPSILON_M,
        ),
        source_local_raw.z.clamp(
            -room.depth + WALL_INTERIOR_EPSILON_M,
            room.depth - WALL_INTERIOR_EPSILON_M,
        ),
    );
    let direct_distance = source_local.length().max(0.05);
    let walls = [
        ReflectionWall::Left,
        ReflectionWall::Right,
        ReflectionWall::Front,
        ReflectionWall::Rear,
        ReflectionWall::Floor,
        ReflectionWall::Ceiling,
    ];

    std::array::from_fn(|index| {
        let wall = walls[index];
        let image_local = match wall {
            ReflectionWall::Left => Vec3::new(
                -2.0 * room.width - source_local.x,
                source_local.y,
                source_local.z,
            ),
            ReflectionWall::Right => Vec3::new(
                2.0 * room.width - source_local.x,
                source_local.y,
                source_local.z,
            ),
            ReflectionWall::Front => Vec3::new(
                source_local.x,
                source_local.y,
                2.0 * room.depth - source_local.z,
            ),
            ReflectionWall::Rear => Vec3::new(
                source_local.x,
                source_local.y,
                -2.0 * room.depth - source_local.z,
            ),
            ReflectionWall::Floor => Vec3::new(
                source_local.x,
                -2.0 * room.height - source_local.y,
                source_local.z,
            ),
            ReflectionWall::Ceiling => Vec3::new(
                source_local.x,
                2.0 * room.height - source_local.y,
                source_local.z,
            ),
        };

        let bounce_scale = match wall {
            ReflectionWall::Left => -room.width / image_local.x,
            ReflectionWall::Right => room.width / image_local.x,
            ReflectionWall::Front => room.depth / image_local.z,
            ReflectionWall::Rear => -room.depth / image_local.z,
            ReflectionWall::Floor => -room.height / image_local.y,
            ReflectionWall::Ceiling => room.height / image_local.y,
        }
        .clamp(0.0, 1.0);
        let bounce_local = image_local * bounce_scale;
        let path_length_meters = image_local.length().max(direct_distance);
        let excess_path_meters = (path_length_meters - direct_distance).max(0.0);
        let excess_delay_samples = (excess_path_meters / SPEED_OF_SOUND_M_S * sample_rate)
            .clamp(0.0, sample_rate * MAX_REFLECTION_DELAY_SECONDS);

        let wall_base = match wall {
            ReflectionWall::Left => 0.62,
            ReflectionWall::Right => 0.60,
            ReflectionWall::Front => 0.56,
            ReflectionWall::Rear => 0.52,
            ReflectionWall::Floor => 0.46,
            ReflectionWall::Ceiling => 0.50,
        };
        let wall_reflectance = wall_base * (1.0 - settings.damping * 0.34);
        let wall_tilt_hz = match wall {
            ReflectionWall::Left | ReflectionWall::Right => 700.0,
            ReflectionWall::Front => 0.0,
            ReflectionWall::Rear => -1_200.0,
            ReflectionWall::Floor => -2_000.0,
            ReflectionWall::Ceiling => -800.0,
        };
        let damping_cutoff_hz =
            (18_500.0 - settings.damping * 11_500.0 + wall_tilt_hz).clamp(3_800.0, 19_500.0);

        SourceReflectionDescriptor {
            wall,
            image_position: listener.position
                + right * image_local.x
                + up * image_local.y
                + forward * image_local.z,
            bounce_position: listener.position
                + right * bounce_local.x
                + up * bounce_local.y
                + forward * bounce_local.z,
            path_length_meters,
            excess_path_meters,
            excess_delay_samples,
            wall_reflectance,
            damping_cutoff_hz,
        }
    })
}

#[inline]
fn sanitize_settings(settings: EnvironmentSettings) -> EnvironmentSettings {
    EnvironmentSettings {
        mix: finite_or_zero(settings.mix).clamp(0.0, 0.45),
        room_size: finite_or_zero(settings.room_size).clamp(0.0, 1.0),
        damping: finite_or_zero(settings.damping).clamp(0.0, 1.0),
    }
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_room_geometry_is_shared_across_sources() {
        let settings = EnvironmentSettings {
            room_size: 0.55,
            ..EnvironmentSettings::default()
        };
        let room = room_half_extents(settings);
        assert!(room.width > 1.0);
        assert!(room.height > 1.0);
        assert!(room.depth > room.width);

        let a = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::new(-0.6, 0.2, 1.0)),
            ListenerPose::identity(),
            settings,
        );
        let b = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::new(0.8, -0.2, -0.5)),
            ListenerPose::identity(),
            settings,
        );
        assert!((a[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((b[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((a[5].bounce_position.y - room.height).abs() < 1.0e-4);
        assert!((b[5].bounce_position.y - room.height).abs() < 1.0e-4);
    }

    #[test]
    fn all_six_image_source_paths_are_not_shorter_than_direct_path() {
        let listener = ListenerPose::identity();
        let source = SourcePose::new(Vec3::new(0.45, 0.15, 1.0));
        let direct = (source.position - listener.position).length();
        let reflections = source_reflection_descriptors(
            48_000.0,
            source,
            listener,
            EnvironmentSettings::default(),
        );
        assert_eq!(reflections.len(), 6);
        for reflection in reflections {
            assert!(reflection.path_length_meters >= direct - 1.0e-4);
            assert!(reflection.excess_path_meters >= 0.0);
            assert!(reflection.excess_delay_samples >= 0.0);
        }
    }

    #[test]
    fn bounce_points_land_on_all_six_room_planes() {
        let listener = ListenerPose::identity();
        let source = SourcePose::new(Vec3::new(0.25, 0.10, 0.80));
        let settings = EnvironmentSettings {
            room_size: 0.5,
            ..EnvironmentSettings::default()
        };
        let room = room_half_extents(settings);
        let reflections = source_reflection_descriptors(48_000.0, source, listener, settings);
        assert!((reflections[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((reflections[1].bounce_position.x - room.width).abs() < 1.0e-4);
        assert!((reflections[2].bounce_position.z - room.depth).abs() < 1.0e-4);
        assert!((reflections[3].bounce_position.z + room.depth).abs() < 1.0e-4);
        assert!((reflections[4].bounce_position.y + room.height).abs() < 1.0e-4);
        assert!((reflections[5].bounce_position.y - room.height).abs() < 1.0e-4);
    }

    #[test]
    fn reflection_delay_is_bounded_by_realtime_history_budget() {
        let reflections = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::new(12.0, 8.0, 8.0)),
            ListenerPose::identity(),
            EnvironmentSettings {
                room_size: 1.0,
                ..EnvironmentSettings::default()
            },
        );
        let max_samples = 48_000.0 * MAX_REFLECTION_DELAY_SECONDS;
        assert!(
            reflections
                .iter()
                .all(|reflection| reflection.excess_delay_samples <= max_samples)
        );
    }

    #[test]
    fn more_damping_reduces_reflectance_and_cutoff() {
        let source = SourcePose::new(Vec3::FORWARD);
        let dry = source_reflection_descriptors(
            48_000.0,
            source,
            ListenerPose::identity(),
            EnvironmentSettings {
                damping: 0.0,
                ..EnvironmentSettings::default()
            },
        );
        let damped = source_reflection_descriptors(
            48_000.0,
            source,
            ListenerPose::identity(),
            EnvironmentSettings {
                damping: 1.0,
                ..EnvironmentSettings::default()
            },
        );
        for index in 0..EARLY_REFLECTION_TAP_COUNT {
            assert!(damped[index].wall_reflectance < dry[index].wall_reflectance);
            assert!(damped[index].damping_cutoff_hz < dry[index].damping_cutoff_hz);
        }
    }
}
