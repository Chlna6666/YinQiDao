use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

const TNS_MAX_ORDER: usize = 8;
const TNS_FILTERS: usize = 2;
const TNS_QUANT_LEVELS: usize = 16;
const TNS_MAX_HUFFMAN_BITS: u8 = 15;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HuffmanCode {
    code: u16,
    bits: u8,
}

const fn h(code: u16, bits: u8) -> HuffmanCode {
    HuffmanCode { code, bits }
}

/// GY/T 363-2023 tables B.25..B.32. Each row is one reflection-coefficient dimension and each
/// column maps quantization index 1..=16 to its MSB-first Huffman codeword.
const TNS_HUFFMAN_TABLES: [[HuffmanCode; TNS_QUANT_LEVELS]; TNS_MAX_ORDER] = [
    [
        h(4053, 12), h(1012, 10), h(507, 9), h(127, 7), h(30, 5), h(0, 3),
        h(1, 3), h(2, 3), h(2, 2), h(3, 3), h(6, 3), h(14, 4), h(62, 6),
        h(252, 8), h(2027, 11), h(8105, 13),
    ],
    [
        h(15360, 15), h(7681, 14), h(3841, 13), h(961, 11), h(241, 9), h(61, 7),
        h(14, 5), h(2, 3), h(2, 2), h(3, 2), h(0, 2), h(6, 4), h(31, 6),
        h(121, 8), h(481, 10), h(1921, 12),
    ],
    [
        h(27136, 15), h(27137, 15), h(3393, 12), h(425, 9), h(107, 7), h(52, 6),
        h(12, 4), h(7, 3), h(0, 1), h(2, 2), h(27, 5), h(213, 8), h(849, 10),
        h(1697, 11), h(6785, 13), h(27138, 15),
    ],
    [
        h(8708, 14), h(8709, 14), h(8710, 14), h(1089, 11), h(273, 9), h(137, 8),
        h(35, 6), h(5, 3), h(0, 1), h(3, 2), h(9, 4), h(16, 5), h(69, 7),
        h(545, 10), h(8711, 14), h(4352, 13),
    ],
    [
        h(4100, 14), h(4101, 14), h(4102, 14), h(257, 10), h(65, 8), h(17, 6),
        h(5, 4), h(0, 2), h(1, 1), h(3, 3), h(9, 5), h(33, 7), h(129, 9),
        h(513, 11), h(4103, 14), h(2048, 13),
    ],
    [
        h(8272, 14), h(8273, 14), h(2069, 12), h(516, 10), h(128, 8), h(65, 7),
        h(17, 5), h(5, 3), h(0, 1), h(3, 2), h(9, 4), h(33, 6), h(259, 9),
        h(1035, 11), h(8274, 14), h(8275, 14),
    ],
    [
        h(13312, 14), h(13313, 14), h(3329, 12), h(833, 10), h(209, 8), h(53, 6),
        h(12, 4), h(2, 2), h(0, 1), h(7, 3), h(27, 5), h(105, 7), h(417, 9),
        h(1665, 11), h(13314, 14), h(13315, 14),
    ],
    [
        h(10490, 14), h(2625, 12), h(657, 10), h(165, 8), h(83, 7), h(21, 5),
        h(4, 3), h(3, 2), h(10497, 14), h(0, 1), h(11, 4), h(40, 6), h(329, 9),
        h(1313, 11), h(10498, 14), h(10499, 14),
    ],
];

/// GY/T 363-2023 table B.33, indexed by the decoded Huffman quantization index 1..=16.
pub const TNS_REFLECTION_COEFFICIENTS: [f32; TNS_QUANT_LEVELS] = [
    -0.995_734_16,
    -0.961_825_67,
    -0.895_163_3,
    -0.798_017_2,
    -0.673_695_6,
    -0.526_432_16,
    -0.361_241_67,
    -0.183_749_51,
    0.0,
    0.207_911_69,
    0.406_736_64,
    0.587_785_24,
    0.743_144_8,
    0.866_025_4,
    0.951_056_54,
    0.994_521_9,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TnsFilterSideInfo {
    pub enabled: bool,
    /// Semantic filter order, 0 when disabled and 1..=8 when enabled.
    pub order: u8,
    /// Quantization indices from B.25..B.32. Only the first `order` entries are present.
    pub quant_indices: [Option<u8>; TNS_MAX_ORDER],
}

impl TnsFilterSideInfo {
    const fn disabled() -> Self {
        Self {
            enabled: false,
            order: 0,
            quant_indices: [None; TNS_MAX_ORDER],
        }
    }

    /// Convert one decoded 1-based quantization index to the scalar reflection coefficient B.33.
    pub fn reflection_coefficient(&self, dimension: usize) -> Option<f32> {
        let index = self.quant_indices.get(dimension).copied().flatten()?;
        reflection_coefficient(index).ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TnsSideInfo {
    pub filters: [TnsFilterSideInfo; TNS_FILTERS],
    pub next_bit_offset: usize,
}

pub fn reflection_coefficient(index: u8) -> Result<f32, CodecError> {
    let offset = usize::from(index.checked_sub(1).ok_or(CodecError::InvalidData(
        "TNS reflection coefficient index is zero",
    ))?);
    TNS_REFLECTION_COEFFICIENTS
        .get(offset)
        .copied()
        .ok_or(CodecError::InvalidData(
            "TNS reflection coefficient index exceeds table B.33",
        ))
}

/// Decode the complete two-filter `DecodeTnsSideBits()` syntax.
pub fn parse_tns_side_info_at(
    bytes: &[u8],
    bit_offset: usize,
) -> Result<TnsSideInfo, CodecError> {
    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let mut filters = [TnsFilterSideInfo::disabled(); TNS_FILTERS];

    for filter in &mut filters {
        let enabled = reader.read_bit()?;
        if !enabled {
            continue;
        }

        let order = (reader.read_bits(3)? as u8).saturating_add(1);
        if usize::from(order) > TNS_MAX_ORDER {
            return Err(CodecError::InvalidData("TNS filter order exceeds eight"));
        }

        filter.enabled = true;
        filter.order = order;
        for dimension in 0..usize::from(order) {
            filter.quant_indices[dimension] = Some(decode_huffman_index(
                &mut reader,
                dimension,
            )?);
        }
    }

    Ok(TnsSideInfo {
        filters,
        next_bit_offset: reader.position_bits(),
    })
}

fn decode_huffman_index(
    reader: &mut BitReader<'_>,
    dimension: usize,
) -> Result<u8, CodecError> {
    let table = TNS_HUFFMAN_TABLES.get(dimension).ok_or(CodecError::InvalidData(
        "TNS Huffman dimension exceeds table B.32",
    ))?;

    let mut code = 0_u16;
    for bit_len in 1..=TNS_MAX_HUFFMAN_BITS {
        code = (code << 1) | u16::from(reader.read_bit()?);
        if let Some((index, _)) = table
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.bits == bit_len && entry.code == code)
        {
            return Ok((index + 1) as u8);
        }
    }

    Err(CodecError::InvalidData("invalid TNS Huffman codeword"))
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
    fn decodes_all_eight_dimensions_and_second_filter_disable() {
        let mut writer = BitWriter::new();
        writer.push(1, 1); // filter 0 enabled
        writer.push(7, 3); // order = 8
        // Short, easy-to-audit entries from B.25..B.32 respectively.
        writer.push(2, 2); // table0 index9
        writer.push(0, 2); // table1 index11
        writer.push(0, 1); // table2 index9
        writer.push(0, 1); // table3 index9
        writer.push(1, 1); // table4 index9
        writer.push(0, 1); // table5 index9
        writer.push(0, 1); // table6 index9
        writer.push(0, 1); // table7 index10
        writer.push(0, 1); // filter 1 disabled
        let expected_end = writer.bit_pos;

        let info = parse_tns_side_info_at(&writer.bytes, 0).unwrap();
        assert_eq!(info.next_bit_offset, expected_end);
        assert_eq!(info.filters[0].order, 8);
        assert_eq!(
            info.filters[0].quant_indices,
            [
                Some(9),
                Some(11),
                Some(9),
                Some(9),
                Some(9),
                Some(9),
                Some(9),
                Some(10),
            ]
        );
        assert!(!info.filters[1].enabled);
    }

    #[test]
    fn decodes_long_table_edges_without_reversing_codewords() {
        let mut writer = BitWriter::new();
        writer.push(1, 1);
        writer.push(0, 3); // order = 1, use B.25
        writer.push(8105, 13); // B.25 index16
        writer.push(0, 1); // second filter disabled

        let info = parse_tns_side_info_at(&writer.bytes, 0).unwrap();
        assert_eq!(info.filters[0].quant_indices[0], Some(16));
    }

    #[test]
    fn maps_quantization_indices_to_b33_coefficients() {
        assert_eq!(reflection_coefficient(9).unwrap(), 0.0);
        assert!((reflection_coefficient(16).unwrap() - 0.994_521_9).abs() < 1.0e-7);
        assert!(reflection_coefficient(0).is_err());
        assert!(reflection_coefficient(17).is_err());
    }

    #[test]
    fn two_disabled_filters_consume_exactly_two_bits() {
        let info = parse_tns_side_info_at(&[0], 0).unwrap();
        assert_eq!(info.next_bit_offset, 2);
        assert!(info.filters.iter().all(|filter| !filter.enabled));
    }
}
