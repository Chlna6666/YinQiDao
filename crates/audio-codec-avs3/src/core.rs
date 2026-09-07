use yinqidao_codec_core::CodecError;

use crate::{
    bitreader::BitReader,
    tns::{TnsSideInfo, parse_tns_side_info_at},
};

/// The fixed 2-bit transform/window selector at the beginning of `DecodeCoreSideBits()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformType {
    Long,
    Short,
    CutIn,
    CutOut,
}

impl TransformType {
    pub const fn from_bits(value: u8) -> Self {
        match value & 0b11 {
            0 => Self::Long,
            1 => Self::Short,
            2 => Self::CutIn,
            _ => Self::CutOut,
        }
    }

    /// Nominal window length from the AVS3 core specification.
    pub const fn window_len(self) -> usize {
        match self {
            Self::Short => 256,
            Self::Long | Self::CutIn | Self::CutOut => 2048,
        }
    }
}

/// Frequency-domain noise-shaping VQ side information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FdShapingSideInfo {
    pub low_bitrate_precision: bool,
    /// Up to seven LSF split-VQ indices. Low-bitrate mode uses indices 0..5 only.
    pub lsf_vq_indices: [Option<u8>; 7],
    pub next_bit_offset: usize,
}

/// Legacy/diagnostic boundary parser retained for focused syntax tests. The normal decoder path now
/// uses the complete B.25..B.32 Huffman decoder from `tns.rs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TnsSideBoundary {
    Complete {
        next_bit_offset: usize,
    },
    HuffmanCodes {
        filter_index: u8,
        order: u8,
        code_bit_offset: usize,
    },
}

/// Deterministic prefix of one `DecodeCoreSideBits()` block through complete TNS side information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoreSidePrefix {
    pub transform_type: TransformType,
    pub fd_shaping: FdShapingSideInfo,
    pub tns: TnsSideInfo,
    /// First bit after TNS. BWE/group/QC parsing continues here.
    pub next_bit_offset: usize,
}

pub fn parse_core_transform_type(core_side_bits: &[u8]) -> Result<TransformType, CodecError> {
    parse_core_transform_type_at(core_side_bits, 0)
}

pub fn parse_core_transform_type_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<TransformType, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    Ok(TransformType::from_bits(reader.read_bits(2)? as u8))
}

/// Parse `DecodeFdShapingSideBits()` at a known bit boundary.
///
/// `lsfLbrFlag` is derived from average per-channel bitrate: <=32 kb/s uses five split-VQ indices,
/// otherwise the seven-index high-precision layout is used.
pub fn parse_fd_shaping_at(
    bytes: &[u8],
    bit_offset: usize,
    low_bitrate_precision: bool,
) -> Result<FdShapingSideInfo, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let widths: &[u8] = if low_bitrate_precision {
        &[8, 8, 7, 7, 6]
    } else {
        &[8, 8, 7, 7, 6, 5, 5]
    };
    let mut indices = [None; 7];
    for (slot, width) in indices.iter_mut().zip(widths.iter().copied()) {
        *slot = Some(reader.read_bits(width)? as u8);
    }

    Ok(FdShapingSideInfo {
        low_bitrate_precision,
        lsf_vq_indices: indices,
        next_bit_offset: reader.position_bits(),
    })
}

/// Parse only the fixed TNS prefix and stop before its variable Huffman coefficient words.
pub fn parse_tns_boundary_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<TnsSideBoundary, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    for filter_index in 0..2_u8 {
        let enabled = reader.read_bit()?;
        if enabled {
            let order = (reader.read_bits(3)? as u8).saturating_add(1);
            return Ok(TnsSideBoundary::HuffmanCodes {
                filter_index,
                order,
                code_bit_offset: reader.position_bits(),
            });
        }
    }
    Ok(TnsSideBoundary::Complete {
        next_bit_offset: reader.position_bits(),
    })
}

/// Parse `transformType`, FD shaping and complete two-filter TNS side information.
pub fn parse_core_side_prefix_at(
    bytes: &[u8],
    bit_offset: usize,
    low_bitrate_precision: bool,
) -> Result<CoreSidePrefix, CodecError> {
    let transform_type = parse_core_transform_type_at(bytes, bit_offset)?;
    let fd_shaping = parse_fd_shaping_at(
        bytes,
        bit_offset.saturating_add(2),
        low_bitrate_precision,
    )?;
    let tns = parse_tns_side_info_at(bytes, fd_shaping.next_bit_offset)?;

    Ok(CoreSidePrefix {
        transform_type,
        fd_shaping,
        next_bit_offset: tns.next_bit_offset,
        tns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_normative_transform_types() {
        assert_eq!(parse_core_transform_type(&[0b00_000000]).unwrap(), TransformType::Long);
        assert_eq!(parse_core_transform_type(&[0b01_000000]).unwrap(), TransformType::Short);
        assert_eq!(parse_core_transform_type(&[0b10_000000]).unwrap(), TransformType::CutIn);
        assert_eq!(parse_core_transform_type(&[0b11_000000]).unwrap(), TransformType::CutOut);
        assert_eq!(TransformType::Short.window_len(), 256);
        assert_eq!(TransformType::Long.window_len(), 2048);
    }

    #[test]
    fn reads_transform_after_two_metadata_flags_without_repacking() {
        assert_eq!(
            parse_core_transform_type_at(&[0b00_10_1111], 2).unwrap(),
            TransformType::CutIn
        );
    }

    #[test]
    fn parses_high_precision_fd_shaping_indices() {
        let bytes = [0x12, 0x34, 0xAA, 0x55, 0xF0, 0xCC, 0x80];
        let info = parse_fd_shaping_at(&bytes, 0, false).unwrap();
        assert!(!info.low_bitrate_precision);
        assert!(info.lsf_vq_indices.iter().all(Option::is_some));
        assert_eq!(info.next_bit_offset, 46);
    }

    #[test]
    fn parses_low_precision_fd_shaping_indices() {
        let bytes = [0x12, 0x34, 0xAA, 0x55, 0xF0];
        let info = parse_fd_shaping_at(&bytes, 0, true).unwrap();
        assert!(info.low_bitrate_precision);
        assert_eq!(info.next_bit_offset, 36);
        assert!(info.lsf_vq_indices[..5].iter().all(Option::is_some));
        assert_eq!(info.lsf_vq_indices[5], None);
        assert_eq!(info.lsf_vq_indices[6], None);
    }

    #[test]
    fn boundary_parser_still_reports_enabled_tns_without_consuming_huffman() {
        assert_eq!(
            parse_tns_boundary_at(&[0b1_011_1111], 0).unwrap(),
            TnsSideBoundary::HuffmanCodes {
                filter_index: 0,
                order: 4,
                code_bit_offset: 4,
            }
        );
    }

    #[test]
    fn core_prefix_chains_transform_fd_and_complete_disabled_tns() {
        // transform=00, high-precision FD shaping consumes 46 zero bits, both TNS filters disabled.
        let bytes = [0_u8; 7];
        let prefix = parse_core_side_prefix_at(&bytes, 0, false).unwrap();
        assert_eq!(prefix.transform_type, TransformType::Long);
        assert_eq!(prefix.fd_shaping.next_bit_offset, 48);
        assert_eq!(prefix.tns.next_bit_offset, 50);
        assert_eq!(prefix.next_bit_offset, 50);
        assert!(prefix.tns.filters.iter().all(|filter| !filter.enabled));
    }

    #[test]
    fn rejects_empty_side_information() {
        assert_eq!(parse_core_transform_type(&[]), Err(CodecError::Truncated));
    }
}
