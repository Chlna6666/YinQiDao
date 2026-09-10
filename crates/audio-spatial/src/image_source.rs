use crate::environment::{EARLY_REFLECTION_TAP_COUNT, ReflectionWall, SPEED_OF_SOUND_M_S};
use crate::{EnvironmentSettings, ListenerPose, RoomPose, SourcePose, Vec3};

pub(crate) const MAX_REFLECTION_DELAY_SECONDS: f32 = 0.080;
const WALL_INTERIOR_EPSILON_M: f32 = 0.05;

const REFLECTION_WALLS: [ReflectionWall; EARLY_REFLECTION_TAP_COUNT] = [
    ReflectionWall::Left,
    ReflectionWall::Right,
    ReflectionWall::Front,
    ReflectionWall::Rear,
    ReflectionWall::Floor,
    ReflectionWall::Ceiling,
];
const WALL_BASE_REFLECTANCE: [f32; EARLY_REFLECTION_TAP_COUNT] =
    [0.62, 0.60, 0.56, 0.52, 0.46, 0.50];
const WALL_TILT_HZ: [f32; EARLY_REFLECTION_TAP_COUNT] =
    [700.0, 700.0, 0.0, -1_200.0, -2_000.0, -800.0];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RoomHalfExtents {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
}

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

/// Solve reflections in the engine's default world-space room.
///
/// The production room is identity/world aligned. Keep this as a dedicated hot path rather than
/// routing every source through `RoomPose::{world_to_local, local_to_world}`: those helpers rebuild
/// an orthonormal basis on every call, and six reflections previously paid that cost repeatedly for
/// both image and bounce points. The generic transformed-room solver remains available below.
pub(crate) fn source_reflection_descriptors(
    sample_rate: f32,
    source: SourcePose,
    listener: ListenerPose,
    settings: EnvironmentSettings,
) -> [SourceReflectionDescriptor; EARLY_REFLECTION_TAP_COUNT] {
    let sample_rate = sample_rate.max(1.0);
    let settings = sanitize_settings(settings);
    let room = room_half_extents(settings);
    let source_local = clamp_inside_room(source.position, room);
    let listener_local = clamp_inside_room(listener.position, room);
    solve_local_reflections(sample_rate, source_local, listener_local, room, settings)
}

/// Solve all six first-order rectangular-room reflections in a world-space room frame.
///
/// The source and listener are projected into `room_pose`, while binaural direction remains a
/// separate listener-space concern in the renderer. Direct programme audio intentionally omits
/// absolute propagation latency, therefore each reflection contributes only its excess path delay.
pub(crate) fn source_reflection_descriptors_in_room(
    sample_rate: f32,
    source: SourcePose,
    listener: ListenerPose,
    room_pose: RoomPose,
    settings: EnvironmentSettings,
) -> [SourceReflectionDescriptor; EARLY_REFLECTION_TAP_COUNT] {
    let sample_rate = sample_rate.max(1.0);
    let settings = sanitize_settings(settings);
    let room = room_half_extents(settings);

    // `RoomPose::world_to_local/local_to_world` each call `basis()`. Compute the orthonormal basis
    // once for the complete six-wall solve so custom/rotated-room diagnostics do not repeat those
    // normalizations fourteen times per source update.
    let (right, up, forward) = room_pose.basis();
    let source_local = clamp_inside_room(
        world_to_room_local(source.position, room_pose.position, right, up, forward),
        room,
    );
    let listener_local = clamp_inside_room(
        world_to_room_local(listener.position, room_pose.position, right, up, forward),
        room,
    );
    let mut descriptors =
        solve_local_reflections(sample_rate, source_local, listener_local, room, settings);
    for descriptor in &mut descriptors {
        descriptor.image_position = room_local_to_world(
            descriptor.image_position,
            room_pose.position,
            right,
            up,
            forward,
        );
        descriptor.bounce_position = room_local_to_world(
            descriptor.bounce_position,
            room_pose.position,
            right,
            up,
            forward,
        );
    }
    descriptors
}

#[inline]
fn solve_local_reflections(
    sample_rate: f32,
    source_local: Vec3,
    listener_local: Vec3,
    room: RoomHalfExtents,
    settings: EnvironmentSettings,
) -> [SourceReflectionDescriptor; EARLY_REFLECTION_TAP_COUNT] {
    let direct_distance = (source_local - listener_local).length().max(0.05);
    let samples_per_meter = sample_rate / SPEED_OF_SOUND_M_S;
    let max_delay_samples = sample_rate * MAX_REFLECTION_DELAY_SECONDS;
    let reflectance_scale = 1.0 - settings.damping * 0.34;
    let damping_cutoff_base_hz = 18_500.0 - settings.damping * 11_500.0;

    std::array::from_fn(|index| {
        let wall = REFLECTION_WALLS[index];
        let image_local = image_source_for_wall(source_local, room, wall);
        let bounce_local = bounce_point(listener_local, image_local, room, wall);
        let path_length_meters = (image_local - listener_local)
            .length()
            .max(direct_distance);
        let excess_path_meters = (path_length_meters - direct_distance).max(0.0);
        let excess_delay_samples =
            (excess_path_meters * samples_per_meter).clamp(0.0, max_delay_samples);

        SourceReflectionDescriptor {
            wall,
            image_position: image_local,
            bounce_position: bounce_local,
            path_length_meters,
            excess_path_meters,
            excess_delay_samples,
            wall_reflectance: WALL_BASE_REFLECTANCE[index] * reflectance_scale,
            damping_cutoff_hz: (damping_cutoff_base_hz + WALL_TILT_HZ[index])
                .clamp(3_800.0, 19_500.0),
        }
    })
}

#[inline]
fn world_to_room_local(
    point: Vec3,
    room_position: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
) -> Vec3 {
    let relative = point - room_position;
    Vec3::new(
        relative.dot(right),
        relative.dot(up),
        relative.dot(forward),
    )
}

#[inline]
fn room_local_to_world(
    point: Vec3,
    room_position: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
) -> Vec3 {
    room_position + right * point.x + up * point.y + forward * point.z
}

#[inline]
fn clamp_inside_room(point: Vec3, room: RoomHalfExtents) -> Vec3 {
    Vec3::new(
        finite_or_zero(point.x).clamp(
            -room.width + WALL_INTERIOR_EPSILON_M,
            room.width - WALL_INTERIOR_EPSILON_M,
        ),
        finite_or_zero(point.y).clamp(
            -room.height + WALL_INTERIOR_EPSILON_M,
            room.height - WALL_INTERIOR_EPSILON_M,
        ),
        finite_or_zero(point.z).clamp(
            -room.depth + WALL_INTERIOR_EPSILON_M,
            room.depth - WALL_INTERIOR_EPSILON_M,
        ),
    )
}

#[inline]
fn image_source_for_wall(
    source: Vec3,
    room: RoomHalfExtents,
    wall: ReflectionWall,
) -> Vec3 {
    match wall {
        ReflectionWall::Left => Vec3::new(-2.0 * room.width - source.x, source.y, source.z),
        ReflectionWall::Right => Vec3::new(2.0 * room.width - source.x, source.y, source.z),
        ReflectionWall::Front => Vec3::new(source.x, source.y, 2.0 * room.depth - source.z),
        ReflectionWall::Rear => Vec3::new(source.x, source.y, -2.0 * room.depth - source.z),
        ReflectionWall::Floor => Vec3::new(source.x, -2.0 * room.height - source.y, source.z),
        ReflectionWall::Ceiling => Vec3::new(source.x, 2.0 * room.height - source.y, source.z),
    }
}

#[inline]
fn bounce_point(
    listener: Vec3,
    image: Vec3,
    room: RoomHalfExtents,
    wall: ReflectionWall,
) -> Vec3 {
    let delta = image - listener;
    let (plane, origin, direction) = match wall {
        ReflectionWall::Left => (-room.width, listener.x, delta.x),
        ReflectionWall::Right => (room.width, listener.x, delta.x),
        ReflectionWall::Front => (room.depth, listener.z, delta.z),
        ReflectionWall::Rear => (-room.depth, listener.z, delta.z),
        ReflectionWall::Floor => (-room.height, listener.y, delta.y),
        ReflectionWall::Ceiling => (room.height, listener.y, delta.y),
    };
    let t = if direction.abs() > 1.0e-8 {
        ((plane - origin) / direction).clamp(0.0, 1.0)
    } else {
        0.0
    };
    listener + delta * t
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
    fn default_wrapper_uses_fixed_world_room() {
        let listener = ListenerPose {
            position: Vec3::new(0.6, 0.2, -0.3),
            forward: Vec3::RIGHT,
            up: Vec3::UP,
        };
        let source = SourcePose::new(Vec3::new(1.2, 0.4, 0.8));
        let settings = EnvironmentSettings::default();
        assert_eq!(
            source_reflection_descriptors(48_000.0, source, listener, settings),
            source_reflection_descriptors_in_room(
                48_000.0,
                source,
                listener,
                RoomPose::identity(),
                settings,
            )
        );
    }

    #[test]
    fn one_room_geometry_is_shared_across_sources() {
        let settings = EnvironmentSettings {
            room_size: 0.55,
            ..EnvironmentSettings::default()
        };
        let room = room_half_extents(settings);
        let room_pose = RoomPose::identity();
        let a = source_reflection_descriptors_in_room(
            48_000.0,
            SourcePose::new(Vec3::new(-0.6, 0.2, 1.0)),
            ListenerPose::identity(),
            room_pose,
            settings,
        );
        let b = source_reflection_descriptors_in_room(
            48_000.0,
            SourcePose::new(Vec3::new(0.8, -0.2, -0.5)),
            ListenerPose::identity(),
            room_pose,
            settings,
        );
        assert!((a[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((b[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((a[5].bounce_position.y - room.height).abs() < 1.0e-4);
        assert!((b[5].bounce_position.y - room.height).abs() < 1.0e-4);
    }

    #[test]
    fn listener_head_rotation_does_not_rotate_absolute_room() {
        let settings = EnvironmentSettings::default();
        let room = room_half_extents(settings);
        let listener = ListenerPose {
            position: Vec3::ZERO,
            forward: Vec3::RIGHT,
            up: Vec3::UP,
        };
        let reflections = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::FORWARD),
            listener,
            settings,
        );
        assert!((reflections[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((reflections[1].bounce_position.x - room.width).abs() < 1.0e-4);
        assert!((reflections[2].bounce_position.z - room.depth).abs() < 1.0e-4);
        assert!((reflections[3].bounce_position.z + room.depth).abs() < 1.0e-4);
    }

    #[test]
    fn moving_listener_changes_paths_without_moving_world_walls() {
        let settings = EnvironmentSettings {
            room_size: 0.5,
            ..EnvironmentSettings::default()
        };
        let room = room_half_extents(settings);
        let listener = ListenerPose {
            position: Vec3::new(0.45, 0.10, -0.35),
            ..ListenerPose::identity()
        };
        let reflections = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::new(-0.2, 0.0, 1.0)),
            listener,
            settings,
        );
        assert!((reflections[0].bounce_position.x + room.width).abs() < 1.0e-4);
        assert!((reflections[5].bounce_position.y - room.height).abs() < 1.0e-4);
        assert!(reflections.iter().all(|reflection| reflection.path_length_meters > 0.0));
    }

    #[test]
    fn rotated_room_moves_planes_in_world_space() {
        let settings = EnvironmentSettings::default();
        let room = room_half_extents(settings);
        let room_pose = RoomPose {
            position: Vec3::new(2.0, 0.0, 1.0),
            forward: Vec3::RIGHT,
            up: Vec3::UP,
        };
        let reflections = source_reflection_descriptors_in_room(
            48_000.0,
            SourcePose::new(room_pose.local_to_world(Vec3::new(0.2, 0.0, 0.8))),
            ListenerPose {
                position: room_pose.position,
                ..ListenerPose::identity()
            },
            room_pose,
            settings,
        );
        let front_local = room_pose.world_to_local(reflections[2].bounce_position);
        let left_local = room_pose.world_to_local(reflections[0].bounce_position);
        assert!((front_local.z - room.depth).abs() < 1.0e-4);
        assert!((left_local.x + room.width).abs() < 1.0e-4);
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
