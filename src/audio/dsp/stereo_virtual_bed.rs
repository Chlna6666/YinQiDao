use std::f32::consts::PI;

use yinqidao_audio_spatial::{ChannelLayout, ChannelRole, SpeakerLayout};

const FRONT_LEFT_RIGHT_GAIN: f32 = 0.96;
const CENTER_GAIN: f32 = 0.28;
const MAX_FRONT_PRE_GAIN: f32 = 1.60;
const MAX_AUXILIARY_PRE_GAIN: f32 = 1.25;

// Authored Side keeps the lateral/difference information of the stereo mix. These coefficients are
// deliberately frequency-selective so the virtual bed does not become a set of delayed full-band
// copies of L/R.
const SIDE_SURROUND_MID: f32 = 0.70;
const SIDE_SURROUND_HIGH: f32 = 0.10;
const SIDE_REAR_LOW: f32 = 0.40;
const SIDE_REAR_MID: f32 = 0.34;
const SIDE_TOP_FRONT_MID: f32 = 0.08;
const SIDE_TOP_FRONT_HIGH: f32 = 0.52;
const SIDE_TOP_REAR_MID: f32 = -0.16;
const SIDE_TOP_REAR_HIGH: f32 = 0.32;

// Mid is not treated as "front only". A centre-heavy vocal or mono programme must still inhabit the
// listener-centric sphere. We therefore build a lower-gain, spectrally complementary spherical
// support field from Mid while retaining the original L/R and centre channels as the direct anchor.
// No random phase or Haas delay is used; each region receives a different deterministic band blend.
const MID_SURROUND_LOW: f32 = 0.08;
const MID_SURROUND_MID: f32 = 0.30;
const MID_SURROUND_HIGH: f32 = 0.12;
const MID_REAR_LOW: f32 = 0.18;
const MID_REAR_MID: f32 = 0.22;
const MID_REAR_HIGH: f32 = -0.06;
const MID_TOP_FRONT_LOW: f32 = -0.04;
const MID_TOP_FRONT_MID: f32 = 0.10;
const MID_TOP_FRONT_HIGH: f32 = 0.28;
const MID_TOP_REAR_LOW: f32 = 0.06;
const MID_TOP_REAR_MID: f32 = -0.14;
const MID_TOP_REAR_HIGH: f32 = 0.20;

#[derive(Clone, Copy, Debug, Default)]
struct VirtualBedRoleMap {
    front_left: Option<usize>,
    front_right: Option<usize>,
    center: Option<usize>,
    lfe: Option<usize>,
    surround_left: Option<usize>,
    surround_right: Option<usize>,
    rear_left: Option<usize>,
    rear_right: Option<usize>,
    top_front_left: Option<usize>,
    top_front_right: Option<usize>,
    top_rear_left: Option<usize>,
    top_rear_right: Option<usize>,
}

impl VirtualBedRoleMap {
    fn for_layout(layout: SpeakerLayout) -> Self {
        let mut map = Self::default();
        for (index, role) in layout.roles().iter().copied().enumerate() {
            let slot = match role {
                ChannelRole::FrontLeft => &mut map.front_left,
                ChannelRole::FrontRight => &mut map.front_right,
                ChannelRole::Center => &mut map.center,
                ChannelRole::Lfe => &mut map.lfe,
                ChannelRole::SurroundLeft => &mut map.surround_left,
                ChannelRole::SurroundRight => &mut map.surround_right,
                ChannelRole::RearLeft => &mut map.rear_left,
                ChannelRole::RearRight => &mut map.rear_right,
                ChannelRole::TopFrontLeft => &mut map.top_front_left,
                ChannelRole::TopFrontRight => &mut map.top_front_right,
                ChannelRole::TopRearLeft => &mut map.top_rear_left,
                ChannelRole::TopRearRight => &mut map.top_rear_right,
            };
            debug_assert!(slot.is_none(), "duplicate virtual-bed channel role");
            *slot = Some(index);
        }
        map
    }
}

#[derive(Clone, Copy, Debug)]
struct VirtualBedMixProfile {
    roles: VirtualBedRoleMap,
    front_pre_gain: f32,
    auxiliary_pre_gain: f32,
}

impl VirtualBedMixProfile {
    fn for_layout(layout_kind: ChannelLayout) -> Self {
        let layout = SpeakerLayout::for_layout(layout_kind);
        let reference = SpeakerLayout::for_layout(ChannelLayout::Surround5_1);

        // The spatial renderer applies `Speaker::gain` per source and then one layout-wide
        // normalization factor. Without compensation that makes the authored front pair about 32%
        // quieter when the virtual bed grows from 5.1 (0.62) to 7.1.4 (0.42), even though no extra
        // authored programme energy was added. Preserve the 5.1 front anchor before HRTF rendering.
        let front_pre_gain = safe_ratio(reference.normalization(), layout.normalization())
            .clamp(0.0, MAX_FRONT_PRE_GAIN);

        // The spherical support field contains both Side-derived directionality and Mid-derived
        // centre energy. Normalize its aggregate nominal band power after speaker gain + layout
        // normalization so .2/.4 layouts spread the same programme around more directions without
        // gaining loudness merely because more virtual sources exist.
        let reference_auxiliary_power = auxiliary_effective_power(reference);
        let layout_auxiliary_power = auxiliary_effective_power(layout);
        let auxiliary_pre_gain = if reference_auxiliary_power > 1.0e-12
            && layout_auxiliary_power > 1.0e-12
        {
            safe_ratio(
                reference_auxiliary_power.sqrt(),
                layout_auxiliary_power.sqrt(),
            )
            .clamp(0.0, MAX_AUXILIARY_PRE_GAIN)
        } else {
            1.0
        };

        Self {
            roles: VirtualBedRoleMap::for_layout(layout),
            front_pre_gain,
            auxiliary_pre_gain,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct StereoVirtualBed {
    side_low: f32,
    side_body: f32,
    mid_low: f32,
    mid_body: f32,
    low_alpha: f32,
    body_alpha: f32,
    profile_layout: ChannelLayout,
    profile: VirtualBedMixProfile,
}

impl StereoVirtualBed {
    pub(super) fn new(sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(1) as f32;
        let profile_layout = ChannelLayout::Surround5_1;
        Self {
            side_low: 0.0,
            side_body: 0.0,
            mid_low: 0.0,
            mid_body: 0.0,
            low_alpha: one_pole_alpha(sample_rate, 680.0),
            body_alpha: one_pole_alpha(sample_rate, 4_200.0),
            profile_layout,
            profile: VirtualBedMixProfile::for_layout(profile_layout),
        }
    }

    pub(super) fn reset(&mut self) {
        self.side_low = 0.0;
        self.side_body = 0.0;
        self.mid_low = 0.0;
        self.mid_body = 0.0;
    }

    /// Expand authored stereo into an explicit listener-centric virtual speaker bed without
    /// inventing a synthetic LFE channel or adding Haas-style copies.
    ///
    /// The original L/R pair remains the direct anchor, but centre-correlated Mid content is also
    /// projected into surround/rear/height roles through complementary frequency bands. This means
    /// centre vocals and mono programme still occupy the complete spherical scene instead of being
    /// artificially pinned to the front. Scene Motion can then rotate this complete bed for
    /// 8D/360/FrontBack/Planetary/NearEar just like an authored multichannel bed.
    ///
    /// The crossover state is fixed-size and streaming. Channel-role lookup and layout energy
    /// compensation are rebuilt only when the selected virtual layout changes; the per-frame path
    /// contains no role matching, allocation, delay line or random phase generator.
    pub(super) fn render(
        &mut self,
        input: &[f32],
        layout_kind: ChannelLayout,
        output: &mut [f32],
    ) -> Option<usize> {
        if input.len() % 2 != 0 {
            return None;
        }
        let layout = SpeakerLayout::for_layout(layout_kind);
        let channels = layout.channels();
        if channels <= 2 {
            return None;
        }
        let frames = input.len() / 2;
        if output.len() < frames.saturating_mul(channels) {
            return None;
        }

        if self.profile_layout != layout_kind {
            self.profile = VirtualBedMixProfile::for_layout(layout_kind);
            self.profile_layout = layout_kind;
        }
        let profile = self.profile;
        let roles = profile.roles;
        let front_pre_gain = profile.front_pre_gain;
        let auxiliary_pre_gain = profile.auxiliary_pre_gain;

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

            self.mid_low += self.low_alpha * (mid - self.mid_low);
            self.mid_body += self.body_alpha * (mid - self.mid_body);
            let mid_low = self.mid_low;
            let mid_mid = self.mid_body - mid_low;
            let mid_high = mid - self.mid_body;

            // Side provides left/right opposition; Mid provides coherent spherical support. Keeping
            // the two components separate until the final role write preserves authored stereo
            // directionality while allowing centre-heavy material to exist behind/above the listener.
            let side_surround = side_mid * SIDE_SURROUND_MID + side_high * SIDE_SURROUND_HIGH;
            let side_rear = side_low * SIDE_REAR_LOW + side_mid * SIDE_REAR_MID;
            let side_top_front =
                side_high * SIDE_TOP_FRONT_HIGH + side_mid * SIDE_TOP_FRONT_MID;
            let side_top_rear = side_high * SIDE_TOP_REAR_HIGH + side_mid * SIDE_TOP_REAR_MID;

            let mid_surround = mid_low * MID_SURROUND_LOW
                + mid_mid * MID_SURROUND_MID
                + mid_high * MID_SURROUND_HIGH;
            let mid_rear =
                mid_low * MID_REAR_LOW + mid_mid * MID_REAR_MID + mid_high * MID_REAR_HIGH;
            let mid_top_front = mid_low * MID_TOP_FRONT_LOW
                + mid_mid * MID_TOP_FRONT_MID
                + mid_high * MID_TOP_FRONT_HIGH;
            let mid_top_rear = mid_low * MID_TOP_REAR_LOW
                + mid_mid * MID_TOP_REAR_MID
                + mid_high * MID_TOP_REAR_HIGH;

            let surround_left = (mid_surround + side_surround) * auxiliary_pre_gain;
            let surround_right = (mid_surround - side_surround) * auxiliary_pre_gain;
            let rear_left = (mid_rear + side_rear) * auxiliary_pre_gain;
            let rear_right = (mid_rear - side_rear) * auxiliary_pre_gain;
            let top_front_left = (mid_top_front + side_top_front) * auxiliary_pre_gain;
            let top_front_right = (mid_top_front - side_top_front) * auxiliary_pre_gain;
            let top_rear_left = (mid_top_rear + side_top_rear) * auxiliary_pre_gain;
            let top_rear_right = (mid_top_rear - side_top_rear) * auxiliary_pre_gain;

            let frame_start = frame_index * channels;
            let frame_out = &mut output[frame_start..frame_start + channels];
            write_channel(
                frame_out,
                roles.front_left,
                left * FRONT_LEFT_RIGHT_GAIN * front_pre_gain,
            );
            write_channel(
                frame_out,
                roles.front_right,
                right * FRONT_LEFT_RIGHT_GAIN * front_pre_gain,
            );
            write_channel(frame_out, roles.center, mid * CENTER_GAIN * front_pre_gain);
            // Stereo has no authored effects/LFE stem. The spherical field uses only full-range
            // speaker roles; duplicating bass into LFE would be a false channel inference.
            write_channel(frame_out, roles.lfe, 0.0);
            write_channel(frame_out, roles.surround_left, surround_left);
            write_channel(frame_out, roles.surround_right, surround_right);
            write_channel(frame_out, roles.rear_left, rear_left);
            write_channel(frame_out, roles.rear_right, rear_right);
            write_channel(frame_out, roles.top_front_left, top_front_left);
            write_channel(frame_out, roles.top_front_right, top_front_right);
            write_channel(frame_out, roles.top_rear_left, top_rear_left);
            write_channel(frame_out, roles.top_rear_right, top_rear_right);
        }

        Some(frames)
    }
}

#[inline(always)]
fn write_channel(frame: &mut [f32], index: Option<usize>, value: f32) {
    if let Some(index) = index {
        frame[index] = value;
    }
}

#[inline]
fn auxiliary_effective_power(layout: SpeakerLayout) -> f32 {
    let mut source_power = 0.0;
    for (speaker, role) in layout
        .speakers()
        .iter()
        .zip(layout.roles().iter().copied())
    {
        let band_power = auxiliary_band_power(role);
        source_power += speaker.gain * speaker.gain * band_power;
    }
    let normalization = layout.normalization();
    source_power * normalization * normalization
}

#[inline]
const fn auxiliary_band_power(role: ChannelRole) -> f32 {
    match role {
        ChannelRole::SurroundLeft | ChannelRole::SurroundRight => {
            SIDE_SURROUND_MID * SIDE_SURROUND_MID
                + SIDE_SURROUND_HIGH * SIDE_SURROUND_HIGH
                + MID_SURROUND_LOW * MID_SURROUND_LOW
                + MID_SURROUND_MID * MID_SURROUND_MID
                + MID_SURROUND_HIGH * MID_SURROUND_HIGH
        }
        ChannelRole::RearLeft | ChannelRole::RearRight => {
            SIDE_REAR_LOW * SIDE_REAR_LOW
                + SIDE_REAR_MID * SIDE_REAR_MID
                + MID_REAR_LOW * MID_REAR_LOW
                + MID_REAR_MID * MID_REAR_MID
                + MID_REAR_HIGH * MID_REAR_HIGH
        }
        ChannelRole::TopFrontLeft | ChannelRole::TopFrontRight => {
            SIDE_TOP_FRONT_MID * SIDE_TOP_FRONT_MID
                + SIDE_TOP_FRONT_HIGH * SIDE_TOP_FRONT_HIGH
                + MID_TOP_FRONT_LOW * MID_TOP_FRONT_LOW
                + MID_TOP_FRONT_MID * MID_TOP_FRONT_MID
                + MID_TOP_FRONT_HIGH * MID_TOP_FRONT_HIGH
        }
        ChannelRole::TopRearLeft | ChannelRole::TopRearRight => {
            SIDE_TOP_REAR_MID * SIDE_TOP_REAR_MID
                + SIDE_TOP_REAR_HIGH * SIDE_TOP_REAR_HIGH
                + MID_TOP_REAR_LOW * MID_TOP_REAR_LOW
                + MID_TOP_REAR_MID * MID_TOP_REAR_MID
                + MID_TOP_REAR_HIGH * MID_TOP_REAR_HIGH
        }
        ChannelRole::FrontLeft
        | ChannelRole::FrontRight
        | ChannelRole::Center
        | ChannelRole::Lfe => 0.0,
    }
}

#[inline]
fn safe_ratio(numerator: f32, denominator: f32) -> f32 {
    if numerator.is_finite() && denominator.is_finite() && denominator.abs() > 1.0e-12 {
        numerator / denominator
    } else {
        1.0
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

    const VIRTUAL_LAYOUTS: [ChannelLayout; 6] = [
        ChannelLayout::Surround5_1,
        ChannelLayout::Surround7_1,
        ChannelLayout::Surround5_1_2,
        ChannelLayout::Surround5_1_4,
        ChannelLayout::Surround7_1_2,
        ChannelLayout::Surround7_1_4,
    ];

    #[test]
    fn mono_programme_populates_the_spherical_bed_without_inventing_lfe() {
        let mut upmixer = StereoVirtualBed::new(48_000);
        let input = (0..512)
            .flat_map(|index| {
                let phase = index as f32 * 0.173;
                let sample = 0.36 * phase.sin() + 0.14 * (phase * 7.1).sin();
                [sample, sample]
            })
            .collect::<Vec<_>>();
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut output = vec![0.0_f32; 512 * layout.channels()];
        assert_eq!(
            upmixer.render(&input, ChannelLayout::Surround7_1_4, &mut output),
            Some(512)
        );

        let mut role_energy = [0.0_f32; 12];
        for frame in output.chunks_exact(layout.channels()).skip(64) {
            for (index, sample) in frame.iter().copied().enumerate() {
                role_energy[index] += sample * sample;
            }
        }
        for (index, role) in layout.roles().iter().copied().enumerate() {
            match role {
                ChannelRole::Lfe => assert_eq!(role_energy[index], 0.0),
                _ => assert!(
                    role_energy[index] > 1.0e-5,
                    "mono spherical support missing for {role:?}"
                ),
            }
        }
    }

    #[test]
    fn mono_spherical_support_keeps_left_right_pairs_symmetric() {
        let mut upmixer = StereoVirtualBed::new(48_000);
        let input = (0..256)
            .flat_map(|index| {
                let sample = (index as f32 * 0.11).sin() * 0.4;
                [sample, sample]
            })
            .collect::<Vec<_>>();
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut output = vec![0.0_f32; 256 * layout.channels()];
        assert_eq!(
            upmixer.render(&input, ChannelLayout::Surround7_1_4, &mut output),
            Some(256)
        );
        let roles = VirtualBedRoleMap::for_layout(layout);
        for frame in output.chunks_exact(layout.channels()) {
            for (left, right) in [
                (roles.surround_left, roles.surround_right),
                (roles.rear_left, roles.rear_right),
                (roles.top_front_left, roles.top_front_right),
                (roles.top_rear_left, roles.top_rear_right),
            ] {
                let left = frame[left.expect("left role")];
                let right = frame[right.expect("right role")];
                assert!((left - right).abs() <= 1.0e-6);
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
        let last = &output[(127 * layout.channels())..];
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
        let mut upmixer = StereoVirtualBed::new(48_000);
        for layout in VIRTUAL_LAYOUTS {
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

    #[test]
    fn seven_one_four_role_map_matches_the_native_channel_contract() {
        let roles = VirtualBedRoleMap::for_layout(SpeakerLayout::for_layout(
            ChannelLayout::Surround7_1_4,
        ));
        assert_eq!(roles.front_left, Some(0));
        assert_eq!(roles.front_right, Some(1));
        assert_eq!(roles.center, Some(2));
        assert_eq!(roles.lfe, Some(3));
        assert_eq!(roles.rear_left, Some(4));
        assert_eq!(roles.rear_right, Some(5));
        assert_eq!(roles.surround_left, Some(6));
        assert_eq!(roles.surround_right, Some(7));
        assert_eq!(roles.top_front_left, Some(8));
        assert_eq!(roles.top_front_right, Some(9));
        assert_eq!(roles.top_rear_left, Some(10));
        assert_eq!(roles.top_rear_right, Some(11));
    }

    #[test]
    fn front_anchor_matches_five_one_after_layout_normalization() {
        let reference = SpeakerLayout::for_layout(ChannelLayout::Surround5_1).normalization();
        for layout_kind in VIRTUAL_LAYOUTS {
            let layout = SpeakerLayout::for_layout(layout_kind);
            let profile = VirtualBedMixProfile::for_layout(layout_kind);
            let effective = profile.front_pre_gain * layout.normalization();
            assert!(
                (effective - reference).abs() <= 1.0e-6,
                "{layout_kind:?}: effective={effective}, reference={reference}"
            );
        }
    }

    #[test]
    fn spherical_support_power_matches_five_one_reference_across_layouts() {
        let reference = auxiliary_effective_power(SpeakerLayout::for_layout(
            ChannelLayout::Surround5_1,
        ))
        .sqrt();
        for layout_kind in VIRTUAL_LAYOUTS {
            let layout = SpeakerLayout::for_layout(layout_kind);
            let profile = VirtualBedMixProfile::for_layout(layout_kind);
            let effective = profile.auxiliary_pre_gain * auxiliary_effective_power(layout).sqrt();
            assert!(
                (effective - reference).abs() <= 1.0e-6,
                "{layout_kind:?}: effective={effective}, reference={reference}"
            );
        }
    }

    #[test]
    fn layout_pre_gains_are_finite_and_bounded() {
        for layout in VIRTUAL_LAYOUTS {
            let profile = VirtualBedMixProfile::for_layout(layout);
            assert!(profile.front_pre_gain.is_finite());
            assert!(profile.auxiliary_pre_gain.is_finite());
            assert!((0.0..=MAX_FRONT_PRE_GAIN).contains(&profile.front_pre_gain));
            assert!((0.0..=MAX_AUXILIARY_PRE_GAIN).contains(&profile.auxiliary_pre_gain));
        }
    }
}
