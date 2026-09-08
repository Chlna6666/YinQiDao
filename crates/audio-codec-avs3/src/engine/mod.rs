//! Complete reference-verified AVS3-P3 synthesis and decoding engine.

pub mod bitstream;
pub mod builtin_decoder;
pub mod builtin_model;
pub mod bwe;
pub mod cnn;
pub mod core_side;
pub mod decoder;
pub mod error;
pub mod fd_shaping;
pub mod feature_scale_tables;
pub mod header;
pub mod hoa_backend;
pub mod hoa_core;
pub mod hoa_side;
pub mod hoa_synthesis;
pub mod imdct;
pub mod latent;
pub mod mc_backend;
pub mod mc_core;
pub mod mc_side;
pub mod mcr;
pub mod mdct;
pub mod mdct_synthesis;
pub mod metadata;
pub mod metadata_values;
pub mod mix_backend;
pub mod model;
pub mod mono_backend;
pub mod mono_core;
pub mod neural_qc;
pub mod random;
pub mod range_coder;
pub mod spectrum;
pub mod stereo_backend;
pub mod stereo_core;
pub mod stereo_side;
pub mod stream;
pub mod tns;

pub use builtin_decoder::BuiltinDecoder;
pub use decoder::{AudioFrame, Decoder, DecoderConfig, FLOAT_FULL_SCALE};
pub use error::DecodeError;
pub use header::{FrameHeader, parse_header};
pub use stream::{EncodedFrame, FrameStream, StreamEvent, parse_frames};

/// Calculate the CRC used by the AVS3 reference implementation.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xffff_u16;
    for &byte in data {
        let table_index = (crc >> 8) as u8;
        let mut table_value = u16::from(table_index) << 8;
        for _ in 0..8 {
            table_value = if table_value & 0x8000 != 0 {
                (table_value << 1) ^ 0x1021
            } else {
                table_value << 1
            };
        }
        crc = (crc << 8) | u16::from(byte);
        crc ^= table_value;
    }
    crc
}
