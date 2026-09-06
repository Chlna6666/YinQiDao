use std::f32::consts::PI;

use crate::model::{SpatialMotionMode, SpatialSettings};

const MAX_SPATIAL_DELAY_SECONDS: f32 = 0.040;
const MAX_ITD_SECONDS: f32 = 0.00068;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialPreset {
    Studio,
    Wide,
    Headphones,
    Cinema,
    Immersive3d,
    Orbit8d,
    Orbit360,
    Pendulum,
    Planetary,
    NearEar,
}

impl SpatialPreset {
    pub const ALL: [Self; 10] = [
        Self::Studio,
        Self::Wide,
        Self::Headphones,
        Self::Cinema,
        Self::Immersive3d,
        Self::Orbit8d,
        Self::Orbit360,
        Self::Pendulum,
        Self::Planetary,
        Self::NearEar,
    ];

    pub fn settings(self) -> SpatialSettings {
        match self {
            Self::Studio => SpatialSettings {
                enabled: true,
                width: 0.52,
                depth: 0.18,
                distance: 0.04,
                mix: 0.38,
                crossfeed: 0.06,
                room_size: 0.10,
                immersive_3d: 0.06,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.55,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Wide => SpatialSettings {
                enabled: true,
                width: 0.78,
                depth: 0.30,
                distance: 0.06,
                mix: 0.58,
                crossfeed: 0.04,
                room_size: 0.18,
                immersive_3d: 0.24,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.65,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Headphones => SpatialSettings {
                enabled: true,
                width: 0.62,
                depth: 0.24,
                distance: 0.10,
                mix: 0.52,
                crossfeed: 0.28,
                room_size: 0.12,
                immersive_3d: 0.32,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.62,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Cinema => SpatialSettings {
                enabled: true,
                width: 0.84,
                depth: 0.58,
                distance: 0.18,
                mix: 0.72,
                crossfeed: 0.08,
                room_size: 0.60,
                immersive_3d: 0.56,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.75,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Immersive3d => SpatialSettings {
                enabled: true,
                width: 0.92,
                depth: 0.72,
                distance: 0.20,
                mix: 0.82,
                crossfeed: 0.12,
                room_size: 0.72,
                immersive_3d: 0.88,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.80,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Orbit8d => SpatialSettings {
                enabled: true,
                width: 0.78,
                depth: 0.58,
                distance: 0.12,
                mix: 0.78,
                crossfeed: 0.08,
                room_size: 0.34,
                immersive_3d: 0.72,
                motion_mode: SpatialMotionMode::Orbit8d,
                motion_speed_hz: 0.075,
                motion_radius: 0.92,
                motion_intensity: 0.88,
                clockwise: true,
            },
            Self::Orbit360 => SpatialSettings {
                enabled: true,
                width: 0.72,
                depth: 0.64,
                distance: 0.18,
                mix: 0.82,
                crossfeed: 0.10,
                room_size: 0.42,
                immersive_3d: 0.84,
                motion_mode: SpatialMotionMode::Orbit360,
                motion_speed_hz: 0.055,
                motion_radius: 0.96,
                motion_intensity: 0.92,
                clockwise: true,
            },
            Self::Pendulum => SpatialSettings {
                enabled: true,
                width: 0.70,
                depth: 0.34,
                distance: 0.06,
                mix: 0.68,
                crossfeed: 0.08,
                room_size: 0.20,
                immersive_3d: 0.54,
                motion_mode: SpatialMotionMode::Pendulum,
                motion_speed_hz: 0.12,
                motion_radius: 0.90,
                motion_intensity: 0.82,
                clockwise: true,
            },
            Self::Planetary => SpatialSettings {
                enabled: true,
                width: 0.86,
                depth: 0.72,
                distance: 0.22,
                mix: 0.84,
                crossfeed: 0.10,
                room_size: 0.56,
                immersive_3d: 0.90,
                motion_mode: SpatialMotionMode::Planetary,
                motion_speed_hz: 0.038,
                motion_radius: 1.0,
                motion_intensity: 0.94,
                clockwise: true,
            },
            Self::NearEar => SpatialSettings {
                enabled: true,
                width: 0.66,
                depth: 0.38,
                distance: 0.02,
                mix: 0.76,
                crossfeed: 0.18,
                room_size: 0.12,
                immersive_3d: 0.62,
                motion_mode: SpatialMotionMode::NearEar,
                motion_speed_hz: 0.095,
                motion_radius: 1.0,
                motion_intensity: 0.90,
                clockwise: true,
            },
        }
    }

    pub fn matches(self, settings: &SpatialSettings) -> bool {
        let preset = self.settings();
        preset.motion_mode == settings.motion_mode
            && preset.clockwise == settings.clockwise
            && [
                (preset.width, settings.width),
                (preset.depth, settings.depth),
                (preset.distance, settings.distance),
                (preset.mix, settings.mix),
                (preset.crossfeed, settings.crossfeed),
                (preset.room_size, settings.room_size),
                (preset.immersive_3d, settings.immersive_3d),
                (preset.motion_speed_hz, settings.motion_speed_hz),
                (preset.motion_radius, settings.motion_radius),
                (preset.motion_intensity, settings.motion_intensity),
            ]
            .into_iter()
            .all(|(left, right)| (left - right).abs() <= 0.01)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Spatializer {
    settings: SpatialSettings,
    sample_rate: f32,
    reflection_left: Vec<f32>,
    reflection_right: Vec<f32>,
    reflection_cursor: usize,
    motion_delay: Vec<f32>,
    motion_cursor: usize,
    lowpass_left: f32,
    lowpass_right: f32,
    rear_lowpass_left: f32,
    rear_lowpass_right: f32,
    oscillator_sin: f32,
    oscillator_cos: f32,
}

impl Spatializer {
    pub(crate) fn new(sample_rate: u32, settings: SpatialSettings) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let reflection_frames = (sample_rate * MAX_SPATIAL_DELAY_SECONDS).ceil() as usize + 2;
        let motion_frames = (sample_rate * (MAX_ITD_SECONDS + 0.002)).ceil() as usize + 4;
        Self {
            settings: clamp_spatial(settings),
            sample_rate,
            reflection_left: vec![0.0; reflection_frames.max(8)],
            reflection_right: vec![0.0; reflection_frames.max(8)],
            reflection_cursor: 0,
            motion_delay: vec![0.0; motion_frames.max(16)],
            motion_cursor: 0,
            lowpass_left: 0.0,
            lowpass_right: 0.0,
            rear_lowpass_left: 0.0,
            rear_lowpass_right: 0.0,
            oscillator_sin: 0.0,
            oscillator_cos: 1.0,
        }
    }

    pub(crate) fn set_settings(&mut self, settings: SpatialSettings) {
        let previous_mode = self.settings.motion_mode;
        let was_enabled = self.settings.enabled;
        self.settings = clamp_spatial(settings);
        if was_enabled && !self.settings.enabled {
            self.reset_state();
        } else if previous_mode != self.settings.motion_mode {
            self.reset_motion_state();
        }
    }

    #[cfg(test)]
    pub(crate) fn settings(&self) -> &SpatialSettings {
        &self.settings
    }

    pub(crate) fn process(&mut self, samples: &mut [f32]) {
        if !self.settings.enabled {
            return;
        }

        let settings = self.settings.clone();
        let motion_enabled = settings.motion_mode != SpatialMotionMode::Static
            && settings.motion_intensity > 0.001;
        let width_gain = 0.72 + settings.width * 1.38;
        let crossfeed_gain = settings.crossfeed * 0.22;
        let attenuation = 1.0 - settings.distance * 0.28;
        let cutoff_hz = 19_000.0 - settings.distance * 13_000.0;
        let lowpass_decay = (-2.0 * PI * cutoff_hz / self.sample_rate).exp();
        let lowpass_input = 1.0 - lowpass_decay;
        let reflection_gain = (settings.depth * 0.11
            + settings.room_size * 0.12
            + settings.immersive_3d * 0.09)
            .clamp(0.0, 0.28);

        let reflection_len = self.reflection_left.len();
        let base_delay_seconds = 0.0035 + settings.room_size * 0.016 + settings.depth * 0.006;
        let left_delay = ((base_delay_seconds * self.sample_rate).round() as usize)
            .clamp(1, reflection_len - 1);
        let right_delay = (((base_delay_seconds + settings.immersive_3d * 0.0018)
            * self.sample_rate)
            .round() as usize)
            .clamp(1, reflection_len - 1);

        // Only one sin/cos pair per audio buffer. Per-sample source motion uses a complex-rotation
        // recurrence, avoiding expensive trigonometric calls at 48/96 kHz.
        let angular_step = if motion_enabled {
            let direction = if settings.clockwise { 1.0 } else { -1.0 };
            direction * 2.0 * PI * settings.motion_speed_hz / self.sample_rate
        } else {
            0.0
        };
        let step_sin = angular_step.sin();
        let step_cos = angular_step.cos();

        for frame in samples.as_chunks_mut::<2>().0 {
            let dry_left = frame[0];
            let dry_right = frame[1];
            let mid = (dry_left + dry_right) * 0.5;
            let side = (dry_left - dry_right) * 0.5;

            let mut widened_left = mid + side * width_gain;
            let mut widened_right = mid - side * width_gain;
            if crossfeed_gain > 0.0 {
                let left = widened_left;
                let right = widened_right;
                widened_left = left * (1.0 - crossfeed_gain) + right * crossfeed_gain;
                widened_right = right * (1.0 - crossfeed_gain) + left * crossfeed_gain;
            }

            if settings.distance > 0.001 {
                self.lowpass_left =
                    widened_left * lowpass_input + self.lowpass_left * lowpass_decay;
                self.lowpass_right =
                    widened_right * lowpass_input + self.lowpass_right * lowpass_decay;
                widened_left = self.lowpass_left;
                widened_right = self.lowpass_right;
            } else {
                self.lowpass_left = widened_left;
                self.lowpass_right = widened_right;
            }

            let left_read = (self.reflection_cursor + reflection_len - left_delay) % reflection_len;
            let right_read =
                (self.reflection_cursor + reflection_len - right_delay) % reflection_len;
            let reflected_left = self.reflection_right[right_read];
            let reflected_right = self.reflection_left[left_read];
            self.reflection_left[self.reflection_cursor] = widened_left;
            self.reflection_right[self.reflection_cursor] = widened_right;
            self.reflection_cursor = (self.reflection_cursor + 1) % reflection_len;

            let static_left =
                (widened_left * (1.0 - reflection_gain) + reflected_left * reflection_gain)
                    * attenuation;
            let static_right =
                (widened_right * (1.0 - reflection_gain) + reflected_right * reflection_gain)
                    * attenuation;

            let (spatial_left, spatial_right) = if motion_enabled {
                let (pan, front, radius_mod) = motion_position(
                    settings.motion_mode,
                    self.oscillator_sin,
                    self.oscillator_cos,
                );
                let radius = (settings.motion_radius * radius_mod).clamp(0.0, 1.0);
                let moving_source = mid * 0.82 + (widened_left + widened_right) * 0.09;

                let motion_len = self.motion_delay.len();
                self.motion_delay[self.motion_cursor] = moving_source;
                let itd_samples = (self.sample_rate * MAX_ITD_SECONDS * pan.abs() * radius)
                    .round() as usize;
                let itd_samples = itd_samples.min(motion_len - 1);
                let delayed_index =
                    (self.motion_cursor + motion_len - itd_samples) % motion_len;
                let delayed_source = self.motion_delay[delayed_index];
                self.motion_cursor = (self.motion_cursor + 1) % motion_len;

                let lateral = pan.abs() * radius;
                let near_gain = 0.82 + 0.18 * radius;
                let far_gain = (1.0 - 0.34 * lateral).clamp(0.58, 1.0);
                let (mut moving_left, mut moving_right) = if pan >= 0.0 {
                    (delayed_source * far_gain, moving_source * near_gain)
                } else {
                    (moving_source * near_gain, delayed_source * far_gain)
                };

                // Rear spectral damping provides a front/back cue without requiring a bundled
                // individualized HRTF database. It is combined with ITD/ILD rather than replacing it.
                let rear_amount = (-front).max(0.0) * radius;
                let rear_alpha = 0.10 + (1.0 - rear_amount) * 0.24;
                self.rear_lowpass_left += rear_alpha * (moving_left - self.rear_lowpass_left);
                self.rear_lowpass_right += rear_alpha * (moving_right - self.rear_lowpass_right);
                moving_left = moving_left * (1.0 - rear_amount * 0.66)
                    + self.rear_lowpass_left * rear_amount * 0.66;
                moving_right = moving_right * (1.0 - rear_amount * 0.66)
                    + self.rear_lowpass_right * rear_amount * 0.66;

                let front_distance_gain = 0.74 + ((front + 1.0) * 0.5) * 0.26;
                moving_left *= front_distance_gain;
                moving_right *= front_distance_gain;

                let sin = self.oscillator_sin;
                let cos = self.oscillator_cos;
                self.oscillator_sin = sin * step_cos + cos * step_sin;
                self.oscillator_cos = cos * step_cos - sin * step_sin;

                (
                    static_left * (1.0 - settings.motion_intensity)
                        + moving_left * settings.motion_intensity,
                    static_right * (1.0 - settings.motion_intensity)
                        + moving_right * settings.motion_intensity,
                )
            } else {
                (static_left, static_right)
            };

            frame[0] = dry_left * (1.0 - settings.mix) + spatial_left * settings.mix;
            frame[1] = dry_right * (1.0 - settings.mix) + spatial_right * settings.mix;
        }

        if motion_enabled {
            // Prevent very slow floating-point radius drift in the recurrence oscillator.
            let norm = (self.oscillator_sin * self.oscillator_sin
                + self.oscillator_cos * self.oscillator_cos)
                .sqrt();
            if norm > 1e-6 {
                self.oscillator_sin /= norm;
                self.oscillator_cos /= norm;
            } else {
                self.oscillator_sin = 0.0;
                self.oscillator_cos = 1.0;
            }
        }
    }

    fn reset_motion_state(&mut self) {
        self.motion_delay.fill(0.0);
        self.motion_cursor = 0;
        self.rear_lowpass_left = 0.0;
        self.rear_lowpass_right = 0.0;
        self.oscillator_sin = 0.0;
        self.oscillator_cos = 1.0;
    }

    fn reset_state(&mut self) {
        self.reflection_left.fill(0.0);
        self.reflection_right.fill(0.0);
        self.reflection_cursor = 0;
        self.lowpass_left = 0.0;
        self.lowpass_right = 0.0;
        self.reset_motion_state();
    }
}

#[inline]
fn motion_position(mode: SpatialMotionMode, sin: f32, cos: f32) -> (f32, f32, f32) {
    let sin2 = 2.0 * sin * cos;
    let cos2 = cos * cos - sin * sin;
    let sin3 = 3.0 * sin - 4.0 * sin * sin * sin;
    match mode {
        SpatialMotionMode::Static => (0.0, 1.0, 1.0),
        SpatialMotionMode::Orbit8d => (sin, sin2 * 0.92, 0.78 + 0.22 * cos2.abs()),
        SpatialMotionMode::Orbit360 => (sin, cos, 1.0),
        SpatialMotionMode::Pendulum => (sin, 0.68, 0.88 + 0.12 * cos.abs()),
        SpatialMotionMode::FrontBack => (sin * 0.16, cos, 0.90 + 0.10 * sin.abs()),
        SpatialMotionMode::Planetary => (sin, cos, 0.62 + 0.38 * sin3.abs()),
        SpatialMotionMode::NearEar => (sin, 0.20 + cos * 0.80, 0.88 + 0.12 * sin2.abs()),
    }
}

pub fn clamp_spatial(mut settings: SpatialSettings) -> SpatialSettings {
    settings.width = settings.width.clamp(0.0, 1.0);
    settings.depth = settings.depth.clamp(0.0, 1.0);
    settings.distance = settings.distance.clamp(0.0, 1.0);
    settings.mix = settings.mix.clamp(0.0, 1.0);
    settings.crossfeed = settings.crossfeed.clamp(0.0, 1.0);
    settings.room_size = settings.room_size.clamp(0.0, 1.0);
    settings.immersive_3d = settings.immersive_3d.clamp(0.0, 1.0);
    settings.motion_speed_hz = settings.motion_speed_hz.clamp(0.01, 0.35);
    settings.motion_radius = settings.motion_radius.clamp(0.0, 1.0);
    settings.motion_intensity = settings.motion_intensity.clamp(0.0, 1.0);
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motion_geometry_is_bounded() {
        for mode in [
            SpatialMotionMode::Orbit8d,
            SpatialMotionMode::Orbit360,
            SpatialMotionMode::Pendulum,
            SpatialMotionMode::FrontBack,
            SpatialMotionMode::Planetary,
            SpatialMotionMode::NearEar,
        ] {
            for step in 0..360 {
                let angle = step as f32 * PI / 180.0;
                let (pan, front, radius) = motion_position(mode, angle.sin(), angle.cos());
                assert!(pan.abs() <= 1.001);
                assert!(front.abs() <= 1.001);
                assert!((0.0..=1.001).contains(&radius));
            }
        }
    }

    #[test]
    fn orbit_changes_channel_energy() {
        let mut settings = SpatialPreset::Orbit360.settings();
        settings.motion_speed_hz = 0.35;
        let mut spatializer = Spatializer::new(48_000, settings);
        let mut samples = vec![0.3; 48_000 * 4];
        spatializer.process(&mut samples);
        assert!(samples.as_chunks::<2>().0.iter().any(|frame| {
            (frame[0] - frame[1]).abs() > 0.02
        }));
    }

    #[test]
    fn custom_motion_breaks_preset_match() {
        let mut settings = SpatialPreset::Orbit8d.settings();
        assert!(SpatialPreset::Orbit8d.matches(&settings));
        settings.motion_speed_hz += 0.02;
        assert!(!SpatialPreset::Orbit8d.matches(&settings));
    }
}
