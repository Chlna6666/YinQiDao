use std::f32::consts::PI;

use crate::model::{
    SourceLayoutOverride, SpatialMotionMode, SpatialSettings, VirtualBedMode,
};

const MAX_SPATIAL_DELAY_SECONDS: f32 = 0.040;
const MAX_ITD_SECONDS: f32 = 0.00068;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpatialPreset {
    Hifi,
    Studio,
    Wide,
    Headphones,
    ConcertHall,
    LiveConcert,
    Cinema,
    Immersive3d,
    Orbit8d,
    Orbit360,
    Pendulum,
    FrontBack,
    Planetary,
    NearEar,
    HelixSphere,
}

impl SpatialPreset {
    pub const ALL: [Self; 15] = [
        Self::Hifi,
        Self::Studio,
        Self::Wide,
        Self::Headphones,
        Self::ConcertHall,
        Self::LiveConcert,
        Self::Cinema,
        Self::Immersive3d,
        Self::Orbit8d,
        Self::Orbit360,
        Self::Pendulum,
        Self::FrontBack,
        Self::Planetary,
        Self::NearEar,
        Self::HelixSphere,
    ];

    pub fn settings(self) -> SpatialSettings {
        match self {
            // HiFi Direct deliberately avoids synthetic room, widening and trajectory processing.
            // It is a low-processing playback preset, not a claim of bit-perfect output: decoding,
            // sample-rate conversion, limiter and the host audio device can still alter samples.
            Self::Hifi => SpatialSettings {
                enabled: false,
                width: 0.50,
                depth: 0.00,
                distance: 0.00,
                mix: 0.00,
                crossfeed: 0.00,
                room_size: 0.00,
                immersive_3d: 0.00,
                virtual_bed: VirtualBedMode::Off,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.55,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Studio => SpatialSettings {
                enabled: true,
                width: 0.50,
                depth: 0.15,
                distance: 0.02,
                mix: 0.30,
                crossfeed: 0.05,
                room_size: 0.08,
                immersive_3d: 0.08,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.55,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Wide => SpatialSettings {
                enabled: true,
                width: 0.82,
                depth: 0.24,
                distance: 0.03,
                mix: 0.48,
                crossfeed: 0.03,
                room_size: 0.12,
                immersive_3d: 0.26,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.65,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Headphones => SpatialSettings {
                enabled: true,
                width: 0.64,
                depth: 0.24,
                distance: 0.04,
                mix: 0.44,
                crossfeed: 0.18,
                room_size: 0.08,
                immersive_3d: 0.30,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.62,
                motion_intensity: 0.0,
                clockwise: true,
            },
            // Parameterized venue scene. This uses the existing image-source early field + FDN
            // late field and must not be described as a measured concert-hall impulse response.
            Self::ConcertHall => SpatialSettings {
                enabled: true,
                width: 0.78,
                depth: 0.72,
                distance: 0.10,
                mix: 0.60,
                crossfeed: 0.05,
                room_size: 0.82,
                immersive_3d: 0.58,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.75,
                motion_intensity: 0.0,
                clockwise: true,
            },
            // Front-stage focus with stronger lateral/rear envelopment than ConcertHall. The scene
            // stays static so vocals/instruments do not orbit merely because the preset is "live".
            Self::LiveConcert => SpatialSettings {
                enabled: true,
                width: 0.92,
                depth: 0.58,
                distance: 0.05,
                mix: 0.66,
                crossfeed: 0.04,
                room_size: 0.58,
                immersive_3d: 0.78,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.82,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Cinema => SpatialSettings {
                enabled: true,
                width: 0.82,
                depth: 0.54,
                distance: 0.10,
                mix: 0.58,
                crossfeed: 0.05,
                room_size: 0.50,
                immersive_3d: 0.56,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.75,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Immersive3d => SpatialSettings {
                enabled: true,
                width: 0.90,
                depth: 0.64,
                distance: 0.08,
                mix: 0.64,
                crossfeed: 0.06,
                room_size: 0.56,
                immersive_3d: 0.88,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Static,
                motion_speed_hz: 0.08,
                motion_radius: 0.80,
                motion_intensity: 0.0,
                clockwise: true,
            },
            Self::Orbit8d => SpatialSettings {
                enabled: true,
                width: 0.74,
                depth: 0.42,
                distance: 0.05,
                mix: 0.72,
                crossfeed: 0.04,
                room_size: 0.22,
                immersive_3d: 0.72,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Orbit8d,
                motion_speed_hz: 0.105,
                motion_radius: 1.00,
                motion_intensity: 0.92,
                clockwise: true,
            },
            Self::Orbit360 => SpatialSettings {
                enabled: true,
                width: 0.72,
                depth: 0.46,
                distance: 0.07,
                mix: 0.70,
                crossfeed: 0.05,
                room_size: 0.24,
                immersive_3d: 0.72,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Orbit360,
                motion_speed_hz: 0.070,
                motion_radius: 1.00,
                motion_intensity: 0.90,
                clockwise: true,
            },
            Self::Pendulum => SpatialSettings {
                enabled: true,
                width: 0.68,
                depth: 0.32,
                distance: 0.03,
                mix: 0.62,
                crossfeed: 0.05,
                room_size: 0.14,
                immersive_3d: 0.52,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Pendulum,
                motion_speed_hz: 0.15,
                motion_radius: 0.95,
                motion_intensity: 0.84,
                clockwise: true,
            },
            Self::FrontBack => SpatialSettings {
                enabled: true,
                width: 0.66,
                depth: 0.56,
                distance: 0.04,
                mix: 0.68,
                crossfeed: 0.05,
                room_size: 0.18,
                immersive_3d: 0.68,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::FrontBack,
                motion_speed_hz: 0.085,
                motion_radius: 0.95,
                motion_intensity: 0.90,
                clockwise: true,
            },
            Self::Planetary => SpatialSettings {
                enabled: true,
                width: 0.78,
                depth: 0.54,
                distance: 0.09,
                mix: 0.72,
                crossfeed: 0.05,
                room_size: 0.32,
                immersive_3d: 0.78,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Planetary,
                motion_speed_hz: 0.050,
                motion_radius: 1.00,
                motion_intensity: 0.90,
                clockwise: true,
            },
            Self::NearEar => SpatialSettings {
                enabled: true,
                width: 0.64,
                depth: 0.30,
                distance: 0.00,
                mix: 0.66,
                crossfeed: 0.10,
                room_size: 0.08,
                immersive_3d: 0.60,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::NearEar,
                motion_speed_hz: 0.12,
                motion_radius: 1.00,
                motion_intensity: 0.88,
                clockwise: true,
            },
            // Full-sphere moving scene. The native trajectory uses signed elevation and therefore
            // passes below as well as above the listener instead of inventing non-standard floor
            // channels in a 7.1.4 speaker layout.
            Self::HelixSphere => SpatialSettings {
                enabled: true,
                width: 0.82,
                depth: 0.50,
                distance: 0.04,
                mix: 0.72,
                crossfeed: 0.04,
                room_size: 0.26,
                immersive_3d: 0.86,
                virtual_bed: VirtualBedMode::Auto,
                source_layout_override: SourceLayoutOverride::None,
                motion_mode: SpatialMotionMode::Helix,
                motion_speed_hz: 0.055,
                motion_radius: 0.95,
                motion_intensity: 0.86,
                clockwise: true,
            },
        }
    }

    pub fn matches(self, settings: &SpatialSettings) -> bool {
        let preset = self.settings();
        preset.enabled == settings.enabled
            && preset.motion_mode == settings.motion_mode
            && preset.virtual_bed == settings.virtual_bed
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

    pub(crate) fn settings(&self) -> &SpatialSettings {
        &self.settings
    }

    pub(crate) fn motion_enabled(&self) -> bool {
        self.settings.enabled
            && self.settings.motion_mode != SpatialMotionMode::Static
            && self.settings.motion_intensity > 0.001
    }

    /// Reset all state that is coupled to the previous transport position while retaining buffers.
    /// Seek/track reopen must not let reflection, ITD or filter history bleed into the new timeline.
    pub(crate) fn reset_transport(&mut self) {
        self.reset_state();
    }

    pub(crate) fn process(&mut self, samples: &mut [f32]) {
        self.process_internal(samples, true);
    }

    /// Apply only the authored-stereo width/crossfeed/distance/reflection stage. Dynamic movement is
    /// owned by `yinqidao-audio-spatial::Trajectory` in the player integration path.
    pub(crate) fn process_static(&mut self, samples: &mut [f32]) {
        self.process_internal(samples, false);
    }

    fn process_internal(&mut self, samples: &mut [f32], allow_motion: bool) {
        if !self.settings.enabled {
            return;
        }

        let settings = self.settings.clone();
        let motion_enabled = allow_motion
            && settings.motion_mode != SpatialMotionMode::Static
            && settings.motion_intensity > 0.001;

        // Preserve the original stereo image. The old range (0.72..2.10) could more than double
        // side energy, exaggerating phase differences and hollowing the centre. 0.92..1.42 keeps
        // width audible without turning ordinary stereo material into a phase effect.
        let width_gain = 0.92 + settings.width * 0.50;
        let crossfeed_gain = settings.crossfeed * 0.18;
        let attenuation = 1.0 - settings.distance * 0.12;
        let cutoff_hz = 20_000.0 - settings.distance * 8_000.0;
        let lowpass_decay = (-2.0 * PI * cutoff_hz / self.sample_rate).exp();
        let lowpass_input = 1.0 - lowpass_decay;
        let reflection_gain =
            (settings.depth * 0.055 + settings.room_size * 0.065 + settings.immersive_3d * 0.045)
                .clamp(0.0, 0.14);

        let reflection_len = self.reflection_left.len();
        let base_delay_seconds = 0.0045 + settings.room_size * 0.014 + settings.depth * 0.004;
        let left_delay =
            ((base_delay_seconds * self.sample_rate).round() as usize).clamp(1, reflection_len - 1);
        let right_delay = (((base_delay_seconds + settings.immersive_3d * 0.0013)
            * self.sample_rate)
            .round() as usize)
            .clamp(1, reflection_len - 1);

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

            // Feed predominantly side/ambient information into the reflection lines. Storing the
            // full opposite channel here made centred vocals reappear delayed in the other ear,
            // causing comb filtering and the "empty centre" character reported by listening tests.
            self.reflection_left[self.reflection_cursor] = side * 0.72 + widened_left * 0.18;
            self.reflection_right[self.reflection_cursor] = -side * 0.72 + widened_right * 0.18;
            self.reflection_cursor = (self.reflection_cursor + 1) % reflection_len;

            let room_compensation = 1.0 - settings.room_size * 0.025;
            let static_left =
                (widened_left + reflected_left * reflection_gain) * attenuation * room_compensation;
            let static_right = (widened_right + reflected_right * reflection_gain)
                * attenuation
                * room_compensation;

            let (spatial_left, spatial_right) = if motion_enabled {
                let (pan, front, radius_mod) = motion_position(
                    settings.motion_mode,
                    self.oscillator_sin,
                    self.oscillator_cos,
                );
                let radius = (settings.motion_radius * radius_mod).clamp(0.0, 1.0);

                // Motion is derived from the centre image, but a small opposite-polarity side
                // component is retained so authored stereo ambience does not collapse to mono.
                let moving_source = mid;
                let side_cue = side * 0.16;

                let motion_len = self.motion_delay.len();
                self.motion_delay[self.motion_cursor] = moving_source;
                let itd_samples = self.sample_rate * MAX_ITD_SECONDS * pan.abs() * radius;
                let delayed_source =
                    read_fractional_delay(&self.motion_delay, self.motion_cursor, itd_samples);
                self.motion_cursor = (self.motion_cursor + 1) % motion_len;

                let lateral = pan.abs() * radius;
                let near_gain = 0.94 + 0.06 * radius;
                let far_gain = (1.0 - 0.22 * lateral).clamp(0.72, 1.0);
                let (mut moving_left, mut moving_right) = if pan >= 0.0 {
                    (
                        delayed_source * far_gain + side_cue,
                        moving_source * near_gain - side_cue,
                    )
                } else {
                    (
                        moving_source * near_gain + side_cue,
                        delayed_source * far_gain - side_cue,
                    )
                };

                let rear_amount = (-front).max(0.0) * radius;
                let rear_alpha = 0.42 + (1.0 - rear_amount) * 0.28;
                self.rear_lowpass_left += rear_alpha * (moving_left - self.rear_lowpass_left);
                self.rear_lowpass_right += rear_alpha * (moving_right - self.rear_lowpass_right);
                let rear_mix = rear_amount * 0.34;
                moving_left = moving_left * (1.0 - rear_mix) + self.rear_lowpass_left * rear_mix;
                moving_right = moving_right * (1.0 - rear_mix) + self.rear_lowpass_right * rear_mix;

                let front_distance_gain = 0.88 + ((front + 1.0) * 0.5) * 0.12;
                moving_left *= front_distance_gain;
                moving_right *= front_distance_gain;

                let sin = self.oscillator_sin;
                let cos = self.oscillator_cos;
                self.oscillator_sin = sin * step_cos + cos * step_sin;
                self.oscillator_cos = cos * step_cos - sin * step_sin;

                let motion_blend = (settings.motion_intensity * 0.55).clamp(0.0, 0.55);
                (
                    static_left * (1.0 - motion_blend) + moving_left * motion_blend,
                    static_right * (1.0 - motion_blend) + moving_right * motion_blend,
                )
            } else {
                (static_left, static_right)
            };

            frame[0] = dry_left * (1.0 - settings.mix) + spatial_left * settings.mix;
            frame[1] = dry_right * (1.0 - settings.mix) + spatial_right * settings.mix;
        }

        if motion_enabled {
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
        // The legacy stereo fallback has no elevation axis; keep phase/radius coherent while the
        // primary Trajectory renderer provides the actual signed-elevation Helix path.
        SpatialMotionMode::Helix => (sin, cos2, 0.88 + 0.12 * cos.abs()),
    }
}

#[inline]
fn read_fractional_delay(buffer: &[f32], write_cursor: usize, delay_samples: f32) -> f32 {
    debug_assert!(buffer.len() >= 2);
    debug_assert!(write_cursor < buffer.len());

    if buffer.len() < 2 {
        return buffer.first().copied().unwrap_or(0.0);
    }

    let delay = delay_samples.clamp(0.0, buffer.len().saturating_sub(2) as f32);
    let whole = delay as usize;
    let fraction = delay - whole as f32;
    let newer_index = (write_cursor + buffer.len() - whole) % buffer.len();
    let older_index = (newer_index + buffer.len() - 1) % buffer.len();
    let newer = buffer[newer_index];
    newer + (buffer[older_index] - newer) * fraction
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
