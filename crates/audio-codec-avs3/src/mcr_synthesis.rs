use core::mem::size_of;

use yinqidao_codec_core::CodecError;

use crate::{BASE_OUTPUT_POSITIONS, StereoCouplingSideInfo, StereoSideInfo};

pub const MCR_SCALE_FACTOR_BANDS: usize = 18;
pub const MCR_SUBVECTOR_DIMENSIONS: usize = 3;
pub const MCR_SUBVECTORS: usize = MCR_SCALE_FACTOR_BANDS / MCR_SUBVECTOR_DIMENSIONS;
pub const MCR_SUBSPECTRA: usize = 2;
pub const MCR_LONG_CODEBOOK_ENTRIES: usize = 512;
pub const MCR_SHORT_CODEBOOK_ENTRIES: usize = 256;

const MCR_SFB_BORDERS: [usize; MCR_SCALE_FACTOR_BANDS + 1] = [
    0, 4, 8, 12, 16, 22, 28, 34, 40, 48, 56, 64, 76, 88, 100, 116, 132, 154, 176,
];
const ROTATION_VALUES_PER_ANGLE: usize = 2;
const ROTATION_BYTES_PER_ANGLE: usize = ROTATION_VALUES_PER_ANGLE * size_of::<f32>();
const LONG_ANGLES: usize = MCR_LONG_CODEBOOK_ENTRIES * MCR_SUBVECTOR_DIMENSIONS;
const SHORT_ANGLES: usize = MCR_SHORT_CODEBOOK_ENTRIES * MCR_SUBVECTOR_DIMENSIONS;
const TOTAL_ANGLES: usize = LONG_ANGLES + SHORT_ANGLES;

pub const MCR_ROTATION_VALUES: usize = TOTAL_ANGLES * ROTATION_VALUES_PER_ANGLE;
pub const MCR_ROTATION_BYTES_LEN: usize = MCR_ROTATION_VALUES * size_of::<f32>();
pub const MCR_ROTATION_FNV1A: u64 = 0x5b62_aa9a_6b23_145a;

const MCR_ROTATION_BYTES: &[u8; MCR_ROTATION_BYTES_LEN] =
    include_bytes!("../assets/avs3a_mcr_rotations.bin");

/// Allocation-free inverse MCR reconstruction for AVS3 stereo at <=32 kb/s.
///
/// The left input is the single neural-decoded, inverse-grouped coded spectrum. The right spectrum
/// is first cloned from it, then the normative 18-band even/odd inverse rotations reconstruct both
/// output spectra in-place. Lines above the last MCR band (352..1024) remain identical in both
/// channels, matching the reference decoder's `McrDecode()` geometry.
pub fn apply_mcr_stereo_upmix(
    side: StereoSideInfo,
    left: &mut [f32],
    right: &mut [f32],
) -> Result<(), CodecError> {
    if left.len() != BASE_OUTPUT_POSITIONS || right.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "MCR stereo upmix requires two 1024-line MDCT spectra",
        ));
    }

    let StereoCouplingSideInfo::Mcr {
        is_short_window,
        vq_bits,
        even_vq_indices,
        odd_vq_indices,
    } = side.coupling
    else {
        return Err(CodecError::InvalidData(
            "MCR stereo upmix received conventional stereo side information",
        ));
    };

    let (entries, expected_bits) = if is_short_window {
        (MCR_SHORT_CODEBOOK_ENTRIES, 8_u8)
    } else {
        (MCR_LONG_CODEBOOK_ENTRIES, 9_u8)
    };
    if vq_bits != expected_bits {
        return Err(CodecError::InvalidData(
            "MCR VQ width disagrees with the transform window mode",
        ));
    }
    for index in even_vq_indices.into_iter().chain(odd_vq_indices) {
        if usize::from(index) >= entries {
            return Err(CodecError::InvalidData(
                "MCR VQ index exceeds the selected normative codebook",
            ));
        }
    }

    right.copy_from_slice(left);
    let indexes = [even_vq_indices, odd_vq_indices];
    for subspectrum in 0..MCR_SUBSPECTRA {
        for band in 0..MCR_SCALE_FACTOR_BANDS {
            let subvector = band / MCR_SUBVECTOR_DIMENSIONS;
            let dimension = band % MCR_SUBVECTOR_DIMENSIONS;
            let codebook_index = usize::from(indexes[subspectrum][subvector]);
            let (cosine, sine) = rotation(is_short_window, codebook_index, dimension);

            for half_line in MCR_SFB_BORDERS[band]..MCR_SFB_BORDERS[band + 1] {
                let line = half_line * MCR_SUBSPECTRA + subspectrum;
                let value = left[line];
                let cosine_product = cosine * value;
                let sine_product = sine * value;
                left[line] = cosine_product - sine_product;
                right[line] = sine_product + cosine_product;
            }
        }
    }
    Ok(())
}

pub fn mcr_rotation_bytes() -> &'static [u8; MCR_ROTATION_BYTES_LEN] {
    MCR_ROTATION_BYTES
}

#[inline]
fn rotation(short_window: bool, codebook_index: usize, dimension: usize) -> (f32, f32) {
    let angle = if short_window { LONG_ANGLES } else { 0 }
        + codebook_index * MCR_SUBVECTOR_DIMENSIONS
        + dimension;
    debug_assert!(angle < TOTAL_ANGLES);
    let offset = angle * ROTATION_BYTES_PER_ANGLE;
    let cosine = f32::from_le_bytes(
        MCR_ROTATION_BYTES[offset..offset + 4]
            .try_into()
            .expect("validated MCR cosine asset offset"),
    );
    let sine = f32::from_le_bytes(
        MCR_ROTATION_BYTES[offset + 4..offset + 8]
            .try_into()
            .expect("validated MCR sine asset offset"),
    );
    (cosine, sine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        })
    }

    #[test]
    fn normative_rotation_asset_has_expected_geometry_and_fingerprint() {
        assert_eq!(mcr_rotation_bytes().len(), 18_432);
        assert_eq!(fnv1a(mcr_rotation_bytes()), MCR_ROTATION_FNV1A);
        assert_eq!(rotation(false, 0, 0).0.to_bits(), 0x3e05_76da);
        assert_eq!(rotation(false, 511, 2).1.to_bits(), 0xbebb_b7db);
        assert_eq!(rotation(true, 0, 0).0.to_bits(), 0x3e24_a553);
        assert_eq!(rotation(true, 255, 2).1.to_bits(), 0xbf77_a16c);
    }

    #[test]
    fn rejects_non_mcr_side_before_mutating_spectra() {
        let side = StereoSideInfo {
            coupling: StereoCouplingSideInfo::Ms {
                is_ms: false,
                ild_q_idx: None,
                bits_ratio: 0,
            },
            next_bit_offset: 0,
        };
        let mut left = [1.0_f32; BASE_OUTPUT_POSITIONS];
        let mut right = [2.0_f32; BASE_OUTPUT_POSITIONS];
        assert!(apply_mcr_stereo_upmix(side, &mut left, &mut right).is_err());
        assert_eq!(left, [1.0; BASE_OUTPUT_POSITIONS]);
        assert_eq!(right, [2.0; BASE_OUTPUT_POSITIONS]);
    }
}
