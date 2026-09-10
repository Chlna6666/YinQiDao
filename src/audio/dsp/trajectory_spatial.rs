#[path = "spatial_environment.rs"]
mod spatial_environment;
pub(crate) use spatial_environment::spatial_environment_settings;

#[path = "stereo_virtual_bed.rs"]
mod stereo_virtual_bed;

use crate::model::{SpatialMotionMode, SpatialSettings, VirtualBedMode};
use stereo_virtual_bed::StereoVirtualBed;
use yinqidao_audio_spatial::{
    ChannelLayout, EngineConfig, EnvironmentSettings, MAX_DEBUG_SOURCES, SourceActivity, SourcePose,
    SpatialDebugSnapshot, SpatialEngine, SpeakerLayout, Trajectory, TrajectoryKind, Vec3,
};

const MIN_TRAJECTORY_RADIUS_METERS: f32 = 0.45;
const TRAJECTORY_RADIUS_RANGE_METERS: f32 = 0.85;
const MIN_STEREO_HALF_ANGLE_DEGREES: f32 = 16.0;
const STEREO_HALF_ANGLE_RANGE_DEGREES: f32 = 44.0;
const STEREO_DEPTH_AZIMUTH_RANGE_DEGREES: f32 = 16.0;
const STEREO_IMMERSIVE_AZIMUTH_RANGE_DEGREES: f32 = 30.0;
const MAX_STEREO_HALF_ANGLE_DEGREES: f32 = 102.0;
const STEREO_DEPTH_ELEVATION_RANGE_DEGREES: f32 = 8.0;
const STEREO_IMMERSIVE_ELEVATION_RANGE_DEGREES: f32 = 20.0;
const MAX_STEREO_ELEVATION_DEGREES: f32 = 28.0;
const MIN_STEREO_DISTANCE_METERS: f32 = 0.72;
const STEREO_DISTANCE_RANGE_METERS: f32 = 2.00;
const MAX_TRAJECTORY_SEGMENT_DEGREES: f32 = 1.0;
const STEREO_SOURCE_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
const MAX_VIRTUAL_BED_CHANNELS: usize = 12;

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
        let speed_hz = if settings.motion_speed_hz.is_finite() {
            settings.motion_speed_hz.clamp(0.005, 2.0)
        } else {
            0.10
        };
        Some(Self {
            kind,
            speed_hz,
            radius_meters: MIN_TRAJECTORY_RADIUS_METERS
                + settings.motion_radius.clamp(0.0, 1.0) * TRAJECTORY_RADIUS_RANGE_METERS,
            clockwise: settings.clockwise,
        })
    }
}

/// Apply the user-facing trajectory to a native authored speaker bed. The spatial crate owns the
/// sample clock once configured, so 5.1/7.1/.2/.4 beds and stereo-derived virtual beds use the same
/// spherical scene-motion semantics without a second UI-clock oscillator.
pub(crate) fn apply_scene_motion_settings(
    engine: &mut SpatialEngine,
    settings: &SpatialSettings,
) {
    if let Some(signature) = TrajectorySignature::from_settings(settings) {
        engine.set_scene_motion(
            Some(signature.kind),
            signature.speed_hz,
            signature.radius_meters,
            settings.motion_intensity.clamp(0.0, 1.0),
            signature.clockwise,
        );
    } else {
        engine.set_scene_motion(None, 0.10, 1.0, 0.0, true);
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
    elevation_sin: f32,
    elevation_cos: f32,
    distance_meters: f32,
    gain: f32,
    spread: f32,
}

impl StereoField {
    fn from_settings(settings: &SpatialSettings) -> Self {
        let width = settings.width.clamp(0.0, 1.0);
        let depth = settings.depth.clamp(0.0, 1.0);
        let immersive = settings.immersive_3d.clamp(0.0, 1.0);
        let effective_width =
            width * (1.0 - settings.crossfeed.clamp(0.0, 1.0) * 0.24);

        // The authored stereo pair lives on a listener-centric spherical shell rather than a
        // horizontal ring. Width still controls the ordinary left/right aperture, while Depth and
        // 3D immersion are allowed to move the wet field continuously around the lateral plane and
        // slightly into the rear hemisphere. The dry programme remains the frontal anchor.
        let half_angle_degrees = (MIN_STEREO_HALF_ANGLE_DEGREES
            + effective_width * STEREO_HALF_ANGLE_RANGE_DEGREES
            + depth * STEREO_DEPTH_AZIMUTH_RANGE_DEGREES
            + immersive * STEREO_IMMERSIVE_AZIMUTH_RANGE_DEGREES)
            .clamp(MIN_STEREO_HALF_ANGLE_DEGREES, MAX_STEREO_HALF_ANGLE_DEGREES);
        let (half_angle_sin, half_angle_cos) = half_angle_degrees.to_radians().sin_cos();

        // Give the two authored channels opposite elevations so the stereo centroid stays near the
        // listener's horizon while the wet field has genuine above/below geometry. This produces
        // signed pinna/elevation cues without inventing extra delayed copies of the programme.
        let elevation_degrees = (depth * STEREO_DEPTH_ELEVATION_RANGE_DEGREES
            + immersive * STEREO_IMMERSIVE_ELEVATION_RANGE_DEGREES)
            .clamp(0.0, MAX_STEREO_ELEVATION_DEGREES);
        let (elevation_sin, elevation_cos) = elevation_degrees.to_radians().sin_cos();

        let distance_meters = MIN_STEREO_DISTANCE_METERS
            + settings.distance.clamp(0.0, 1.0) * STEREO_DISTANCE_RANGE_METERS
            + depth * 0.24;
        // `spread` intentionally stays conservative. Large spread values weaken the directional
        // pinna cue, which made the previous strong presets paradoxically sound less localized.
        let spread = (0.035
            + immersive * 0.14
            + settings.crossfeed.clamp(0.0, 1.0) * 0.07)
            .clamp(0.0, 0.25);
        Self {
            half_angle_sin,
            half_angle_cos,
            elevation_sin,
            elevation_cos,
            distance_meters,
            gain: STEREO_SOURCE_GAIN,
            spread,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EnvironmentSignature {
    settings: EnvironmentSettings,
}

impl EnvironmentSignature {
    fn from_settings(settings: &SpatialSettings) -> Self {
        Self {
            settings: spatial_environment_settings(settings),
        }
    }

    fn settings(self) -> EnvironmentSettings {
        self.settings
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
    virtual_bed: StereoVirtualBed,
    virtual_bed_layout: Option<ChannelLayout>,
    virtual_bed_scratch: Vec<f32>,
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
                elevation_sin: 0.0,
                elevation_cos: 1.0,
                distance_meters: 1.0,
                gain: STEREO_SOURCE_GAIN,
                spread: 0.1,
            },
            environment_signature: None,
            virtual_bed: StereoVirtualBed::new(sample_rate),
            virtual_bed_layout: None,
            virtual_bed_scratch: vec![
                0.0;
                block_frames.saturating_mul(MAX_VIRTUAL_BED_CHANNELS)
            ],
            wet_scratch: vec![0.0; block_frames.saturating_mul(2)],
        })
    }

    pub(crate) fn reset(&mut self) {
        self.engine.reset();
        self.virtual_bed.reset();
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

    pub(crate) fn debug_source_activity(
        &self,
    ) -> Option<[SourceActivity; MAX_DEBUG_SOURCES]> {
        self.engine.debug_source_activity()
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
        let virtual_layout = virtual_bed_layout(settings, trajectory_signature.is_some());
        let wet_mix = stereo_wet_mix(settings, trajectory_signature.is_some());
        if wet_mix <= 1.0e-5 {
            return true;
        }

        if self.virtual_bed_layout != virtual_layout {
            self.virtual_bed.reset();
            self.engine.reset();
            self.virtual_bed_layout = virtual_layout;
        }

        if let Some(layout) = virtual_layout {
            // The native engine owns motion for a speaker bed. Clear the legacy two-source
            // trajectory so there is exactly one sample clock and exactly one spatialization pass.
            self.ensure_trajectory(None);
            apply_scene_motion_settings(&mut self.engine, settings);
            return self.process_virtual_bed(samples, layout, wet_mix);
        }

        self.engine.set_scene_motion(None, 0.10, 1.0, 0.0, true);
        self.ensure_trajectory(trajectory_signature);
        let segment_frames = trajectory_segment_frames(
            self.sample_rate,
            self.block_frames,
            trajectory_signature,
        );

        let dry_mix = 1.0 - wet_mix;
        let total_frames = samples.len() / 2;
        let mut frame_offset = 0usize;

        while frame_offset < total_frames {
            let frames = (total_frames - frame_offset).min(segment_frames);
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
                samples[output_index] =
                    samples[output_index] * dry_mix + self.wet_scratch[index] * wet_mix;
            }
            frame_offset += frames;
        }
        true
    }

    fn process_virtual_bed(
        &mut self,
        samples: &mut [f32],
        layout: ChannelLayout,
        wet_mix: f32,
    ) -> bool {
        let channels = SpeakerLayout::for_layout(layout).channels();
        if channels <= 2 || channels > MAX_VIRTUAL_BED_CHANNELS {
            return false;
        }
        let dry_mix = 1.0 - wet_mix;
        let total_frames = samples.len() / 2;
        let mut frame_offset = 0usize;

        while frame_offset < total_frames {
            let frames = (total_frames - frame_offset).min(self.block_frames);
            let sample_start = frame_offset * 2;
            let sample_end = sample_start + frames * 2;
            let virtual_samples = frames * channels;
            if self
                .virtual_bed
                .render(
                    &samples[sample_start..sample_end],
                    layout,
                    &mut self.virtual_bed_scratch[..virtual_samples],
                )
                .is_none()
            {
                return false;
            }
            if self
                .engine
                .render_interleaved_layout(
                    &self.virtual_bed_scratch[..virtual_samples],
                    layout,
                    &mut self.wet_scratch[..frames * 2],
                )
                .is_err()
            {
                return false;
            }
            for index in 0..frames * 2 {
                let output_index = sample_start + index;
                samples[output_index] =
                    samples[output_index] * dry_mix + self.wet_scratch[index] * wet_mix;
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
        self.engine
            .scene_motion_sample_clock()
            .or_else(|| self.trajectory.as_ref().map(Trajectory::sample_clock))
    }

    #[cfg(test)]
    fn active_virtual_layout(&self) -> Option<ChannelLayout> {
        self.virtual_bed_layout
    }
}

#[inline]
fn virtual_bed_layout(settings: &SpatialSettings, dynamic: bool) -> Option<ChannelLayout> {
    if !settings.enabled {
        return None;
    }

    match settings.virtual_bed {
        VirtualBedMode::Off => None,
        VirtualBedMode::Surround5_1 => Some(ChannelLayout::Surround5_1),
        VirtualBedMode::Surround7_1 => Some(ChannelLayout::Surround7_1),
        VirtualBedMode::Surround5_1_2 => Some(ChannelLayout::Surround5_1_2),
        VirtualBedMode::Surround5_1_4 => Some(ChannelLayout::Surround5_1_4),
        VirtualBedMode::Surround7_1_2 => Some(ChannelLayout::Surround7_1_2),
        VirtualBedMode::Surround7_1_4 => Some(ChannelLayout::Surround7_1_4),
        VirtualBedMode::Auto => {
            let immersive = settings.immersive_3d.clamp(0.0, 1.0);
            let depth = settings.depth.clamp(0.0, 1.0);
            let width = settings.width.clamp(0.0, 1.0);

            if dynamic || immersive >= 0.72 {
                Some(ChannelLayout::Surround7_1_4)
            } else if immersive >= 0.52 || depth >= 0.50 {
                Some(ChannelLayout::Surround5_1_4)
            } else if immersive >= 0.36 {
                Some(ChannelLayout::Surround7_1_2)
            } else if width >= 0.78 && depth >= 0.18 {
                Some(ChannelLayout::Surround7_1)
            } else {
                None
            }
        }
    }
}

#[inline]
fn stereo_wet_mix(settings: &SpatialSettings, dynamic: bool) -> f32 {
    let mix = settings.mix.clamp(0.0, 1.0);
    if !dynamic {
        return mix;
    }
    // Motion intensity should shape the trajectory contribution, not multiply the complete spatial
    // path nearly out of existence. Keep a strong direct binaural bed and let intensity provide the
    // final 18% of dynamic wet strength.
    mix * (0.82 + settings.motion_intensity.clamp(0.0, 1.0) * 0.18)
}

#[inline]
fn trajectory_segment_frames(
    sample_rate: u32,
    block_frames: usize,
    signature: Option<TrajectorySignature>,
) -> usize {
    let block_frames = block_frames.max(1);
    let Some(signature) = signature else {
        return block_frames;
    };
    let speed_hz = if signature.speed_hz.is_finite() {
        signature.speed_hz.abs().clamp(0.005, 2.0)
    } else {
        0.10
    };
    let frames_for_limit =
        (sample_rate.max(1) as f32 * MAX_TRAJECTORY_SEGMENT_DEGREES / (speed_hz * 360.0))
            .floor()
            .max(1.0) as usize;
    block_frames.min(frames_for_limit.max(1))
}

#[inline]
fn stereo_pair(center: SourcePose, field: StereoField) -> (SourcePose, SourcePose) {
    let left_azimuth = rotate_y(center.position, -field.half_angle_sin, field.half_angle_cos);
    let right_azimuth = rotate_y(center.position, field.half_angle_sin, field.half_angle_cos);
    let left_velocity_azimuth =
        rotate_y(center.velocity, -field.half_angle_sin, field.half_angle_cos);
    let right_velocity_azimuth =
        rotate_y(center.velocity, field.half_angle_sin, field.half_angle_cos);

    // Opposite signed elevations preserve a centred stereo image while giving both hemispheres real
    // source geometry. The positions remain on exactly the same radius as the authored centre.
    let left_position = rotate_x(left_azimuth, field.elevation_sin, field.elevation_cos);
    let right_position = rotate_x(right_azimuth, -field.elevation_sin, field.elevation_cos);
    let left_velocity = rotate_x(
        left_velocity_azimuth,
        field.elevation_sin,
        field.elevation_cos,
    );
    let right_velocity = rotate_x(
        right_velocity_azimuth,
        -field.elevation_sin,
        field.elevation_cos,
    );
    (
        SourcePose {
            position: left_position,
            velocity: left_velocity,
            gain: field.gain,
            spread: field.spread,
        },
        SourcePose {
            position: right_position,
            velocity: right_velocity,
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

#[inline]
fn rotate_x(position: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        position.x,
        position.y * cos + position.z * sin,
        -position.y * sin + position.z * cos,
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
    fn front_back_preset_maps_to_audio_clock_front_back() {
        let settings = SpatialPreset::FrontBack.settings();
        let signature = TrajectorySignature::from_settings(&settings).expect("dynamic");
        assert_eq!(signature.kind, TrajectoryKind::FrontBack);
        assert!(signature.radius_meters > MIN_TRAJECTORY_RADIUS_METERS);
    }

    #[test]
    fn dynamic_mix_no_longer_collapses_the_binaural_bed() {
        let settings = SpatialPreset::Orbit8d.settings();
        let wet = stereo_wet_mix(&settings, true);
        assert!(wet > settings.mix * settings.motion_intensity);
        assert!(wet <= settings.mix);
    }

    #[test]
    fn wide_stereo_promotes_to_virtual_seven_one_bed() {
        let settings = SpatialPreset::Wide.settings();
        assert_eq!(
            virtual_bed_layout(&settings, false),
            Some(ChannelLayout::Surround7_1)
        );
    }

    #[test]
    fn cinema_and_immersive_promote_to_height_beds() {
        assert_eq!(
            virtual_bed_layout(&SpatialPreset::Cinema.settings(), false),
            Some(ChannelLayout::Surround5_1_4)
        );
        assert_eq!(
            virtual_bed_layout(&SpatialPreset::Immersive3d.settings(), false),
            Some(ChannelLayout::Surround7_1_4)
        );
    }

    #[test]
    fn every_dynamic_stereo_mode_uses_full_virtual_seven_one_four() {
        for preset in [
            SpatialPreset::Orbit8d,
            SpatialPreset::Orbit360,
            SpatialPreset::Pendulum,
            SpatialPreset::FrontBack,
            SpatialPreset::Planetary,
            SpatialPreset::NearEar,
        ] {
            let settings = preset.settings();
            assert_eq!(
                virtual_bed_layout(&settings, true),
                Some(ChannelLayout::Surround7_1_4)
            );
        }
    }

    #[test]
    fn virtual_bed_off_keeps_dynamic_stereo_on_two_source_trajectory() {
        let mut settings = SpatialPreset::Orbit8d.settings();
        settings.virtual_bed = VirtualBedMode::Off;
        assert_eq!(virtual_bed_layout(&settings, true), None);

        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.25_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.active_virtual_layout(), None);
        assert_eq!(spatializer.sample_clock(), Some(128));
    }

    #[test]
    fn explicit_virtual_bed_modes_preserve_ambiguous_layout_semantics() {
        let mut settings = SpatialPreset::Studio.settings();
        settings.virtual_bed = VirtualBedMode::Surround5_1_2;
        assert_eq!(
            virtual_bed_layout(&settings, false),
            Some(ChannelLayout::Surround5_1_2)
        );
        settings.virtual_bed = VirtualBedMode::Surround7_1_2;
        assert_eq!(
            virtual_bed_layout(&settings, false),
            Some(ChannelLayout::Surround7_1_2)
        );
    }

    #[test]
    fn every_explicit_virtual_bed_mode_maps_without_channel_count_guessing() {
        let modes = [
            (VirtualBedMode::Surround5_1, ChannelLayout::Surround5_1),
            (VirtualBedMode::Surround7_1, ChannelLayout::Surround7_1),
            (VirtualBedMode::Surround5_1_2, ChannelLayout::Surround5_1_2),
            (VirtualBedMode::Surround5_1_4, ChannelLayout::Surround5_1_4),
            (VirtualBedMode::Surround7_1_2, ChannelLayout::Surround7_1_2),
            (VirtualBedMode::Surround7_1_4, ChannelLayout::Surround7_1_4),
        ];
        for (mode, layout) in modes {
            let mut settings = SpatialPreset::Studio.settings();
            settings.virtual_bed = mode;
            assert_eq!(virtual_bed_layout(&settings, false), Some(layout));
        }
    }

    #[test]
    fn stereo_pair_keeps_left_and_right_as_distinct_sources() {
        let field = StereoField::from_settings(&SpatialPreset::Immersive3d.settings());
        let center = SourcePose::new(Vec3::new(0.0, 0.0, field.distance_meters));
        let (left, right) = stereo_pair(center, field);
        assert!(left.position.x < 0.0);
        assert!(right.position.x > 0.0);
        assert!(left.position.y > 0.0);
        assert!(right.position.y < 0.0);
        assert!((left.position.length() - right.position.length()).abs() < 1.0e-5);
        assert!((left.position.length() - field.distance_meters).abs() < 1.0e-5);
    }

    #[test]
    fn immersive_static_field_reaches_the_rear_hemisphere() {
        let field = StereoField::from_settings(&SpatialPreset::Immersive3d.settings());
        let center = SourcePose::new(Vec3::new(0.0, 0.0, field.distance_meters));
        let (left, right) = stereo_pair(center, field);
        assert!(left.position.z < 0.0);
        assert!(right.position.z < 0.0);
    }

    #[test]
    fn stereo_pair_rotates_velocity_with_each_virtual_source() {
        let field = StereoField::from_settings(&SpatialPreset::Immersive3d.settings());
        let center = SourcePose {
            position: Vec3::FORWARD,
            velocity: Vec3::RIGHT,
            gain: 1.0,
            spread: 0.0,
        };
        let (left, right) = stereo_pair(center, field);
        assert!(left.velocity.z > 0.0);
        assert!(right.velocity.z < 0.0);
        assert!((left.velocity.length() - 1.0).abs() < 1.0e-5);
        assert!((right.velocity.length() - 1.0).abs() < 1.0e-5);
    }

    #[test]
    fn high_speed_motion_is_segmented_to_about_one_degree() {
        let signature = TrajectorySignature {
            kind: TrajectoryKind::Orbit360,
            speed_hz: 2.0,
            radius_meters: 1.0,
            clockwise: true,
        };
        let frames = trajectory_segment_frames(44_100, 64, Some(signature));
        let degrees = frames as f32 * signature.speed_hz * 360.0 / 44_100.0;
        assert!(frames < 64);
        assert!(degrees <= MAX_TRAJECTORY_SEGMENT_DEGREES + 1.0e-5);
    }

    #[test]
    fn normal_motion_keeps_default_block_size() {
        let signature = TrajectorySignature {
            kind: TrajectoryKind::Orbit360,
            speed_hz: 0.5,
            radius_meters: 1.0,
            clockwise: true,
        };
        assert_eq!(trajectory_segment_frames(48_000, 64, Some(signature)), 64);
    }

    #[test]
    fn processing_advances_and_reset_rewinds_scene_clock() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.25_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(
            spatializer.active_virtual_layout(),
            Some(ChannelLayout::Surround7_1_4)
        );
        assert_eq!(spatializer.sample_clock(), Some(128));
        spatializer.reset();
        assert_eq!(spatializer.sample_clock(), Some(0));
    }

    #[test]
    fn static_immersive_uses_virtual_seven_one_four_without_motion_clock() {
        let settings = SpatialPreset::Immersive3d.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.20_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(
            spatializer.active_virtual_layout(),
            Some(ChannelLayout::Surround7_1_4)
        );
        assert_eq!(spatializer.sample_clock(), None);
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn debug_snapshot_tracks_virtual_bed_sources() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        spatializer.set_debug_enabled(true);
        let mut samples = vec![0.10_f32; 64 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        let snapshot = spatializer.debug_snapshot().expect("debug snapshot");
        let activity = spatializer
            .debug_source_activity()
            .expect("debug source activity");
        assert_eq!(snapshot.layout, Some(ChannelLayout::Surround7_1_4));
        assert_eq!(snapshot.source_count, 12);
        assert_eq!(snapshot.rendered_frames, 64);
        assert!((activity[0].peak - 0.096).abs() < 1.0e-5);
        assert_eq!(activity[3].peak, 0.0);
    }

    #[test]
    fn wet_and_virtual_workspaces_are_fixed_at_construction() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = StereoSpatializer::new(48_000).expect("engine");
        let wet_ptr = spatializer.wet_scratch.as_ptr();
        let wet_capacity = spatializer.wet_scratch.capacity();
        let virtual_ptr = spatializer.virtual_bed_scratch.as_ptr();
        let virtual_capacity = spatializer.virtual_bed_scratch.capacity();
        let mut samples = vec![0.1_f32; 1024 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.wet_scratch.as_ptr(), wet_ptr);
        assert_eq!(spatializer.wet_scratch.capacity(), wet_capacity);
        assert_eq!(spatializer.virtual_bed_scratch.as_ptr(), virtual_ptr);
        assert_eq!(spatializer.virtual_bed_scratch.capacity(), virtual_capacity);
    }
}
