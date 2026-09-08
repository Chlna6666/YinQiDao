use yinqidao_codec_core::CodecError;

use crate::BitRange;

pub const RANGE_DEFAULT_PRECISION: u8 = 16;
pub const RANGE_OVERFLOW_WIDTH: u8 = 4;

const OVERFLOW_CDF: [u32; 17] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];

/// One zero-copy byte window inside a bit-packed AVS3 packet.
///
/// `DecodeQcBits()` counts context/base payloads in bytes, but the first byte is not guaranteed to
/// start on an eight-bit packet boundary. This view reconstructs logical bytes directly from the
/// packet without allocating/repacking a temporary Vec.
#[derive(Clone, Copy, Debug)]
pub struct RangeByteWindow<'a> {
    packet: &'a [u8],
    bit_offset: usize,
    byte_len: usize,
}

impl<'a> RangeByteWindow<'a> {
    pub fn new(packet: &'a [u8], range: BitRange) -> Result<Self, CodecError> {
        if range.bit_len & 7 != 0 {
            return Err(CodecError::InvalidData(
                "range-coded QC payload length is not byte-aligned",
            ));
        }
        let packet_bits = packet.len().saturating_mul(8);
        if range.end_bit_offset() > packet_bits {
            return Err(CodecError::Truncated);
        }
        Ok(Self {
            packet,
            bit_offset: range.bit_offset,
            byte_len: range.bit_len / 8,
        })
    }

    pub const fn len_bytes(self) -> usize {
        self.byte_len
    }

    pub const fn is_empty(self) -> bool {
        self.byte_len == 0
    }

    #[inline]
    fn byte_at(self, index: usize) -> u8 {
        if index >= self.byte_len {
            return 0;
        }
        let absolute_bit = self.bit_offset + index * 8;
        let byte_index = absolute_bit / 8;
        let shift = absolute_bit & 7;
        let first = self.packet[byte_index];
        if shift == 0 {
            return first;
        }
        let second = self.packet.get(byte_index + 1).copied().unwrap_or(0);
        let pair = (u32::from(first) << 8) | u32::from(second);
        ((pair << shift) >> 8) as u8
    }
}

/// One AVS3 range-coding cumulative distribution.
///
/// The CDF is MSB/range-coder order, begins at zero and ends at exactly `2^precision`. The final
/// ordinary symbol is the escape symbol used by AVS3's signed overflow extension.
#[derive(Clone, Copy, Debug)]
pub struct RangeModel<'a> {
    cumulative: &'a [u32],
    offset: i32,
    precision: u8,
}

impl<'a> RangeModel<'a> {
    pub fn new(cumulative: &'a [u32], offset: i32, precision: u8) -> Result<Self, CodecError> {
        if !(1..=16).contains(&precision) {
            return Err(CodecError::InvalidData(
                "range CDF precision must be between one and sixteen bits",
            ));
        }
        if cumulative.len() < 3 {
            return Err(CodecError::InvalidData(
                "range CDF requires at least one ordinary and one escape symbol",
            ));
        }
        let total = 1_u32 << precision;
        if cumulative.first().copied() != Some(0) || cumulative.last().copied() != Some(total) {
            return Err(CodecError::InvalidData(
                "range CDF endpoints do not match configured precision",
            ));
        }
        if cumulative.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(CodecError::InvalidData(
                "range CDF must be strictly increasing",
            ));
        }
        Ok(Self {
            cumulative,
            offset,
            precision,
        })
    }

    pub const fn cumulative(self) -> &'a [u32] {
        self.cumulative
    }

    pub const fn offset(self) -> i32 {
        self.offset
    }

    pub const fn precision(self) -> u8 {
        self.precision
    }

    pub fn escape_symbol(self) -> usize {
        self.cumulative.len() - 2
    }
}

/// AVS3's 32-bit interval decoder with 16-bit renormalization.
///
/// Input is conceptually zero-extended after the declared range-coded byte window, matching the
/// encoder finalization rule that permits omitted trailing zero bytes.
pub struct RangeDecoder<'a> {
    input: RangeByteWindow<'a>,
    input_pos: usize,
    base: u32,
    size_minus_one: u32,
    value: u32,
    initialized: bool,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(input: RangeByteWindow<'a>) -> Self {
        Self {
            input,
            input_pos: 0,
            base: 0,
            size_minus_one: u32::MAX,
            value: 0,
            initialized: false,
        }
    }

    pub const fn input_bytes_consumed(&self) -> usize {
        self.input_pos
    }

    pub fn decode_value(&mut self, model: RangeModel<'_>) -> Result<i32, CodecError> {
        let symbol = self.decode_symbol(model.cumulative, model.precision)?;
        let escape = model.escape_symbol();
        let decoded = if symbol != escape {
            i64::try_from(symbol)
                .map_err(|_| CodecError::InvalidData("range symbol exceeds signed decode domain"))?
        } else {
            let overflow = self.decode_overflow()?;
            map_escape_value(escape, overflow)?
        };
        let restored = decoded + i64::from(model.offset);
        i32::try_from(restored)
            .map_err(|_| CodecError::InvalidData("range-decoded value exceeds i32 domain"))
    }

    pub fn decode_sequence(
        &mut self,
        models: &[RangeModel<'_>],
        model_indices: &[usize],
        output: &mut [i32],
    ) -> Result<(), CodecError> {
        if model_indices.len() != output.len() {
            return Err(CodecError::InvalidData(
                "range model-index count does not match output length",
            ));
        }
        for (slot, &model_index) in output.iter_mut().zip(model_indices) {
            let model = models
                .get(model_index)
                .copied()
                .ok_or(CodecError::InvalidData(
                    "range CDF index exceeds model table",
                ))?;
            *slot = self.decode_value(model)?;
        }
        Ok(())
    }

    fn decode_overflow(&mut self) -> Result<u32, CodecError> {
        let mut sections = 0_usize;
        loop {
            let value = self.decode_symbol(&OVERFLOW_CDF, RANGE_OVERFLOW_WIDTH)?;
            sections = sections.checked_add(value).ok_or(CodecError::InvalidData(
                "range overflow section count overflow",
            ))?;
            if sections > 8 {
                return Err(CodecError::InvalidData(
                    "range overflow exceeds 32-bit signed extension",
                ));
            }
            if value != 15 {
                break;
            }
        }

        let mut overflow = 0_u32;
        for section in 0..sections {
            let nibble = self.decode_symbol(&OVERFLOW_CDF, RANGE_OVERFLOW_WIDTH)? as u32;
            overflow |= nibble << (section * usize::from(RANGE_OVERFLOW_WIDTH));
        }
        Ok(overflow)
    }

    fn decode_symbol(&mut self, cdf: &[u32], precision: u8) -> Result<usize, CodecError> {
        if !self.initialized {
            self.refill_16();
            self.refill_16();
            self.initialized = true;
        }
        if !(1..=16).contains(&precision) || cdf.len() < 2 {
            return Err(CodecError::InvalidData("invalid range symbol model"));
        }

        let total = 1_u32 << precision;
        if cdf.first().copied() != Some(0) || cdf.last().copied() != Some(total) {
            return Err(CodecError::InvalidData("invalid range CDF endpoints"));
        }

        let size = u64::from(self.size_minus_one) + 1;
        let relative = u64::from(self.value.wrapping_sub(self.base));
        if relative >= size {
            return Err(CodecError::InvalidData(
                "range decoder code value lies outside current interval",
            ));
        }
        let scaled_offset = ((relative + 1) << precision) - 1;

        // Find the first cumulative boundary whose scaled interval end is above the code value.
        let mut low = 1_usize;
        let mut high = cdf.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if mid == cdf.len() || size.saturating_mul(u64::from(cdf[mid])) > scaled_offset {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        if low >= cdf.len() {
            return Err(CodecError::InvalidData(
                "range decoder could not locate symbol in CDF",
            ));
        }

        let lower = (size * u64::from(cdf[low - 1])) >> precision;
        let upper = (size * u64::from(cdf[low])) >> precision;
        if upper <= lower {
            return Err(CodecError::InvalidData(
                "range decoder selected an empty CDF interval",
            ));
        }

        self.base = self.base.wrapping_add(lower as u32);
        let new_size = upper - lower;
        self.size_minus_one = (new_size - 1) as u32;

        if self.size_minus_one >> 16 == 0 {
            self.base = self.base.wrapping_shl(16);
            self.size_minus_one = self.size_minus_one.wrapping_shl(16) | 0xFFFF;
            self.refill_16();
        }

        Ok(low - 1)
    }

    #[inline]
    fn refill_16(&mut self) {
        for _ in 0..2 {
            self.value = self.value.wrapping_shl(8);
            if self.input_pos < self.input.len_bytes() {
                self.value |= u32::from(self.input.byte_at(self.input_pos));
                self.input_pos += 1;
            }
        }
    }
}

fn map_escape_value(escape_symbol: usize, overflow: u32) -> Result<i64, CodecError> {
    let half = i64::from(overflow >> 1);
    if overflow & 1 != 0 {
        Ok(-half - 1)
    } else {
        let escape = i64::try_from(escape_symbol).map_err(|_| {
            CodecError::InvalidData("range escape symbol exceeds signed decode domain")
        })?;
        escape.checked_add(half).ok_or(CodecError::InvalidData(
            "range positive overflow exceeds signed decode domain",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_copy_window_extracts_unaligned_bytes() {
        let packet = [0b1011_0010, 0b0110_0001, 0b1111_0000];
        let window = RangeByteWindow::new(
            &packet,
            BitRange {
                bit_offset: 3,
                bit_len: 16,
            },
        )
        .unwrap();
        assert_eq!(window.byte_at(0), 0b1001_0011);
        assert_eq!(window.byte_at(1), 0b0000_1111);
        assert_eq!(window.byte_at(2), 0);
    }

    #[test]
    fn validates_normative_cdf_contract() {
        assert!(RangeModel::new(&[0, 32_768, 65_536], 0, 16).is_ok());
        assert!(RangeModel::new(&[0, 32_768, 32_768, 65_536], 0, 16).is_err());
        assert!(RangeModel::new(&[1, 32_768, 65_536], 0, 16).is_err());
    }

    #[test]
    fn decodes_binary_interval_from_initial_32_bits() {
        let packet = [0x80, 0x00, 0x00, 0x00];
        let input = RangeByteWindow::new(
            &packet,
            BitRange {
                bit_offset: 0,
                bit_len: 32,
            },
        )
        .unwrap();
        let mut decoder = RangeDecoder::new(input);
        assert_eq!(decoder.decode_symbol(&[0, 32_768, 65_536], 16).unwrap(), 1);
    }

    #[test]
    fn renormalizes_by_sixteen_bits_without_extra_buffer() {
        // First symbol selects the tiny [0,1/65536) interval. The low 16 bits of the initial code
        // become the high 16 bits after renormalization, so the second binary symbol is one.
        let packet = [0x00, 0x00, 0x80, 0x00];
        let input = RangeByteWindow::new(
            &packet,
            BitRange {
                bit_offset: 0,
                bit_len: 32,
            },
        )
        .unwrap();
        let mut decoder = RangeDecoder::new(input);
        assert_eq!(decoder.decode_symbol(&[0, 1, 65_536], 16).unwrap(), 0);
        assert_eq!(decoder.decode_symbol(&[0, 32_768, 65_536], 16).unwrap(), 1);
    }

    #[test]
    fn escape_mapping_restores_signed_tails() {
        assert_eq!(map_escape_value(10, 0).unwrap(), 10);
        assert_eq!(map_escape_value(10, 2).unwrap(), 11);
        assert_eq!(map_escape_value(10, 1).unwrap(), -1);
        assert_eq!(map_escape_value(10, 3).unwrap(), -2);
    }
}
