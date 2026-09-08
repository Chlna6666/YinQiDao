use std::f32::consts::PI;

use crate::{EnvironmentSettings, ListenerPose, SourceKind, SourcePose, Vec3};

pub const MAX_DEBUG_SOURCES: usize = 32;

const SPEED_OF_SOUND_M_S: f32 = 343.0;
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
