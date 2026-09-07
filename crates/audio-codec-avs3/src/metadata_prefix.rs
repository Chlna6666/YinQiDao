use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

/// Fixed prefix of `Avs3SmDec()` before the variable-size `BasicL1()` body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticMetadataPrefix {
    pub vr_extension_present: bool,
    pub basic_level: u8,
    /// Bit position where `BasicL1()` starts for level 0/1 streams.
    pub basic_l1_bit_offset: Option<usize>,
}

impl StaticMetadataPrefix {
    pub const fn basic_level_supported(self) -> bool {
        self.basic_level <= 1
    }
}

/// Fixed prefix of `Avs3DmDec()` before per-object metadata bodies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicMetadataPrefix {
    pub dm_level: u8,
    pub channel_count: u16,
    pub first_channel_bit_offset: usize,
    pub first_channel: Option<DynamicChannelPrefix>,
}

impl DynamicMetadataPrefix {
    pub const fn dm_level_supported(self) -> bool {
        self.dm_level <= 1
    }
}

/// Per-object envelope that precedes `Avs3DmL1Dec()` / `Avs3DmL2Dec()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicChannelPrefix {
    pub mute: bool,
    pub transport_channel_ref: u8,
    /// Bit position where the level-specific dynamic metadata body begins.
    pub body_bit_offset: usize,
}

/// Parse `b_vrExt` and `basicLevel` from `Avs3SmDec()` at an arbitrary bit position.
///
/// For basic levels 0 and 1 the next syntax element is `BasicL1()`. We deliberately stop there:
/// `vrExtLevel` follows *after* the variable-length BasicL1 body, so consuming it early would shift
/// every following field.
pub fn parse_static_metadata_prefix_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<StaticMetadataPrefix, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let vr_extension_present = reader.read_bit()?;
    let basic_level = reader.read_bits(3)? as u8;
    let basic_l1_bit_offset = (basic_level <= 1).then_some(reader.position_bits());

    Ok(StaticMetadataPrefix {
        vr_extension_present,
        basic_level,
        basic_l1_bit_offset,
    })
}

/// Parse the fixed `dmLevel` prefix and the first object's envelope from `Avs3DmDec()`.
///
/// The metadata standard defines `numDmChans` as the object-channel count from the AATF header.
/// A channel envelope is six fixed bits (`muteFlag` + `transChRef`) followed immediately by a
/// variable-size level body. Until that body is implemented we must not pretend we can seek to the
/// second channel by multiplying six by the object count.
pub fn parse_dynamic_metadata_prefix_at(
    bytes: &[u8],
    bit_offset: usize,
    channel_count: u16,
) -> Result<DynamicMetadataPrefix, CodecError> {
    if channel_count == 0 {
        return Err(CodecError::InvalidData(
            "dynamic Audio Vivid metadata has zero object channels",
        ));
    }

    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let dm_level = reader.read_bits(3)? as u8;
    let first_channel_bit_offset = reader.position_bits();
    let first_channel = Some(parse_dynamic_channel_prefix_at(
        bytes,
        first_channel_bit_offset,
    )?);

    Ok(DynamicMetadataPrefix {
        dm_level,
        channel_count,
        first_channel_bit_offset,
        first_channel,
    })
}

/// Parse the fixed envelope for one dynamic-metadata object at a known object boundary.
pub fn parse_dynamic_channel_prefix_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<DynamicChannelPrefix, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let mute = reader.read_bit()?;
    let transport_channel_ref = reader.read_bits(5)? as u8;

    Ok(DynamicChannelPrefix {
        mute,
        transport_channel_ref,
        body_bit_offset: reader.position_bits(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_prefix_locates_basic_l1_without_byte_alignment() {
        // outer smFlag=1, then b_vrExt=0, basicLevel=1.
        let bytes = [0b1_0_001_101];
        let prefix = parse_static_metadata_prefix_at(&bytes, 1).unwrap();
        assert!(!prefix.vr_extension_present);
        assert_eq!(prefix.basic_level, 1);
        assert_eq!(prefix.basic_l1_bit_offset, Some(5));
    }

    #[test]
    fn reserved_static_level_does_not_claim_a_basic_l1_body() {
        // b_vrExt=1, basicLevel=5.
        let bytes = [0b1_101_0000];
        let prefix = parse_static_metadata_prefix_at(&bytes, 0).unwrap();
        assert!(prefix.vr_extension_present);
        assert_eq!(prefix.basic_level, 5);
        assert!(!prefix.basic_level_supported());
        assert_eq!(prefix.basic_l1_bit_offset, None);
    }

    #[test]
    fn dynamic_prefix_parses_level_and_first_object_envelope() {
        // dmLevel=1, muteFlag=0, transChRef=17, body starts at bit 9.
        let bytes = [0b001_0_1000, 0b1_1111111];
        let prefix = parse_dynamic_metadata_prefix_at(&bytes, 0, 8).unwrap();
        assert_eq!(prefix.dm_level, 1);
        assert_eq!(prefix.channel_count, 8);
        assert_eq!(prefix.first_channel_bit_offset, 3);
        let channel = prefix.first_channel.unwrap();
        assert!(!channel.mute);
        assert_eq!(channel.transport_channel_ref, 17);
        assert_eq!(channel.body_bit_offset, 9);
    }

    #[test]
    fn dynamic_prefix_rejects_zero_object_channels() {
        assert_eq!(
            parse_dynamic_metadata_prefix_at(&[0; 2], 0, 0),
            Err(CodecError::InvalidData(
                "dynamic Audio Vivid metadata has zero object channels"
            ))
        );
    }
}
