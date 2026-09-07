use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

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

/// Boundary reached while parsing `DecodeTnsSideBits()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TnsSideBoundary {
    /// Both TNS filters are disabled, therefore parsing may continue at `next_bit_offset`.
    Complete {
        next_bit_offset: usize,
    },
    /// A filter is enabled. Its order is known, but the following coefficient codewords use the
    /// order-specific variable-length Huffman tables B.25..B.32.
    HuffmanCodes {
        filter_index: u8,
        order: u8,
        code_bit_offset: usize,
    },
}

/// Parsed portion of one `DecodeCoreSideBits()` block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoreSidePrefix {
    pub transform_type: TransformType,
    pub fd_shaping: FdShapingSideInfo,
    pub tns: TnsSideBoundary,
}

/// Parse only the fixed transform prefix at bit offset zero.
pub fn parse_core_transform_type(core_side_bits: &[u8]) -> Result<TransformType, CodecError> {
    parse_core_transform_type_at(core_side_bits, 0)
}

/// Parse `transformType` from an arbitrary bit position in a GA payload.
pub fn parse_core_transform_type_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<TransformType, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    Ok(TransformType::from_bits(reader.read_bits(2)? as u8))
}

/// Parse `DecodeFdShapingSideBits()` at a known bit boundary.
///
/// `lsfLbrFlag` is not transmitted inside this syntax block. The codec derives it from average
/// per-channel bitrate: <=32 kb/s uses the five-index low-precision layout, otherwise the seven-
/// index high-precision layout.
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

/// Parse the fixed part of the two-filter TNS side information.
///
/// When an enabled filter is encountered, parsing stops exactly at the first Huffman codeword. The
/// coefficient code cannot be skipped by a guessed width because each coefficient order has its
/// own normative variable-length table.
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

/// Parse the portion of `DecodeCoreSideBits()` whose widths are fully determined before the TNS
/// coefficient Huffman tables are consulted.
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
    let tns = parse_tns_boundary_at(bytes, fd_shaping.next_bit_offset)?;

    Ok(CoreSidePrefix {
        transform_type,
        fd_shaping,
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
        // smFlag=0, dmFlag=0, transformType=10 (cut-in).
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
    fn tns_disabled_filters_expose_next_boundary() {
        assert_eq!(
            parse_tns_boundary_at(&[0b00_111111], 0).unwrap(),
            TnsSideBoundary::Complete { next_bit_offset: 2 }
        );
    }

    #[test]
    fn tns_enabled_filter_stops_before_variable_huffman_code() {
        // filter0 enabled, raw order=3 => semantic order 4.
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
    fn core_prefix_chains_transform_fd_and_disabled_tns() {
        // transform=00, high-precision FD shaping consumes 46 zero bits, then tnsEnable[0..2]=00.
        let bytes = [0_u8; 7];
        let prefix = parse_core_side_prefix_at(&bytes, 0, false).unwrap();
        assert_eq!(prefix.transform_type, TransformType::Long);
        assert_eq!(prefix.fd_shaping.next_bit_offset, 48);
        assert_eq!(
            prefix.tns,
            TnsSideBoundary::Complete { next_bit_offset: 50 }
        );
    }

    #[test]
    fn rejects_empty_side_information() {
        assert_eq!(parse_core_transform_type(&[]), Err(CodecError::Truncated));
    }
}
