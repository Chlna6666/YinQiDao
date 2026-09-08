use crate::{EnvironmentSettings, ListenerPose, SourcePose, Vec3};
use crate::environment::{EARLY_REFLECTION_TAP_COUNT, ReflectionWall, SPEED_OF_SOUND_M_S};

pub(crate) const MAX_REFLECTION_DELAY_SECONDS: f32 = 0.080;

/// First-order image-source solution for one source/wall pair.
///
/// The current public room controls expose only scalar size/damping values, so the room is centered
/// on the listener and expressed in the listener's local basis. Wall planes expand when necessary so
/// the current source always remains inside the modeled room. This keeps the geometry stable for
/// music-oriented virtual sources without introducing an absolute world/room transform yet.
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

/// Solve the four horizontal first-order reflections with the image-source method.
///
/// The delay is the *excess* path delay relative to the direct source/listener distance. YinQiDao's
/// direct parametric renderer intentionally avoids absolute propagation latency, so adding only the
/// excess path preserves correct direct-vs-reflection timing without delaying the whole programme.
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
    let source_local = Vec3::new(
        finite_or_zero(relative.dot(right)),
        finite_or_zero(relative.dot(up)),
        finite_or_zero(relative.dot(forward)),
    );
    let direct_distance = source_local.length().max(0.05);

    let room = settings.room_size;
    let half_width = (1.45 + room * 2.55).max(source_local.x.abs() + 0.65);
    let half_depth = (1.80 + room * 3.20).max(source_local.z.abs() + 0.75);
    let walls = [
        ReflectionWall::Left,
        ReflectionWall::Right,
        ReflectionWall::Front,
        ReflectionWall::Rear,
    ];

    std::array::from_fn(|index| {
        let wall = walls[index];
        let image_local = match wall {
            ReflectionWall::Left => Vec3::new(
                -2.0 * half_width - source_local.x,
                source_local.y,
                source_local.z,
            ),
            ReflectionWall::Right => Vec3::new(
                2.0 * half_width - source_local.x,
                source_local.y,
                source_local.z,
            ),
            ReflectionWall::Front => Vec3::new(
                source_local.x,
                source_local.y,
                2.0 * half_depth - source_local.z,
            ),
            ReflectionWall::Rear => Vec3::new(
                source_local.x,
                source_local.y,
                -2.0 * half_depth - source_local.z,
            ),
        };

        let bounce_scale = match wall {
            ReflectionWall::Left => (-half_width / image_local.x).clamp(0.0, 1.0),
            ReflectionWall::Right => (half_width / image_local.x).clamp(0.0, 1.0),
            ReflectionWall::Front => (half_depth / image_local.z).clamp(0.0, 1.0),
            ReflectionWall::Rear => (-half_depth / image_local.z).clamp(0.0, 1.0),
        };
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
        };
        let wall_reflectance = wall_base * (1.0 - settings.damping * 0.34);
        let wall_tilt_hz = match wall {
            ReflectionWall::Left | ReflectionWall::Right => 700.0,
            ReflectionWall::Front => 0.0,
            ReflectionWall::Rear => -1_200.0,
        };
        let damping_cutoff_hz =
            (18_500.0 - settings.damping * 11_500.0 + wall_tilt_hz).clamp(4_500.0, 19_500.0);

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
    fn image_source_path_is_not_shorter_than_direct_path() {
        let listener = ListenerPose::identity();
        let source = SourcePose::new(Vec3::new(0.45, 0.15, 1.0));
        let direct = (source.position - listener.position).length();
        let reflections = source_reflection_descriptors(
            48_000.0,
            source,
            listener,
            EnvironmentSettings::default(),
        );
        for reflection in reflections {
            assert!(reflection.path_length_meters >= direct);
            assert!(reflection.excess_path_meters >= 0.0);
            assert!(reflection.excess_delay_samples >= 0.0);
        }
    }

    #[test]
    fn bounce_points_land_on_listener_local_wall_planes() {
        let listener = ListenerPose::identity();
        let source = SourcePose::new(Vec3::new(0.25, 0.10, 0.80));
        let settings = EnvironmentSettings {
            room_size: 0.5,
            ..EnvironmentSettings::default()
        };
        let reflections = source_reflection_descriptors(48_000.0, source, listener, settings);
        let half_width = (1.45 + settings.room_size * 2.55).max(source.position.x.abs() + 0.65);
        let half_depth = (1.80 + settings.room_size * 3.20).max(source.position.z.abs() + 0.75);
        assert!((reflections[0].bounce_position.x + half_width).abs() < 1.0e-4);
        assert!((reflections[1].bounce_position.x - half_width).abs() < 1.0e-4);
        assert!((reflections[2].bounce_position.z - half_depth).abs() < 1.0e-4);
        assert!((reflections[3].bounce_position.z + half_depth).abs() < 1.0e-4);
    }

    #[test]
    fn reflection_delay_is_bounded_by_realtime_history_budget() {
        let reflections = source_reflection_descriptors(
            48_000.0,
            SourcePose::new(Vec3::new(12.0, 0.0, 8.0)),
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
