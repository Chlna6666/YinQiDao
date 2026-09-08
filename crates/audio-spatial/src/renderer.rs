use std::f32::consts::PI;

use crate::{ListenerPose, SourceKind, SourcePose, SpatialError, Vec3, delay::CubicDelayLine};

const SPEED_OF_SOUND_M_S: f32 = 343.0;
const HEAD_RADIUS_M: f32 = 0.0875;
const COMMON_CAUSAL_DELAY_SAMPLES: f32 = 2.0;
const MAX_DISTANCE_METERS: f32 = 32.0;
const NEAR_FIELD_FULL_METERS: f32 = 0.25;
const NEAR_FIELD_FADE_METERS: f32 = 1.20;
const AIR_ABSORPTION_START_METERS: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default)]
struct RenderParameters {
    left_delay: f32,
    right_delay: f32,
    left_gain: f32,
    right_gain: f32,
    left_filter_alpha: f32,
    right_filter_alpha: f32,
}

impl RenderParameters {
    #[inline]
    fn step_to(self, end: Self, frames: usize) -> RenderParameterStep {
        if frames == 0 {
            return RenderParameterStep::default();
        }
        // `end` is the pose/parameter state at the next block boundary (n + frames), not at the
        // final audible sample (n + frames - 1). Use an end-exclusive ramp so the hot loop consumes
        // exactly `frames` sample-clock intervals before reaching that boundary.
        let scale = 1.0 / frames as f32;
        RenderParameterStep {
            left_delay: (end.left_delay - self.left_delay) * scale,
            right_delay: (end.right_delay - self.right_delay) * scale,
            left_gain: (end.left_gain - self.left_gain) * scale,
            right_gain: (end.right_gain - self.right_gain) * scale,
            left_filter_alpha: (end.left_filter_alpha - self.left_filter_alpha) * scale,
            right_filter_alpha: (end.right_filter_alpha - self.right_filter_alpha) * scale,
        }
    }

    #[inline]
    fn advance(&mut self, step: RenderParameterStep) {
        self.left_delay += step.left_delay;
        self.right_delay += step.right_delay;
        self.left_gain += step.left_gain;
        self.right_gain += step.right_gain;
        self.left_filter_alpha += step.left_filter_alpha;
        self.right_filter_alpha += step.right_filter_alpha;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderParameterStep {
    left_delay: f32,
    right_delay: f32,
    left_gain: f32,
    right_gain: f32,
    left_filter_alpha: f32,
    right_filter_alpha: f32,
}

#[derive(Clone, Debug)]
struct SourceState {
    delay: CubicDelayLine,
    filter_left: f32,
    filter_right: f32,
    lfe_state: f32,
    scratch_left: Vec<f32>,
    scratch_right: Vec<f32>,
    cached_pose: Option<SourcePose>,
    cached_listener: Option<ListenerPose>,
    cached_parameters: RenderParameters,
}

impl SourceState {
    fn new(delay_capacity: usize, block_frames: usize) -> Self {
        Self {
            delay: CubicDelayLine::new(delay_capacity),
            filter_left: 0.0,
            filter_right: 0.0,
            lfe_state: 0.0,
            scratch_left: vec![0.0; block_frames],
            scratch_right: vec![0.0; block_frames],
            cached_pose: None,
            cached_listener: None,
            cached_parameters: RenderParameters::default(),
        }
    }

    #[inline]
    fn parameters_for(
        &mut self,
        sample_rate: f32,
        pose: SourcePose,
        listener: ListenerPose,
    ) -> RenderParameters {
        if self.cached_pose == Some(pose) && self.cached_listener == Some(listener) {
            return self.cached_parameters;
        }
        let parameters = parameters_for_pose(sample_rate, pose, listener);
        self.cached_pose = Some(pose);
        self.cached_listener = Some(listener);
        self.cached_parameters = parameters;
        parameters
    }

    fn reset(&mut self) {
        self.delay.reset();
        self.filter_left = 0.0;
        self.filter_right = 0.0;
        self.lfe_state = 0.0;
        self.scratch_left.fill(0.0);
        self.scratch_right.fill(0.0);
        self.cached_pose = None;
        self.cached_listener = None;
        self.cached_parameters = RenderParameters::default();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CpuRenderer {
    sample_rate: f32,
    block_frames: usize,
    sources: Vec<SourceState>,
    lfe_alpha: f32,
}

impl CpuRenderer {
    pub(crate) fn new(
        sample_rate: u32,
        block_frames: usize,
        max_sources: usize,
    ) -> Result<Self, SpatialError> {
        if sample_rate == 0 {
            return Err(SpatialError::InvalidSampleRate);
        }
        if block_frames == 0 {
            return Err(SpatialError::InvalidBlockFrames);
        }
        if max_sources == 0 {
            return Err(SpatialError::InvalidSourceCapacity);
        }
        let sample_rate_f32 = sample_rate as f32;
        let maximum_itd_seconds = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * (PI * 0.5 + 1.0);
        let delay_capacity = (sample_rate_f32 * (maximum_itd_seconds + 0.0015)).ceil() as usize + 8;
        let lfe_alpha = 1.0 - (-2.0 * PI * 120.0 / sample_rate_f32).exp();
        let mut sources = Vec::with_capacity(max_sources);
        for _ in 0..max_sources {
            sources.push(SourceState::new(delay_capacity, block_frames));
        }
        Ok(Self {
            sample_rate: sample_rate_f32,
            block_frames,
            sources,
            lfe_alpha,
        })
    }

    pub(crate) fn source_capacity(&self) -> usize {
        self.sources.len()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_strided_source(
        &mut self,
        source_index: usize,
        input: &[f32],
        input_stride: usize,
        input_channel: usize,
        frames: usize,
        start_pose: SourcePose,
        end_pose: SourcePose,
        listener: ListenerPose,
        kind: SourceKind,
        mix_left: &mut [f32],
        mix_right: &mut [f32],
    ) -> Result<(), SpatialError> {
        if source_index >= self.sources.len() {
            return Err(SpatialError::SourceCapacityExceeded);
        }
        debug_assert!(frames <= self.block_frames);
        debug_assert!(frames <= mix_left.len());
        debug_assert!(frames <= mix_right.len());
        let lfe_alpha = self.lfe_alpha;
        let sample_rate = self.sample_rate;
        let state = &mut self.sources[source_index];

        match kind {
            SourceKind::FullRange => {
                // Fixed speaker layouts reuse the exact same pose/listener for every block. Cache
                // the expensive pose solve; trajectories normally reuse the previous end as start.
                let start = state.parameters_for(sample_rate, start_pose, listener);
                let end = if start_pose == end_pose {
                    start
                } else {
                    state.parameters_for(sample_rate, end_pose, listener)
                };

                let mut parameters = start;
                let parameter_step = start.step_to(end, frames);
                let mut input_index = input_channel;
                for frame in 0..frames {
                    let sample = sanitize_sample(input[input_index]);
                    input_index += input_stride;
                    state.delay.push(sample);
                    let (delayed_left, delayed_right) = state
                        .delay
                        .read_pair(parameters.left_delay, parameters.right_delay);
                    state.filter_left +=
                        parameters.left_filter_alpha * (delayed_left - state.filter_left);
                    state.filter_right +=
                        parameters.right_filter_alpha * (delayed_right - state.filter_right);
                    state.scratch_left[frame] = state.filter_left * parameters.left_gain;
                    state.scratch_right[frame] = state.filter_right * parameters.right_gain;
                    parameters.advance(parameter_step);
                }
            }
            SourceKind::Lfe => {
                // LFE is direction-independent: only a causal delay, 120 Hz low-pass and gain ramp.
                let mut gain = finite_or_zero(start_pose.gain).clamp(0.0, 4.0);
                let end_gain = finite_or_zero(end_pose.gain).clamp(0.0, 4.0);
                let gain_step = if frames == 0 {
                    0.0
                } else {
                    (end_gain - gain) / frames as f32
                };
                let mut input_index = input_channel;
                for frame in 0..frames {
                    let sample = sanitize_sample(input[input_index]);
                    input_index += input_stride;
                    state.delay.push(sample);
                    let delayed = state.delay.read(COMMON_CAUSAL_DELAY_SAMPLES);
                    state.lfe_state += lfe_alpha * (delayed - state.lfe_state);
                    let value = state.lfe_state * gain;
                    state.scratch_left[frame] = value;
                    state.scratch_right[frame] = value;
                    gain += gain_step;
                }
            }
        }

        yinqidao_audio_simd::mix_accumulate(
            &mut mix_left[..frames],
            &state.scratch_left[..frames],
            1.0,
        );
        yinqidao_audio_simd::mix_accumulate(
            &mut mix_right[..frames],
            &state.scratch_right[..frames],
            1.0,
        );
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        for state in &mut self.sources {
            state.reset();
        }
    }
}

#[inline]
fn parameters_for_pose(
    sample_rate: f32,
    pose: SourcePose,
    listener: ListenerPose,
) -> RenderParameters {
    // Object metadata and future debug injection are not allowed to poison persistent delay/IIR
    // state. Non-finite pose coordinates collapse to the listener origin and then use FORWARD as
    // the direction fallback; non-finite gains become silence.
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
    let spread = finite_or_zero(pose.spread).clamp(0.0, 1.0);
    let lateral = azimuth.sin().abs() * (1.0 - spread * 0.72);
    let front = azimuth.cos();
    let rear = (-front).max(0.0);
    let elevation_sin = elevation.sin();
    let elevation_up = elevation_sin.max(0.0);
    let elevation_down = (-elevation_sin).max(0.0);

    // Woodworth spherical-head ITD remains the far-field baseline. For close sources blend a small
    // amount of exact point-to-ear geometric path difference. This is a deterministic parametric
    // near-field correction, not a measured HRTF/HRTF database substitute.
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
    let (left_delay, right_delay) = if azimuth >= 0.0 {
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

    // Base spherical-head ILD plus a close-range ear-distance correction. The square root keeps the
    // point-source 1/r geometry from becoming an exaggerated hard-pan effect next to the listener.
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

    // Conservative distance law: avoid near-field gain boost/headroom loss, but attenuate remote
    // sources smoothly. Frequency-dependent air loss is represented by a high-frequency cutoff that
    // only starts after one metre and remains subtle at normal music-listening distances.
    let distance_gain = if distance <= 1.0 {
        1.0
    } else {
        1.0 / (1.0 + (distance - 1.0) * 0.34)
    };
    let air_amount = smoothstep01(
        (distance - AIR_ABSORPTION_START_METERS)
            / (MAX_DISTANCE_METERS - AIR_ABSORPTION_START_METERS),
    );
    let rear_gain = 1.0 - rear * 0.06;
    let elevation_gain = 1.0 - elevation_down * 0.025;
    let common_gain = finite_or_zero(pose.gain).clamp(0.0, 4.0)
        * distance_gain
        * rear_gain
        * elevation_gain;

    // Front/back and elevation cues stay deliberately parametric and smooth. Rear, lower and far
    // positions progressively reduce upper-band energy; overhead sources receive a smaller tilt.
    let near_cutoff = (20_000.0
        - rear * 4_200.0
        - elevation_up * 900.0
        - elevation_down * 1_700.0
        - air_amount * 6_500.0)
        .clamp(4_500.0, 20_000.0);
    let far_cutoff = (near_cutoff - lateral * 9_500.0).clamp(3_200.0, 20_000.0);
    let near_alpha = one_pole_alpha(sample_rate, near_cutoff);
    let far_alpha = one_pole_alpha(sample_rate, far_cutoff);

    if azimuth >= 0.0 {
        RenderParameters {
            left_delay,
            right_delay,
            left_gain: common_gain * far_ear_attenuation,
            right_gain: common_gain,
            left_filter_alpha: far_alpha,
            right_filter_alpha: near_alpha,
        }
    } else {
        RenderParameters {
            left_delay,
            right_delay,
            left_gain: common_gain,
            right_gain: common_gain * far_ear_attenuation,
            left_filter_alpha: near_alpha,
            right_filter_alpha: far_alpha,
        }
    }
}

#[inline]
fn one_pole_alpha(sample_rate: f32, cutoff_hz: f32) -> f32 {
    1.0 - (-2.0 * PI * cutoff_hz.min(sample_rate * 0.45) / sample_rate).exp()
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

#[inline]
fn sanitize_sample(sample: f32) -> f32 {
    finite_or_zero(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_source_delays_and_attenuates_far_left_ear() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::RIGHT),
            ListenerPose::identity(),
        );
        assert!(parameters.left_delay > parameters.right_delay);
        assert!(parameters.left_gain < parameters.right_gain);
        assert!(parameters.left_filter_alpha < parameters.right_filter_alpha);
    }

    #[test]
    fn front_source_is_symmetric() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::FORWARD),
            ListenerPose::identity(),
        );
        assert!((parameters.left_delay - parameters.right_delay).abs() < 1.0e-6);
        assert!((parameters.left_gain - parameters.right_gain).abs() < 1.0e-6);
    }

    #[test]
    fn near_lateral_source_has_stronger_ild_than_far_lateral_source() {
        let near = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.22, 0.0, 0.0)),
            ListenerPose::identity(),
        );
        let far = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(4.0, 0.0, 0.0)),
            ListenerPose::identity(),
        );
        let near_ratio = near.left_gain / near.right_gain.max(1.0e-8);
        let far_ratio = far.left_gain / far.right_gain.max(1.0e-8);
        assert!(near_ratio < far_ratio);
    }

    #[test]
    fn distant_front_source_has_more_air_absorption() {
        let near = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.0, 0.0, 1.0)),
            ListenerPose::identity(),
        );
        let far = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.0, 0.0, 24.0)),
            ListenerPose::identity(),
        );
        assert!(far.left_filter_alpha < near.left_filter_alpha);
        assert!(far.right_filter_alpha < near.right_filter_alpha);
    }

    #[test]
    fn source_parameter_cache_tracks_pose_and_listener() {
        let mut state = SourceState::new(64, 64);
        let listener = ListenerPose::identity();
        let pose = SourcePose::new(Vec3::RIGHT);
        let first = state.parameters_for(48_000.0, pose, listener);
        assert_eq!(state.cached_pose, Some(pose));
        assert_eq!(state.cached_listener, Some(listener));
        let second = state.parameters_for(48_000.0, pose, listener);
        assert!((first.left_delay - second.left_delay).abs() < f32::EPSILON);
        assert!((first.right_gain - second.right_gain).abs() < f32::EPSILON);

        let moved = SourcePose::new(Vec3::LEFT);
        let third = state.parameters_for(48_000.0, moved, listener);
        assert_eq!(state.cached_pose, Some(moved));
        assert!(third.right_delay > third.left_delay);
    }

    #[test]
    fn end_exclusive_parameter_ramp_reaches_next_block_start() {
        let start = RenderParameters {
            left_delay: 2.0,
            right_delay: 8.0,
            left_gain: 0.4,
            right_gain: 1.0,
            left_filter_alpha: 0.2,
            right_filter_alpha: 0.8,
        };
        let end = RenderParameters {
            left_delay: 10.0,
            right_delay: 3.0,
            left_gain: 0.9,
            right_gain: 0.5,
            left_filter_alpha: 0.7,
            right_filter_alpha: 0.3,
        };
        let frames = 64;
        let step = start.step_to(end, frames);
        let mut current = start;
        for _ in 0..frames {
            current.advance(step);
        }

        assert!((current.left_delay - end.left_delay).abs() < 1.0e-4);
        assert!((current.right_delay - end.right_delay).abs() < 1.0e-4);
        assert!((current.left_gain - end.left_gain).abs() < 1.0e-5);
        assert!((current.right_gain - end.right_gain).abs() < 1.0e-5);
        assert!((current.left_filter_alpha - end.left_filter_alpha).abs() < 1.0e-5);
        assert!((current.right_filter_alpha - end.right_filter_alpha).abs() < 1.0e-5);
    }

    #[test]
    fn single_frame_parameter_ramp_advances_to_next_block_start_after_sample() {
        let start = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::RIGHT),
            ListenerPose::identity(),
        );
        let end = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::LEFT),
            ListenerPose::identity(),
        );
        let step = start.step_to(end, 1);
        let mut current = start;
        current.advance(step);
        assert!((current.left_delay - end.left_delay).abs() < 1.0e-5);
        assert!((current.right_delay - end.right_delay).abs() < 1.0e-5);
        assert!((current.left_gain - end.left_gain).abs() < 1.0e-5);
        assert!((current.right_gain - end.right_gain).abs() < 1.0e-5);
    }

    #[test]
    fn non_finite_pcm_isolated_before_delay_state() {
        assert_eq!(sanitize_sample(f32::NAN), 0.0);
        assert_eq!(sanitize_sample(f32::INFINITY), 0.0);
        assert_eq!(sanitize_sample(f32::NEG_INFINITY), 0.0);
        assert_eq!(sanitize_sample(0.25), 0.25);
    }

    #[test]
    fn non_finite_pose_does_not_produce_non_finite_parameters() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose {
                position: Vec3::new(f32::NAN, f32::INFINITY, 1.0),
                gain: f32::NAN,
                spread: f32::NAN,
                ..SourcePose::default()
            },
            ListenerPose::identity(),
        );
        assert!(parameters.left_delay.is_finite());
        assert!(parameters.right_delay.is_finite());
        assert!(parameters.left_gain.is_finite());
        assert!(parameters.right_gain.is_finite());
        assert!(parameters.left_filter_alpha.is_finite());
        assert!(parameters.right_filter_alpha.is_finite());
    }
}
