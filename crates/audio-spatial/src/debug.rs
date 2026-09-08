use std::f32::consts::PI;

use crate::{EnvironmentSettings, ListenerPose, SourceKind, SourcePose, Vec3};
use crate::environment::{EARLY_REFLECTION_TAP_COUNT, ReflectionWall, SPEED_OF_SOUND_M_S};
use crate::image_source::source_reflection_descriptors;

pub const MAX_DEBUG_SOURCES: usize = 32;
pub const MAX_DEBUG_REFLECTION_SOURCES: usize = 12;
pub const REFLECTIONS_PER_DEBUG_SOURCE: usize = EARLY_REFLECTION_TAP_COUNT;
pub const MAX_DEBUG_REFLECTIONS: usize =
    MAX_DEBUG_REFLECTION_SOURCES * REFLECTIONS_PER_DEBUG_SOURCE;

const HEAD_RADIUS_M: f32 = 0.0875;
const COMMON_CAUSAL_DELAY_SAMPLES: f32 = 2.0;
const MAX_DISTANCE_METERS: f32 = 32.0;
const NEAR_FIELD_FULL_METERS: f32 = 0.25;
const NEAR_FIELD_FADE_METERS: f32 = 1.20;
const AIR_ABSORPTION_START_METERS: f32 = 1.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialDebugSourceKind {
    FullRange,
    Lfe,
}

impl Default for SpatialDebugSourceKind {
    fn default() -> Self {
        Self::FullRange
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpatialDebugSource {
    pub active: bool,
    pub source_index: u16,
    pub kind: SpatialDebugSourceKind,
    pub position: Vec3,
    pub velocity: Vec3,
    pub gain: f32,
    pub spread: f32,
    pub azimuth_degrees: f32,
    pub elevation_degrees: f32,
    pub distance_meters: f32,
    pub left_delay_samples: f32,
    pub right_delay_samples: f32,
    pub itd_samples: f32,
    /// Signed ILD in dB. Positive means the right ear is louder than the left ear.
    pub ild_db: f32,
    pub left_gain: f32,
    pub right_gain: f32,
    pub near_field_amount: f32,
    pub head_shadow_amount: f32,
    pub air_absorption_amount: f32,
    pub direct_contribution: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialDebugReflectionWall {
    Left,
    Right,
    Front,
    Rear,
}

impl Default for SpatialDebugReflectionWall {
    fn default() -> Self {
        Self::Front
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpatialDebugReflection {
    pub active: bool,
    pub source_index: u16,
    pub tap_index: u8,
    pub wall: SpatialDebugReflectionWall,
    pub image_position: Vec3,
    pub bounce_position: Vec3,
    /// Full first-order source -> wall -> listener image-source path length.
    pub path_length_meters: f32,
    /// Extra distance relative to the direct source/listener path.
    pub excess_path_meters: f32,
    /// Extra delay used by the realtime renderer; direct programme latency is not added.
    pub excess_delay_samples: f32,
    pub delay_milliseconds: f32,
    pub wall_reflectance: f32,
    /// Approximate rendered reflection contribution after direction/distance binaural gains.
    pub wet_contribution: f32,
    pub arrival_azimuth_degrees: f32,
    pub arrival_elevation_degrees: f32,
    pub left_delay_samples: f32,
    pub right_delay_samples: f32,
    pub left_gain: f32,
    pub right_gain: f32,
    /// Compatibility aliases for the existing GPUI while it migrates to the full matrix fields.
    pub virtual_position: Vec3,
    pub delay_samples: u32,
    pub gain: f32,
    pub cross_ear: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpatialDebugSnapshot {
    pub sequence: u64,
    pub sample_rate: u32,
    pub rendered_frames: u64,
    pub listener: ListenerPose,
    pub environment: EnvironmentSettings,
    pub environment_contribution: f32,
    pub source_count: usize,
    pub sources: [SpatialDebugSource; MAX_DEBUG_SOURCES],
    pub reflection_count: usize,
    pub reflections: [SpatialDebugReflection; MAX_DEBUG_REFLECTIONS],
}

impl SpatialDebugSnapshot {
    pub const fn new(sample_rate: u32) -> Self {
        Self {
            sequence: 0,
            sample_rate,
            rendered_frames: 0,
            listener: ListenerPose::identity(),
            environment: EnvironmentSettings {
                mix: 0.0,
                room_size: 0.0,
                damping: 0.0,
            },
            environment_contribution: 0.0,
            source_count: 0,
            sources: [SpatialDebugSource {
                active: false,
                source_index: 0,
                kind: SpatialDebugSourceKind::FullRange,
                position: Vec3::ZERO,
                velocity: Vec3::ZERO,
                gain: 0.0,
                spread: 0.0,
                azimuth_degrees: 0.0,
                elevation_degrees: 0.0,
                distance_meters: 0.0,
                left_delay_samples: 0.0,
                right_delay_samples: 0.0,
                itd_samples: 0.0,
                ild_db: 0.0,
                left_gain: 0.0,
                right_gain: 0.0,
                near_field_amount: 0.0,
                head_shadow_amount: 0.0,
                air_absorption_amount: 0.0,
                direct_contribution: 0.0,
            }; MAX_DEBUG_SOURCES],
            reflection_count: 0,
            reflections: [SpatialDebugReflection {
                active: false,
                source_index: 0,
                tap_index: 0,
                wall: SpatialDebugReflectionWall::Front,
                image_position: Vec3::ZERO,
                bounce_position: Vec3::ZERO,
                path_length_meters: 0.0,
                excess_path_meters: 0.0,
                excess_delay_samples: 0.0,
                delay_milliseconds: 0.0,
                wall_reflectance: 0.0,
                wet_contribution: 0.0,
                arrival_azimuth_degrees: 0.0,
                arrival_elevation_degrees: 0.0,
                left_delay_samples: 0.0,
                right_delay_samples: 0.0,
                left_gain: 0.0,
                right_gain: 0.0,
                virtual_position: Vec3::ZERO,
                delay_samples: 0,
                gain: 0.0,
                cross_ear: false,
            }; MAX_DEBUG_REFLECTIONS],
        }
    }

    pub(crate) fn begin_capture(
        &mut self,
        listener: ListenerPose,
        environment: EnvironmentSettings,
    ) {
        self.listener = listener;
        self.environment = environment;
        self.environment_contribution = environment.mix.clamp(0.0, 1.0);
        self.source_count = 0;
        self.reflection_count = 0;
    }

    pub(crate) fn record_source(
        &mut self,
        source_index: usize,
        kind: SourceKind,
        pose: SourcePose,
    ) {
        if source_index >= MAX_DEBUG_SOURCES {
            return;
        }
        self.sources[source_index] = analyze_source(
            self.sample_rate.max(1) as f32,
            source_index,
            kind,
            pose,
            self.listener,
        );
        self.source_count = self.source_count.max(source_index + 1);

        if source_index < MAX_DEBUG_REFLECTION_SOURCES {
            let base = source_index * REFLECTIONS_PER_DEBUG_SOURCE;
            self.reflections[base..base + REFLECTIONS_PER_DEBUG_SOURCE]
                .fill(SpatialDebugReflection::default());
            self.reflection_count = self
                .reflection_count
                .max(base + REFLECTIONS_PER_DEBUG_SOURCE);
            if matches!(kind, SourceKind::FullRange) {
                self.capture_reflections_for_source(source_index, pose);
            }
        }
    }

    pub(crate) fn finish_capture(&mut self, frames: usize) {
        self.rendered_frames = self.rendered_frames.saturating_add(frames as u64);
        self.sequence = self.sequence.wrapping_add(1);
    }

    pub(crate) fn reset_timeline(
        &mut self,
        listener: ListenerPose,
        environment: EnvironmentSettings,
    ) {
        self.listener = listener;
        self.environment = environment;
        self.environment_contribution = environment.mix.clamp(0.0, 1.0);
        self.rendered_frames = 0;
        self.source_count = 0;
        self.reflection_count = 0;
    }

    fn capture_reflections_for_source(&mut self, source_index: usize, pose: SourcePose) {
        let sample_rate = self.sample_rate.max(1) as f32;
        let descriptors =
            source_reflection_descriptors(sample_rate, pose, self.listener, self.environment);
        let base = source_index * REFLECTIONS_PER_DEBUG_SOURCE;
        for (tap_index, descriptor) in descriptors.into_iter().enumerate() {
            let reflected_pose = SourcePose {
                position: descriptor.image_position,
                velocity: pose.velocity,
                gain: finite_or_zero(pose.gain).clamp(0.0, 4.0)
                    * self.environment.mix.clamp(0.0, 0.45)
                    * descriptor.wall_reflectance,
                spread: finite_or_zero(pose.spread).clamp(0.0, 1.0),
            };
            let arrival = analyze_source(
                sample_rate,
                source_index,
                SourceKind::FullRange,
                reflected_pose,
                self.listener,
            );
            let excess_delay_samples = descriptor.excess_delay_samples.max(0.0);
            let rounded_delay = excess_delay_samples.round().min(u32::MAX as f32) as u32;
            let slot = base + tap_index;
            self.reflections[slot] = SpatialDebugReflection {
                active: self.environment.mix > 1.0e-5 && descriptor.wall_reflectance > 0.0,
                source_index: source_index.min(u16::MAX as usize) as u16,
                tap_index: tap_index.min(u8::MAX as usize) as u8,
                wall: match descriptor.wall {
                    ReflectionWall::Left => SpatialDebugReflectionWall::Left,
                    ReflectionWall::Right => SpatialDebugReflectionWall::Right,
                    ReflectionWall::Front => SpatialDebugReflectionWall::Front,
                    ReflectionWall::Rear => SpatialDebugReflectionWall::Rear,
                },
                image_position: descriptor.image_position,
                bounce_position: descriptor.bounce_position,
                path_length_meters: descriptor.path_length_meters,
                excess_path_meters: descriptor.excess_path_meters,
                excess_delay_samples,
                delay_milliseconds: excess_delay_samples / sample_rate * 1_000.0,
                wall_reflectance: descriptor.wall_reflectance,
                wet_contribution: ((arrival.left_gain + arrival.right_gain) * 0.5).max(0.0),
                arrival_azimuth_degrees: arrival.azimuth_degrees,
                arrival_elevation_degrees: arrival.elevation_degrees,
                left_delay_samples: arrival.left_delay_samples + excess_delay_samples,
                right_delay_samples: arrival.right_delay_samples + excess_delay_samples,
                left_gain: arrival.left_gain,
                right_gain: arrival.right_gain,
                virtual_position: descriptor.bounce_position,
                delay_samples: rounded_delay,
                gain: descriptor.wall_reflectance,
                cross_ear: false,
            };
        }
    }
}

impl Default for SpatialDebugSnapshot {
    fn default() -> Self {
        Self::new(0)
    }
}

fn analyze_source(
    sample_rate: f32,
    source_index: usize,
    kind: SourceKind,
    pose: SourcePose,
    listener: ListenerPose,
) -> SpatialDebugSource {
    let source_kind = match kind {
        SourceKind::FullRange => SpatialDebugSourceKind::FullRange,
        SourceKind::Lfe => SpatialDebugSourceKind::Lfe,
    };
    let raw_relative = pose.position - listener.position;
    let relative = Vec3::new(
        finite_or_zero(raw_relative.x),
        finite_or_zero(raw_relative.y),
        finite_or_zero(raw_relative.z),
    );
    let distance = relative.length().clamp(0.05, MAX_DISTANCE_METERS);
    let direction = relative.normalized_or(Vec3::FORWARD);
    let (right, up, forward) = listener.basis();
    let local_right = direction.dot(right);
    let local_up = direction.dot(up);
    let local_forward = direction.dot(forward);
    let azimuth = local_right.atan2(local_forward);
    let horizontal = (local_right * local_right + local_forward * local_forward).sqrt();
    let elevation = local_up.atan2(horizontal);
    let gain = finite_or_zero(pose.gain).clamp(0.0, 4.0);
    let spread = finite_or_zero(pose.spread).clamp(0.0, 1.0);

    if matches!(kind, SourceKind::Lfe) {
        return SpatialDebugSource {
            active: true,
            source_index: source_index.min(u16::MAX as usize) as u16,
            kind: source_kind,
            position: pose.position,
            velocity: pose.velocity,
            gain,
            spread,
            azimuth_degrees: azimuth.to_degrees(),
            elevation_degrees: elevation.to_degrees(),
            distance_meters: distance,
            left_delay_samples: COMMON_CAUSAL_DELAY_SAMPLES,
            right_delay_samples: COMMON_CAUSAL_DELAY_SAMPLES,
            itd_samples: 0.0,
            ild_db: 0.0,
            left_gain: gain,
            right_gain: gain,
            near_field_amount: 0.0,
            head_shadow_amount: 0.0,
            air_absorption_amount: 0.0,
            direct_contribution: gain,
        };
    }

    let lateral = azimuth.sin().abs() * (1.0 - spread * 0.72);
    let front = azimuth.cos();
    let rear = (-front).max(0.0);
    let elevation_sin = elevation.sin();
    let elevation_down = (-elevation_sin).max(0.0);

    let theta = azimuth.abs().clamp(0.0, PI);
    let path_term = if theta <= PI * 0.5 {
        theta + theta.sin()
    } else {
        PI - theta + theta.sin()
    };
    let far_itd_samples = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * path_term * sample_rate;

    let local_right_m = relative.dot(right);
    let local_up_m = relative.dot(up);
    let local_forward_m = relative.dot(forward);
    let left_ear_distance = ((local_right_m + HEAD_RADIUS_M).powi(2)
        + local_up_m * local_up_m
        + local_forward_m * local_forward_m)
        .sqrt()
        .max(0.03);
    let right_ear_distance = ((local_right_m - HEAD_RADIUS_M).powi(2)
        + local_up_m * local_up_m
        + local_forward_m * local_forward_m)
        .sqrt()
        .max(0.03);
    let geometric_itd_samples =
        (left_ear_distance - right_ear_distance).abs() / SPEED_OF_SOUND_M_S * sample_rate;
    let near_field_amount = 1.0
        - smoothstep01(
            (distance - NEAR_FIELD_FULL_METERS)
                / (NEAR_FIELD_FADE_METERS - NEAR_FIELD_FULL_METERS),
        );
    let itd_samples = far_itd_samples
        + (geometric_itd_samples - far_itd_samples) * (near_field_amount * 0.35);
    let (left_delay_samples, right_delay_samples) = if azimuth >= 0.0 {
        (
            COMMON_CAUSAL_DELAY_SAMPLES + itd_samples,
            COMMON_CAUSAL_DELAY_SAMPLES,
        )
    } else {
        (
            COMMON_CAUSAL_DELAY_SAMPLES,
            COMMON_CAUSAL_DELAY_SAMPLES + itd_samples,
        )
    };

    let base_far_ear_attenuation =
        (1.0 - lateral * (0.18 + 0.10 / distance.max(0.35))).clamp(0.62, 1.0);
    let (near_ear_distance, far_ear_distance) = if azimuth >= 0.0 {
        (right_ear_distance, left_ear_distance)
    } else {
        (left_ear_distance, right_ear_distance)
    };
    let geometric_far_ear_attenuation =
        (near_ear_distance / far_ear_distance).clamp(0.30, 1.0).sqrt();
    let far_ear_attenuation = base_far_ear_attenuation
        * (1.0
            + (geometric_far_ear_attenuation - 1.0) * (near_field_amount * 0.55));
    let distance_gain = if distance <= 1.0 {
        1.0
    } else {
        1.0 / (1.0 + (distance - 1.0) * 0.34)
    };
    let air_absorption_amount = smoothstep01(
        (distance - AIR_ABSORPTION_START_METERS)
            / (MAX_DISTANCE_METERS - AIR_ABSORPTION_START_METERS),
    );
    let common_gain = gain * distance_gain * (1.0 - rear * 0.06) * (1.0 - elevation_down * 0.025);
    let (left_gain, right_gain) = if azimuth >= 0.0 {
        (common_gain * far_ear_attenuation, common_gain)
    } else {
        (common_gain, common_gain * far_ear_attenuation)
    };
    let ild_db = 20.0 * ((right_gain + 1.0e-8) / (left_gain + 1.0e-8)).log10();

    SpatialDebugSource {
        active: true,
        source_index: source_index.min(u16::MAX as usize) as u16,
        kind: source_kind,
        position: pose.position,
        velocity: pose.velocity,
        gain,
        spread,
        azimuth_degrees: azimuth.to_degrees(),
        elevation_degrees: elevation.to_degrees(),
        distance_meters: distance,
        left_delay_samples,
        right_delay_samples,
        itd_samples,
        ild_db,
        left_gain,
        right_gain,
        near_field_amount,
        head_shadow_amount: (1.0 - far_ear_attenuation).clamp(0.0, 1.0),
        air_absorption_amount,
        direct_contribution: ((left_gain + right_gain) * 0.5).max(0.0),
    }
}

#[inline]
fn smoothstep01(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_snapshot_has_no_dynamic_storage() {
        let snapshot = SpatialDebugSnapshot::new(48_000);
        assert_eq!(snapshot.sources.len(), MAX_DEBUG_SOURCES);
        assert_eq!(snapshot.reflections.len(), 48);
        assert_eq!(snapshot.source_count, 0);
    }

    #[test]
    fn right_source_reports_positive_signed_ild() {
        let mut snapshot = SpatialDebugSnapshot::new(48_000);
        snapshot.begin_capture(ListenerPose::identity(), EnvironmentSettings::default());
        snapshot.record_source(0, SourceKind::FullRange, SourcePose::new(Vec3::RIGHT));
        assert_eq!(snapshot.source_count, 1);
        assert!(snapshot.sources[0].ild_db > 0.0);
        assert!(snapshot.sources[0].left_delay_samples > snapshot.sources[0].right_delay_samples);
    }

    #[test]
    fn reflection_matrix_tracks_source_and_real_bounce_geometry() {
        let mut snapshot = SpatialDebugSnapshot::new(48_000);
        snapshot.begin_capture(ListenerPose::identity(), EnvironmentSettings::default());
        snapshot.record_source(
            0,
            SourceKind::FullRange,
            SourcePose::new(Vec3::new(0.25, 0.0, 1.0)),
        );
        snapshot.record_source(
            1,
            SourceKind::FullRange,
            SourcePose::new(Vec3::new(-0.25, 0.0, 1.0)),
        );
        assert_eq!(snapshot.reflection_count, 8);
        assert_eq!(snapshot.reflections[0].source_index, 0);
        assert_eq!(snapshot.reflections[4].source_index, 1);
        assert!(snapshot.reflections[0].active);
        assert!(snapshot.reflections[0].path_length_meters > 0.0);
        assert!(snapshot.reflections[0].excess_delay_samples > 0.0);
        assert!(snapshot.reflections[0].left_delay_samples > 0.0);
        assert!(snapshot.reflections[0].right_delay_samples > 0.0);
        assert_eq!(snapshot.reflections[0].virtual_position, snapshot.reflections[0].bounce_position);
    }

    #[test]
    fn lfe_reflection_group_is_explicitly_inactive() {
        let mut snapshot = SpatialDebugSnapshot::new(48_000);
        snapshot.begin_capture(ListenerPose::identity(), EnvironmentSettings::default());
        snapshot.record_source(3, SourceKind::Lfe, SourcePose::default());
        assert_eq!(snapshot.reflection_count, 16);
        assert!(snapshot.reflections[12..16]
            .iter()
            .all(|reflection| !reflection.active));
    }

    #[test]
    fn capture_is_truncated_without_growing_storage() {
        let mut snapshot = SpatialDebugSnapshot::new(48_000);
        snapshot.begin_capture(ListenerPose::identity(), EnvironmentSettings::default());
        snapshot.record_source(
            MAX_DEBUG_SOURCES + 4,
            SourceKind::FullRange,
            SourcePose::default(),
        );
        assert_eq!(snapshot.source_count, 0);
    }
}
