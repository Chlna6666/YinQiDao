mod eq;
mod spatial;

pub use eq::{EqPreset, clamp_eq};
pub use spatial::{SpatialPreset, clamp_spatial};

use crate::model::{EqSettings, SpatialSettings};

use super::debug::{
    AudioDebugMonitorMode, audio_debug_enabled, audio_debug_monitor_mode,
    capture_audio_debug_frame,
};
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
    source_debug_scratch: Vec<f32>,
    eq_debug_scratch: Vec<f32>,
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
            source_debug_scratch: Vec::new(),
            eq_debug_scratch: Vec::new(),
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

        // Decode-domain multichannel audio is reduced to a binaural stereo reference before the
        // player EQ. In particular this keeps AV3A 7.1.4 / 12ch content from silently dropping
        // channels 3..12 as the old frame[0]/frame[1] fallback did.
        to_stereo_into(input, input_channels, &mut self.stereo_scratch);
        self.resampler.process_into(
            &self.stereo_scratch,
            input_rate,
            output_rate,
            output,
        );

        let debug_enabled = audio_debug_enabled();
        if debug_enabled {
            copy_reuse(output, &mut self.source_debug_scratch);
        }

        self.eq.process(output);
        if debug_enabled {
            copy_reuse(output, &mut self.eq_debug_scratch);
        }

        self.spatial.process(output);

        if debug_enabled {
            capture_audio_debug_frame(
                &self.source_debug_scratch,
                &self.eq_debug_scratch,
                output,
                output_rate,
            );

            // A/B/C is a real listening comparison, not just a graph selector. Selection happens
            // before the common volume stage so loudness differences are not introduced by three
            // independent output gains.
            match audio_debug_monitor_mode() {
                AudioDebugMonitorMode::Source => {
                    output.clear();
                    output.extend_from_slice(&self.source_debug_scratch);
                }
                AudioDebugMonitorMode::PostEq => {
                    output.clear();
                    output.extend_from_slice(&self.eq_debug_scratch);
                }
                AudioDebugMonitorMode::PostSpatial => {}
            }
        }

        let gain = perceptual_volume_gain(self.volume);
        for sample in output {
            // Normal PCM must stay linear. Only constrain true overs here; the output conversion
            // performs the final device-format guard as well.
            *sample = (*sample * gain).clamp(-1.0, 1.0);
        }
    }
}

#[inline]
fn copy_reuse(source: &[f32], destination: &mut Vec<f32>) {
    destination.clear();
    destination.extend_from_slice(source);
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

    match channels {
        1 => {
            for sample in input.iter().take(frames) {
                output.push(*sample);
                output.push(*sample);
            }
        }
        2 => output.extend_from_slice(&input[..frames.saturating_mul(2)]),
        _ => binaural_downmix_into(input, channels, output),
    }
}

#[derive(Clone, Copy)]
struct Speaker {
    azimuth_deg: f32,
    elevation_deg: f32,
    gain: f32,
    rear: bool,
}

const LAYOUT_7_1_4: [Speaker; 12] = [
    Speaker { azimuth_deg: -30.0, elevation_deg: 0.0, gain: 1.00, rear: false },
    Speaker { azimuth_deg:  30.0, elevation_deg: 0.0, gain: 1.00, rear: false },
    Speaker { azimuth_deg:   0.0, elevation_deg: 0.0, gain: 0.82, rear: false },
    Speaker { azimuth_deg:   0.0, elevation_deg: 0.0, gain: 0.34, rear: false },
    Speaker { azimuth_deg: -145.0, elevation_deg: 0.0, gain: 0.70, rear: true },
    Speaker { azimuth_deg:  145.0, elevation_deg: 0.0, gain: 0.70, rear: true },
    Speaker { azimuth_deg:  -90.0, elevation_deg: 0.0, gain: 0.76, rear: false },
    Speaker { azimuth_deg:   90.0, elevation_deg: 0.0, gain: 0.76, rear: false },
    Speaker { azimuth_deg:  -35.0, elevation_deg: 45.0, gain: 0.58, rear: false },
    Speaker { azimuth_deg:   35.0, elevation_deg: 45.0, gain: 0.58, rear: false },
    Speaker { azimuth_deg: -145.0, elevation_deg: 45.0, gain: 0.52, rear: true },
    Speaker { azimuth_deg:  145.0, elevation_deg: 45.0, gain: 0.52, rear: true },
];

const LAYOUT_5_1_4: [Speaker; 10] = [
    Speaker { azimuth_deg: -30.0, elevation_deg: 0.0, gain: 1.00, rear: false },
    Speaker { azimuth_deg:  30.0, elevation_deg: 0.0, gain: 1.00, rear: false },
    Speaker { azimuth_deg:   0.0, elevation_deg: 0.0, gain: 0.82, rear: false },
    Speaker { azimuth_deg:   0.0, elevation_deg: 0.0, gain: 0.34, rear: false },
    Speaker { azimuth_deg: -125.0, elevation_deg: 0.0, gain: 0.72, rear: true },
    Speaker { azimuth_deg:  125.0, elevation_deg: 0.0, gain: 0.72, rear: true },
    Speaker { azimuth_deg:  -35.0, elevation_deg: 45.0, gain: 0.58, rear: false },
    Speaker { azimuth_deg:   35.0, elevation_deg: 45.0, gain: 0.58, rear: false },
    Speaker { azimuth_deg: -145.0, elevation_deg: 45.0, gain: 0.52, rear: true },
    Speaker { azimuth_deg:  145.0, elevation_deg: 45.0, gain: 0.52, rear: true },
];

fn binaural_downmix_into(input: &[f32], channels: usize, output: &mut Vec<f32>) {
    let frames = input.len() / channels;
    for frame in input.chunks_exact(channels).take(frames) {
        let mut left = 0.0_f32;
        let mut right = 0.0_f32;
        let mut energy = 0.0_f32;

        for (index, sample) in frame.iter().copied().enumerate() {
            let speaker = speaker_for_channel(channels, index);
            let (mut left_gain, mut right_gain) = equal_power_pan(speaker.azimuth_deg);

            let elevation = (1.0 - speaker.elevation_deg.abs() / 180.0 * 0.18).clamp(0.78, 1.0);
            if speaker.rear {
                let crossfeed = 0.12;
                let l = left_gain;
                let r = right_gain;
                left_gain = l * (1.0 - crossfeed) + r * crossfeed;
                right_gain = r * (1.0 - crossfeed) + l * crossfeed;
            }

            let gain = speaker.gain * elevation;
            left += sample * left_gain * gain;
            right += sample * right_gain * gain;
            energy += gain * gain;
        }

        let normalization = (2.0 / energy.max(2.0)).sqrt() * 0.94;
        output.push((left * normalization).clamp(-1.5, 1.5));
        output.push((right * normalization).clamp(-1.5, 1.5));
    }
}

fn speaker_for_channel(channels: usize, index: usize) -> Speaker {
    match channels {
        12 => LAYOUT_7_1_4[index.min(LAYOUT_7_1_4.len() - 1)],
        10 => LAYOUT_5_1_4[index.min(LAYOUT_5_1_4.len() - 1)],
        _ => {
            let azimuth = -180.0 + (index as f32 + 0.5) * (360.0 / channels as f32);
            Speaker {
                azimuth_deg: azimuth,
                elevation_deg: 0.0,
                gain: 0.72,
                rear: azimuth.abs() > 100.0,
            }
        }
    }
}

fn equal_power_pan(azimuth_deg: f32) -> (f32, f32) {
    let folded = if azimuth_deg > 90.0 {
        180.0 - azimuth_deg
    } else if azimuth_deg < -90.0 {
        -180.0 - azimuth_deg
    } else {
        azimuth_deg
    };
    let pan = (folded / 90.0).clamp(-1.0, 1.0);
    (((1.0 - pan) * 0.5).sqrt(), ((1.0 + pan) * 0.5).sqrt())
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
    fn multichannel_binaural_reference_keeps_non_front_channels() {
        let mut input = vec![0.0_f32; 12 * 8];
        for frame in input.chunks_exact_mut(12) {
            frame[2] = 0.5;
            frame[10] = 0.3;
        }
        let mut output = Vec::new();
        to_stereo_into(&input, 12, &mut output);
        assert_eq!(output.len(), 16);
        assert!(output.iter().any(|sample| sample.abs() > 0.01));
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
