use yinqidao_codec_core::CodecError;

use crate::{bitreader::BitReader, config::AudioCodingMethod, frame::AatfFrameHeader};

/// Width of `frame_error_check().crc_check` in the AATF syntax.
pub const LOSSLESS_FRAME_ERROR_CHECK_BITS: u8 = 8;

/// Byte-level envelope of one complete lossless AATF frame.
///
/// The raw lossless block and optional ancillary block deliberately remain opaque here. Their
/// boundary is defined by the Chapter 8 / amendment syntax, while AATF independently fixes the
/// complete frame length and the trailing 8-bit `frame_error_check()` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LosslessAatfEnvelope<'a> {
    /// Exactly the number of bytes declared by `raw_frame_length`.
    pub frame_bytes: &'a [u8],
    /// Bytes after the byte-aligned AATF header and before the trailing frame check.
    ///
    /// This contains `ll_raw_data_block()` and, when `anc_data_index` is set, the following
    /// `anc_data_block()`. It must not be split without the normative inner syntax.
    pub raw_block_and_ancillary: &'a [u8],
    /// Raw 8-bit `crc_check` value carried by `frame_error_check()`.
    pub frame_crc: u8,
    /// Absolute bit position of `frame_error_check().crc_check` within the AATF frame.
    pub frame_crc_bit_offset: usize,
}

/// Decode the fixed-width `frame_error_check().crc_check` syntax element at an arbitrary bit
/// position.
///
/// This only extracts the transmitted check byte. CRC polynomial/initialization/residue handling
/// belongs to the normative CRC verifier and is intentionally not inferred here.
pub fn decode_lossless_frame_error_check_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<(u8, usize), CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let crc = reader.read_bits(LOSSLESS_FRAME_ERROR_CHECK_BITS)? as u8;
    Ok((crc, reader.position_bits()))
}

/// Resolve the lossless AATF payload envelope without interpreting `ll_raw_data_block()`.
///
/// `raw_frame_length` is the declared total byte length of the current AATF frame. Lossless AATF
/// places `frame_error_check()` after the lossless raw block and optional ancillary block, so the
/// final byte can be separated exactly even while the Chapter 8 parser is still evolving.
pub fn parse_lossless_aatf_envelope<'a>(
    packet: &'a [u8],
    header: &AatfFrameHeader,
) -> Result<LosslessAatfEnvelope<'a>, CodecError> {
    if header.coding_method != AudioCodingMethod::Lossless {
        return Err(CodecError::InvalidData(
            "lossless AATF envelope requested for non-lossless frame",
        ));
    }

    let declared_len = usize::from(header.raw_frame_length.ok_or(CodecError::InvalidData(
        "lossless AATF frame is missing raw_frame_length",
    ))?);
    if packet.len() < declared_len {
        return Err(CodecError::Truncated);
    }
    if packet.len() != declared_len {
        return Err(CodecError::InvalidData(
            "AATF packet length does not match raw_frame_length",
        ));
    }
    if declared_len <= header.payload_offset_bytes {
        return Err(CodecError::Truncated);
    }

    let frame_crc_byte_offset = declared_len - 1;
    if frame_crc_byte_offset < header.payload_offset_bytes {
        return Err(CodecError::Truncated);
    }
    let frame_crc_bit_offset = frame_crc_byte_offset
        .checked_mul(8)
        .ok_or(CodecError::InvalidData("AATF frame CRC offset overflows"))?;
    let (frame_crc, next_bit_offset) =
        decode_lossless_frame_error_check_at(packet, frame_crc_bit_offset)?;
    if next_bit_offset != declared_len.saturating_mul(8) {
        return Err(CodecError::InvalidData(
            "lossless frame_error_check is not the final AATF syntax element",
        ));
    }

    Ok(LosslessAatfEnvelope {
        frame_bytes: &packet[..declared_len],
        raw_block_and_ancillary: &packet[header.payload_offset_bytes..frame_crc_byte_offset],
        frame_crc,
        frame_crc_bit_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::parse_aatf_frame_header;

    struct BitWriter {
        bytes: Vec<u8>,
        bit_pos: usize,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit_pos: 0,
            }
        }

        fn push(&mut self, value: u32, bits: usize) {
            for shift in (0..bits).rev() {
                if self.bit_pos & 7 == 0 {
                    self.bytes.push(0);
                }
                if (value >> shift) & 1 != 0 {
                    let index = self.bytes.len() - 1;
                    self.bytes[index] |= 1 << (7 - (self.bit_pos & 7));
                }
                self.bit_pos += 1;
            }
        }
    }

    fn synthetic_lossless_frame() -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.push(0x0fff, 12); // syncword
        writer.push(1, 4); // audio_codec_id: lossless
        writer.push(0, 1); // anc_data_index
        writer.push(0, 3); // coding_profile
        writer.push(2, 4); // 48 kHz
        writer.push(9, 16); // total AATF frame length in bytes
        writer.push(0x5a, 8); // aatf_error_check()
        writer.push(2, 4); // channel_number
        writer.push(1, 2); // 16-bit resolution
        writer.push(0, 2); // byte_alignment()
        writer.push(0xde, 8); // opaque ll_raw_data_block()
        writer.push(0xa5, 8); // frame_error_check().crc_check
        writer.bytes
    }

    #[test]
    fn extracts_trailing_frame_check_without_parsing_lossless_raw_data() {
        let packet = synthetic_lossless_frame();
        let header = parse_aatf_frame_header(&packet).unwrap();
        let envelope = parse_lossless_aatf_envelope(&packet, &header).unwrap();

        assert_eq!(envelope.frame_bytes, packet.as_slice());
        assert_eq!(envelope.raw_block_and_ancillary, &[0xde]);
        assert_eq!(envelope.frame_crc, 0xa5);
        assert_eq!(envelope.frame_crc_bit_offset, 64);
    }

    #[test]
    fn rejects_packet_length_disagreement() {
        let mut packet = synthetic_lossless_frame();
        let header = parse_aatf_frame_header(&packet).unwrap();
        packet.push(0);
        assert!(parse_lossless_aatf_envelope(&packet, &header).is_err());
    }

    #[test]
    fn decodes_check_byte_at_unaligned_bit_offset() {
        let (crc, next) =
            decode_lossless_frame_error_check_at(&[0b1010_0101, 0b1100_0000], 3).unwrap();
        assert_eq!(crc, 0b0010_1110);
        assert_eq!(next, 11);
    }
}
