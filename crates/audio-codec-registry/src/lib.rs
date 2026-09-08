//! Capability registry for codecs that are not handled by Symphonia's primary decode path.
//!
//! This crate deliberately separates *routing metadata* from decoder implementations. A codec is
//! never reported as supported merely because it appears in this registry; `CodecMaturity` makes
//! the implementation state explicit while individual codec crates are completed and validated.

use yinqidao_codec_core::CodecId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CodecMaturity {
    /// Routing/probing is defined but the decoder is not ready for user playback.
    InDevelopment,
    /// Decoder is usable behind an opt-in/experimental path and has format-level tests.
    Experimental,
    /// Decoder has conformance/regression coverage suitable for the normal playback path.
    Production,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecDescriptor {
    pub id: CodecId,
    pub name: &'static str,
    pub extensions: &'static [&'static str],
    pub iso_bmff_sample_entries: &'static [[u8; 4]],
    pub maturity: CodecMaturity,
    pub pure_rust: bool,
}

const AV3A_SAMPLE_ENTRIES: &[[u8; 4]] = &[*b"av3a"];
const NO_SAMPLE_ENTRIES: &[[u8; 4]] = &[];

/// Non-Symphonia codec roadmap in routing priority order.
///
/// AVS3 already has its own crate and container parser. The remaining entries are intentionally
/// marked `InDevelopment` until a real decoder crate exists and passes codec vectors. Keeping them
/// here lets file routing/UI diagnostics share one canonical capability table without pretending a
/// planned decoder is already available.
pub const EXTRA_CODECS: &[CodecDescriptor] = &[
    CodecDescriptor {
        id: CodecId::Avs3,
        name: "AVS3-P3 / Audio Vivid",
        extensions: &["m4a", "mp4", "av3a"],
        iso_bmff_sample_entries: AV3A_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Ape,
        name: "Monkey's Audio",
        extensions: &["ape"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::WavPack,
        name: "WavPack",
        extensions: &["wv"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Opus,
        name: "Opus",
        extensions: &["opus", "ogg", "oga", "webm"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Musepack,
        name: "Musepack",
        extensions: &["mpc", "mpp", "mp+"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Ac3,
        name: "Dolby Digital / AC-3",
        extensions: &["ac3"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Eac3,
        name: "Dolby Digital Plus / E-AC-3",
        extensions: &["eac3", "ec3"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
    CodecDescriptor {
        id: CodecId::Dts,
        name: "DTS Core",
        extensions: &["dts"],
        iso_bmff_sample_entries: NO_SAMPLE_ENTRIES,
        maturity: CodecMaturity::InDevelopment,
        pure_rust: true,
    },
];

pub fn descriptor(id: CodecId) -> Option<&'static CodecDescriptor> {
    EXTRA_CODECS.iter().find(|codec| codec.id == id)
}

pub fn by_extension(extension: &str) -> Option<&'static CodecDescriptor> {
    let extension = extension.trim_start_matches('.');
    EXTRA_CODECS.iter().find(|codec| {
        codec
            .extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(extension))
    })
}

pub fn by_iso_bmff_sample_entry(fourcc: [u8; 4]) -> Option<&'static CodecDescriptor> {
    EXTRA_CODECS
        .iter()
        .find(|codec| codec.iso_bmff_sample_entries.contains(&fourcc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_av3a_without_calling_generic_m4a_aac() {
        let codec = by_iso_bmff_sample_entry(*b"av3a").expect("AV3A descriptor");
        assert_eq!(codec.id, CodecId::Avs3);
        assert_eq!(codec.maturity, CodecMaturity::InDevelopment);
    }

    #[test]
    fn extension_lookup_is_case_insensitive() {
        assert_eq!(
            by_extension(".APE").map(|codec| codec.id),
            Some(CodecId::Ape)
        );
        assert_eq!(
            by_extension("WV").map(|codec| codec.id),
            Some(CodecId::WavPack)
        );
    }
}
