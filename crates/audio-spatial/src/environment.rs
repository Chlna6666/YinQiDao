use std::f32::consts::PI;

pub(crate) const EARLY_REFLECTION_TAP_COUNT: usize = 4;
pub(crate) const SPEED_OF_SOUND_M_S: f32 = 343.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvironmentSettings {
    pub mix: f32,
    pub room_size: f32,
    pub damping: f32,
}

impl Default for EnvironmentSettings {
    fn default() -> Self {
        Self {
            mix: 0.10,
            room_size: 0.30,
            damping: 0.45,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReflectionWall {
    Left,
    Right,
    Front,
    Rear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReflectionTapDescriptor {
    pub wall: ReflectionWall,
    pub delay_samples: usize,
    pub gain: f32,
    pub cross_ear: bool,
}

#[derive(Clone, Debug)]
pub struct EarlyReflectionNetwork {
    left: Vec<f32>,
    right: Vec<f32>,
    cursor: usize,
    delays: [usize; EARLY_REFLECTION_TAP_COUNT],
    gains: [f32; EARLY_REFLECTION_TAP_COUNT],
    damping_state_left: f32,
    damping_state_right: f32,
    damping_alpha: f32,
    settings: EnvironmentSettings,
    sample_rate: f32,
}

impl EarlyReflectionNetwork {
    pub fn new(sample_rate: u32, settings: EnvironmentSettings) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let maximum_frames = reflection_ring_len(sample_rate);
        let mut network = Self {
            left: vec![0.0; maximum_frames],
            right: vec![0.0; maximum_frames],
            cursor: 0,
            delays: [1; EARLY_REFLECTION_TAP_COUNT],
            gains: [0.0; EARLY_REFLECTION_TAP_COUNT],
            damping_state_left: 0.0,
            damping_state_right: 0.0,
            damping_alpha: 1.0,
            settings: EnvironmentSettings::default(),
            sample_rate,
        };
        network.set_settings(settings);
        network
    }

    pub fn set_settings(&mut self, settings: EnvironmentSettings) {
        self.settings = EnvironmentSettings {
            mix: settings.mix.clamp(0.0, 0.45),
            room_size: settings.room_size.clamp(0.0, 1.0),
            damping: settings.damping.clamp(0.0, 1.0),
        };
        let descriptors = reflection_tap_descriptors(
            self.sample_rate,
            self.left.len(),
            self.settings,
        );
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            self.delays[index] = descriptor.delay_samples;
            self.gains[index] = descriptor.gain;
        }
        let cutoff_hz = 18_000.0 - self.settings.damping * 12_000.0;
        self.damping_alpha = 1.0 - (-2.0 * PI * cutoff_hz / self.sample_rate).exp();
    }

    #[inline]
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        let frames = left.len().min(right.len());
        let mix = self.settings.mix;
        if mix <= 1.0e-5 {
            return;
        }
        let length = self.left.len();
        for frame in 0..frames {
            let dry_left = left[frame];
            let dry_right = right[frame];
            let mut reflected_left = 0.0;
            let mut reflected_right = 0.0;
            for tap in 0..self.delays.len() {
                let read = (self.cursor + length - self.delays[tap]) % length;
                let gain = self.gains[tap];
                if tap & 1 == 0 {
                    reflected_left += self.right[read] * gain;
                    reflected_right += self.left[read] * gain;
                } else {
                    reflected_left += self.left[read] * gain;
                    reflected_right += self.right[read] * gain;
                }
            }
            self.damping_state_left +=
                self.damping_alpha * (reflected_left - self.damping_state_left);
            self.damping_state_right +=
                self.damping_alpha * (reflected_right - self.damping_state_right);
            self.left[self.cursor] = dry_left;
            self.right[self.cursor] = dry_right;
            self.cursor += 1;
            if self.cursor == length {
                self.cursor = 0;
            }
            left[frame] = dry_left + self.damping_state_left * mix;
            right[frame] = dry_right + self.damping_state_right * mix;
        }
    }

    pub fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.cursor = 0;
        self.damping_state_left = 0.0;
        self.damping_state_right = 0.0;
    }
}

pub(crate) fn reflection_ring_len(sample_rate: f32) -> usize {
    ((sample_rate.max(1.0) * 0.080).ceil() as usize + 8).max(16)
}

pub(crate) fn reflection_tap_descriptors(
    sample_rate: f32,
    ring_len: usize,
    settings: EnvironmentSettings,
) -> [ReflectionTapDescriptor; EARLY_REFLECTION_TAP_COUNT] {
    let room = settings.room_size.clamp(0.0, 1.0);
    let times = [
        0.004 + room * 0.008,
        0.007 + room * 0.013,
        0.011 + room * 0.020,
        0.017 + room * 0.030,
    ];
    let gains = [0.30, 0.22, 0.16, 0.11];
    let walls = [
        ReflectionWall::Left,
        ReflectionWall::Right,
        ReflectionWall::Front,
        ReflectionWall::Rear,
    ];
    std::array::from_fn(|index| ReflectionTapDescriptor {
        wall: walls[index],
        delay_samples: ((times[index] * sample_rate.max(1.0)).round() as usize)
            .clamp(1, ring_len.saturating_sub(1).max(1)),
        gain: gains[index],
        cross_ear: index & 1 == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_descriptors_match_network_delay_and_gain_state() {
        let settings = EnvironmentSettings {
            mix: 0.12,
            room_size: 0.65,
            damping: 0.40,
        };
        let network = EarlyReflectionNetwork::new(48_000, settings);
        let descriptors = reflection_tap_descriptors(
            network.sample_rate,
            network.left.len(),
            network.settings,
        );
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            assert_eq!(descriptor.delay_samples, network.delays[index]);
            assert!((descriptor.gain - network.gains[index]).abs() < f32::EPSILON);
            assert_eq!(descriptor.cross_ear, index & 1 == 0);
        }
    }
}
