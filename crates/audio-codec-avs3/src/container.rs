use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

use crate::config::parse_dca3;

const PROBE_PREFIX_BYTES: u64 = 1024 * 1024;
const PROBE_TAIL_BYTES: u64 = 8 * 1024 * 1024;
/// Offset from the first byte of the sample-entry type (`av3a`) to the first child box.
/// ISO AudioSampleEntry occupies 36 bytes including size+type, therefore children begin 32 bytes
/// after the type. The legacy ChannelCount/SampleSize/SampleRate fields inside this region are not
/// AV3A decoder configuration and must not be used as validity gates.
const AUDIO_SAMPLE_ENTRY_CHILD_OFFSET_FROM_TYPE: usize = 32;
const AUDIO_SAMPLE_ENTRY_MIN_BOX_BYTES: usize = 36;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Av3aSampleEntry {
    /// Effective sample rate. For conforming AV3A this is resolved from `dca3`; the legacy
    /// AudioSampleEntry value is retained only as a compatibility fallback when `dca3` cannot yet
    /// be interpreted by this decoder.
    pub sample_rate: u32,
    /// Effective signal/channel count resolved from `dca3` whenever possible. AudioSampleEntry
    /// ChannelCount is only a compatibility fallback and is never an AV3A validity condition.
    pub channels: u16,
    /// Effective decoded precision resolved from `dca3` whenever possible.
    pub sample_size_bits: Option<u16>,
    /// Raw payload of the optional `dca3` decoder-configuration box.
    ///
    /// Keeping this lossless is important while the AVS3 syntax decoder is implemented: parsing
    /// policy can evolve without rescanning/reopening the MP4 container.
    pub decoder_config: Vec<u8>,
}

pub fn probe_av3a_path(path: &Path) -> io::Result<Option<Av3aSampleEntry>> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();

    let prefix_len = length.min(PROBE_PREFIX_BYTES) as usize;
    let mut prefix = vec![0_u8; prefix_len];
    file.read_exact(&mut prefix)?;
    if let Some(entry) = probe_av3a_bytes(&prefix) {
        return Ok(Some(entry));
    }

    if length > PROBE_PREFIX_BYTES {
        let tail_len = length.min(PROBE_TAIL_BYTES) as usize;
        file.seek(SeekFrom::End(-(tail_len as i64)))?;
        let mut tail = vec![0_u8; tail_len];
        file.read_exact(&mut tail)?;
        if let Some(entry) = probe_av3a_bytes(&tail) {
            return Ok(Some(entry));
        }
    }

    Ok(None)
}

/// Probe an in-memory ISO-BMFF region for a structurally valid `av3a` AudioSampleEntry.
///
/// The four bytes `av3a` are not sufficient evidence by themselves: cover artwork and arbitrary
/// metadata can contain the same byte sequence. A candidate is accepted only when the enclosing
/// box size, SampleEntry reserved/data-reference fields and complete box extent are available.
///
/// GY/T 420 requires AV3A decoders to ignore AudioSampleEntry ChannelCount, SampleSize and
/// SampleRate because CA3SpecificBox (`dca3`) supersedes them. Consequently none of those legacy
/// fields is used as a structural validity filter here. When `dca3` is understood, its values also
/// replace the legacy fields in the returned metadata.
pub fn probe_av3a_bytes(bytes: &[u8]) -> Option<Av3aSampleEntry> {
    for (type_pos, fourcc) in bytes.windows(4).enumerate() {
        if fourcc != b"av3a" {
            continue;
        }

        let Some(box_end) = valid_audio_sample_entry_end(bytes, type_pos) else {
            continue;
        };
        if type_pos + AUDIO_SAMPLE_ENTRY_CHILD_OFFSET_FROM_TYPE > box_end {
            continue;
        }

        // These three fields are explicitly ignored for AV3A decoding. Read them only so legacy or
        // non-conforming files without a usable dca3 can still carry diagnostic/fallback metadata.
        let legacy_channels = u16::from_be_bytes([bytes[type_pos + 20], bytes[type_pos + 21]]);
        let legacy_sample_size_bits =
            u16::from_be_bytes([bytes[type_pos + 22], bytes[type_pos + 23]]);
        let sample_rate_fixed = u32::from_be_bytes([
            bytes[type_pos + 28],
            bytes[type_pos + 29],
            bytes[type_pos + 30],
            bytes[type_pos + 31],
        ]);
        let legacy_sample_rate = sample_rate_fixed >> 16;

        let decoder_config = child_box_payload(bytes, type_pos, box_end, b"dca3")
            .unwrap_or_default();
        let parsed = (!decoder_config.is_empty())
            .then(|| parse_dca3(&decoder_config).ok())
            .flatten();

        let sample_rate = parsed
            .as_ref()
            .and_then(|config| config.sample_rate())
            .unwrap_or(legacy_sample_rate);
        let channels = parsed
            .as_ref()
            .and_then(|config| config.channels())
            .unwrap_or(legacy_channels);
        let sample_size_bits = parsed
            .as_ref()
            .and_then(|config| config.bits_per_sample())
            .map(u16::from)
            .or((legacy_sample_size_bits != 0).then_some(legacy_sample_size_bits));

        return Some(Av3aSampleEntry {
            sample_rate,
            channels,
            sample_size_bits,
            decoder_config,
        });
    }

    None
}

fn valid_audio_sample_entry_end(bytes: &[u8], type_pos: usize) -> Option<usize> {
    let box_start = type_pos.checked_sub(4)?;
    let declared_size =
        u32::from_be_bytes(bytes.get(box_start..type_pos)?.try_into().ok()?) as usize;
    if declared_size < AUDIO_SAMPLE_ENTRY_MIN_BOX_BYTES {
        return None;
    }
    let box_end = box_start.checked_add(declared_size)?;
    if box_end > bytes.len() {
        return None;
    }

    // ISO-BMFF SampleEntry: six reserved zero bytes followed by a non-zero data-reference index.
    if bytes.get(type_pos + 4..type_pos + 10)? != [0_u8; 6] {
        return None;
    }
    let data_reference_index =
        u16::from_be_bytes(bytes.get(type_pos + 10..type_pos + 12)?.try_into().ok()?);
    (data_reference_index != 0).then_some(box_end)
}

fn child_box_payload(
    bytes: &[u8],
    type_pos: usize,
    box_end: usize,
    wanted: &[u8; 4],
) -> Option<Vec<u8>> {
    let mut cursor = type_pos.checked_add(AUDIO_SAMPLE_ENTRY_CHILD_OFFSET_FROM_TYPE)?;
    while cursor.checked_add(8)? <= box_end {
        let size = u32::from_be_bytes(bytes.get(cursor..cursor + 4)?.try_into().ok()?) as usize;
        let child_type: &[u8; 4] = bytes.get(cursor + 4..cursor + 8)?.try_into().ok()?;
        if size < 8 {
            break;
        }
        let next = cursor.checked_add(size)?;
        if next > box_end {
            break;
        }
        if child_type == wanted {
            return Some(bytes[cursor + 8..next].to_vec());
        }
        cursor = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_7_1_4_sample_entry_metadata() {
        let mut bytes = vec![0_u8; 64];
        // Sample entry occupies bytes 4..64; type begins at byte 8.
        bytes[4..8].copy_from_slice(&(60_u32).to_be_bytes());
        bytes[8..12].copy_from_slice(b"av3a");
        bytes[18..20].copy_from_slice(&1_u16.to_be_bytes());
        bytes[28..30].copy_from_slice(&12_u16.to_be_bytes());
        bytes[30..32].copy_from_slice(&24_u16.to_be_bytes());
        bytes[36..40].copy_from_slice(&(44_100_u32 << 16).to_be_bytes());

        let entry = probe_av3a_bytes(&bytes).expect("av3a");
        assert_eq!(entry.sample_rate, 44_100);
        assert_eq!(entry.channels, 12);
        assert_eq!(entry.sample_size_bits, Some(24));
    }

    #[test]
    fn preserves_dca3_payload() {
        let mut bytes = vec![0_u8; 56];
        // Full box starts at 0, therefore the type is at byte 4.
        bytes[0..4].copy_from_slice(&(56_u32).to_be_bytes());
        bytes[4..8].copy_from_slice(b"av3a");
        bytes[14..16].copy_from_slice(&1_u16.to_be_bytes());
        bytes[24..26].copy_from_slice(&2_u16.to_be_bytes());
        bytes[26..28].copy_from_slice(&24_u16.to_be_bytes());
        bytes[32..36].copy_from_slice(&(48_000_u32 << 16).to_be_bytes());
        bytes[36..40].copy_from_slice(&(12_u32).to_be_bytes());
        bytes[40..44].copy_from_slice(b"dca3");
        bytes[44..48].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);

        let entry = probe_av3a_bytes(&bytes).expect("av3a");
        assert_eq!(entry.decoder_config, vec![0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn dca3_supersedes_zeroed_legacy_audio_sample_entry_fields() {
        // Channel-based GA: codec=2, 48 kHz, baseline NN, 5.1.4(index 8), 704 kb/s, 24-bit.
        let dca3 = [0x22, 0x00, 0x10, 0x02, 0xC0, 0x80];
        let box_size = 36 + 8 + dca3.len();
        let mut bytes = vec![0_u8; box_size];
        bytes[0..4].copy_from_slice(&(box_size as u32).to_be_bytes());
        bytes[4..8].copy_from_slice(b"av3a");
        bytes[14..16].copy_from_slice(&1_u16.to_be_bytes());
        // ChannelCount/SampleSize/SampleRate deliberately remain zero. They are ignored for AV3A.
        bytes[36..40].copy_from_slice(&((8 + dca3.len()) as u32).to_be_bytes());
        bytes[40..44].copy_from_slice(b"dca3");
        bytes[44..].copy_from_slice(&dca3);

        let entry = probe_av3a_bytes(&bytes).expect("av3a with dca3");
        assert_eq!(entry.sample_rate, 48_000);
        assert_eq!(entry.channels, 10);
        assert_eq!(entry.sample_size_bits, Some(24));
        assert_eq!(entry.decoder_config, dca3);
    }

    #[test]
    fn legacy_audio_sample_entry_values_are_not_validity_gates() {
        let mut bytes = vec![0_u8; 36];
        bytes[0..4].copy_from_slice(&(36_u32).to_be_bytes());
        bytes[4..8].copy_from_slice(b"av3a");
        bytes[14..16].copy_from_slice(&1_u16.to_be_bytes());
        // All legacy audio geometry remains zero. The sample entry is still structurally AV3A;
        // decoder capability is decided later from dca3/AATF rather than these ignored fields.
        let entry = probe_av3a_bytes(&bytes).expect("structural av3a");
        assert_eq!((entry.sample_rate, entry.channels), (0, 0));
    }

    #[test]
    fn ignores_av3a_signature_inside_unrelated_payload() {
        let mut bytes = vec![0xAA_u8; 96];
        bytes[40..44].copy_from_slice(b"av3a");
        assert_eq!(probe_av3a_bytes(&bytes), None);
    }

    #[test]
    fn skips_false_signature_before_real_sample_entry() {
        let mut bytes = vec![0xAA_u8; 128];
        bytes[8..12].copy_from_slice(b"av3a");

        let start = 64;
        bytes[start..start + 4].copy_from_slice(&(64_u32).to_be_bytes());
        bytes[start + 4..start + 8].copy_from_slice(b"av3a");
        bytes[start + 14..start + 16].copy_from_slice(&1_u16.to_be_bytes());
        bytes[start + 24..start + 26].copy_from_slice(&2_u16.to_be_bytes());
        bytes[start + 26..start + 28].copy_from_slice(&24_u16.to_be_bytes());
        bytes[start + 32..start + 36].copy_from_slice(&(48_000_u32 << 16).to_be_bytes());

        let entry = probe_av3a_bytes(&bytes).expect("real av3a after false signature");
        assert_eq!(entry.channels, 2);
        assert_eq!(entry.sample_rate, 48_000);
    }

    #[test]
    fn rejects_sample_entry_split_at_probe_boundary() {
        let mut bytes = vec![0_u8; 32];
        bytes[0..4].copy_from_slice(&(64_u32).to_be_bytes());
        bytes[4..8].copy_from_slice(b"av3a");
        bytes[14..16].copy_from_slice(&1_u16.to_be_bytes());
        assert_eq!(probe_av3a_bytes(&bytes), None);
    }
}
