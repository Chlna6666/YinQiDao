#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SourceActivity {
    /// Absolute sample peak in linear full-scale units.
    pub peak: f32,
    /// Root-mean-square level in linear full-scale units.
    pub rms: f32,
}

impl SourceActivity {
    #[inline]
    pub fn peak_dbfs(self) -> f32 {
        linear_to_dbfs(self.peak)
    }

    #[inline]
    pub fn rms_dbfs(self) -> f32 {
        linear_to_dbfs(self.rms)
    }
}

const MAX_ACTIVITY_CHANNELS: usize = 32;

/// Analyze an interleaved PCM block into caller-owned per-channel activity slots.
///
/// This helper is intended for opt-in debug capture. It performs no allocation, locking or I/O and
/// treats non-finite samples as silence so malformed debug input cannot poison persistent telemetry.
/// The returned count is the number of channel slots actually populated, capped to the engine's
/// fixed debug-source capacity.
pub fn analyze_interleaved_activity(
    input: &[f32],
    channels: usize,
    output: &mut [SourceActivity],
) -> usize {
    if channels == 0 || input.len() % channels != 0 {
        output.fill(SourceActivity::default());
        return 0;
    }

    let measured_channels = channels.min(output.len()).min(MAX_ACTIVITY_CHANNELS);
    output.fill(SourceActivity::default());
    if measured_channels == 0 {
        return 0;
    }

    let frames = input.len() / channels;
    if frames == 0 {
        return measured_channels;
    }

    // RMS sums deliberately use f64. This code is debug-only and the wider accumulator avoids
    // visible RMS drift on long decoder chunks without changing the realtime render state.
    let mut sums = [0.0_f64; MAX_ACTIVITY_CHANNELS];

    for frame in input.chunks_exact(channels) {
        for channel in 0..measured_channels {
            let sample = frame[channel];
            let finite = if sample.is_finite() { sample } else { 0.0 };
            let magnitude = finite.abs();
            output[channel].peak = output[channel].peak.max(magnitude);
            let value = f64::from(finite);
            sums[channel] += value * value;
        }
    }

    let reciprocal_frames = 1.0 / frames as f64;
    for channel in 0..measured_channels {
        output[channel].rms = (sums[channel] * reciprocal_frames).sqrt() as f32;
    }
    measured_channels
}

#[inline]
fn linear_to_dbfs(value: f32) -> f32 {
    let value = if value.is_finite() { value.abs() } else { 0.0 };
    if value <= 1.0e-9 {
        -180.0
    } else {
        20.0 * value.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_interleaved_channel_activity_without_allocation() {
        let input = [
            0.5, 0.25, 0.0,
            -0.5, 0.25, 1.0,
            0.25, -0.25, -1.0,
            -0.25, -0.25, 0.5,
        ];
        let mut activity = [SourceActivity::default(); 3];
        assert_eq!(analyze_interleaved_activity(&input, 3, &mut activity), 3);
        assert!((activity[0].peak - 0.5).abs() < f32::EPSILON);
        assert!((activity[1].peak - 0.25).abs() < f32::EPSILON);
        assert!((activity[2].peak - 1.0).abs() < f32::EPSILON);
        assert!((activity[0].rms - 0.395_284_7).abs() < 1.0e-6);
        assert!((activity[1].rms - 0.25).abs() < 1.0e-6);
        assert!((activity[2].rms - 0.75).abs() < 1.0e-6);
    }

    #[test]
    fn non_finite_samples_are_treated_as_silence() {
        let input = [f32::NAN, f32::INFINITY, 0.5, -0.5];
        let mut activity = [SourceActivity::default(); 2];
        assert_eq!(analyze_interleaved_activity(&input, 2, &mut activity), 2);
        assert!((activity[0].peak - 0.5).abs() < f32::EPSILON);
        assert_eq!(activity[1].peak, 0.5);
        assert!(activity.iter().all(|value| value.rms.is_finite()));
    }

    #[test]
    fn malformed_layout_clears_output() {
        let mut activity = [SourceActivity { peak: 1.0, rms: 1.0 }; 2];
        assert_eq!(analyze_interleaved_activity(&[0.0; 3], 2, &mut activity), 0);
        assert_eq!(activity, [SourceActivity::default(); 2]);
    }

    #[test]
    fn oversized_layout_is_truncated_to_fixed_capacity() {
        let channels = 64;
        let frames = 2;
        let input = vec![0.25_f32; channels * frames];
        let mut activity = [SourceActivity::default(); 64];
        assert_eq!(
            analyze_interleaved_activity(&input, channels, &mut activity),
            MAX_ACTIVITY_CHANNELS
        );
        assert!(activity[..MAX_ACTIVITY_CHANNELS]
            .iter()
            .all(|value| (value.peak - 0.25).abs() < f32::EPSILON));
        assert!(activity[MAX_ACTIVITY_CHANNELS..]
            .iter()
            .all(|value| *value == SourceActivity::default()));
    }

    #[test]
    fn dbfs_floor_is_finite() {
        assert_eq!(SourceActivity::default().peak_dbfs(), -180.0);
        assert!((SourceActivity { peak: 1.0, rms: 0.5 }.rms_dbfs() + 6.020_6).abs() < 1.0e-3);
    }
}
