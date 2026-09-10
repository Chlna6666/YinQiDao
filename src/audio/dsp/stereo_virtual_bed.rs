use std::f32::consts::PI;

use yinqidao_audio_spatial::{ChannelLayout, ChannelRole, SpeakerLayout};

#[derive(Clone, Debug)]
pub(super) struct StereoVirtualBed {
    side_low: f32,
    side_body: f32,
    low_alpha: f32,
    body_alpha: f32,
}

impl StereoVirtualBed {
    pub(super) fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        Self {
            side_low: 0.0,
            side_body: 0.0,
            low_alpha: one_pole_alpha(sample_rate, 680.0),
            body_alpha: one_pole_alpha(sample_rate, 4_200.0),
        }
    }

    pub(super) fn reset(&mut self) {
        self.side_low = 0.0;
        self.side_body = 0.0;
    }

    /// Expand authored stereo into an explicit virtual speaker bed without inventing a synthetic
    /// LFE channel or adding Haas-style copies. Only the authored side component is distributed to
    /// surround/rear/height roles, so mono/centre programme remains anchored at the front.
    ///
    /// The crossover state is fixed-size and streaming. No allocation, delay line or random phase
    /// generator exists in this path.
    pub(super) fn render(
        &mut self,
        input: &[f32],
        layout: ChannelLayout,
        output: &mut [f32],
    ) -> Option<usize> {
        if input.len() % 2 != 0 {
            return None;
        }
        let layout = SpeakerLayout::for_layout(layout);
        let channels = layout.channels();
        if channels <= 2 {
            return None;
        }
        let frames = input.len() / 2;
        if output.len() < frames.saturating_mul(channels) {
            return None;
        }

        for (frame_index, stereo) in input.as_chunks::<2>().0.iter().enumerate() {
            let left = finite_or_zero(stereo[0]);
            let right = finite_or_zero(stereo[1]);
            let mid = (left + right) * 0.5;
            let side = (left - right) * 0.5;

            self.side_low += self.low_alpha * (side - self.side_low);
            self.side_body += self.body_alpha * (side - self.side_body);
            let side_low = self.side_low;
            let side_mid = self.side_body - side_low;
            let side_high = side - self.side_body;

            // Keep the original L/R pair dominant. Surround roles receive complementary spectral
            // portions of the authored side signal, which decorrelates spatial feeds without
            // duplicate delayed full-band programme and the resulting comb filtering.
            let surround = side_mid * 0.70 + side_high * 0.10;
            let rear = side_low * 0.40 + side_mid * 0.34;
            let top_front = side_high * 0.52 + side_mid * 0.08;
            let top_rear = side_high * 0.32 - side_mid * 0.16;

            let frame_start = frame_index * channels;
            let frame_out = &mut output[frame_start..frame_start + channels];
            for (channel_index, role) in layout.roles().iter().copied().enumerate() {
                frame_out[channel_index] = match role {
                    ChannelRole::FrontLeft => left * 0.96,
                    ChannelRole::FrontRight => right * 0.96,
                    ChannelRole::Center => mid * 0.28,
                    // Stereo has no authored effects/LFE stem. Duplicating bass into LFE would
                    // increase low-frequency energy and is not a valid upmix inference.
                    ChannelRole::Lfe => 0.0,
                    ChannelRole::SurroundLeft => surround,
                    ChannelRole::SurroundRight => -surround,
                    ChannelRole::RearLeft => rear,
                    ChannelRole::RearRight => -rear,
                    ChannelRole::TopFrontLeft => top_front,
                    ChannelRole::TopFrontRight => -top_front,
                    ChannelRole::TopRearLeft => top_rear,
                    ChannelRole::TopRearRight => -top_rear,
                };
            }
        }

        Some(frames)
    }
}

#[inline]
fn one_pole_alpha(sample_rate: f32, cutoff_hz: f32) -> f32 {
    1.0 - (-2.0 * PI * cutoff_hz / sample_rate.max(1.0)).exp()
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_programme_stays_front_anchored_and_does_not_invent_lfe() {
        let mut upmixer = StereoVirtualBed::new(48_000);
        let input = [0.25_f32, 0.25, 0.50, 0.50];
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut output = [0.0_f32; 24];
        assert_eq!(
            upmixer.render(&input, ChannelLayout::Surround7_1_4, &mut output),
            Some(2)
        );
        for frame in output.chunks_exact(layout.channels()) {
            for (index, role) in layout.roles().iter().copied().enumerate() {
                match role {
                    ChannelRole::FrontLeft | ChannelRole::FrontRight | ChannelRole::Center => {}
                    _ => assert_eq!(frame[index], 0.0),
                }
            }
        }
    }

    #[test]
    fn stereo_side_energy_populates_surround_rear_and_height_roles() {
        let mut upmixer = StereoVirtualBed::new(48_000);
        let input = (0..128).flat_map(|_| [0.5_f32, -0.5]).collect::<Vec<_>>();
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut output = vec![0.0_f32; 128 * layout.channels()];
        assert_eq!(
            upmixer.render(&input, ChannelLayout::Surround7_1_4, &mut output),
            Some(128)
        );
        let last = &output[..layout.channels()];
        let role_value = |role| {
            let index = layout
                .roles()
                .iter()
                .position(|candidate| *candidate == role)
                .expect("role");
            last[index]
        };
        assert_eq!(role_value(ChannelRole::Lfe), 0.0);
        assert!(role_value(ChannelRole::SurroundLeft).abs() > 1.0e-3);
        assert!(role_value(ChannelRole::RearLeft).abs() > 1.0e-3);
        assert!(role_value(ChannelRole::TopFrontLeft).abs() > 1.0e-3);
        assert!(role_value(ChannelRole::TopRearLeft).abs() > 1.0e-3);
    }

    #[test]
    fn every_supported_native_layout_uses_its_explicit_role_contract() {
        let layouts = [
            ChannelLayout::Surround5_1,
            ChannelLayout::Surround7_1,
            ChannelLayout::Surround5_1_2,
            ChannelLayout::Surround5_1_4,
            ChannelLayout::Surround7_1_2,
            ChannelLayout::Surround7_1_4,
        ];
        let mut upmixer = StereoVirtualBed::new(48_000);
        for layout in layouts {
            upmixer.reset();
            let speakers = SpeakerLayout::for_layout(layout);
            let mut output = vec![0.0_f32; speakers.channels()];
            assert_eq!(upmixer.render(&[0.2, -0.1], layout, &mut output), Some(1));
            assert_eq!(output.len(), speakers.roles().len());
            let lfe = speakers
                .roles()
                .iter()
                .position(|role| *role == ChannelRole::Lfe)
                .expect("LFE role");
            assert_eq!(output[lfe], 0.0);
        }
    }
}
