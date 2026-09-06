mod eq;
mod spatial;

pub use eq::{EqPreset, clamp_eq};
pub use spatial::{SpatialPreset, clamp_spatial};

use crate::model::{EqSettings, SpatialSettings};

use eq::EqProcessor;
use spatial::Spatializer;

#[derive(Clone, Debug)]
pub struct AudioProcessor {
    pub(crate) eq: EqProcessor,
    pub(crate) spatial: Spatializer,
    volume: f32,
    stereo_scratch: Vec<f32>,
}

impl AudioProcessor {
    pub fn new(sample_rate: u32, eq: EqSettings, spatial: SpatialSettings, volume: f32) -> Self {
        Self {
            eq: EqProcessor::new(sample_rate, eq),
            spatial: Spatializer::new(sample_rate, spatial),
            volume: volume.clamp(0.0, 1.0),
            stereo_scratch: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn process(&mut self, input: &[f32], input_rate: u32, input_channels: u16) -> Vec<f32> {
        let mut output = Vec::new();
        self.process_into(input, input_rate, input_channels, &mut output);
        output
    }

    pub fn process_into(
        &mut self,
        input: &[f32],
        input_rate: u32,
        input_channels: u16,
        output: &mut Vec<f32>,
    ) {
        let output_rate = self.eq.sample_rate();
        if input_rate == output_rate {
            to_stereo_into(input, input_channels, output);
        } else {
            to_stereo_into(input, input_channels, &mut self.stereo_scratch);
            resample_linear_into(&self.stereo_scratch, input_rate, output_rate, output);
        }
        self.eq.process(output);
        self.spatial.process(output);
        let gain = perceptual_volume_gain(self.volume);
        for sample in output {
            *sample = (*sample * gain).tanh();
        }
    }
}

pub(crate) fn perceptual_volume_gain(volume: f32) -> f32 {
    let volume = volume.clamp(0.0, 1.0);
    volume * volume
}

fn to_stereo_into(input: &[f32], channels: u16, output: &mut Vec<f32>) {
    let channels = channels.max(1) as usize;
    let frames = input.len() / channels;
    output.clear();
    output.reserve(frames.saturating_mul(2));
    for frame in input.chunks_exact(channels) {
        match channels {
            1 => {
                output.push(frame[0]);
                output.push(frame[0]);
            }
            _ => {
                output.push(frame[0]);
                output.push(frame[1]);
            }
        }
    }
}

fn resample_linear_into(input: &[f32], input_rate: u32, output_rate: u32, output: &mut Vec<f32>) {
    if input_rate == output_rate || input.len() < 4 {
        output.clear();
        output.extend_from_slice(input);
        return;
    }
    let input_frames = input.len() / 2;
    let output_frames = ((input_frames as u64 * output_rate as u64) / input_rate as u64) as usize;
    output.clear();
    output.reserve(output_frames.saturating_mul(2));
    for output_frame in 0..output_frames {
        let source_position = output_frame as f32 * input_rate as f32 / output_rate as f32;
        let source_frame = source_position.floor() as usize;
        let next_frame = (source_frame + 1).min(input_frames - 1);
        let fraction = source_position.fract();
        for channel in 0..2 {
            let first = input[source_frame * 2 + channel];
            let second = input[next_frame * 2 + channel];
            output.push(first + (second - first) * fraction);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_spatial_and_limiter_keep_output_bounded() {
        let mut eq = EqPreset::Rock.settings();
        eq.preamp_db = 99.0;
        let spatial = SpatialPreset::Immersive3d.settings();
        let mut processor = AudioProcessor::new(48_000, clamp_eq(eq), spatial, 1.0);
        let output = processor.process(&vec![8.0; 192], 48_000, 2);
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn process_into_reuses_allocations() {
        let mut processor = AudioProcessor::new(
            48_000,
            EqSettings::default(),
            SpatialSettings::default(),
            1.0,
        );
        let input = vec![0.25; 96];
        let mut output = Vec::new();
        processor.process_into(&input, 24_000, 1, &mut output);
        let output_ptr = output.as_ptr();
        let output_capacity = output.capacity();
        let scratch_capacity = processor.stereo_scratch.capacity();
        processor.process_into(&input, 24_000, 1, &mut output);
        assert_eq!(output.as_ptr(), output_ptr);
        assert_eq!(output.capacity(), output_capacity);
        assert_eq!(processor.stereo_scratch.capacity(), scratch_capacity);
    }

    #[test]
    fn perceptual_volume_curve_preserves_low_level_headroom() {
        assert_eq!(perceptual_volume_gain(0.0), 0.0);
        assert!((perceptual_volume_gain(0.5) - 0.25).abs() < f32::EPSILON);
        assert_eq!(perceptual_volume_gain(1.0), 1.0);
    }
}
