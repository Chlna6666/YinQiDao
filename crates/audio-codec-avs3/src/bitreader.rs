use yinqidao_codec_core::CodecError;

#[derive(Clone, Copy, Debug)]
pub(crate) struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    pub(crate) fn bits_remaining(&self) -> usize {
        self.bytes.len().saturating_mul(8).saturating_sub(self.bit_pos)
    }

    pub(crate) fn position_bits(&self) -> usize {
        self.bit_pos
    }

    pub(crate) fn read_bit(&mut self) -> Result<bool, CodecError> {
        Ok(self.read_bits(1)? != 0)
    }

    pub(crate) fn read_bits(&mut self, count: u8) -> Result<u32, CodecError> {
        if count > 32 {
            return Err(CodecError::InvalidData("bit read wider than 32 bits"));
        }
        let count = usize::from(count);
        if self.bits_remaining() < count {
            return Err(CodecError::Truncated);
        }
        let mut value = 0_u32;
        for _ in 0..count {
            let byte = self.bytes[self.bit_pos / 8];
            let shift = 7 - (self.bit_pos & 7);
            value = (value << 1) | u32::from((byte >> shift) & 1);
            self.bit_pos += 1;
        }
        Ok(value)
    }

    pub(crate) fn skip_bits(&mut self, count: usize) -> Result<(), CodecError> {
        if self.bits_remaining() < count {
            return Err(CodecError::Truncated);
        }
        self.bit_pos += count;
        Ok(())
    }

    pub(crate) fn align_byte(&mut self) {
        self.bit_pos = (self.bit_pos + 7) & !7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first_without_allocation() {
        let bytes = [0b1011_0010, 0b0110_0001];
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read_bits(4).unwrap(), 0b1011);
        assert!(!reader.read_bit().unwrap());
        assert_eq!(reader.read_bits(3).unwrap(), 0b010);
        assert_eq!(reader.position_bits(), 8);
        assert_eq!(reader.read_bits(8).unwrap(), 0b0110_0001);
        assert_eq!(reader.bits_remaining(), 0);
        assert_eq!(reader.position_bits(), 16);
    }
}
