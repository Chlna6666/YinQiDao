use yinqidao_codec_core::CodecError;

use crate::{MultichannelSideInfo, stereo_synthesis::inverse_ms_pair};

const MDCT_LINES: usize = 1024;

/// GY/T 363-2023 Annex-B mcILD scalar codebook.
///
/// The published Annex labels this table B.24. Section 7.6.3.3 refers to B.26 in prose, but the
/// actual B.26 in the same Annex is a TNS Huffman table; the 30 values below are the explicit mcILD
/// table immediately following the neural-network parameters. Five-bit indices 30 and 31 are
/// therefore reserved/invalid.
pub const MC_ILD_CODEBOOK: [f32; 30] = [
    f32::from_bits(0x3FE3_8E39), // 16/9
    f32::from_bits(0x3F40_0000), // 3/4
    f32::from_bits(0x3F10_0000), // 9/16
    f32::from_bits(0x404C_CCCD), // 16/5
    f32::from_bits(0x40AA_AAAB), // 16/3
    f32::from_bits(0x3F50_0000), // 13/16
    f32::from_bits(0x3F88_8889), // 16/15
    f32::from_bits(0x4080_0000), // 4
    f32::from_bits(0x3E40_0000), // 3/16
    f32::from_bits(0x3F92_4925), // 8/7
    f32::from_bits(0x3EE0_0000), // 7/16
    f32::from_bits(0x3FBA_2E8C), // 16/11
    f32::from_bits(0x3E00_0000), // 1/8
    f32::from_bits(0x3F20_0000), // 5/8
    f32::from_bits(0x4012_4925), // 16/7
    f32::from_bits(0x3F00_0000), // 1/2
    f32::from_bits(0x4180_0000), // 16
    f32::from_bits(0x4000_0000), // 2
    f32::from_bits(0x3F60_0000), // 7/8
    f32::from_bits(0x3E80_0000), // 1/4
    f32::from_bits(0x3FAA_AAAB), // 4/3
    f32::from_bits(0x3EC0_0000), // 3/8
    f32::from_bits(0x3FCC_CCCD), // 8/5
    f32::from_bits(0x4100_0000), // 8
    f32::from_bits(0x3F30_0000), // 11/16
    f32::from_bits(0x3D80_0000), // 1/16
    f32::from_bits(0x3F9D_89D9), // 16/13
    f32::from_bits(0x3EA0_0000), // 5/16
    f32::from_bits(0x3F70_0000), // 15/16
    f32::from_bits(0x402A_AAAB), // 8/3
];

#[inline]
pub fn mc_ild_factor(index: u8) -> Result<f32, CodecError> {
    MC_ILD_CODEBOOK
        .get(usize::from(index))
        .copied()
        .ok_or(CodecError::InvalidData("mcILD index exceeds the Annex-B codebook"))
}

/// Resolve `channelPairIndex` as the row-major sequence number of the upper-triangular channel-pair
/// matrix with the main diagonal omitted: (0,1), (0,2), ..., (1,2), ... .
pub fn resolve_multichannel_pair_index(
    couple_ch_num: u16,
    pair_index: u16,
) -> Result<(usize, usize), CodecError> {
    if couple_ch_num < 2 {
        return Err(CodecError::InvalidData(
            "multichannel pair resolution requires at least two coupled channels",
        ));
    }
    let total_pairs = u32::from(couple_ch_num)
        .saturating_mul(u32::from(couple_ch_num - 1))
        / 2;
    if u32::from(pair_index) >= total_pairs {
        return Err(CodecError::InvalidData(
            "channelPairIndex exceeds available channel pairs",
        ));
    }

    let mut remaining = usize::from(pair_index);
    let count = usize::from(couple_ch_num);
    for first in 0..count - 1 {
        let row_width = count - first - 1;
        if remaining < row_width {
            return Ok((first, first + 1 + remaining));
        }
        remaining -= row_width;
    }
    Err(CodecError::Internal(
        "channelPairIndex resolution escaped validated upper triangle".into(),
    ))
}

#[inline]
fn coupled_to_coded_channel(
    coupled_index: usize,
    channel_count: usize,
    lfe_index: Option<usize>,
) -> Result<usize, CodecError> {
    if let Some(lfe) = lfe_index {
        if lfe >= channel_count {
            return Err(CodecError::InvalidData(
                "MCAC LFE index exceeds coded channel count",
            ));
        }
        let actual = if coupled_index >= lfe {
            coupled_index + 1
        } else {
            coupled_index
        };
        if actual >= channel_count {
            return Err(CodecError::InvalidData(
                "MCAC coupled channel index exceeds coded channel count",
            ));
        }
        Ok(actual)
    } else if coupled_index < channel_count {
        Ok(coupled_index)
    } else {
        Err(CodecError::InvalidData(
            "MCAC coupled channel index exceeds coded channel count",
        ))
    }
}

fn two_spectra_mut(
    spectra: &mut [[f32; MDCT_LINES]],
    first: usize,
    second: usize,
) -> Result<(&mut [f32; MDCT_LINES], &mut [f32; MDCT_LINES]), CodecError> {
    if first == second || first >= spectra.len() || second >= spectra.len() {
        return Err(CodecError::InvalidData("invalid MCAC coded-channel pair"));
    }
    if first < second {
        let (left, right) = spectra.split_at_mut(second);
        Ok((&mut left[first], &mut right[0]))
    } else {
        let (left, right) = spectra.split_at_mut(first);
        Ok((&mut right[0], &mut left[second]))
    }
}

/// Apply AVS3 multi-channel M/S upmixing and per-channel inverse mcILD adjustment in place.
///
/// `spectra` contains one already inverse-QC and inverse-grouped 1024-line spectrum per coded
/// channel, including LFE when present. Pair indices address the logical non-LFE channel list; this
/// function maps them back to coded-channel positions before upmixing. Pairing is required to be
/// disjoint so reconstruction is independent of pair iteration order.
pub fn apply_multichannel_mcac(
    side: &MultichannelSideInfo,
    lfe_index: Option<usize>,
    spectra: &mut [[f32; MDCT_LINES]],
) -> Result<(), CodecError> {
    let channel_count = spectra.len();
    if channel_count < 3 {
        return Err(CodecError::InvalidData(
            "MCAC requires at least three coded channels",
        ));
    }
    let couple_ch_num = channel_count
        .checked_sub(usize::from(lfe_index.is_some()))
        .ok_or(CodecError::InvalidData("invalid MCAC coupled-channel count"))?;
    if side.silence_flags.len() != couple_ch_num {
        return Err(CodecError::InvalidData(
            "MCAC silence-flag count does not match non-LFE channels",
        ));
    }
    if side.pairs.len() != usize::from(side.pair_count) {
        return Err(CodecError::InvalidData(
            "MCAC pair list length does not match pairCnt",
        ));
    }

    for (pair_pos, pair) in side.pairs.iter().enumerate() {
        let (logical_first, logical_second) =
            resolve_multichannel_pair_index(couple_ch_num as u16, pair.pair_index)?;
        if side.silence_flags[logical_first] || side.silence_flags[logical_second] {
            return Err(CodecError::InvalidData(
                "MCAC channel pair references a silent channel",
            ));
        }

        for previous in &side.pairs[..pair_pos] {
            let (previous_first, previous_second) =
                resolve_multichannel_pair_index(couple_ch_num as u16, previous.pair_index)?;
            if logical_first == previous_first
                || logical_first == previous_second
                || logical_second == previous_first
                || logical_second == previous_second
            {
                return Err(CodecError::InvalidData(
                    "MCAC channel pairs must be disjoint",
                ));
            }
        }

        let coded_first =
            coupled_to_coded_channel(logical_first, channel_count, lfe_index)?;
        let coded_second =
            coupled_to_coded_channel(logical_second, channel_count, lfe_index)?;
        let first_factor = mc_ild_factor(pair.ild_first)?;
        let second_factor = mc_ild_factor(pair.ild_second)?;
        let (first, second) = two_spectra_mut(spectra, coded_first, coded_second)?;
        inverse_ms_pair(first, second)?;
        for value in first {
            *value *= first_factor;
        }
        for value in second {
            *value *= second_factor;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MultichannelPairSideInfo;

    #[test]
    fn pair_index_walks_upper_triangle_in_row_major_order() {
        assert_eq!(resolve_multichannel_pair_index(5, 0).unwrap(), (0, 1));
        assert_eq!(resolve_multichannel_pair_index(5, 3).unwrap(), (0, 4));
        assert_eq!(resolve_multichannel_pair_index(5, 4).unwrap(), (1, 2));
        assert_eq!(resolve_multichannel_pair_index(5, 9).unwrap(), (3, 4));
        assert!(resolve_multichannel_pair_index(5, 10).is_err());
    }

    #[test]
    fn mcild_codebook_reserves_two_unused_five_bit_values() {
        assert_eq!(MC_ILD_CODEBOOK.len(), 30);
        assert_eq!(mc_ild_factor(16).unwrap(), 16.0);
        assert_eq!(mc_ild_factor(25).unwrap(), 0.0625);
        assert!(mc_ild_factor(30).is_err());
        assert!(mc_ild_factor(31).is_err());
    }

    #[test]
    fn mcac_maps_non_lfe_pair_and_applies_independent_ild() {
        let mut spectra = [[0.0_f32; MDCT_LINES]; 4];
        spectra[0][0] = 1.0; // M
        spectra[3][0] = 1.0; // S; logical non-LFE channel 2 because channel 2 is LFE
        let side = MultichannelSideInfo {
            has_silence: false,
            silence_flags: vec![false; 3],
            pair_count: 1,
            pair_index_bits: 2,
            pairs: vec![MultichannelPairSideInfo {
                pair_index: 1, // logical pair (0,2) -> coded pair (0,3)
                ild_first: 15, // 0.5
                ild_second: 17, // 2.0
            }],
            channel_bit_ratios: vec![Some(21), Some(21), Some(22)],
            next_bit_offset: 0,
        };
        apply_multichannel_mcac(&side, Some(2), &mut spectra).unwrap();
        assert!((spectra[0][0] - 0.5 * 2.0_f32.sqrt()).abs() < 1.0e-6);
        assert!(spectra[3][0].abs() < 1.0e-6);
        assert_eq!(spectra[2][0], 0.0); // LFE untouched
    }

    #[test]
    fn overlapping_pairs_are_rejected_before_order_dependent_reconstruction() {
        let mut spectra = [[0.0_f32; MDCT_LINES]; 4];
        let side = MultichannelSideInfo {
            has_silence: false,
            silence_flags: vec![false; 4],
            pair_count: 2,
            pair_index_bits: 3,
            pairs: vec![
                MultichannelPairSideInfo {
                    pair_index: 0, // (0,1)
                    ild_first: 15,
                    ild_second: 15,
                },
                MultichannelPairSideInfo {
                    pair_index: 1, // (0,2)
                    ild_first: 15,
                    ild_second: 15,
                },
            ],
            channel_bit_ratios: vec![Some(16); 4],
            next_bit_offset: 0,
        };
        assert!(apply_multichannel_mcac(&side, None, &mut spectra).is_err());
    }
}
