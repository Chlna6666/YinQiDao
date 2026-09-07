use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

/// The part of `Avs3MetadataDec()` that can be consumed without knowing the variable-sized static
/// or dynamic metadata syntax which follows a set flag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataBoundary {
    /// Both metadata flags are zero; audio core syntax begins at `core_bit_offset`.
    None { core_bit_offset: usize },
    /// `smFlag == 1`. Static metadata starts immediately after the first bit and must be decoded
    /// before `dmFlag` can even be located.
    StaticPresent { static_bit_offset: usize },
    /// `smFlag == 0 && dmFlag == 1`. Dynamic metadata starts after the two flag bits.
    DynamicPresent { dynamic_bit_offset: usize },
}

/// Parse the fixed prefix of `Avs3MetadataDec()` without skipping unknown metadata structures.
pub fn parse_metadata_boundary(payload: &[u8]) -> Result<MetadataBoundary, CodecError> {
    let mut reader = BitReader::new(payload);
    let sm_flag = reader.read_bit()?;
    if sm_flag {
        return Ok(MetadataBoundary::StaticPresent {
            static_bit_offset: reader.position_bits(),
        });
    }

    let dm_flag = reader.read_bit()?;
    if dm_flag {
        Ok(MetadataBoundary::DynamicPresent {
            dynamic_bit_offset: reader.position_bits(),
        })
    } else {
        Ok(MetadataBoundary::None {
            core_bit_offset: reader.position_bits(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_metadata_leaves_core_at_bit_two() {
        assert_eq!(
            parse_metadata_boundary(&[0b00_101010]).unwrap(),
            MetadataBoundary::None { core_bit_offset: 2 }
        );
    }

    #[test]
    fn static_metadata_stops_before_unknown_static_payload() {
        assert_eq!(
            parse_metadata_boundary(&[0b10_000000]).unwrap(),
            MetadataBoundary::StaticPresent { static_bit_offset: 1 }
        );
    }

    #[test]
    fn dynamic_metadata_stops_after_flags() {
        assert_eq!(
            parse_metadata_boundary(&[0b01_000000]).unwrap(),
            MetadataBoundary::DynamicPresent { dynamic_bit_offset: 2 }
        );
    }

    #[test]
    fn empty_payload_is_truncated() {
        assert_eq!(parse_metadata_boundary(&[]), Err(CodecError::Truncated));
    }
}
