use std::f32::consts::PI;

use crate::EnvironmentSettings;

const FDN_LINES: usize = 8;
const LATE_FIELD_EPSILON: f32 = 1.0e-5;
const MAX_DELAY_SECONDS: f32 = 0.105;
const HADAMARD_NORMALIZATION: f32 = 0.353_553_38;
const STEREO_INJECTION_NORMALIZATION: f32 = 0.25;
const DELAY_SECONDS: [f32; FDN_LINES] = [
    0.031_1, 0.037_7, 0.041_9, 0.047_3, 0.053_9, 0.061_1, 0.067_9, 0.073_7,
];
const LEFT_INPUT_SIGNS: [f32; FDN_LINES] = [1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0];
const RIGHT_INPUT_SIGNS: [f32; FDN_LINES] = [1.0, 1.0, -1.0, 1.0, -1.0, -1.0, -1.0, 1.0];
const LEFT_SIGNS: [f32; FDN_LINES] = [1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0];
const RIGHT_SIGNS: [f32; FDN_LINES] = [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0];

#[derive(Clone, Debug)]
struct LateDelayLine {
    buffer: Vec<f32>,
    cursor: usize,
    damping_state: f32,
}

impl LateDelayLine {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity.max(16)],
            cursor: 0,
            damping_state: 0.0,
        }
    }

    #[inline]
    fn read(&self, delay_samples: usize) -> f32 {
        let length = self.buffer.len();
        let delay = delay_samples.clamp(1, length.saturating_sub(1).max(1));
        self.buffer[(self.cursor + length - delay) % length]
    }

    #[inline]
    fn write_advance(&mut self, value: f32) {
        self.buffer[self.cursor] = finite_or_zero(value);
        self.cursor += 1;
        if self.cursor == self.buffer.len() {
            self.cursor = 0;
        }
    }

    fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.cursor = 0;
        self.damping_state = 0.0;
    }
}

/// Low-cost diffuse late field shared by the final stereo mix.
///
/// The six first-order image sources own geometric localization. This FDN intentionally has no
/// source position and runs once after all sources have been accumulated, supplying only decorrelated
/// late energy. All delay storage is allocated at construction; `process_planar` is allocation-free.
#[derive(Clone, Debug)]
pub(crate) struct LateDiffuseField {
    sample_rate: f32,
    lines: [LateDelayLine; FDN_LINES],
    delay_samples: [usize; FDN_LINES],
    damping_alpha: f32,
    feedback_gain: f32,
    wet_gain: f32,
    settings: EnvironmentSettings,
}

impl LateDiffuseField {
    pub(crate) fn new(sample_rate: u32, settings: EnvironmentSettings) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let capacity = (sample_rate * MAX_DELAY_SECONDS).ceil() as usize + 8;
        let mut field = Self {
            sample_rate,
            lines: std::array::from_fn(|_| LateDelayLine::new(capacity)),
            delay_samples: [1; FDN_LINES],
            damping_alpha: 1.0,
            feedback_gain: 0.0,
            wet_gain: 0.0,
            settings: EnvironmentSettings::default(),
        };
        field.set_environment(settings);
        field
    }

    pub(crate) fn set_environment(&mut self, settings: EnvironmentSettings) {
        let settings = sanitize_settings(settings);
        let room_changed = (settings.room_size - self.settings.room_size).abs() > 1.0e-4;
        let disabled = settings.mix <= LATE_FIELD_EPSILON;
        self.settings = settings;

        let room_scale = 0.72 + settings.room_size * 0.45;
        let capacity = self.lines[0].buffer.len();
        for (index, delay_seconds) in DELAY_SECONDS.into_iter().enumerate() {
            self.delay_samples[index] = ((delay_seconds * room_scale * self.sample_rate).round()
                as usize)
                .clamp(8, capacity.saturating_sub(1).max(8));
        }

        let max_cutoff_hz = (self.sample_rate * 0.45).max(120.0);
        let min_cutoff_hz = 3_800.0_f32.min(max_cutoff_hz * 0.80);
        let cutoff_hz = (13_500.0 - settings.damping * 9_000.0)
            .clamp(min_cutoff_hz, max_cutoff_hz);
        self.damping_alpha = 1.0 - (-2.0 * PI * cutoff_hz / self.sample_rate).exp();
        self.feedback_gain = (0.56 + settings.room_size * 0.23).clamp(0.50, 0.82);
        self.wet_gain = settings.mix * 0.38;

        // Changing active delay lengths reads a different part of the fixed history. Resetting on
        // room-size edits avoids turning that control change into a discontinuous old-tail splice.
        if room_changed || disabled {
            self.reset();
        }
    }

    #[inline]
    pub(crate) fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        if frames == 0 || self.wet_gain <= LATE_FIELD_EPSILON {
            return;
        }

        for frame in 0..frames {
            let dry_left = finite_or_zero(left[frame]);
            let dry_right = finite_or_zero(right[frame]);

            let mut delayed = [0.0_f32; FDN_LINES];
            for line in 0..FDN_LINES {
                let raw = self.lines[line].read(self.delay_samples[line]);
                self.lines[line].damping_state +=
                    self.damping_alpha * (raw - self.lines[line].damping_state);
                delayed[line] = finite_or_zero(self.lines[line].damping_state);
            }

            let mut feedback = delayed;
            hadamard8(&mut feedback);
            for line in 0..FDN_LINES {
                let injection = (dry_left * LEFT_INPUT_SIGNS[line]
                    + dry_right * RIGHT_INPUT_SIGNS[line])
                    * STEREO_INJECTION_NORMALIZATION;
                self.lines[line]
                    .write_advance(injection + feedback[line] * self.feedback_gain);
            }

            let late_left = signed_sum(&delayed, &LEFT_SIGNS) * HADAMARD_NORMALIZATION;
            let late_right = signed_sum(&delayed, &RIGHT_SIGNS) * HADAMARD_NORMALIZATION;
            left[frame] = dry_left + late_left * self.wet_gain;
            right[frame] = dry_right + late_right * self.wet_gain;
        }
    }

    pub(crate) fn reset(&mut self) {
        for line in &mut self.lines {
            line.reset();
        }
    }
}

#[inline]
fn hadamard8(values: &mut [f32; FDN_LINES]) {
    let a0 = values[0] + values[1];
    let a1 = values[0] - values[1];
    let a2 = values[2] + values[3];
    let a3 = values[2] - values[3];
    let a4 = values[4] + values[5];
    let a5 = values[4] - values[5];
    let a6 = values[6] + values[7];
    let a7 = values[6] - values[7];

    let b0 = a0 + a2;
    let b1 = a1 + a3;
    let b2 = a0 - a2;
    let b3 = a1 - a3;
    let b4 = a4 + a6;
    let b5 = a5 + a7;
    let b6 = a4 - a6;
    let b7 = a5 - a7;

    values[0] = (b0 + b4) * HADAMARD_NORMALIZATION;
    values[1] = (b1 + b5) * HADAMARD_NORMALIZATION;
    values[2] = (b2 + b6) * HADAMARD_NORMALIZATION;
    values[3] = (b3 + b7) * HADAMARD_NORMALIZATION;
    values[4] = (b0 - b4) * HADAMARD_NORMALIZATION;
    values[5] = (b1 - b5) * HADAMARD_NORMALIZATION;
    values[6] = (b2 - b6) * HADAMARD_NORMALIZATION;
    values[7] = (b3 - b7) * HADAMARD_NORMALIZATION;
}

#[inline]
fn signed_sum(values: &[f32; FDN_LINES], signs: &[f32; FDN_LINES]) -> f32 {
    values[0] * signs[0]
        + values[1] * signs[1]
        + values[2] * signs[2]
        + values[3] * signs[3]
        + values[4] * signs[4]
        + values[5] * signs[5]
        + values[6] * signs[6]
        + values[7] * signs[7]
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
    fn zero_mix_is_bit_exact_bypass() {
        let mut field = LateDiffuseField::new(
            48_000,
            EnvironmentSettings {
                mix: 0.0,
                ..EnvironmentSettings::default()
            },
        );
        let mut left = [0.25_f32, -0.5, 0.125];
        let mut right = [-0.2_f32, 0.4, -0.1];
        let expected_left = left;
        let expected_right = right;
        field.process_planar(&mut left, &mut right);
        assert_eq!(left, expected_left);
        assert_eq!(right, expected_right);
    }

    #[test]
    fn impulse_generates_delayed_diffuse_energy() {
        let mut field = LateDiffuseField::new(
            48_000,
            EnvironmentSettings {
                mix: 0.18,
                room_size: 0.45,
                damping: 0.40,
            },
        );
        let mut left = vec![0.0_f32; 4_096];
        let mut right = vec![0.0_f32; 4_096];
        left[0] = 1.0;
        right[0] = 1.0;
        field.process_planar(&mut left, &mut right);
        assert!(left[800..].iter().any(|sample| sample.abs() > 1.0e-6));
        assert!(right[800..].iter().any(|sample| sample.abs() > 1.0e-6));
        assert!(left.iter().all(|sample| sample.is_finite()));
        assert!(right.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn anti_phase_stereo_still_excites_late_field() {
        let mut field = LateDiffuseField::new(
            48_000,
            EnvironmentSettings {
                mix: 0.18,
                room_size: 0.45,
                damping: 0.40,
            },
        );
        let mut left = vec![0.0_f32; 4_096];
        let mut right = vec![0.0_f32; 4_096];
        left[0] = 1.0;
        right[0] = -1.0;
        field.process_planar(&mut left, &mut right);
        assert!(left[800..].iter().any(|sample| sample.abs() > 1.0e-6));
        assert!(right[800..].iter().any(|sample| sample.abs() > 1.0e-6));
    }

    #[test]
    fn stereo_injection_vectors_are_orthogonal() {
        let dot: f32 = LEFT_INPUT_SIGNS
            .iter()
            .zip(RIGHT_INPUT_SIGNS)
            .map(|(left, right)| left * right)
            .sum();
        assert_eq!(dot, 0.0);
    }

    #[test]
    fn reset_clears_tail_history() {
        let settings = EnvironmentSettings {
            mix: 0.20,
            ..EnvironmentSettings::default()
        };
        let mut field = LateDiffuseField::new(48_000, settings);
        let mut left = vec![0.0_f32; 2_048];
        let mut right = vec![0.0_f32; 2_048];
        left[0] = 1.0;
        right[0] = 1.0;
        field.process_planar(&mut left, &mut right);
        field.reset();

        left.fill(0.0);
        right.fill(0.0);
        field.process_planar(&mut left, &mut right);
        assert!(left.iter().all(|sample| *sample == 0.0));
        assert!(right.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn hadamard_feedback_preserves_vector_energy() {
        let mut values = [0.5, -0.2, 0.1, 0.7, -0.4, 0.3, 0.8, -0.6];
        let before: f32 = values.iter().map(|value| value * value).sum();
        hadamard8(&mut values);
        let after: f32 = values.iter().map(|value| value * value).sum();
        assert!((before - after).abs() < 1.0e-5);
    }

    #[test]
    fn low_sample_rate_keeps_damping_cutoff_valid() {
        let field = LateDiffuseField::new(4_000, EnvironmentSettings::default());
        assert!(field.damping_alpha.is_finite());
        assert!(field.damping_alpha > 0.0);
        assert!(field.damping_alpha <= 1.0);
    }
}