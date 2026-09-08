use std::f32::consts::PI;

use crate::model::EqSettings;

pub const EQ_FREQUENCIES: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

// A slightly wider graphic-EQ bell gives the ten fixed bands a smoother, more musical overlap.
// The previous SQRT_2 Q made large neighbouring boosts pile up into obvious boxiness/harshness.
const GRAPHIC_EQ_Q: f32 = 1.10;

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
        // Keep presets broad and conservative. Large neighbouring boosts in the old presets caused
        // the downstream limiter to become part of the sound, masking transients and collapsing
        // stereo depth. Vocal in particular used to add +2/+4/+4 dB at 250/500/1 kHz while
        // cutting the top octave, which strongly emphasised chest/box resonance and sounded dull.
        let (preamp_db, bands_db) = match self {
            Self::Flat => (0.0, [0.0; 10]),
            Self::Pop => (-1.5, [0.5, 1.0, 1.2, 0.4, -0.4, 0.2, 1.0, 1.4, 1.0, 0.4]),
            Self::Rock => (-2.0, [1.8, 1.4, 0.6, -0.4, -0.8, 0.4, 1.4, 1.9, 1.4, 0.5]),
            Self::Vocal => (
                -2.0,
                [-2.0, -1.5, -1.0, -1.8, -1.0, 0.8, 2.4, 2.0, 1.2, 0.5],
            ),
            Self::Classical => (-1.0, [0.4, 0.5, 0.2, -0.4, -0.5, 0.0, 0.5, 1.0, 0.9, 0.4]),
        };
        EqSettings {
            enabled: true,
            preamp_db,
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
    transport_reset_requested: bool,
}

impl EqProcessor {
    pub(crate) fn new(sample_rate: u32, settings: EqSettings) -> Self {
        let mut processor = Self {
            settings: clamp_eq(settings),
            sample_rate: sample_rate.max(1) as f32,
            left: [Biquad::default(); 10],
            right: [Biquad::default(); 10],
            transport_reset_requested: false,
        };
        processor.rebuild();
        processor
    }

    pub(crate) fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    pub(crate) fn set_settings(&mut self, settings: EqSettings) {
        self.settings = clamp_eq(settings);
        // `AudioWorker::apply_processing_for_track` passes through here on every track reopen.
        // Treat the next PCM block as a fresh DSP timeline. The same behaviour on an interactive
        // EQ/spatial profile change is intentional: old IIR/delay history must not colour new params.
        self.transport_reset_requested = true;
        self.rebuild();
    }

    pub(crate) fn take_transport_reset_request(&mut self) -> bool {
        let requested = self.transport_reset_requested;
        self.transport_reset_requested = false;
        requested
    }

    pub(crate) fn reset_state(&mut self) {
        // Rebuilding coefficients also zeroes every biquad z1/z2 without changing settings.
        self.rebuild();
        self.transport_reset_requested = false;
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
        assert_eq!(flat.preamp_db, 0.0);
    }

    #[test]
    fn vocal_reduces_boxiness_and_restores_presence() {
        let vocal = EqPreset::Vocal.settings();
        assert!(vocal.bands_db[3] < 0.0); // 250 Hz
        assert!(vocal.bands_db[4] < 0.0); // 500 Hz
        assert!(vocal.bands_db[6] > 0.0); // 2 kHz presence
        assert!(vocal.bands_db[7] > 0.0); // 4 kHz clarity
        assert!(vocal.bands_db[8] > 0.0); // 8 kHz air
        assert!(vocal.preamp_db < 0.0);
    }

    #[test]
    fn boosted_presets_reserve_headroom() {
        for preset in [
            EqPreset::Pop,
            EqPreset::Rock,
            EqPreset::Vocal,
            EqPreset::Classical,
        ] {
            let settings = preset.settings();
            assert!(settings.preamp_db <= 0.0);
            assert!(settings.bands_db.iter().any(|gain| *gain > 0.0));
        }
    }

    #[test]
    fn custom_band_breaks_preset_match() {
        let mut pop = EqPreset::Pop.settings();
        assert!(EqPreset::Pop.matches(&pop));
        pop.bands_db[2] += 0.25;
        assert!(!EqPreset::Pop.matches(&pop));
    }

    #[test]
    fn settings_change_requests_a_transport_state_reset() {
        let mut processor = EqProcessor::new(48_000, EqPreset::Flat.settings());
        assert!(!processor.take_transport_reset_request());
        processor.set_settings(EqPreset::Rock.settings());
        assert!(processor.take_transport_reset_request());
        assert!(!processor.take_transport_reset_request());
    }
}
