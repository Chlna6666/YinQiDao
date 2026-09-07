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
}

/// Parse only the fixed prefix of AVS3 core decoder side information at bit offset zero.
pub fn parse_core_transform_type(core_side_bits: &[u8]) -> Result<TransformType, CodecError> {
    parse_core_transform_type_at(core_side_bits, 0)
}

/// Parse `transformType` from an arbitrary bit position in a GA payload.
///
/// This is required because `Avs3MetadataDec()` is bit-packed: a frame with neither static nor
/// dynamic metadata enters `DecodeCoreSideBits()` at bit offset 2 rather than a byte boundary.
pub fn parse_core_transform_type_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<TransformType, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    Ok(TransformType::from_bits(reader.read_bits(2)? as u8))
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
    fn rejects_empty_side_information() {
        assert_eq!(parse_core_transform_type(&[]), Err(CodecError::Truncated));
    }
}
