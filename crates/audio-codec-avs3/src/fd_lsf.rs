use yinqidao_codec_core::CodecError;

use crate::FdShapingSideInfo;

pub const LSF_ORDER: usize = 16;
pub const FD_SHAPING_SUBBANDS: usize = 49;
pub const FD_SHAPING_SFB_BOUNDARIES: [u16; FD_SHAPING_SUBBANDS + 1] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160,
    176, 196, 216, 240, 264, 292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704,
    736, 768, 800, 832, 864, 896, 928, 1024,
];

/// GY/T 363-2023 Annex B table B.46.
///
/// The published decimal values are materialized as binary32 constants once in source so the
/// decoder never parses text or allocates model data at runtime.
pub const LSF_MEAN: [f32; LSF_ORDER] = [
    f32::from_bits(0x444C_20B4),
    f32::from_bits(0x450B_7D3A),
    f32::from_bits(0x4563_F247),
    f32::from_bits(0x459E_33AA),
    f32::from_bits(0x45CA_6E31),
    f32::from_bits(0x45F6_A8B7),
    f32::from_bits(0x4611_719F),
    f32::from_bits(0x4627_8EE2),
    f32::from_bits(0x463D_AC25),
    f32::from_bits(0x4653_C969),
    f32::from_bits(0x4669_E6AC),
    f32::from_bits(0x4680_01F7),
    f32::from_bits(0x468B_1099),
    f32::from_bits(0x4696_1F3B),
    f32::from_bits(0x46A1_2DDC),
    f32::from_bits(0x46AC_3C7E),
];

const LSF_MIN_GAP_HZ: f32 = 50.0;
const LSF_NYQUIST_HZ: f32 = 24_000.0;

#[derive(Clone, Copy, Debug)]
pub struct LsfCodebook<'a> {
    pub values: &'a [f32],
}

#[derive(Clone, Copy, Debug)]
pub struct LsfCodebooks<'a> {
    pub high_stage1: [LsfCodebook<'a>; 2],
    pub high_stage2: [LsfCodebook<'a>; 5],
    pub low_stage1: [LsfCodebook<'a>; 2],
    pub low_stage2: [LsfCodebook<'a>; 3],
}

#[derive(Clone, Copy, Debug)]
struct SubvectorSpec {
    start: usize,
    dimension: usize,
    entries: usize,
}

const STAGE1_SPECS: [SubvectorSpec; 2] = [
    SubvectorSpec {
        start: 0,
        dimension: 9,
        entries: 256,
    },
    SubvectorSpec {
        start: 9,
        dimension: 7,
        entries: 256,
    },
];
const HIGH_STAGE2_SPECS: [SubvectorSpec; 5] = [
    SubvectorSpec {
        start: 0,
        dimension: 3,
        entries: 128,
    },
    SubvectorSpec {
        start: 3,
        dimension: 3,
        entries: 128,
    },
    SubvectorSpec {
        start: 6,
        dimension: 3,
        entries: 64,
    },
    SubvectorSpec {
        start: 9,
        dimension: 3,
        entries: 32,
    },
    SubvectorSpec {
        start: 12,
        dimension: 4,
        entries: 32,
    },
];
const LOW_STAGE2_SPECS: [SubvectorSpec; 3] = [
    SubvectorSpec {
        start: 0,
        dimension: 5,
        entries: 128,
    },
    SubvectorSpec {
        start: 5,
        dimension: 4,
        entries: 128,
    },
    SubvectorSpec {
        start: 9,
        dimension: 7,
        entries: 64,
    },
];

fn codeword<'a>(
    table: LsfCodebook<'a>,
    spec: SubvectorSpec,
    index: u8,
) -> Result<&'a [f32], CodecError> {
    let expected = spec
        .entries
        .checked_mul(spec.dimension)
        .ok_or(CodecError::InvalidData(
            "LSF codebook geometry overflows address space",
        ))?;
    if table.values.len() != expected {
        return Err(CodecError::InvalidData(
            "LSF codebook length does not match Annex B geometry",
        ));
    }
    if table.values.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "LSF codebook contains a non-finite value",
        ));
    }
    let index = usize::from(index);
    if index >= spec.entries {
        return Err(CodecError::InvalidData(
            "LSF VQ index exceeds codebook size",
        ));
    }
    let start = index * spec.dimension;
    Ok(&table.values[start..start + spec.dimension])
}

fn require_index(side: &FdShapingSideInfo, slot: usize) -> Result<u8, CodecError> {
    side.lsf_vq_indices
        .get(slot)
        .copied()
        .flatten()
        .ok_or(CodecError::InvalidData(
            "frequency shaping is missing an LSF VQ index",
        ))
}

fn decode_stage1(
    side: &FdShapingSideInfo,
    tables: [LsfCodebook<'_>; 2],
    output: &mut [f32; LSF_ORDER],
) -> Result<(), CodecError> {
    for (slot, spec) in STAGE1_SPECS.iter().copied().enumerate() {
        let vector = codeword(tables[slot], spec, require_index(side, slot)?)?;
        output[spec.start..spec.start + spec.dimension].copy_from_slice(vector);
    }
    Ok(())
}

fn add_stage2<const N: usize>(
    side: &FdShapingSideInfo,
    index_offset: usize,
    tables: [LsfCodebook<'_>; N],
    specs: [SubvectorSpec; N],
    output: &mut [f32; LSF_ORDER],
) -> Result<(), CodecError> {
    for slot in 0..N {
        let spec = specs[slot];
        let vector = codeword(
            tables[slot],
            spec,
            require_index(side, index_offset + slot)?,
        )?;
        for (destination, residual) in output[spec.start..spec.start + spec.dimension]
            .iter_mut()
            .zip(vector)
        {
            *destination += *residual;
        }
    }
    Ok(())
}

/// Apply the AVS3 decoder LSF ordering guard before LSF -> LSP conversion.
///
/// The normative/reference decoder performs two ordered passes: first enforce the 50-Hz minimum
/// gap from low to high frequency, then constrain the top coefficient to Nyquist-50 Hz and
/// propagate the same gap backwards. Keeping both passes is required for bitstream interoperability
/// on edge codewords.
fn stabilize_lsf(lsf: &mut [f32; LSF_ORDER]) {
    let mut minimum = LSF_MIN_GAP_HZ;
    for value in lsf.iter_mut() {
        if *value < minimum {
            *value = minimum;
        }
        minimum = *value + LSF_MIN_GAP_HZ;
    }

    let mut maximum = LSF_NYQUIST_HZ - LSF_MIN_GAP_HZ;
    for value in lsf.iter_mut().rev() {
        if *value > maximum {
            *value = maximum;
        }
        maximum = *value - LSF_MIN_GAP_HZ;
    }
}

/// Decode the 16-dimensional LSF vector used by inverse frequency-domain spectrum shaping.
///
/// This is a direct, allocation-free implementation of the two-stage split-VQ geometry in
/// section 7.11.3.2. Table lookup is O(1): the transmitted indices directly address the selected
/// Annex-B codewords; no decoder-side vector search is performed.
pub fn dequantize_lsf(
    side: &FdShapingSideInfo,
    codebooks: LsfCodebooks<'_>,
    output: &mut [f32; LSF_ORDER],
) -> Result<(), CodecError> {
    output.fill(0.0);
    if side.low_bitrate_precision {
        decode_stage1(side, codebooks.low_stage1, output)?;
        add_stage2(side, 2, codebooks.low_stage2, LOW_STAGE2_SPECS, output)?;
    } else {
        decode_stage1(side, codebooks.high_stage1, output)?;
        add_stage2(side, 2, codebooks.high_stage2, HIGH_STAGE2_SPECS, output)?;
    }

    for (value, mean) in output.iter_mut().zip(LSF_MEAN) {
        *value += mean;
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "inverse-quantized LSF contains a non-finite value",
        ));
    }

    stabilize_lsf(output);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: usize, dimension: usize, base: f32) -> Vec<f32> {
        let mut values = vec![0.0; entries * dimension];
        for entry in 0..entries {
            for dim in 0..dimension {
                values[entry * dimension + dim] = base + entry as f32 * 10.0 + dim as f32;
            }
        }
        values
    }

    #[test]
    fn annex_b46_and_b47_shapes_are_exact() {
        assert_eq!(LSF_MEAN.len(), 16);
        assert_eq!(FD_SHAPING_SFB_BOUNDARIES.len(), 50);
        assert_eq!(FD_SHAPING_SFB_BOUNDARIES[0], 0);
        assert_eq!(FD_SHAPING_SFB_BOUNDARIES[49], 1024);
        assert!(
            FD_SHAPING_SFB_BOUNDARIES
                .windows(2)
                .all(|pair| pair[0] < pair[1])
        );
        assert_eq!(LSF_MEAN[0].to_bits(), 0x444C_20B4);
        assert_eq!(LSF_MEAN[15].to_bits(), 0x46AC_3C7E);
    }

    #[test]
    fn lsf_stabilization_matches_two_pass_decoder_semantics() {
        let mut lsf = [25_000.0_f32; LSF_ORDER];
        stabilize_lsf(&mut lsf);
        assert_eq!(lsf[LSF_ORDER - 1], 23_950.0);
        assert_eq!(lsf[0], 23_200.0);
        assert!(
            lsf.windows(2)
                .all(|pair| pair[1] - pair[0] >= LSF_MIN_GAP_HZ)
        );
    }

    #[test]
    fn high_precision_split_vq_maps_all_seven_indices_without_search() {
        let h11 = table(256, 9, 1.0);
        let h12 = table(256, 7, 2.0);
        let h21 = table(128, 3, 3.0);
        let h22 = table(128, 3, 4.0);
        let h23 = table(64, 3, 5.0);
        let h24 = table(32, 3, 6.0);
        let h25 = table(32, 4, 7.0);
        let dummy = [0.0_f32; 1];
        let codebooks = LsfCodebooks {
            high_stage1: [LsfCodebook { values: &h11 }, LsfCodebook { values: &h12 }],
            high_stage2: [
                LsfCodebook { values: &h21 },
                LsfCodebook { values: &h22 },
                LsfCodebook { values: &h23 },
                LsfCodebook { values: &h24 },
                LsfCodebook { values: &h25 },
            ],
            low_stage1: [
                LsfCodebook { values: &dummy },
                LsfCodebook { values: &dummy },
            ],
            low_stage2: [LsfCodebook { values: &dummy }; 3],
        };
        let side = FdShapingSideInfo {
            low_bitrate_precision: false,
            lsf_vq_indices: [
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
            ],
            next_bit_offset: 0,
        };
        let mut output = [0.0_f32; LSF_ORDER];
        dequantize_lsf(&side, codebooks, &mut output).unwrap();
        assert!(
            output
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= LSF_MIN_GAP_HZ)
        );
        assert!(output[15] <= LSF_NYQUIST_HZ - LSF_MIN_GAP_HZ);
    }

    #[test]
    fn low_precision_rejects_index_outside_64_entry_final_codebook() {
        let s11 = vec![0.0_f32; 256 * 9];
        let s12 = vec![0.0_f32; 256 * 7];
        let s21 = vec![0.0_f32; 128 * 5];
        let s22 = vec![0.0_f32; 128 * 4];
        let s23 = vec![0.0_f32; 64 * 7];
        let dummy = [0.0_f32; 1];
        let codebooks = LsfCodebooks {
            high_stage1: [LsfCodebook { values: &dummy }; 2],
            high_stage2: [LsfCodebook { values: &dummy }; 5],
            low_stage1: [LsfCodebook { values: &s11 }, LsfCodebook { values: &s12 }],
            low_stage2: [
                LsfCodebook { values: &s21 },
                LsfCodebook { values: &s22 },
                LsfCodebook { values: &s23 },
            ],
        };
        let side = FdShapingSideInfo {
            low_bitrate_precision: true,
            lsf_vq_indices: [Some(0), Some(0), Some(0), Some(0), Some(64), None, None],
            next_bit_offset: 0,
        };
        let mut output = [0.0_f32; LSF_ORDER];
        assert!(dequantize_lsf(&side, codebooks, &mut output).is_err());
    }
}
