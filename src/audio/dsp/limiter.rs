const DEFAULT_CEILING_DBFS: f32 = -0.30;
const DEFAULT_RELEASE_MS: f32 = 90.0;
const MIN_RELEASE_MS: f32 = 10.0;
const MAX_RELEASE_MS: f32 = 2_000.0;
const PEAK_EPSILON: f32 = 1.0e-12;

/// Zero-lookahead linked-stereo peak safety limiter.
///
/// Both channels share one gain envelope, so limiting cannot pull the stereo image toward the
/// quieter side. Overs are attenuated on the current frame (instantaneous attack); recovery uses a
/// one-pole release. The processor owns only scalar state and performs no allocation, locking or I/O.
#[derive(Clone, Debug)]
pub(crate) struct StereoPeakLimiter {
    ceiling: f32,
    release_alpha: f32,
    gain: f32,
}

impl StereoPeakLimiter {
    pub(crate) fn new(sample_rate: u32) -> Self {
        Self::with_parameters(sample_rate, DEFAULT_CEILING_DBFS, DEFAULT_RELEASE_MS)
    }

    fn with_parameters(sample_rate: u32, ceiling_dbfs: f32, release_ms: f32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let ceiling_dbfs = finite_or(ceiling_dbfs, DEFAULT_CEILING_DBFS).clamp(-6.0, -0.01);
        let release_ms = finite_or(release_ms, DEFAULT_RELEASE_MS)
            .clamp(MIN_RELEASE_MS, MAX_RELEASE_MS);
        let ceiling = 10.0_f32.powf(ceiling_dbfs / 20.0);
        let release_seconds = release_ms * 0.001;
        let release_alpha = 1.0 - (-1.0 / (release_seconds * sample_rate)).exp();
        Self {
            ceiling,
            release_alpha: release_alpha.clamp(0.0, 1.0),
            gain: 1.0,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Apply input gain and peak limiting in one scalar pass.
    ///
    /// `input_gain` is expected to be the player's perceptual volume gain. Folding it into the
    /// limiter avoids hard-clipping a hot post-spatial signal before the user volume is applied.
    pub(crate) fn process_interleaved_stereo(&mut self, samples: &mut [f32], input_gain: f32) {
        let input_gain = finite_or(input_gain, 0.0).clamp(0.0, 1.0);
        let mut frames = samples.chunks_exact_mut(2);
        for frame in &mut frames {
            let left = sanitize_sample(frame[0]) * input_gain;
            let right = sanitize_sample(frame[1]) * input_gain;
            let peak = left.abs().max(right.abs());
            let required_gain = if peak > self.ceiling {
                (self.ceiling / peak.max(PEAK_EPSILON)).clamp(0.0, 1.0)
            } else {
                1.0
            };

            if required_gain < self.gain {
                // Instantaneous attack: the current sample pair is already protected.
                self.gain = required_gain;
            } else {
                // Smooth release toward unity. Because the detector is linked, this scalar is used
                // for both channels and therefore preserves instantaneous L/R balance.
                self.gain += self.release_alpha * (1.0 - self.gain);
                self.gain = self.gain.min(required_gain).clamp(0.0, 1.0);
            }

            frame[0] = left * self.gain;
            frame[1] = right * self.gain;
        }

        // Stereo output should always be even-length, but keep a safe deterministic tail behavior
        // rather than letting malformed input retain NaN/Inf or bypass the current envelope.
        if let Some(sample) = frames.into_remainder().first_mut() {
            let value = sanitize_sample(*sample) * input_gain;
            let peak = value.abs();
            let required_gain = if peak > self.ceiling {
                (self.ceiling / peak.max(PEAK_EPSILON)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if required_gain < self.gain {
                self.gain = required_gain;
            } else {
                self.gain += self.release_alpha * (1.0 - self.gain);
                self.gain = self.gain.min(required_gain).clamp(0.0, 1.0);
            }
            *sample = value * self.gain;
        }
    }

    #[cfg(test)]
    fn ceiling(&self) -> f32 {
        self.ceiling
    }

    #[cfg(test)]
    fn gain(&self) -> f32 {
        self.gain
    }
}

#[inline]
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[inline]
fn sanitize_sample(value: f32) -> f32 {
    finite_or(value, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_ceiling_is_transparent_at_unity_gain() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut samples = [0.25_f32, -0.40, 0.50, -0.20];
        let expected = samples;
        limiter.process_interleaved_stereo(&mut samples, 1.0);
        assert_eq!(samples, expected);
        assert!((limiter.gain() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn over_ceiling_uses_one_linked_gain_for_both_channels() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut samples = [2.0_f32, 1.0];
        limiter.process_interleaved_stereo(&mut samples, 1.0);
        assert!(samples[0].abs() <= limiter.ceiling() + 1.0e-6);
        assert!(samples[1].abs() <= limiter.ceiling() + 1.0e-6);
        assert!((samples[0] / samples[1] - 2.0).abs() < 1.0e-5);
        assert!(limiter.gain() < 1.0);
    }

    #[test]
    fn release_recovers_smoothly_without_jumping_to_unity() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut hot = [2.0_f32, -2.0];
        limiter.process_interleaved_stereo(&mut hot, 1.0);
        let reduced = limiter.gain();
        let mut quiet = [0.1_f32, -0.1];
        limiter.process_interleaved_stereo(&mut quiet, 1.0);
        assert!(limiter.gain() > reduced);
        assert!(limiter.gain() < 1.0);
    }

    #[test]
    fn volume_gain_is_applied_before_peak_detection() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut samples = [1.5_f32, -1.5];
        limiter.process_interleaved_stereo(&mut samples, 0.5);
        assert_eq!(samples, [0.75, -0.75]);
        assert!((limiter.gain() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn non_finite_samples_are_isolated() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut samples = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.25];
        limiter.process_interleaved_stereo(&mut samples, 1.0);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[1], 0.0);
        assert_eq!(samples[2], 0.0);
    }

    #[test]
    fn reset_restores_unity_envelope() {
        let mut limiter = StereoPeakLimiter::new(48_000);
        let mut hot = [4.0_f32, 4.0];
        limiter.process_interleaved_stereo(&mut hot, 1.0);
        assert!(limiter.gain() < 1.0);
        limiter.reset();
        assert!((limiter.gain() - 1.0).abs() < f32::EPSILON);
    }
}
