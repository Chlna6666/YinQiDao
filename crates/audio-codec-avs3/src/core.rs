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

/// Parse only the fixed prefix of AVS3 core decoder side information.
///
/// `DecodeCoreSideBits()` starts with a 2-bit `transformType`, followed by variable-sized
/// FdShaping/TNS/BWE side information. The variable sections are intentionally not skipped here:
/// their exact syntax must be implemented before a caller can locate `DecodeGroupBits()` safely.
pub fn parse_core_transform_type(core_side_bits: &[u8]) -> Result<TransformType, CodecError> {
    let mut reader = BitReader::new(core_side_bits);
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
    fn rejects_empty_side_information() {
        assert_eq!(parse_core_transform_type(&[]), Err(CodecError::Truncated));
    }
}
