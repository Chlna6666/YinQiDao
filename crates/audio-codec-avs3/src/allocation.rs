use yinqidao_codec_core::CodecError;

use crate::{GroupSideInfo, MultichannelSideInfo, NeuralNetworkType, qc_fixed_header_bits};

const SAFE_CHANNEL_BYTES: usize = 8;

/// Interoperability ceiling used by the published UWA/Huawei AVS3 reference codec.
///
/// `BIT_FRAME_MAX` is defined there as `256000 * FRAME_LEN / 48000` bits. With the normative
/// 48-kHz 1024-sample frame this becomes 5461 bits, and the multichannel allocator applies the
/// integer byte ceiling `BIT_FRAME_MAX / 8 = 682`. GY/T 363-2023 requires redistribution above a
/// single-channel upper limit but does not publish the numeric value, so this is deliberately
/// labelled as an interoperability parameter rather than an Annex constant.
pub const MC_CHANNEL_MAX_BYTES: usize = (256_000usize * 1024 / 48_000) / 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McBitAllocation {
    pub channel_bytes: Vec<usize>,
    pub available_bytes: usize,
    pub qc_header_bits: usize,
    pub trailing_bits: usize,
    pub lfe_index: Option<usize>,
    pub lfe_bytes: Option<usize>,
}

/// Fixed LFE allocation from section 7.6.3.2 using exact rational channel-pair bitrate.
pub fn lfe_allocation_bytes(
    total_bitrate_kbps: u32,
    non_lfe_channels: usize,
) -> Result<usize, CodecError> {
    if non_lfe_channels == 0 {
        return Err(CodecError::InvalidData(
            "LFE bit allocation requires non-LFE channels",
        ));
    }
    let numerator = u64::from(total_bitrate_kbps).saturating_mul(2);
    let denominator = non_lfe_channels as u64;
    Ok(if numerator < 64 * denominator {
        10
    } else if numerator < 96 * denominator {
        15
    } else {
        20
    })
}

/// Redistribute bytes above the reference-codec single-channel ceiling.
///
/// Only active non-LFE channels participate. A capped channel is removed from the Q6 ratio sum and
/// its excess is distributed proportionally to the remaining active channels. Integer remainder
/// follows the reference allocator and is assigned to the channel that had the largest allocation
/// after the first ratio pass. The outer loop then handles any newly over-limit uncapped channel.
fn apply_reference_channel_cap(
    channel_bytes: &mut [usize],
    active: &[(usize, u8)],
    most_bits_channel: usize,
) -> Result<(), CodecError> {
    if active.is_empty() {
        return Ok(());
    }

    let mut used = vec![false; channel_bytes.len()];
    let mut ratio_sum: usize = active.iter().map(|(_, ratio)| usize::from(*ratio)).sum();

    for _ in 0..active.len() {
        let Some(&(capped_channel, capped_ratio)) = active
            .iter()
            .find(|(channel, _)| channel_bytes[*channel] > MC_CHANNEL_MAX_BYTES && !used[*channel])
        else {
            break;
        };

        let excess = channel_bytes[capped_channel] - MC_CHANNEL_MAX_BYTES;
        channel_bytes[capped_channel] = MC_CHANNEL_MAX_BYTES;
        used[capped_channel] = true;
        ratio_sum =
            ratio_sum
                .checked_sub(usize::from(capped_ratio))
                .ok_or(CodecError::Internal(
                    "multichannel cap ratio sum underflow".into(),
                ))?;

        if excess == 0 {
            continue;
        }
        if ratio_sum == 0 {
            return Err(CodecError::InvalidData(
                "multichannel payload cannot satisfy the per-channel byte ceiling",
            ));
        }

        let mut distributed = 0usize;
        for &(channel, ratio) in active {
            if used[channel] {
                continue;
            }
            let extra = excess.saturating_mul(usize::from(ratio)) / ratio_sum;
            channel_bytes[channel] = channel_bytes[channel].saturating_add(extra);
            distributed = distributed.saturating_add(extra);
        }
        if distributed > excess {
            return Err(CodecError::Internal(
                "multichannel cap redistribution exceeded its byte pool".into(),
            ));
        }

        // Keep reference-codec integer remainder behaviour for bitstream interoperability.
        channel_bytes[most_bits_channel] =
            channel_bytes[most_bits_channel].saturating_add(excess - distributed);
    }

    Ok(())
}

/// Perform multichannel allocation through section 7.6.3.2 steps 1..5.
///
/// `remaining_payload_bits` starts immediately after `DecodeMcSideBits()`. Future fixed QC fields
/// are removed before byte allocation. Eight safe bytes are reserved for every non-LFE channel;
/// LFE receives its bitrate-dependent fixed allocation. Remaining bytes are distributed twice with
/// the transmitted Q6 ratios, integer remainder goes to the largest first-pass allocation, then
/// the published reference codec's 682-byte single-channel ceiling is applied with proportional
/// redistribution. This last numeric ceiling is an interoperability parameter because the standard
/// text specifies the redistribution step but omits the ceiling value.
pub fn allocate_multichannel_bytes(
    remaining_payload_bits: usize,
    nn_type: NeuralNetworkType,
    groups: &[GroupSideInfo],
    side_info: &MultichannelSideInfo,
    total_bitrate_kbps: u32,
    lfe_index: Option<usize>,
) -> Result<McBitAllocation, CodecError> {
    let channel_count = groups.len();
    if channel_count < 3 {
        return Err(CodecError::InvalidData(
            "McBitsAllocation requires at least three coded channels",
        ));
    }
    if let Some(index) = lfe_index
        && index >= channel_count
    {
        return Err(CodecError::InvalidData(
            "LFE channel index exceeds coded channel count",
        ));
    }

    let coupled_indices: Vec<usize> = (0..channel_count)
        .filter(|index| Some(*index) != lfe_index)
        .collect();
    if side_info.silence_flags.len() != coupled_indices.len()
        || side_info.channel_bit_ratios.len() != coupled_indices.len()
    {
        return Err(CodecError::InvalidData(
            "DecodeMcSideBits channel count does not match coded multichannel layout",
        ));
    }

    let qc_header_bits = groups.iter().try_fold(0_usize, |sum, group| {
        let bits = qc_fixed_header_bits(nn_type, group.num_groups)?;
        sum.checked_add(bits).ok_or(CodecError::Truncated)
    })?;
    let range_and_padding_bits = remaining_payload_bits
        .checked_sub(qc_header_bits)
        .ok_or(CodecError::Truncated)?;
    let available_bytes = range_and_padding_bits / 8;
    let trailing_bits = range_and_padding_bits & 7;

    let mut channel_bytes = vec![0_usize; channel_count];
    let mut fixed_bytes = 0_usize;
    for &channel in &coupled_indices {
        channel_bytes[channel] = SAFE_CHANNEL_BYTES;
        fixed_bytes = fixed_bytes
            .checked_add(SAFE_CHANNEL_BYTES)
            .ok_or(CodecError::Truncated)?;
    }

    let lfe_bytes = if let Some(index) = lfe_index {
        let bytes = lfe_allocation_bytes(total_bitrate_kbps, coupled_indices.len())?;
        channel_bytes[index] = bytes;
        fixed_bytes = fixed_bytes
            .checked_add(bytes)
            .ok_or(CodecError::Truncated)?;
        Some(bytes)
    } else {
        None
    };

    let mut pool = available_bytes
        .checked_sub(fixed_bytes)
        .ok_or(CodecError::InvalidData(
            "multichannel payload is smaller than mandatory safe/LFE byte allocation",
        ))?;

    let active: Vec<(usize, u8)> = coupled_indices
        .iter()
        .copied()
        .zip(side_info.silence_flags.iter().copied())
        .zip(side_info.channel_bit_ratios.iter().copied())
        .filter_map(|((channel, silent), ratio)| {
            if silent {
                None
            } else {
                ratio.map(|ratio| (channel, ratio))
            }
        })
        .collect();
    let ratio_sum: usize = active.iter().map(|(_, ratio)| usize::from(*ratio)).sum();
    if ratio_sum > 64 {
        return Err(CodecError::InvalidData(
            "multichannel chBitRatios sum exceeds Q6 unity",
        ));
    }

    let mut most_bits_channel = active.first().map(|(channel, _)| *channel);
    if !active.is_empty() {
        // First proportional pass uses the whole post-safe-byte pool.
        let first_pool = pool;
        let mut distributed = 0_usize;
        let mut largest_first_pass = 0usize;
        for &(channel, ratio) in &active {
            let extra = first_pool.saturating_mul(usize::from(ratio)) / 64;
            channel_bytes[channel] = channel_bytes[channel].saturating_add(extra);
            distributed = distributed.saturating_add(extra);
            if channel_bytes[channel] > largest_first_pass {
                largest_first_pass = channel_bytes[channel];
                most_bits_channel = Some(channel);
            }
        }
        if distributed > pool {
            return Err(CodecError::InvalidData(
                "multichannel first-pass allocation exceeds available byte pool",
            ));
        }
        pool -= distributed;

        // Reference allocator applies the same Q6 ratios to the first-pass integer remainder.
        let second_pool = pool;
        let mut distributed_second = 0_usize;
        for &(channel, ratio) in &active {
            let extra = second_pool.saturating_mul(usize::from(ratio)) / 64;
            channel_bytes[channel] = channel_bytes[channel].saturating_add(extra);
            distributed_second = distributed_second.saturating_add(extra);
        }
        if distributed_second > pool {
            return Err(CodecError::InvalidData(
                "multichannel second-pass allocation exceeds available byte pool",
            ));
        }
        pool -= distributed_second;

        if pool != 0 {
            let target = most_bits_channel.expect("active channel list checked non-empty");
            channel_bytes[target] = channel_bytes[target].saturating_add(pool);
            pool = 0;
        }
    }

    if pool != 0 {
        return Err(CodecError::InvalidData(
            "multichannel frame has allocatable bytes but no active non-LFE channel",
        ));
    }

    if let Some(most_bits_channel) = most_bits_channel {
        apply_reference_channel_cap(&mut channel_bytes, &active, most_bits_channel)?;
    }

    if channel_bytes.iter().sum::<usize>() != available_bytes {
        return Err(CodecError::Internal(
            "McBitsAllocation did not conserve available bytes".into(),
        ));
    }

    Ok(McBitAllocation {
        channel_bytes,
        available_bytes,
        qc_header_bits,
        trailing_bits,
        lfe_index,
        lfe_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(count: usize) -> Vec<GroupSideInfo> {
        (0..count)
            .map(|_| GroupSideInfo {
                num_groups: 1,
                group_indicator: [false; 8],
                next_bit_offset: 0,
            })
            .collect()
    }

    fn side(ratios: &[u8]) -> MultichannelSideInfo {
        MultichannelSideInfo {
            has_silence: false,
            silence_flags: vec![false; ratios.len()],
            pair_count: 0,
            pair_index_bits: 0,
            pairs: Vec::new(),
            channel_bit_ratios: ratios.iter().copied().map(Some).collect(),
            next_bit_offset: 0,
        }
    }

    #[test]
    fn reference_single_channel_ceiling_is_682_bytes() {
        assert_eq!(MC_CHANNEL_MAX_BYTES, 682);
    }

    #[test]
    fn lfe_thresholds_use_exact_channel_pair_rate() {
        assert_eq!(lfe_allocation_bytes(300, 10).unwrap(), 10);
        assert_eq!(lfe_allocation_bytes(320, 10).unwrap(), 15);
        assert_eq!(lfe_allocation_bytes(480, 10).unwrap(), 20);
    }

    #[test]
    fn allocates_5_1_and_conserves_all_whole_range_bytes() {
        let groups = groups(6);
        let side = side(&[13, 13, 13, 13, 12]);
        let qc_bits = groups
            .iter()
            .map(|group| qc_fixed_header_bits(NeuralNetworkType::Basic, group.num_groups).unwrap())
            .sum::<usize>();
        let range_bytes = 200;
        let allocation = allocate_multichannel_bytes(
            qc_bits + range_bytes * 8,
            NeuralNetworkType::Basic,
            &groups,
            &side,
            384,
            Some(3),
        )
        .unwrap();
        assert_eq!(allocation.available_bytes, range_bytes);
        assert_eq!(allocation.channel_bytes.iter().sum::<usize>(), range_bytes);
        assert_eq!(allocation.lfe_bytes, Some(20));
        assert_eq!(allocation.channel_bytes[3], 20);
        assert_eq!(allocation.trailing_bits, 0);
    }

    #[test]
    fn cap_redistribution_matches_reference_allocator_geometry() {
        let groups = groups(3);
        let side = side(&[40, 12, 12]);
        let qc_bits = groups.len() * 19;
        let allocation = allocate_multichannel_bytes(
            qc_bits + 1_200 * 8,
            NeuralNetworkType::Basic,
            &groups,
            &side,
            768,
            None,
        )
        .unwrap();
        assert_eq!(allocation.channel_bytes, vec![682, 259, 259]);
        assert_eq!(allocation.channel_bytes.iter().sum::<usize>(), 1_200);
    }

    #[test]
    fn silent_channels_keep_only_safe_allocation() {
        let groups = groups(4);
        let mut side = side(&[32, 0, 32]);
        side.has_silence = true;
        side.silence_flags = vec![false, true, false];
        side.channel_bit_ratios = vec![Some(32), None, Some(32)];
        let qc_bits = groups.len() * 19;
        let allocation = allocate_multichannel_bytes(
            qc_bits + 100 * 8,
            NeuralNetworkType::Basic,
            &groups,
            &side,
            256,
            Some(3),
        )
        .unwrap();
        assert_eq!(allocation.channel_bytes[1], SAFE_CHANNEL_BYTES);
        assert_eq!(allocation.channel_bytes.iter().sum::<usize>(), 100);
    }

    #[test]
    fn rejects_ratio_sum_above_q6_unity() {
        let groups = groups(3);
        let side = side(&[40, 40, 0]);
        let qc_bits = groups.len() * 19;
        assert!(matches!(
            allocate_multichannel_bytes(
                qc_bits + 100 * 8,
                NeuralNetworkType::Basic,
                &groups,
                &side,
                192,
                None,
            ),
            Err(CodecError::InvalidData(_))
        ));
    }

    #[test]
    fn rejects_missing_mandatory_safe_bytes() {
        let groups = groups(3);
        let side = side(&[32, 32]);
        let qc_bits = groups.len() * 19;
        assert!(matches!(
            allocate_multichannel_bytes(
                qc_bits + 4 * 8,
                NeuralNetworkType::Basic,
                &groups,
                &side,
                128,
                Some(2),
            ),
            Err(CodecError::InvalidData(_))
        ));
    }
}
