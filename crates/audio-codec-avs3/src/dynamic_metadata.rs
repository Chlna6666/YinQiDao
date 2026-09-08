use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolarExtent {
    pub width_horizontal: u8,
    pub height_vertical: u8,
    pub depth_distance: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CartesianExtent {
    pub width_x: u8,
    pub height_y: u8,
    pub depth_z: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectPosition {
    Polar {
        azimuth: u8,
        elevation: u8,
        distance: u8,
        extent: Option<PolarExtent>,
    },
    Cartesian {
        x: u8,
        y: u8,
        z: u8,
        extent: Option<CartesianExtent>,
    },
}

/// Raw Level-1 Audio Vivid object metadata.
///
/// Values intentionally remain quantized integers. Rendering converts them to angles/normalized
/// coordinates later so the bitstream decoder stays lossless and deterministic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicLevel1 {
    pub position: ObjectPosition,
    pub gain: Option<u8>,
    pub diffuse: Option<u8>,
    pub jump_position: bool,
    pub importance: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelLock {
    pub locked: bool,
    pub max_distance: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectDivergence {
    pub value: u8,
    pub azimuth_range: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicLevel2 {
    pub channel_lock: Option<ChannelLock>,
    pub object_divergence: Option<ObjectDivergence>,
    pub object_screen_ref: Option<bool>,
    pub screen_edge_lock: Option<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicObjectMetadata {
    pub mute: bool,
    pub transport_channel_ref: u8,
    pub level1: Option<DynamicLevel1>,
    pub level2: Option<DynamicLevel2>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicMetadata {
    pub level: u8,
    pub objects: Vec<DynamicObjectMetadata>,
    /// First bit after `Avs3DmDec()`, i.e. the beginning of codec core side information.
    pub core_bit_offset: usize,
}

/// Decode the complete `Avs3DmDec()` object-metadata block.
///
/// T/AI 109.3 / T/UWA 009.1 define `numDmChans` as the AATF object-channel count. Levels 0 and 1
/// are fully specified: level 0 carries L1, while level 1 carries L1 followed by L2 for every
/// object. Reserved levels are rejected before attempting to consume object bodies.
pub fn parse_dynamic_metadata_at(
    bytes: &[u8],
    bit_offset: usize,
    channel_count: u16,
) -> Result<DynamicMetadata, CodecError> {
    if channel_count == 0 {
        return Err(CodecError::InvalidData(
            "dynamic Audio Vivid metadata has zero object channels",
        ));
    }

    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let level = reader.read_bits(3)? as u8;
    if level > 1 {
        return Err(CodecError::Unsupported(
            "reserved AVS3 dynamic metadata level",
        ));
    }

    let mut objects = Vec::with_capacity(usize::from(channel_count));
    for _ in 0..channel_count {
        let mute = reader.read_bit()?;
        let transport_channel_ref = reader.read_bits(5)? as u8;
        let level1 = parse_level1(&mut reader, mute)?;
        let level2 = if level == 1 {
            parse_level2(&mut reader, mute)?
        } else {
            None
        };
        objects.push(DynamicObjectMetadata {
            mute,
            transport_channel_ref,
            level1,
            level2,
        });
    }

    Ok(DynamicMetadata {
        level,
        objects,
        core_bit_offset: reader.position_bits(),
    })
}

fn parse_level1(
    reader: &mut BitReader<'_>,
    mute: bool,
) -> Result<Option<DynamicLevel1>, CodecError> {
    if mute {
        return Ok(None);
    }

    let cartesian = reader.read_bit()?;
    let has_extent = reader.read_bit()?;
    let has_gain = reader.read_bit()?;
    let has_diffuse = reader.read_bit()?;
    let has_importance = reader.read_bit()?;

    let position = if !cartesian {
        let azimuth = reader.read_bits(8)? as u8;
        let elevation = reader.read_bits(6)? as u8;
        let distance = reader.read_bits(4)? as u8;
        let extent = if has_extent {
            Some(PolarExtent {
                width_horizontal: reader.read_bits(7)? as u8,
                height_vertical: reader.read_bits(5)? as u8,
                depth_distance: reader.read_bits(4)? as u8,
            })
        } else {
            None
        };
        ObjectPosition::Polar {
            azimuth,
            elevation,
            distance,
            extent,
        }
    } else {
        let x = reader.read_bits(8)? as u8;
        let y = reader.read_bits(6)? as u8;
        let z = reader.read_bits(4)? as u8;
        let extent = if has_extent {
            Some(CartesianExtent {
                width_x: reader.read_bits(7)? as u8,
                height_y: reader.read_bits(5)? as u8,
                depth_z: reader.read_bits(4)? as u8,
            })
        } else {
            None
        };
        ObjectPosition::Cartesian { x, y, z, extent }
    };

    let gain = has_gain
        .then(|| reader.read_bits(7).map(|value| value as u8))
        .transpose()?;
    let diffuse = has_diffuse
        .then(|| reader.read_bits(7).map(|value| value as u8))
        .transpose()?;
    let jump_position = reader.read_bit()?;
    let importance = has_importance
        .then(|| reader.read_bits(4).map(|value| value as u8))
        .transpose()?;

    Ok(Some(DynamicLevel1 {
        position,
        gain,
        diffuse,
        jump_position,
        importance,
    }))
}

fn parse_level2(
    reader: &mut BitReader<'_>,
    mute: bool,
) -> Result<Option<DynamicLevel2>, CodecError> {
    if mute {
        return Ok(None);
    }

    let channel_lock = if reader.read_bit()? {
        let locked = reader.read_bit()?;
        let max_distance = if locked {
            Some(reader.read_bits(4)? as u8)
        } else {
            None
        };
        Some(ChannelLock {
            locked,
            max_distance,
        })
    } else {
        None
    };

    let object_divergence = if reader.read_bit()? {
        let value = reader.read_bits(4)? as u8;
        let azimuth_range = if value != 0 {
            Some(reader.read_bits(6)? as u8)
        } else {
            None
        };
        Some(ObjectDivergence {
            value,
            azimuth_range,
        })
    } else {
        None
    };

    let object_screen_ref = if reader.read_bit()? {
        Some(reader.read_bit()?)
    } else {
        None
    };

    let screen_edge_lock = if reader.read_bit()? {
        Some(reader.read_bits(2)? as u8)
    } else {
        None
    };

    Ok(Some(DynamicLevel2 {
        channel_lock,
        object_divergence,
        object_screen_ref,
        screen_edge_lock,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    let byte = self.bytes.len() - 1;
                    self.bytes[byte] |= 1 << (7 - (self.bit_pos & 7));
                }
                self.bit_pos += 1;
            }
        }
    }

    #[test]
    fn parses_level0_polar_object_and_exposes_core_boundary() {
        let mut writer = BitWriter::new();
        writer.push(0, 3); // dmLevel
        writer.push(0, 1); // muteFlag
        writer.push(9, 5); // transChRef
        writer.push(0, 1); // polar
        writer.push(1, 1); // extent
        writer.push(1, 1); // gain
        writer.push(1, 1); // diffuse
        writer.push(1, 1); // importance
        writer.push(200, 8);
        writer.push(41, 6);
        writer.push(12, 4);
        writer.push(90, 7);
        writer.push(17, 5);
        writer.push(9, 4);
        writer.push(80, 7);
        writer.push(55, 7);
        writer.push(1, 1); // jumpPosition
        writer.push(7, 4);
        let expected_end = writer.bit_pos;

        let metadata = parse_dynamic_metadata_at(&writer.bytes, 0, 1).unwrap();
        assert_eq!(metadata.level, 0);
        assert_eq!(metadata.core_bit_offset, expected_end);
        let object = metadata.objects[0];
        assert_eq!(object.transport_channel_ref, 9);
        let level1 = object.level1.unwrap();
        assert_eq!(level1.gain, Some(80));
        assert_eq!(level1.diffuse, Some(55));
        assert_eq!(level1.importance, Some(7));
        assert!(level1.jump_position);
        assert!(matches!(level1.position, ObjectPosition::Polar { .. }));
        assert_eq!(object.level2, None);
    }

    #[test]
    fn parses_level1_cartesian_and_level2_extensions() {
        let mut writer = BitWriter::new();
        writer.push(1, 3); // dmLevel
        writer.push(0, 1);
        writer.push(3, 5);
        writer.push(1, 1); // cartesian
        writer.push(0, 1); // no extent
        writer.push(0, 1); // no gain
        writer.push(0, 1); // no diffuse
        writer.push(0, 1); // no importance
        writer.push(128, 8);
        writer.push(32, 6);
        writer.push(8, 4);
        writer.push(0, 1); // jumpPosition

        writer.push(1, 1); // hasChannelLock
        writer.push(1, 1); // channelLock
        writer.push(6, 4); // max distance
        writer.push(1, 1); // has divergence
        writer.push(5, 4);
        writer.push(33, 6);
        writer.push(1, 1); // has screen ref
        writer.push(1, 1); // screen ref
        writer.push(1, 1); // has edge lock
        writer.push(2, 2);
        let expected_end = writer.bit_pos;

        let metadata = parse_dynamic_metadata_at(&writer.bytes, 0, 1).unwrap();
        assert_eq!(metadata.core_bit_offset, expected_end);
        let object = metadata.objects[0];
        assert!(matches!(
            object.level1.unwrap().position,
            ObjectPosition::Cartesian { .. }
        ));
        let level2 = object.level2.unwrap();
        assert_eq!(
            level2.channel_lock,
            Some(ChannelLock {
                locked: true,
                max_distance: Some(6)
            })
        );
        assert_eq!(
            level2.object_divergence,
            Some(ObjectDivergence {
                value: 5,
                azimuth_range: Some(33)
            })
        );
        assert_eq!(level2.object_screen_ref, Some(true));
        assert_eq!(level2.screen_edge_lock, Some(2));
    }

    #[test]
    fn muted_objects_consume_only_fixed_envelope() {
        let mut writer = BitWriter::new();
        writer.push(1, 3); // level 1
        writer.push(1, 1); // muted object 0
        writer.push(4, 5);
        writer.push(1, 1); // muted object 1
        writer.push(5, 5);
        let expected_end = writer.bit_pos;

        let metadata = parse_dynamic_metadata_at(&writer.bytes, 0, 2).unwrap();
        assert_eq!(metadata.core_bit_offset, expected_end);
        assert!(
            metadata
                .objects
                .iter()
                .all(|object| object.level1.is_none())
        );
        assert!(
            metadata
                .objects
                .iter()
                .all(|object| object.level2.is_none())
        );
    }
}
