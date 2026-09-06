use crate::{
    audio::{EqPreset, SpatialPreset, clamp_eq, clamp_spatial},
    model::{EqSettings, SpatialMotionMode, SpatialSettings, Track},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmartAudioProfileKind {
    AlreadySpatial,
    Classical,
    Pop,
    Rock,
    Electronic,
    HipHop,
    VocalAcoustic,
    JazzSoul,
    AmbientSoundtrack,
    Live,
    Balanced,
}

impl SmartAudioProfileKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AlreadySpatial => "已空间化音源",
            Self::Classical => "古典 / 交响",
            Self::Pop => "流行",
            Self::Rock => "摇滚 / 金属",
            Self::Electronic => "电子 / 舞曲",
            Self::HipHop => "Hip-Hop / R&B",
            Self::VocalAcoustic => "人声 / 原声",
            Self::JazzSoul => "Jazz / Soul / Blues",
            Self::AmbientSoundtrack => "氛围 / 原声带",
            Self::Live => "Live / 演唱会",
            Self::Balanced => "均衡",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SmartAudioDecision {
    pub profile: SmartAudioProfileKind,
    pub confidence: f32,
    pub eq: EqSettings,
    pub spatial: SpatialSettings,
}

pub fn resolve_smart_audio(
    track: &Track,
    baseline_eq: &EqSettings,
    baseline_spatial: &SpatialSettings,
    intensity: f32,
) -> SmartAudioDecision {
    let (profile, confidence) = classify(track);
    let (target_eq, target_spatial) = target_settings(profile);
    let amount = (intensity.clamp(0.0, 1.0) * confidence.clamp(0.0, 1.0)).clamp(0.0, 1.0);

    SmartAudioDecision {
        profile,
        confidence,
        eq: blend_eq(baseline_eq, &target_eq, amount),
        spatial: blend_spatial(baseline_spatial, &target_spatial, amount),
    }
}

pub fn classify(track: &Track) -> (SmartAudioProfileKind, f32) {
    let genre = normalize(track.genre.as_deref().unwrap_or_default());
    let title = normalize(&track.title);
    let album = normalize(&track.album);
    let artist = normalize(&track.artist);
    let all = format!("{genre} {title} {album} {artist}");

    if has_any(
        &all,
        &[
            "8d", "8 d", "360 audio", "360°", "360声", "binaural", "双耳", "spatial audio",
            "空间音频", "dolby atmos", "atmos", "immersive audio",
        ],
    ) {
        return (SmartAudioProfileKind::AlreadySpatial, 1.0);
    }

    if has_any(
        &genre,
        &[
            "classical", "orchestral", "symphony", "baroque", "chamber", "opera", "古典", "交响",
            "室内乐", "歌剧",
        ],
    ) {
        return (SmartAudioProfileKind::Classical, 0.98);
    }
    if has_any(
        &genre,
        &[
            "metal", "rock", "punk", "grunge", "hardcore", "摇滚", "金属", "朋克",
        ],
    ) {
        return (SmartAudioProfileKind::Rock, 0.96);
    }
    if has_any(
        &genre,
        &[
            "electronic", "electronica", "edm", "dance", "house", "techno", "trance", "dubstep",
            "dnb", "drum and bass", "电子", "电音", "舞曲",
        ],
    ) {
        return (SmartAudioProfileKind::Electronic, 0.97);
    }
    if has_any(
        &genre,
        &[
            "hip hop", "hip-hop", "rap", "trap", "r&b", "rnb", "说唱", "嘻哈",
        ],
    ) {
        return (SmartAudioProfileKind::HipHop, 0.96);
    }
    if has_any(
        &genre,
        &[
            "jazz", "blues", "soul", "funk", "爵士", "蓝调", "灵魂", "放克",
        ],
    ) {
        return (SmartAudioProfileKind::JazzSoul, 0.94);
    }
    if has_any(
        &genre,
        &[
            "acoustic", "folk", "singer songwriter", "vocal", "unplugged", "民谣", "原声", "人声",
            "不插电",
        ],
    ) {
        return (SmartAudioProfileKind::VocalAcoustic, 0.94);
    }
    if has_any(
        &genre,
        &[
            "ambient", "new age", "soundtrack", "score", "ost", "cinematic", "氛围", "新世纪",
            "原声带", "影视原声",
        ],
    ) {
        return (SmartAudioProfileKind::AmbientSoundtrack, 0.95);
    }
    if has_any(
        &genre,
        &["pop", "city pop", "k-pop", "j-pop", "c-pop", "流行"],
    ) {
        return (SmartAudioProfileKind::Pop, 0.93);
    }

    if has_any(
        &all,
        &[" live ", "live at", "concert", "演唱会", "现场版", "现场录音"],
    ) || title.ends_with("live")
    {
        return (SmartAudioProfileKind::Live, 0.88);
    }
    if has_any(
        &all,
        &["acoustic", "unplugged", "piano version", "钢琴版", "清唱", "人声版"],
    ) {
        return (SmartAudioProfileKind::VocalAcoustic, 0.82);
    }

    (SmartAudioProfileKind::Balanced, 0.52)
}

fn target_settings(profile: SmartAudioProfileKind) -> (EqSettings, SpatialSettings) {
    match profile {
        SmartAudioProfileKind::AlreadySpatial => {
            let mut spatial = SpatialSettings::default();
            spatial.enabled = false;
            (EqPreset::Flat.settings(), spatial)
        }
        SmartAudioProfileKind::Classical => {
            let mut spatial = SpatialPreset::Studio.settings();
            spatial.width = 0.62;
            spatial.depth = 0.30;
            (EqPreset::Classical.settings(), spatial)
        }
        SmartAudioProfileKind::Pop => (EqPreset::Pop.settings(), SpatialPreset::Wide.settings()),
        SmartAudioProfileKind::Rock => (EqPreset::Rock.settings(), SpatialPreset::Wide.settings()),
        SmartAudioProfileKind::Electronic => {
            let eq = EqSettings {
                enabled: true,
                preamp_db: -1.5,
                bands_db: [3.5, 3.0, 1.5, -0.5, -1.0, 0.0, 1.5, 2.5, 3.0, 2.5],
            };
            let mut spatial = SpatialPreset::Immersive3d.settings();
            spatial.motion_mode = SpatialMotionMode::Static;
            spatial.motion_intensity = 0.0;
            spatial.mix = 0.66;
            (eq, spatial)
        }
        SmartAudioProfileKind::HipHop => {
            let eq = EqSettings {
                enabled: true,
                preamp_db: -2.0,
                bands_db: [4.0, 3.5, 2.0, 0.5, -0.5, 0.0, 1.0, 1.5, 1.5, 1.0],
            };
            (eq, SpatialPreset::Wide.settings())
        }
        SmartAudioProfileKind::VocalAcoustic => {
            (EqPreset::Vocal.settings(), SpatialPreset::Headphones.settings())
        }
        SmartAudioProfileKind::JazzSoul => {
            let eq = EqSettings {
                enabled: true,
                preamp_db: -1.0,
                bands_db: [1.0, 1.5, 1.0, 0.5, 1.0, 1.5, 1.0, 0.5, 0.0, -0.5],
            };
            (eq, SpatialPreset::Studio.settings())
        }
        SmartAudioProfileKind::AmbientSoundtrack => {
            let mut spatial = SpatialPreset::Cinema.settings();
            spatial.mix = 0.70;
            (EqPreset::Classical.settings(), spatial)
        }
        SmartAudioProfileKind::Live => {
            let mut spatial = SpatialPreset::Cinema.settings();
            spatial.depth = 0.70;
            spatial.room_size = 0.72;
            (EqPreset::Rock.settings(), spatial)
        }
        SmartAudioProfileKind::Balanced => {
            let mut spatial = SpatialPreset::Studio.settings();
            spatial.enabled = true;
            spatial.mix = 0.28;
            spatial.width = 0.56;
            (EqPreset::Flat.settings(), spatial)
        }
    }
}

fn blend_eq(baseline: &EqSettings, target: &EqSettings, amount: f32) -> EqSettings {
    if amount <= 0.001 {
        return baseline.clone();
    }
    let mut result = baseline.clone();
    result.enabled = baseline.enabled || target.enabled;
    result.preamp_db = lerp(baseline.preamp_db, target.preamp_db, amount);
    for index in 0..result.bands_db.len() {
        result.bands_db[index] = lerp(baseline.bands_db[index], target.bands_db[index], amount);
    }
    clamp_eq(result)
}

fn blend_spatial(
    baseline: &SpatialSettings,
    target: &SpatialSettings,
    amount: f32,
) -> SpatialSettings {
    if amount <= 0.001 {
        return baseline.clone();
    }
    if target.motion_mode != SpatialMotionMode::Static {
        // Automatic genre matching intentionally never enables moving-source effects.
        return baseline.clone();
    }
    let mut result = baseline.clone();
    result.enabled = if amount >= 0.35 {
        target.enabled
    } else {
        baseline.enabled
    };
    result.width = lerp(baseline.width, target.width, amount);
    result.depth = lerp(baseline.depth, target.depth, amount);
    result.distance = lerp(baseline.distance, target.distance, amount);
    result.mix = lerp(baseline.mix, target.mix, amount);
    result.crossfeed = lerp(baseline.crossfeed, target.crossfeed, amount);
    result.room_size = lerp(baseline.room_size, target.room_size, amount);
    result.immersive_3d = lerp(baseline.immersive_3d, target.immersive_3d, amount);
    result.motion_mode = SpatialMotionMode::Static;
    result.motion_intensity = 0.0;
    clamp_spatial(result)
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount.clamp(0.0, 1.0)
}

fn normalize(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .replace(['(', ')', '[', ']', '{', '}', '_', '-', '/', '\\', '·', '，', '、'], " ")
}

fn has_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn track(genre: Option<&str>, title: &str) -> Track {
        Track {
            id: 1,
            path: PathBuf::from("song.flac"),
            title: title.into(),
            artist: "artist".into(),
            album: "album".into(),
            year: None,
            genre: genre.map(str::to_owned),
            duration_ms: 180_000,
            codec: "flac".into(),
            sample_rate: 48_000,
            channels: 2,
            artwork_key: None,
        }
    }

    #[test]
    fn genre_selects_expected_profile() {
        assert_eq!(classify(&track(Some("Classical"), "x")).0, SmartAudioProfileKind::Classical);
        assert_eq!(classify(&track(Some("EDM"), "x")).0, SmartAudioProfileKind::Electronic);
        assert_eq!(classify(&track(Some("Rock"), "x")).0, SmartAudioProfileKind::Rock);
    }

    #[test]
    fn already_spatial_source_is_not_spatialized_again() {
        let source = track(Some("Electronic"), "Example 8D Audio");
        let decision = resolve_smart_audio(
            &source,
            &EqSettings::default(),
            &SpatialSettings::default(),
            1.0,
        );
        assert_eq!(decision.profile, SmartAudioProfileKind::AlreadySpatial);
        assert!(!decision.spatial.enabled);
    }
}
