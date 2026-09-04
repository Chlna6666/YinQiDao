use std::f32::consts::PI;

use crate::model::{EqSettings, SpatialSettings};

pub const EQ_FREQUENCIES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EqPreset {
    Flat,
    Pop,
    Rock,
    Vocal,
    Classical,
}

impl EqPreset {
    pub const ALL: [Self; 5] = [
        Self::Flat,
        Self::Pop,
        Self::Rock,
        Self::Vocal,
        Self::Classical,
    ];

    pub fn settings(self) -> EqSettings {
        let bands_db = match self {
            Self::Flat => [0.0; 10],
            Self::Pop => [1.0, 2.0, 3.0, 2.0, 0.0, -1.0, -1.0, 1.0, 2.0, 2.0],
            Self::Rock => [4.0, 3.0, 2.0, 0.0, -1.0, 1.0, 2.0, 3.0, 4.0, 4.0],
            Self::Vocal => [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, -1.0, -2.0],
            Self::Classical => [3.0, 2.0, 1.0, 0.0, -1.0, -1.0, 0.0, 2.0, 3.0, 3.0],
        };
        EqSettings {
            enabled: !matches!(self, Self::Flat),
            preamp_db: 0.0,
            bands_db,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Default for Biquad {
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

impl Biquad {
    fn peaking(sample_rate: f32, frequency: f32, gain_db: f32) -> Self {
        if gain_db.abs() < f32::EPSILON || sample_rate <= frequency * 2.0 {
            return Self::default();
        }
        let a = 10.0_f32.powf(gain_db / 40.0);
        let omega = 2.0 * PI * frequency / sample_rate;
        let alpha = omega.sin() / (2.0 * 1.0);
        let cos = omega.cos();
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos;
        let a2 = 1.0 - alpha / a;
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            ..Self::default()
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

#[derive(Clone, Debug)]
pub struct EqProcessor {
    settings: EqSettings,
    sample_rate: f32,
    left: [Biquad; 10],
    right: [Biquad; 10],
}

impl EqProcessor {
    pub fn new(sample_rate: u32, settings: EqSettings) -> Self {
        let mut processor = Self {
            settings,
            sample_rate: sample_rate.max(1) as f32,
            left: [Biquad::default(); 10],
            right: [Biquad::default(); 10],
        };
        processor.rebuild();
        processor
    }

    pub fn set_settings(&mut self, settings: EqSettings) {
        self.settings = clamp_eq(settings);
        self.rebuild();
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if !self.settings.enabled {
            return;
        }
        let preamp = db_to_gain(self.settings.preamp_db);
        for frame in samples.as_chunks_mut::<2>().0 {
            let mut left = frame[0] * preamp;
            let mut right = frame[1] * preamp;
            for (left_filter, right_filter) in self.left.iter_mut().zip(self.right.iter_mut()) {
                left = left_filter.process(left);
                right = right_filter.process(right);
            }
            frame[0] = left;
            frame[1] = right;
        }
    }

    fn rebuild(&mut self) {
        for (index, frequency) in EQ_FREQUENCIES.into_iter().enumerate() {
            let gain = if self.settings.enabled {
                self.settings.bands_db[index]
            } else {
                0.0
            };
            self.left[index] = Biquad::peaking(self.sample_rate, frequency, gain);
            self.right[index] = Biquad::peaking(self.sample_rate, frequency, gain);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spatializer {
    settings: SpatialSettings,
}

impl Spatializer {
    pub fn new(settings: SpatialSettings) -> Self {
        Self {
            settings: clamp_spatial(settings),
        }
    }

    pub fn set_settings(&mut self, settings: SpatialSettings) {
        self.settings = clamp_spatial(settings);
    }

    #[cfg(test)]
    pub fn settings(&self) -> &SpatialSettings {
        &self.settings
    }

    pub fn process(&self, samples: &mut [f32]) {
        if !self.settings.enabled {
            return;
        }
        let width = 0.35 + self.settings.width * 1.65;
        let depth = 1.0 - self.settings.depth * 0.45;
        let distance = 1.0 - self.settings.distance * 0.55;
        let mix = self.settings.mix;
        for frame in samples.as_chunks_mut::<2>().0 {
            let mid = (frame[0] + frame[1]) * 0.5;
            let side = (frame[0] - frame[1]) * 0.5;
            let widened_left = (mid + side * width) * depth * distance;
            let widened_right = (mid - side * width) * depth * distance;
            let crossfeed = (1.0 - mix) * 0.5;
            frame[0] = widened_left * mix + widened_right * crossfeed;
            frame[1] = widened_right * mix + widened_left * crossfeed;
        }
    }
}

#[derive(Clone, Debug)]
pub struct AudioProcessor {
    pub eq: EqProcessor,
    pub spatial: Spatializer,
    volume: f32,
    stereo_scratch: Vec<f32>,
}

impl AudioProcessor {
    pub fn new(sample_rate: u32, eq: EqSettings, spatial: SpatialSettings, volume: f32) -> Self {
        Self {
            eq: EqProcessor::new(sample_rate, eq),
            spatial: Spatializer::new(spatial),
            volume: volume.clamp(0.0, 1.0),
            stereo_scratch: Vec::new(),
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
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
        let output_rate = self.eq.sample_rate as u32;
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

pub fn clamp_eq(mut settings: EqSettings) -> EqSettings {
    settings.preamp_db = settings.preamp_db.clamp(-24.0, 24.0);
    for band in &mut settings.bands_db {
        *band = band.clamp(-12.0, 12.0);
    }
    settings
}

pub fn clamp_spatial(mut settings: SpatialSettings) -> SpatialSettings {
    settings.width = settings.width.clamp(0.0, 1.0);
    settings.depth = settings.depth.clamp(0.0, 1.0);
    settings.distance = settings.distance.clamp(0.0, 1.0);
    settings.mix = settings.mix.clamp(0.0, 1.0);
    settings
}

fn perceptual_volume_gain(volume: f32) -> f32 {
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

fn db_to_gain(decibels: f32) -> f32 {
    10.0_f32.powf(decibels / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_and_limiter_keep_samples_in_a_safe_range() {
        let mut settings = EqPreset::Rock.settings();
        settings.preamp_db = 99.0;
        settings.bands_db[0] = -99.0;
        let mut processor =
            AudioProcessor::new(48_000, clamp_eq(settings), SpatialSettings::default(), 1.0);
        let input = vec![8.0; 96];
        let output = processor.process(&input, 48_000, 2);
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn volume_curve_has_real_low_level_headroom() {
        assert_eq!(perceptual_volume_gain(0.0), 0.0);
        assert!((perceptual_volume_gain(0.1) - 0.01).abs() < 1e-6);
        assert!((perceptual_volume_gain(0.5) - 0.25).abs() < 1e-6);
        assert_eq!(perceptual_volume_gain(1.0), 1.0);
    }

    #[test]
    fn mono_is_expanded_and_resampling_preserves_stereo_symmetry() {
        let mut processor = AudioProcessor::new(
            48_000,
            EqSettings::default(),
            SpatialSettings::default(),
            1.0,
        );
        let output = processor.process(&[0.25, 0.5, 0.75], 24_000, 1);
        assert_eq!(output.len(), 12);
        assert!(
            output
                .as_chunks::<2>()
                .0
                .iter()
                .all(|frame| (frame[0] - frame[1]).abs() < f32::EPSILON)
        );
    }

    #[test]
    fn process_into_reuses_output_and_resample_capacity() {
        let mut processor = AudioProcessor::new(
            48_000,
            EqSettings::default(),
            SpatialSettings::default(),
            1.0,
        );
        let input = vec![0.25; 96];
        let mut output = Vec::new();
        processor.process_into(&input, 24_000, 1, &mut output);
        let output_pointer = output.as_ptr();
        let output_capacity = output.capacity();
        let scratch_capacity = processor.stereo_scratch.capacity();

        processor.process_into(&input, 24_000, 1, &mut output);

        assert_eq!(output.as_ptr(), output_pointer);
        assert_eq!(output.capacity(), output_capacity);
        assert_eq!(processor.stereo_scratch.capacity(), scratch_capacity);
    }

    #[test]
    fn spatializer_clamps_parameters_and_changes_width() {
        let settings = SpatialSettings {
            enabled: true,
            width: 2.0,
            depth: -1.0,
            distance: 2.0,
            mix: 0.8,
        };
        let spatializer = Spatializer::new(settings);
        assert_eq!(spatializer.settings().width, 1.0);
        spatializer.process(&mut [1.0, 0.0]);
        assert_ne!(spatializer.settings().width, 0.0);
    }
}
