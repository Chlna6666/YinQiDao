use yinqidao_codec_avs3::parse_aatf_frame_header;

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
                let index = self.bytes.len() - 1;
                self.bytes[index] |= 1 << (7 - (self.bit_pos & 7));
            }
            self.bit_pos += 1;
        }
    }
}

#[test]
fn lossless_frame_crc_remains_after_raw_data_block() {
    let mut writer = BitWriter::new();
    writer.push(0x0fff, 12); // syncword
    writer.push(1, 4); // audio_codec_id: lossless
    writer.push(0, 1); // anc_data_index
    writer.push(0, 3); // coding_profile: basic
    writer.push(2, 4); // sampling_frequency_index: 48 kHz
    writer.push(9, 16); // raw_frame_length: complete synthetic frame
    writer.push(0x5a, 8); // aatf_error_check()
    writer.push(2, 4); // channel_number
    writer.push(1, 2); // resolution: 16-bit
    writer.push(0, 2); // byte_alignment()

    // The lossless raw-data block starts here. Its internal syntax is intentionally opaque to this
    // regression test: the Chapter 8 + amendment parser is a separate milestone.
    writer.push(0xde, 8);
    // For audio_codec_id == 1, frame_error_check() follows ll_raw_data_block() at the AATF-frame end.
    writer.push(0xa5, 8);

    let header = parse_aatf_frame_header(&writer.bytes).unwrap();
    assert_eq!(header.payload_offset_bytes, 7);
    assert_eq!(header.frame_crc, None);
    assert_eq!(writer.bytes[header.payload_offset_bytes], 0xde);
    assert_eq!(writer.bytes.last().copied(), Some(0xa5));
}
