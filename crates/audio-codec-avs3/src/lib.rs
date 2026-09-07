//! Pure-Rust AVS3-P3 / AV3A codec work for YinQiDao.
//!
//! The crate is deliberately split from the player so container parsing, bitstream syntax,
//! synthesis and future object/HOA rendering can evolve independently and be tested on desktop
//! and mobile targets. No FFmpeg process or native codec DLL is required by this crate.

mod bitreader;
mod container;
mod decoder;
mod synthesis;

pub use container::{Av3aSampleEntry, probe_av3a_bytes, probe_av3a_path};
pub use decoder::Avs3Decoder;
pub use synthesis::{
    apply_window_in_place, overlap_add, scale_pcm_in_place, spectral_dot,
};
