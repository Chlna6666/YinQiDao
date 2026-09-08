use std::f32::consts::PI;

use crate::{ListenerPose, SourceKind, SourcePose, SpatialError, delay::CubicDelayLine};

const SPEED_OF_SOUND_M_S: f32 = 343.0;
const HEAD_RADIUS_M: f32 = 0.0875;
const COMMON_CAUSAL_DELAY_SAMPLES: f32 = 2.0;
const MAX_DISTANCE_METERS: f32 = 32.0;

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
    fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            left_delay: self.left_delay + (other.left_delay - self.left_delay) * t,
            right_delay: self.right_delay + (other.right_delay - self.right_delay) * t,
            left_gain: self.left_gain + (other.left_gain - self.left_gain) * t,
            right_gain: self.right_gain + (other.right_gain - self.right_gain) * t,
            left_filter_alpha: self.left_filter_alpha + (other.left_filter_alpha - self.left_filter_alpha) * t,
            right_filter_alpha: self.right_filter_alpha + (other.right_filter_alpha - self.right_filter_alpha) * t,
        }
    }
}

#[derive(Clone, Debug)]
struct SourceState {
    delay: CubicDelayLine,
    filter_left: f32,
    filter_right: f32,
    lfe_state: f32,
    scratch_left: Vec<f32>,
    scratch_right: Vec<f32>,
}

impl SourceState {
    fn new(delay_capacity: usize, block_frames: usize) -> Self {
        Self {
            delay: CubicDelayLine::new(delay_capacity), filter_left: 0.0, filter_right: 0.0,
            lfe_state: 0.0, scratch_left: vec![0.0; block_frames], scratch_right: vec![0.0; block_frames],
        }
    }
    fn reset(&mut self) {
        self.delay.reset(); self.filter_left = 0.0; self.filter_right = 0.0; self.lfe_state = 0.0;
        self.scratch_left.fill(0.0); self.scratch_right.fill(0.0);
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
    pub(crate) fn new(sample_rate: u32, block_frames: usize, max_sources: usize) -> Result<Self, SpatialError> {
        if sample_rate == 0 { return Err(SpatialError::InvalidSampleRate); }
        if block_frames == 0 { return Err(SpatialError::InvalidBlockFrames); }
        if max_sources == 0 { return Err(SpatialError::InvalidSourceCapacity); }
        let sample_rate_f32 = sample_rate as f32;
        let maximum_itd_seconds = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * (PI * 0.5 + 1.0);
        let delay_capacity = (sample_rate_f32 * (maximum_itd_seconds + 0.0015)).ceil() as usize + 8;
        let lfe_alpha = 1.0 - (-2.0 * PI * 120.0 / sample_rate_f32).exp();
        let mut sources = Vec::with_capacity(max_sources);
        for _ in 0..max_sources { sources.push(SourceState::new(delay_capacity, block_frames)); }
        Ok(Self { sample_rate: sample_rate_f32, block_frames, sources, lfe_alpha })
    }

    pub(crate) fn source_capacity(&self) -> usize { self.sources.len() }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_strided_source(
        &mut self, source_index: usize, input: &[f32], input_stride: usize, input_channel: usize,
        frames: usize, start_pose: SourcePose, end_pose: SourcePose, listener: ListenerPose,
        kind: SourceKind, mix_left: &mut [f32], mix_right: &mut [f32],
    ) -> Result<(), SpatialError> {
        if source_index >= self.sources.len() { return Err(SpatialError::SourceCapacityExceeded); }
        debug_assert!(frames <= self.block_frames);
        debug_assert!(frames <= mix_left.len());
        debug_assert!(frames <= mix_right.len());
        let start = parameters_for_pose(self.sample_rate, start_pose, listener);
        let end = parameters_for_pose(self.sample_rate, end_pose, listener);
        let lfe_alpha = self.lfe_alpha;
        let state = &mut self.sources[source_index];
        let denominator = frames.saturating_sub(1).max(1) as f32;

        match kind {
            SourceKind::FullRange => {
                for frame in 0..frames {
                    let sample = input[frame * input_stride + input_channel];
                    state.delay.push(sample);
                    let t = frame as f32 / denominator;
                    let parameters = start.lerp(end, t);
                    let delayed_left = state.delay.read(parameters.left_delay);
                    let delayed_right = state.delay.read(parameters.right_delay);
                    state.filter_left += parameters.left_filter_alpha * (delayed_left - state.filter_left);
                    state.filter_right += parameters.right_filter_alpha * (delayed_right - state.filter_right);
                    state.scratch_left[frame] = state.filter_left * parameters.left_gain;
                    state.scratch_right[frame] = state.filter_right * parameters.right_gain;
                }
            }
            SourceKind::Lfe => {
                let gain_start = start_pose.gain;
                let gain_end = end_pose.gain;
                for frame in 0..frames {
                    let sample = input[frame * input_stride + input_channel];
                    state.delay.push(sample);
                    let delayed = state.delay.read(COMMON_CAUSAL_DELAY_SAMPLES);
                    state.lfe_state += lfe_alpha * (delayed - state.lfe_state);
                    let t = frame as f32 / denominator;
                    let gain = gain_start + (gain_end - gain_start) * t;
                    let value = state.lfe_state * gain;
                    state.scratch_left[frame] = value;
                    state.scratch_right[frame] = value;
                }
            }
        }

        yinqidao_audio_simd::mix_accumulate(&mut mix_left[..frames], &state.scratch_left[..frames], 1.0);
        yinqidao_audio_simd::mix_accumulate(&mut mix_right[..frames], &state.scratch_right[..frames], 1.0);
        Ok(())
    }

    pub(crate) fn reset(&mut self) { for state in &mut self.sources { state.reset(); } }
}

#[inline]
fn parameters_for_pose(sample_rate: f32, pose: SourcePose, listener: ListenerPose) -> RenderParameters {
    let relative = pose.position - listener.position;
    let distance = relative.length().clamp(0.05, MAX_DISTANCE_METERS);
    let direction = relative.normalized_or(crate::Vec3::FORWARD);
    let (right, up, forward) = listener.basis();
    let local_right = direction.dot(right);
    let local_up = direction.dot(up);
    let local_forward = direction.dot(forward);
    let azimuth = local_right.atan2(local_forward);
    let horizontal = (local_right * local_right + local_forward * local_forward).sqrt();
    let elevation = local_up.atan2(horizontal);
    let spread = pose.spread.clamp(0.0, 1.0);
    let lateral = azimuth.sin().abs() * (1.0 - spread * 0.72);
    let front = azimuth.cos();
    let rear = (-front).max(0.0);
    let height = elevation.sin().abs();

    // Woodworth spherical-head ITD approximation: parameterized binaural localization, not measured HRTF.
    let theta = azimuth.abs().clamp(0.0, PI);
    let path_term = if theta <= PI * 0.5 { theta + theta.sin() } else { PI - theta + theta.sin() };
    let itd_samples = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * path_term * sample_rate;
    let (left_delay, right_delay) = if azimuth >= 0.0 {
        (COMMON_CAUSAL_DELAY_SAMPLES + itd_samples, COMMON_CAUSAL_DELAY_SAMPLES)
    } else {
        (COMMON_CAUSAL_DELAY_SAMPLES, COMMON_CAUSAL_DELAY_SAMPLES + itd_samples)
    };

    let far_ear_attenuation = (1.0 - lateral * (0.18 + 0.10 / distance.max(0.35))).clamp(0.62, 1.0);
    let distance_gain = if distance <= 1.0 { 1.0 } else { 1.0 / (1.0 + (distance - 1.0) * 0.34) };
    let rear_gain = 1.0 - rear * 0.08;
    let height_gain = 1.0 - height * 0.035;
    let common_gain = pose.gain.max(0.0) * distance_gain * rear_gain * height_gain;
    let near_cutoff = (20_000.0 - rear * 4_500.0 - height * 1_500.0).clamp(4_500.0, 20_000.0);
    let far_cutoff = (near_cutoff - lateral * 9_500.0).clamp(3_500.0, 20_000.0);
    let near_alpha = one_pole_alpha(sample_rate, near_cutoff);
    let far_alpha = one_pole_alpha(sample_rate, far_cutoff);

    if azimuth >= 0.0 {
        RenderParameters { left_delay, right_delay, left_gain: common_gain * far_ear_attenuation, right_gain: common_gain, left_filter_alpha: far_alpha, right_filter_alpha: near_alpha }
    } else {
        RenderParameters { left_delay, right_delay, left_gain: common_gain, right_gain: common_gain * far_ear_attenuation, left_filter_alpha: near_alpha, right_filter_alpha: far_alpha }
    }
}

#[inline]
fn one_pole_alpha(sample_rate: f32, cutoff_hz: f32) -> f32 {
    1.0 - (-2.0 * PI * cutoff_hz.min(sample_rate * 0.45) / sample_rate).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vec3;
    #[test]
    fn right_source_delays_and_attenuates_far_left_ear() {
        let parameters = parameters_for_pose(48_000.0, SourcePose::new(Vec3::RIGHT), ListenerPose::identity());
        assert!(parameters.left_delay > parameters.right_delay);
        assert!(parameters.left_gain < parameters.right_gain);
        assert!(parameters.left_filter_alpha < parameters.right_filter_alpha);
    }
    #[test]
    fn front_source_is_symmetric() {
        let parameters = parameters_for_pose(48_000.0, SourcePose::new(Vec3::FORWARD), ListenerPose::identity());
        assert!((parameters.left_delay - parameters.right_delay).abs() < 1.0e-6);
        assert!((parameters.left_gain - parameters.right_gain).abs() < 1.0e-6);
    }
}
