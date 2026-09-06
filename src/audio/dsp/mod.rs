mod eq;
mod spatial;

pub use eq::{EqPreset, clamp_eq};
pub use spatial::{SpatialPreset, clamp_spatial};

use crate::model::{EqSettings, SpatialSettings};

use eq::EqProcessor;
use spatial::Spatializer;

#[derive(Clone, Debug)]
struct StreamingLinearResampler {
    input_rate: u32,
    output_rate: u32,
    input_frames: u64,
    next_source_position: f64,
    previous_frame: Option<[f32; 2]>,
}

impl StreamingLinearResampler {
    fn new(output_rate: u32) -> Self {
        Self {
            input_rate: 0,
            output_rate: output_rate.max(1),
            input_frames: 0,
            next_source_position: 0.0,
            previous_frame: None,
        }
    }

    fn reset(&mut self) {
        self.input_rate = 0;
        self.input_frames = 0;
        self.next_source_position = 0.0;
        self.previous_frame = None;
    }

    fn configure(&mut self, input_rate: u32, output_rate: u32) {
        let input_rate = input_rate.max(1);
        let output_rate = output_rate.max(1);
        if self.input_rate != input_rate || self.output_rate != output_rate {
            self.input_rate = input_rate;
            self.output_rate = output_rate;
            self.input_frames = 0;
            self.next_source_position = 0.0;
            self.previous_frame = None;
        }
    }

    fn process_into(
        &mut self,
        input: &[f32],
        input_rate: u32,
        output_rate: u32,
        output: &mut Vec<f32>,
    ) {
        let input_rate = input_rate.max(1);
        let output_rate = output_rate.max(1);
        if input_rate == output_rate {
            self.reset();
            self.output_rate = output_rate;
            output.clear();
            output.extend_from_slice(input);
            return;
        }

        self.configure(input_rate, output_rate);
        output.clear();

        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let estimated_frames = ((frames as u64)
            .saturating_mul(u64::from(output_rate))
            .saturating_add(u64::from(input_rate) - 1)
            / u64::from(input_rate))
            .saturating_add(2) as usize;
        output.reserve(estimated_frames.saturating_mul(2));

        let base_frame = self.input_frames;
        let last_frame = base_frame.saturating_add(frames as u64 - 1);
        let source_step = f64::from(input_rate) / f64::from(output_rate);
        const EPSILON: f64 = 1.0e-9;

        while self.next_source_position <= last_frame as f64 + EPSILON {
            let source_floor = self.next_source_position.floor();
            let source_index = source_floor.max(0.0) as u64;
            let fraction = (self.next_source_position - source_floor).clamp(0.0, 1.0);

            let Some(first) = self.frame_at(input, base_frame, source_index) else {
                break;
            };
            let second = if fraction <= EPSILON {
                first
            } else {
                let Some(next) = self.frame_at(input, base_frame, source_index.saturating_add(1))
                else {
                    break;
                };
                next
            };

            output.push(first[0] + (second[0] - first[0]) * fraction as f32);
            output.push(first[1] + (second[1] - first[1]) * fraction as f32);
            self.next_source_position += source_step;
        }

        let last_index = (frames - 1) * 2;
        self.previous_frame = Some([input[last_index], input[last_index + 1]]);
        self.input_frames = self.input_frames.saturating_add(frames as u64);
    }

    fn frame_at(&self, input: &[f32], base_frame: u64, absolute_index: u64) -> Option<[f32; 2]> {
        if absolute_index < base_frame {
            return (absolute_index.saturating_add(1) == base_frame)
                .then_some(self.previous_frame)
                .flatten();
        }
        let relative = absolute_index.saturating_sub(base_frame) as usize;
        let sample_index = relative.checked_mul(2)?;
        Some([*input.get(sample_index)?, *input.get(sample_index + 1)?])
    }
}

#[derive(Clone, Debug)]
pub struct AudioProcessor {
    pub(crate) eq: EqProcessor,
    pub(crate) spatial: Spatializer,
    volume: f32,
    stereo_scratch: Vec<f32>,
    resampler: StreamingLinearResampler,
}

impl AudioProcessor {
    pub fn new(sample_rate: u32, eq: EqSettings, spatial: SpatialSettings, volume: f32) -> Self {
        let sample_rate = sample_rate.max(1);
        Self {
            eq: EqProcessor::new(sample_rate, eq),
            spatial: Spatializer::new(sample_rate, spatial),
            volume: volume.clamp(0.0, 1.0),
            stereo_scratch: Vec::new(),
            resampler: StreamingLinearResampler::new(sample_rate),
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
        to_stereo_into(input, input_channels, &mut self.stereo_scratch);
        self.resampler.process_into(
            &self.stereo_scratch,
            input_rate,
            output_rate,
            output,
        );
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
    fn streaming_resampler_matches_one_shot_timeline_across_chunk_boundaries() {
        let mut input = Vec::new();
        for frame in 0..257 {
            let value = frame as f32 / 257.0;
            input.extend_from_slice(&[value, -value]);
        }

        let mut one_shot = StreamingLinearResampler::new(48_000);
        let mut expected = Vec::new();
        one_shot.process_into(&input, 44_100, 48_000, &mut expected);

        let mut streaming = StreamingLinearResampler::new(48_000);
        let mut actual = Vec::new();
        let mut chunk = Vec::new();
        for range in [0..74, 74..161, 161..257] {
            streaming.process_into(
                &input[range.start * 2..range.end * 2],
                44_100,
                48_000,
                &mut chunk,
            );
            actual.extend_from_slice(&chunk);
        }

        assert_eq!(actual.len(), expected.len());
        assert!(actual
            .iter()
            .zip(expected.iter())
            .all(|(left, right)| (left - right).abs() < 1.0e-5));
    }

    #[test]
    fn perceptual_volume_curve_preserves_low_level_headroom() {
        assert_eq!(perceptual_volume_gain(0.0), 0.0);
        assert!((perceptual_volume_gain(0.5) - 0.25).abs() < f32::EPSILON);
        assert_eq!(perceptual_volume_gain(1.0), 1.0);
    }
}
