use std::f32::consts::PI;

use crate::model::EqSettings;

pub const EQ_FREQUENCIES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

const GRAPHIC_EQ_Q: f32 = std::f32::consts::SQRT_2;

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
            enabled: true,
            preamp_db: 0.0,
            bands_db,
        }
    }

    pub fn matches(self, settings: &EqSettings) -> bool {
        let preset = self.settings();
        preset
            .bands_db
            .iter()
            .zip(settings.bands_db.iter())
            .all(|(left, right)| (left - right).abs() <= 0.01)
            && (preset.preamp_db - settings.preamp_db).abs() <= 0.01
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
        let alpha = omega.sin() / (2.0 * GRAPHIC_EQ_Q);
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

    #[inline]
    fn process(&mut self, input: f32) -> f32 {
        let output = self.b0 * input + self.z1;
        self.z1 = self.b1 * input - self.a1 * output + self.z2;
        self.z2 = self.b2 * input - self.a2 * output;
        output
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EqProcessor {
    settings: EqSettings,
    sample_rate: f32,
    left: [Biquad; 10],
    right: [Biquad; 10],
}

impl EqProcessor {
    pub(crate) fn new(sample_rate: u32, settings: EqSettings) -> Self {
        let mut processor = Self {
            settings: clamp_eq(settings),
            sample_rate: sample_rate.max(1) as f32,
            left: [Biquad::default(); 10],
            right: [Biquad::default(); 10],
        };
        processor.rebuild();
        processor
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    pub(crate) fn settings(&self) -> &EqSettings {
        &self.settings
    }

    pub(crate) fn set_settings(&mut self, settings: EqSettings) {
        self.settings = clamp_eq(settings);
        self.rebuild();
    }

    pub(crate) fn process(&mut self, samples: &mut [f32]) {
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

pub fn clamp_eq(mut settings: EqSettings) -> EqSettings {
    settings.preamp_db = settings.preamp_db.clamp(-24.0, 12.0);
    for band in &mut settings.bands_db {
        *band = band.clamp(-12.0, 12.0);
    }
    settings
}

#[inline]
fn db_to_gain(decibels: f32) -> f32 {
    10.0_f32.powf(decibels / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_is_an_enabled_zero_db_preset() {
        let flat = EqPreset::Flat.settings();
        assert!(flat.enabled);
        assert_eq!(flat.bands_db, [0.0; 10]);
    }

    #[test]
    fn custom_band_breaks_preset_match() {
        let mut pop = EqPreset::Pop.settings();
        assert!(EqPreset::Pop.matches(&pop));
        pop.bands_db[2] += 0.25;
        assert!(!EqPreset::Pop.matches(&pop));
    }
}
