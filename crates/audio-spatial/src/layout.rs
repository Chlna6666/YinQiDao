use crate::pose::Vec3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    FullRange,
    Lfe,
}

/// Semantic channel role for one authored interleaved PCM slot.
///
/// The role array is deliberately kept beside `SpeakerLayout` so renderer geometry, Debug labels and
/// AVS3/Audio Vivid channel-bed conformance can share one explicit ordering contract instead of
/// inferring meaning from a bare channel index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelRole {
    FrontLeft,
    FrontRight,
    Center,
    Lfe,
    SurroundLeft,
    SurroundRight,
    RearLeft,
    RearRight,
    TopFrontLeft,
    TopFrontRight,
    TopRearLeft,
    TopRearRight,
}

impl ChannelRole {
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::FrontLeft => "FL",
            Self::FrontRight => "FR",
            Self::Center => "C",
            Self::Lfe => "LFE",
            Self::SurroundLeft => "SL",
            Self::SurroundRight => "SR",
            Self::RearLeft => "RL",
            Self::RearRight => "RR",
            Self::TopFrontLeft => "TFL",
            Self::TopFrontRight => "TFR",
            Self::TopRearLeft => "TRL",
            Self::TopRearRight => "TRR",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speaker {
    pub direction: Vec3,
    pub gain: f32,
    pub kind: SourceKind,
}

impl Speaker {
    pub const fn full_range(direction: Vec3, gain: f32) -> Self {
        Self {
            direction,
            gain,
            kind: SourceKind::FullRange,
        }
    }

    pub const fn lfe(gain: f32) -> Self {
        Self {
            direction: Vec3::FORWARD,
            gain,
            kind: SourceKind::Lfe,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelLayout {
    Stereo,
    Surround5_1,
    Surround7_1,
    Surround5_1_2,
    Surround5_1_4,
    Surround7_1_2,
    Surround7_1_4,
}

#[derive(Clone, Copy, Debug)]
pub struct SpeakerLayout {
    speakers: &'static [Speaker],
    roles: &'static [ChannelRole],
    normalization: f32,
}

impl SpeakerLayout {
    pub const fn speakers(self) -> &'static [Speaker] {
        self.speakers
    }

    pub const fn roles(self) -> &'static [ChannelRole] {
        self.roles
    }

    pub const fn role(self, channel_index: usize) -> Option<ChannelRole> {
        if channel_index < self.roles.len() {
            Some(self.roles[channel_index])
        } else {
            None
        }
    }

    pub const fn channels(self) -> usize {
        self.speakers.len()
    }

    pub const fn normalization(self) -> f32 {
        self.normalization
    }

    pub const fn for_layout(layout: ChannelLayout) -> Self {
        match layout {
            ChannelLayout::Stereo => Self {
                speakers: &STEREO,
                roles: &STEREO_ROLES,
                normalization: 0.92,
            },
            ChannelLayout::Surround5_1 => Self {
                speakers: &SURROUND_5_1,
                roles: &SURROUND_5_1_ROLES,
                normalization: 0.62,
            },
            ChannelLayout::Surround7_1 => Self {
                speakers: &SURROUND_7_1,
                roles: &SURROUND_7_1_ROLES,
                normalization: 0.54,
            },
            ChannelLayout::Surround5_1_2 => Self {
                speakers: &SURROUND_5_1_2,
                roles: &SURROUND_5_1_2_ROLES,
                normalization: 0.54,
            },
            ChannelLayout::Surround5_1_4 => Self {
                speakers: &SURROUND_5_1_4,
                roles: &SURROUND_5_1_4_ROLES,
                normalization: 0.46,
            },
            ChannelLayout::Surround7_1_2 => Self {
                speakers: &SURROUND_7_1_2,
                roles: &SURROUND_7_1_2_ROLES,
                normalization: 0.46,
            },
            ChannelLayout::Surround7_1_4 => Self {
                speakers: &SURROUND_7_1_4,
                roles: &SURROUND_7_1_4_ROLES,
                normalization: 0.42,
            },
        }
    }
}

const FRONT_LEFT: Vec3 = Vec3::new(-0.5, 0.0, 0.866_025_4);
const FRONT_RIGHT: Vec3 = Vec3::new(0.5, 0.0, 0.866_025_4);
const CENTER: Vec3 = Vec3::FORWARD;
const SIDE_LEFT: Vec3 = Vec3::new(-1.0, 0.0, 0.0);
const SIDE_RIGHT: Vec3 = Vec3::RIGHT;
const REAR_125_LEFT: Vec3 = Vec3::new(-0.819_152, 0.0, -0.573_576_5);
const REAR_125_RIGHT: Vec3 = Vec3::new(0.819_152, 0.0, -0.573_576_5);
const REAR_145_LEFT: Vec3 = Vec3::new(-0.573_576_5, 0.0, -0.819_152);
const REAR_145_RIGHT: Vec3 = Vec3::new(0.573_576_5, 0.0, -0.819_152);
const TOP_FRONT_LEFT: Vec3 = Vec3::new(-0.405_579_8, 0.707_106_77, 0.579_228);
const TOP_FRONT_RIGHT: Vec3 = Vec3::new(0.405_579_8, 0.707_106_77, 0.579_228);
const TOP_REAR_LEFT: Vec3 = Vec3::new(-0.405_579_8, 0.707_106_77, -0.579_228);
const TOP_REAR_RIGHT: Vec3 = Vec3::new(0.405_579_8, 0.707_106_77, -0.579_228);

const STEREO_ROLES: [ChannelRole; 2] = [ChannelRole::FrontLeft, ChannelRole::FrontRight];
const SURROUND_5_1_ROLES: [ChannelRole; 6] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
];
const SURROUND_7_1_ROLES: [ChannelRole; 8] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::RearLeft,
    ChannelRole::RearRight,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
];
/// AVS3/Audio Vivid 5.1.2 bed extends the 5.1 slot order with top-front L/R.
const SURROUND_5_1_2_ROLES: [ChannelRole; 8] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
    ChannelRole::TopFrontLeft,
    ChannelRole::TopFrontRight,
];
const SURROUND_5_1_4_ROLES: [ChannelRole; 10] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
    ChannelRole::TopFrontLeft,
    ChannelRole::TopFrontRight,
    ChannelRole::TopRearLeft,
    ChannelRole::TopRearRight,
];
/// AVS3/Audio Vivid 7.1.2 bed extends the 7.1 slot order with top-front L/R.
const SURROUND_7_1_2_ROLES: [ChannelRole; 10] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::RearLeft,
    ChannelRole::RearRight,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
    ChannelRole::TopFrontLeft,
    ChannelRole::TopFrontRight,
];
/// AVS3/Audio Vivid channel-bed contract used by the decoder→spatial handoff:
/// FL, FR, C, LFE, rear-L/R, side-L/R, top-front-L/R, top-rear-L/R.
const SURROUND_7_1_4_ROLES: [ChannelRole; 12] = [
    ChannelRole::FrontLeft,
    ChannelRole::FrontRight,
    ChannelRole::Center,
    ChannelRole::Lfe,
    ChannelRole::RearLeft,
    ChannelRole::RearRight,
    ChannelRole::SurroundLeft,
    ChannelRole::SurroundRight,
    ChannelRole::TopFrontLeft,
    ChannelRole::TopFrontRight,
    ChannelRole::TopRearLeft,
    ChannelRole::TopRearRight,
];

const STEREO: [Speaker; 2] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
];
const SURROUND_5_1: [Speaker; 6] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_125_LEFT, 0.78),
    Speaker::full_range(REAR_125_RIGHT, 0.78),
];
const SURROUND_7_1: [Speaker; 8] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_145_LEFT, 0.72),
    Speaker::full_range(REAR_145_RIGHT, 0.72),
    Speaker::full_range(SIDE_LEFT, 0.78),
    Speaker::full_range(SIDE_RIGHT, 0.78),
];
const SURROUND_5_1_2: [Speaker; 8] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_125_LEFT, 0.76),
    Speaker::full_range(REAR_125_RIGHT, 0.76),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64),
    Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
];
const SURROUND_5_1_4: [Speaker; 10] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_125_LEFT, 0.76),
    Speaker::full_range(REAR_125_RIGHT, 0.76),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64),
    Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
    Speaker::full_range(TOP_REAR_LEFT, 0.58),
    Speaker::full_range(TOP_REAR_RIGHT, 0.58),
];
const SURROUND_7_1_2: [Speaker; 10] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_145_LEFT, 0.72),
    Speaker::full_range(REAR_145_RIGHT, 0.72),
    Speaker::full_range(SIDE_LEFT, 0.78),
    Speaker::full_range(SIDE_RIGHT, 0.78),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64),
    Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
];
const SURROUND_7_1_4: [Speaker; 12] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90),
    Speaker::lfe(0.34),
    Speaker::full_range(REAR_145_LEFT, 0.72),
    Speaker::full_range(REAR_145_RIGHT, 0.72),
    Speaker::full_range(SIDE_LEFT, 0.78),
    Speaker::full_range(SIDE_RIGHT, 0.78),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64),
    Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
    Speaker::full_range(TOP_REAR_LEFT, 0.58),
    Speaker::full_range(TOP_REAR_RIGHT, 0.58),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_layout_has_one_role_per_speaker() {
        for layout in [
            ChannelLayout::Stereo,
            ChannelLayout::Surround5_1,
            ChannelLayout::Surround7_1,
            ChannelLayout::Surround5_1_2,
            ChannelLayout::Surround5_1_4,
            ChannelLayout::Surround7_1_2,
            ChannelLayout::Surround7_1_4,
        ] {
            let layout = SpeakerLayout::for_layout(layout);
            assert_eq!(layout.roles().len(), layout.speakers().len());
        }
    }

    #[test]
    fn avs3_five_one_two_channel_order_is_explicit() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround5_1_2);
        assert_eq!(
            layout.roles(),
            &[
                ChannelRole::FrontLeft,
                ChannelRole::FrontRight,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::SurroundLeft,
                ChannelRole::SurroundRight,
                ChannelRole::TopFrontLeft,
                ChannelRole::TopFrontRight,
            ]
        );
        assert_eq!(layout.channels(), 8);
        assert!(layout.speakers()[6..8].iter().all(|speaker| speaker.direction.y > 0.0));
    }

    #[test]
    fn avs3_five_one_four_channel_order_is_explicit() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround5_1_4);
        assert_eq!(
            layout.roles(),
            &[
                ChannelRole::FrontLeft,
                ChannelRole::FrontRight,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::SurroundLeft,
                ChannelRole::SurroundRight,
                ChannelRole::TopFrontLeft,
                ChannelRole::TopFrontRight,
                ChannelRole::TopRearLeft,
                ChannelRole::TopRearRight,
            ]
        );
        assert_eq!(layout.speakers()[3].kind, SourceKind::Lfe);
    }

    #[test]
    fn avs3_seven_one_two_channel_order_is_explicit() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_2);
        assert_eq!(
            layout.roles(),
            &[
                ChannelRole::FrontLeft,
                ChannelRole::FrontRight,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::RearLeft,
                ChannelRole::RearRight,
                ChannelRole::SurroundLeft,
                ChannelRole::SurroundRight,
                ChannelRole::TopFrontLeft,
                ChannelRole::TopFrontRight,
            ]
        );
        assert_eq!(layout.channels(), 10);
        assert!(layout.speakers()[8..10].iter().all(|speaker| speaker.direction.y > 0.0));
    }

    #[test]
    fn avs3_seven_one_four_channel_order_is_explicit() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        assert_eq!(
            layout.roles(),
            &[
                ChannelRole::FrontLeft,
                ChannelRole::FrontRight,
                ChannelRole::Center,
                ChannelRole::Lfe,
                ChannelRole::RearLeft,
                ChannelRole::RearRight,
                ChannelRole::SurroundLeft,
                ChannelRole::SurroundRight,
                ChannelRole::TopFrontLeft,
                ChannelRole::TopFrontRight,
                ChannelRole::TopRearLeft,
                ChannelRole::TopRearRight,
            ]
        );
        assert_eq!(layout.channels(), 12);
        assert_eq!(layout.speakers()[3].kind, SourceKind::Lfe);
        assert!(layout.speakers()[4].direction.z < 0.0);
        assert!(layout.speakers()[5].direction.z < 0.0);
        assert_eq!(layout.speakers()[6].direction.z, 0.0);
        assert_eq!(layout.speakers()[7].direction.z, 0.0);
        assert!(layout.speakers()[8..12].iter().all(|speaker| speaker.direction.y > 0.0));
    }

    #[test]
    fn role_short_names_are_stable_for_debug_and_transport_contracts() {
        assert_eq!(ChannelRole::Lfe.short_name(), "LFE");
        assert_eq!(ChannelRole::RearLeft.short_name(), "RL");
        assert_eq!(ChannelRole::SurroundRight.short_name(), "SR");
        assert_eq!(ChannelRole::TopRearRight.short_name(), "TRR");
    }
}
