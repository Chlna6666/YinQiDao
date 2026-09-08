use crate::pose::Vec3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    FullRange,
    Lfe,
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
    Surround5_1_4,
    Surround7_1_4,
}

#[derive(Clone, Copy, Debug)]
pub struct SpeakerLayout {
    speakers: &'static [Speaker],
    normalization: f32,
}

impl SpeakerLayout {
    pub const fn speakers(self) -> &'static [Speaker] {
        self.speakers
    }

    pub const fn channels(self) -> usize {
        self.speakers.len()
    }

    pub const fn normalization(self) -> f32 {
        self.normalization
    }

    pub const fn for_layout(layout: ChannelLayout) -> Self {
        match layout {
            ChannelLayout::Stereo => Self { speakers: &STEREO, normalization: 0.92 },
            ChannelLayout::Surround5_1 => Self { speakers: &SURROUND_5_1, normalization: 0.62 },
            ChannelLayout::Surround7_1 => Self { speakers: &SURROUND_7_1, normalization: 0.54 },
            ChannelLayout::Surround5_1_4 => Self { speakers: &SURROUND_5_1_4, normalization: 0.46 },
            ChannelLayout::Surround7_1_4 => Self { speakers: &SURROUND_7_1_4, normalization: 0.42 },
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

const STEREO: [Speaker; 2] = [
    Speaker::full_range(FRONT_LEFT, 1.0),
    Speaker::full_range(FRONT_RIGHT, 1.0),
];
const SURROUND_5_1: [Speaker; 6] = [
    Speaker::full_range(FRONT_LEFT, 1.0), Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90), Speaker::lfe(0.34),
    Speaker::full_range(REAR_125_LEFT, 0.78), Speaker::full_range(REAR_125_RIGHT, 0.78),
];
const SURROUND_7_1: [Speaker; 8] = [
    Speaker::full_range(FRONT_LEFT, 1.0), Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90), Speaker::lfe(0.34),
    Speaker::full_range(REAR_145_LEFT, 0.72), Speaker::full_range(REAR_145_RIGHT, 0.72),
    Speaker::full_range(SIDE_LEFT, 0.78), Speaker::full_range(SIDE_RIGHT, 0.78),
];
const SURROUND_5_1_4: [Speaker; 10] = [
    Speaker::full_range(FRONT_LEFT, 1.0), Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90), Speaker::lfe(0.34),
    Speaker::full_range(REAR_125_LEFT, 0.76), Speaker::full_range(REAR_125_RIGHT, 0.76),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64), Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
    Speaker::full_range(TOP_REAR_LEFT, 0.58), Speaker::full_range(TOP_REAR_RIGHT, 0.58),
];
const SURROUND_7_1_4: [Speaker; 12] = [
    Speaker::full_range(FRONT_LEFT, 1.0), Speaker::full_range(FRONT_RIGHT, 1.0),
    Speaker::full_range(CENTER, 0.90), Speaker::lfe(0.34),
    Speaker::full_range(REAR_145_LEFT, 0.72), Speaker::full_range(REAR_145_RIGHT, 0.72),
    Speaker::full_range(SIDE_LEFT, 0.78), Speaker::full_range(SIDE_RIGHT, 0.78),
    Speaker::full_range(TOP_FRONT_LEFT, 0.64), Speaker::full_range(TOP_FRONT_RIGHT, 0.64),
    Speaker::full_range(TOP_REAR_LEFT, 0.58), Speaker::full_range(TOP_REAR_RIGHT, 0.58),
];

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_audio_vivid_layout_exposes_twelve_sources() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        assert_eq!(layout.channels(), 12);
        assert_eq!(layout.speakers()[3].kind, SourceKind::Lfe);
    }
}
