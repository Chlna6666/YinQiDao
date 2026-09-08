use yinqidao_codec_avs3::{Av3aSampleEntry, Avs3Decoder};
use yinqidao_codec_core::{AudioDecoder, AudioFrame, CodecError};

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

fn synthetic_lossless_frame() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.push(0x0fff, 12); // syncword
    writer.push(1, 4); // audio_codec_id: lossless
    writer.push(0, 1); // anc_data_index
    writer.push(0, 3); // coding_profile
    writer.push(2, 4); // 48 kHz
    writer.push(9, 16); // complete AATF frame length in bytes
    writer.push(0x5a, 8); // aatf_error_check()
    writer.push(2, 4); // channel_number
    writer.push(1, 2); // 16-bit resolution
    writer.push(0, 2); // byte_alignment()
    writer.push(0xde, 8); // opaque ll_raw_data_block()
    writer.push(0xa5, 8); // frame_error_check().crc_check
    writer.bytes
}

fn decoder() -> Avs3Decoder {
    Avs3Decoder::new(&Av3aSampleEntry {
        sample_rate: 48_000,
        channels: 2,
        sample_size_bits: Some(16),
        decoder_config: Vec::new(),
    })
    .unwrap()
}

#[test]
fn lossless_decoder_validates_envelope_before_unsupported_gate() {
    let packet = synthetic_lossless_frame();
    let mut decoder = decoder();
    let mut output = AudioFrame::default();

    assert_eq!(
        decoder.decode_packet(&packet, &mut output),
        Err(CodecError::Unsupported(
            "AVS3-P3 ll_raw_data_block lossless synthesis is not implemented yet"
        ))
    );
    assert_eq!(decoder.last_lossless_frame_crc(), Some(0xa5));
    assert_eq!(decoder.last_lossless_payload_bytes(), Some(1));
    assert_eq!(decoder.packets_seen(), 1);
}

#[test]
fn malformed_lossless_frame_is_rejected_before_unsupported_gate() {
    let mut packet = synthetic_lossless_frame();
    packet.push(0);
    let mut decoder = decoder();
    let mut output = AudioFrame::default();

    assert_eq!(
        decoder.decode_packet(&packet, &mut output),
        Err(CodecError::InvalidData(
            "AATF packet length does not match raw_frame_length"
        ))
    );
    assert_eq!(decoder.last_lossless_frame_crc(), None);
    assert_eq!(decoder.last_lossless_payload_bytes(), None);
    assert_eq!(decoder.packets_seen(), 0);
}
