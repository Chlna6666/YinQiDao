use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};

const PROBE_PREFIX_BYTES: u64 = 1024 * 1024;
const PROBE_TAIL_BYTES: u64 = 8 * 1024 * 1024;
const AUDIO_SAMPLE_ENTRY_FIXED_BYTES_AFTER_TYPE: usize = 32;
const AUDIO_SAMPLE_ENTRY_MIN_BOX_BYTES: usize = 8 + AUDIO_SAMPLE_ENTRY_FIXED_BYTES_AFTER_TYPE;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Av3aSampleEntry {
    pub sample_rate: u32,
    pub channels: u16,
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
/// box size, AudioSampleEntry reserved/data-reference fields, fixed header and complete box extent
/// are available in this probe region. This prevents embedded JPEG/PNG data from selecting the AVS
/// decoder accidentally.
pub fn probe_av3a_bytes(bytes: &[u8]) -> Option<Av3aSampleEntry> {
    for (type_pos, fourcc) in bytes.windows(4).enumerate() {
        if fourcc != b"av3a" {
            continue;
        }

        let box_end = valid_audio_sample_entry_end(bytes, type_pos)?;
        if type_pos + AUDIO_SAMPLE_ENTRY_FIXED_BYTES_AFTER_TYPE > box_end {
            continue;
        }

        let channels = u16::from_be_bytes([bytes[type_pos + 20], bytes[type_pos + 21]]);
        let sample_size_bits = u16::from_be_bytes([bytes[type_pos + 22], bytes[type_pos + 23]]);
        let sample_rate_fixed = u32::from_be_bytes([
            bytes[type_pos + 28],
            bytes[type_pos + 29],
            bytes[type_pos + 30],
            bytes[type_pos + 31],
        ]);
        let sample_rate = sample_rate_fixed >> 16;

        if !(1..=32).contains(&channels) || !(8_000..=384_000).contains(&sample_rate) {
            continue;
        }

        let decoder_config = child_box_payload(bytes, type_pos, box_end, b"dca3").unwrap_or_default();
        return Some(Av3aSampleEntry {
            sample_rate,
            channels,
            sample_size_bits: (sample_size_bits != 0).then_some(sample_size_bits),
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
    let mut cursor = type_pos.checked_add(AUDIO_SAMPLE_ENTRY_FIXED_BYTES_AFTER_TYPE)?;
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
    fn ignores_av3a_signature_inside_unrelated_payload() {
        let mut bytes = vec![0xAA_u8; 96];
        bytes[40..44].copy_from_slice(b"av3a");
        assert_eq!(probe_av3a_bytes(&bytes), None);
    }

    #[test]
    fn rejects_sample_entry_split_at_probe_boundary() {
        let mut bytes = vec![0_u8; 48];
        bytes[0..4].copy_from_slice(&(64_u32).to_be_bytes());
        bytes[4..8].copy_from_slice(b"av3a");
        bytes[14..16].copy_from_slice(&1_u16.to_be_bytes());
        bytes[24..26].copy_from_slice(&2_u16.to_be_bytes());
        bytes[32..36].copy_from_slice(&(48_000_u32 << 16).to_be_bytes());
        assert_eq!(probe_av3a_bytes(&bytes), None);
    }
}
