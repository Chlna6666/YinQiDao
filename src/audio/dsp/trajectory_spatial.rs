use crate::model::{SpatialMotionMode, SpatialSettings};
use yinqidao_audio_spatial::{
    EngineConfig, EnvironmentSettings, SourcePose, SpatialDebugSnapshot, SpatialEngine, Trajectory,
    TrajectoryKind, Vec3,
};

const MIN_TRAJECTORY_RADIUS_METERS: f32 = 0.45;
const TRAJECTORY_RADIUS_RANGE_METERS: f32 = 0.85;
const MIN_STEREO_HALF_ANGLE_DEGREES: f32 = 12.0;
const STEREO_HALF_ANGLE_RANGE_DEGREES: f32 = 38.0;
const MIN_STEREO_DISTANCE_METERS: f32 = 0.80;
const STEREO_DISTANCE_RANGE_METERS: f32 = 2.20;
const MAX_ENVIRONMENT_MIX: f32 = 0.20;
const STEREO_SOURCE_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrajectorySignature {
    kind: TrajectoryKind,
    speed_hz: f32,
    radius_meters: f32,
    clockwise: bool,
}

impl TrajectorySignature {
    fn from_settings(settings: &SpatialSettings) -> Option<Self> {
        if !settings.enabled || settings.motion_intensity <= 0.001 {
            return None;
        }
        let kind = match settings.motion_mode {
            SpatialMotionMode::Static => return None,
            SpatialMotionMode::Orbit8d => TrajectoryKind::FigureEight,
            SpatialMotionMode::Orbit360 => TrajectoryKind::Orbit360,
            SpatialMotionMode::Pendulum => TrajectoryKind::Pendulum,
            SpatialMotionMode::FrontBack => TrajectoryKind::FrontBack,
            SpatialMotionMode::Planetary => TrajectoryKind::Planetary,
            SpatialMotionMode::NearEar => TrajectoryKind::NearEar,
        };
        Some(Self {
            kind,
            speed_hz: settings.motion_speed_hz,
            radius_meters: MIN_TRAJECTORY_RADIUS_METERS
                + settings.motion_radius.clamp(0.0, 1.0) * TRAJECTORY_RADIUS_RANGE_METERS,
            clockwise: settings.clockwise,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FieldSignature {
    width: f32,
    crossfeed: f32,
    distance: f32,
    depth: f32,
    immersive_3d: f32,
}

impl FieldSignature {
    fn from_settings(settings: &SpatialSettings) -> Self {
        Self {
            width: settings.width,
            crossfeed: settings.crossfeed,
            distance: settings.distance,
            depth: settings.depth,
            immersive_3d: settings.immersive_3d,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct StereoField {
    half_angle_sin: f32,
    half_angle_cos: f32,
    distance_meters: f32,
    gain: f32,
    spread: f32,
}

impl StereoField {
    fn from_settings(settings: &SpatialSettings) -> Self {
        let effective_width =
            settings.width.clamp(0.0, 1.0) * (1.0 - settings.crossfeed.clamp(0.0, 1.0) * 0.30);
        let half_angle_degrees =
            MIN_STEREO_HALF_ANGLE_DEGREES + effective_width * STEREO_HALF_ANGLE_RANGE_DEGREES;
        let (half_angle_sin, half_angle_cos) = half_angle_degrees.to_radians().sin_cos();
        let distance_meters = MIN_STEREO_DISTANCE_METERS
            + settings.distance.clamp(0.0, 1.0) * STEREO_DISTANCE_RANGE_METERS
            + settings.depth.clamp(0.0, 1.0) * 0.30;
        let spread = (0.06
            + settings.immersive_3d.clamp(0.0, 1.0) * 0.24
            + settings.crossfeed.clamp(0.0, 1.0) * 0.12)
            .clamp(0.0, 0.45);
        Self {
            half_angle_sin,
            half_angle_cos,
            distance_meters,
            gain: STEREO_SOURCE_GAIN,
            spread,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EnvironmentSignature {
    mix: f32,
    room_size: f32,
    damping: f32,
}

impl EnvironmentSignature {
    fn from_settings(settings: &SpatialSettings) -> Self {
        let mix = (settings.depth.clamp(0.0, 1.0) * 0.09
            + settings.room_size.clamp(0.0, 1.0) * 0.08
            + settings.immersive_3d.clamp(0.0, 1.0) * 0.05)
            .clamp(0.0, MAX_ENVIRONMENT_MIX);
        Self {
            mix,
            room_size: settings.room_size.clamp(0.0, 1.0),
            damping: (0.34
                + settings.room_size.clamp(0.0, 1.0) * 0.28
                + settings.distance.clamp(0.0, 1.0) * 0.18)
                .clamp(0.0, 1.0),
        }
    }

    fn settings(self) -> EnvironmentSettings {
        EnvironmentSettings {
            mix: self.mix,
            room_size: self.room_size,
            damping: self.damping,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StereoSpatializer {
    sample_rate: u32,
    block_frames: usize,
    engine: SpatialEngine,
    trajectory: Option<Trajectory>,
    trajectory_signature: Option<TrajectorySignature>,
    field_signature: Option<FieldSignature>,
    field: StereoField,
    environment_signature: Option<EnvironmentSignature>,
    wet_scratch: Vec<f32>,
}

impl StereoSpatializer {
    pub(crate) fn new(sample_rate: u32) -> Option<Self> {
        let sample_rate = sample_rate.max(1);
        let mut config = EngineConfig::new(sample_rate);
        config.environment.mix = 0.0;
        let block_frames = config.block_frames;
        Some(Self {
            sample_rate,
            block_frames,
            engine: SpatialEngine::new(config).ok()?,
            trajectory: None,
            trajectory_signature: None,
            field_signature: None,
            field: StereoField {
                half_angle_sin: 0.5,
                half_angle_cos: 0.866_025_4,
                distance_meters: 1.0,
                gain: STEREO_SOURCE_GAIN,
                spread: 0.1,
            },
            environment_signature: None,
            wet_scratch: vec![0.0; block_frames.saturating_mul(2)],
        })
    }

    pub(crate) fn reset(&mut self) {
        self.engine.reset();
        if let Some(trajectory) = self.trajectory.as_mut() {
            trajectory.reset();
        }
    }

    pub(crate) fn set_debug_enabled(&mut self, enabled: bool) {
        self.engine.set_debug_enabled(enabled);
    }

    pub(crate) fn debug_snapshot(&self) -> Option<SpatialDebugSnapshot> {
        self.engine.debug_snapshot()
    }

    pub(crate) fn process_in_place(
        &mut self,
        samples: &mut [f32],
        settings: &SpatialSettings,
    ) -> bool {
        if !settings.enabled || samples.is_empty() {
            return true;
        }
        if samples.len() % 2 != 0 {
            return false;
        }

        self.ensure_field(settings);
        self.ensure_environment(settings);
        let trajectory_signature = TrajectorySignature::from_settings(settings);
        self.ensure_trajectory(trajectory_signature);

        let wet_mix = match trajectory_signature {
            Some(_) => settings.mix.clamp(0.0, 1.0) * settings.motion_intensity.clamp(0.0, 1.0),
            None => settings.mix.clamp(0.0, 1.0),
        };
        if wet_mix <= 1.0e-5 {
            return true;
        }
        let dry_mix = 1.0 - wet_mix;
        let total_frames = samples.len() / 2;
        let mut frame_offset = 0usize;

        while frame_offset < total_frames {
            let frames = (total_frames - frame_offset).min(self.block_frames);
            let sample_start = frame_offset * 2;
            let sample_end = sample_start + frames * 2;
            let (center_start, center_end) = if let Some(trajectory) = self.trajectory.as_mut() {
                trajectory.next_segment(frames)
            } else {
                let center = SourcePose::new(Vec3::new(0.0, 0.0, self.field.distance_meters));
                (center, center)
            };
            let (left_start, right_start) = stereo_pair(center_start, self.field);
            let (left_end, right_end) = stereo_pair(center_end, self.field);

            let rendered = self.engine.render_interleaved_stereo_pair(
                &samples[sample_start..sample_end],
                left_start,
                left_end,
                right_start,
                right_end,
                &mut self.wet_scratch[..frames * 2],
            );
            if rendered.is_err() {
                return false;
            }
            for index in 0..frames * 2 {
                let output_index = sample_start + index;
                samples[output_index] = samples[output_index] * dry_mix
                    + self.wet_scratch[index] * wet_mix;
            }
            frame_offset += frames;
        }
        true
    }

    fn ensure_field(&mut self, settings: &SpatialSettings) {
        let signature = FieldSignature::from_settings(settings);
        if self.field_signature == Some(signature) {
            return;
        }
        self.field = StereoField::from_settings(settings);
        self.field_signature = Some(signature);
    }

    fn ensure_environment(&mut self, settings: &SpatialSettings) {
        let signature = EnvironmentSignature::from_settings(settings);
        if self.environment_signature == Some(signature) {
            return;
        }
        self.engine.set_environment(signature.settings());
        self.environment_signature = Some(signature);
    }

    fn ensure_trajectory(&mut self, signature: Option<TrajectorySignature>) {
        if signature == self.trajectory_signature {
            return;
        }
        self.engine.reset();
        self.trajectory = signature.map(|signature| {
            let mut trajectory = Trajectory::new(
                signature.kind,
                self.sample_rate,
                signature.speed_hz,
                signature.radius_meters,
                0.0,
            );
            trajectory.set_clockwise(signature.clockwise);
            trajectory
        });
        self.trajectory_signature = signature;
    }

    #[cfg(test)]
    pub(crate) fn sample_clock(&self) -> Option<u64> {
        self.trajectory.as_ref().map(Trajectory::sample_clock)
    }
}

#[inline]
fn stereo_pair(center: SourcePose, field: StereoField) -> (SourcePose, SourcePose) {
    let left_position = rotate_y(center.position, -field.half_angle_sin, field.half_angle_cos);
    let right_position = rotate_y(center.position, field.half_angle_sin, field.half_angle_cos);
    (
        SourcePose {
            position: left_position,
            velocity: center.velocity,
            gain: field.gain,
            spread: field.spread,
        },
        SourcePose {
            position: right_position,
            velocity: center.velocity,
            gain: field.gain,
            spread: field.spread,
        },
    )
}

#[inline]
fn rotate_y(position: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        position.x * cos + position.z * sin,
        position.y,
        -position.x * sin + position.z * cos,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::SpatialPreset;

    #[test]
    fn orbit8d_maps_to_audio_clock_figure_eight() {
        let settings = SpatialPreset::Orbit8d.settings();
        let signature = TrajectorySignature::from_settings(&settings).expect("dynamic");
        assert_eq!(signature.kind, TrajectoryKind::FigureEight);
    }

    #[test]
    fn stereo_pair_keeps_left_and_right_as_distinct_sources() {
        let field = StereoField::from_settings(&SpatialPreset::Immersive3d.settings());
        let center = SourcePose::new(Vec3::new(0.0, 0.0, field.distance_meters));
        let (left, right) = stereo_pair(center, field);
        assert!(left.position.x < 0.0);
        assert!(right.position.x > 0.0);
        assert!((left.position.length() - right.position.length()).abs() < 1.0e-5);
    }

    #[test]
    fn processing_advances_and_reset_rewinds_trajectory_clock() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.25_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.sample_clock(), Some(128));
        spatializer.reset();
        assert_eq!(spatializer.sample_clock(), Some(0));
    }

    #[test]
    fn static_settings_use_virtual_stereo_pair_without_trajectory() {
        let settings = SpatialPreset::Immersive3d.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.20_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.sample_clock(), None);
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn debug_snapshot_tracks_rendered_stereo_pair() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        spatializer.set_debug_enabled(true);
        let mut samples = vec![0.10_f32; 64 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        let snapshot = spatializer.debug_snapshot().expect("debug snapshot");
        assert_eq!(snapshot.source_count, 2);
        assert_eq!(snapshot.rendered_frames, 64);
    }

    #[test]
    fn wet_workspace_is_fixed_at_construction() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let ptr = spatializer.wet_scratch.as_ptr();
        let capacity = spatializer.wet_scratch.capacity();
        let mut samples = vec![0.1_f32; 1024 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.wet_scratch.as_ptr(), ptr);
        assert_eq!(spatializer.wet_scratch.capacity(), capacity);
    }
}
