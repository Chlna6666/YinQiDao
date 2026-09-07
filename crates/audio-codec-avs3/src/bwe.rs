use yinqidao_codec_core::CodecError;

use crate::bitreader::BitReader;

const MAX_SFB: usize = 6;
const MAX_TILES: usize = 3;
const MAX_SFB_BOUNDARIES: usize = MAX_SFB + 1;
const MAX_TILE_BOUNDARIES: usize = MAX_TILES + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BweMode {
    Mono,
    Stereo,
    /// Multichannel/object path. `non_lfe_channels` is the number of coded signals participating in
    /// the equivalent-stereo bitrate calculation. A channel bed with one LFE subtracts that LFE;
    /// object channels are ordinary full-band signals and remain in the count.
    Multichannel { non_lfe_channels: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WhiteningLevel {
    Off,
    Mid,
    High,
}

/// Tables 28..42 collapsed into the one configuration selected for the current bitrate/mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BweConfig {
    pub mode: BweMode,
    pub num_sfb: u8,
    pub num_tiles: u8,
    /// `num_sfb + 1` MDCT boundaries; unused tail entries are `None`.
    pub sfb_boundaries: [Option<u16>; MAX_SFB_BOUNDARIES],
    /// `num_tiles + 1` target-region boundaries.
    pub target_tiles: [Option<u16>; MAX_TILE_BOUNDARIES],
    /// `num_tiles` source-region starts.
    pub source_tiles: [Option<u16>; MAX_TILES],
    /// `num_tiles + 1` SFB indices mapping target regions to the SFB partition.
    pub target_tile_sfb: [Option<u8>; MAX_TILE_BOUNDARIES],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BweSideInfo {
    pub envelope_indices: [Option<u8>; MAX_SFB],
    pub whitening_levels: [Option<WhiteningLevel>; MAX_TILES],
    pub next_bit_offset: usize,
}

impl BweConfig {
    /// Resolve the normative BWE enable condition and tables for mono/stereo/multichannel modes.
    /// Returns `None` when BWE is disabled by bitrate.
    pub fn for_bitrate(mode: BweMode, total_bitrate_kbps: u32) -> Result<Option<Self>, CodecError> {
        match mode {
            BweMode::Mono => {
                if total_bitrate_kbps > 96 {
                    return Ok(None);
                }
                Ok(Some(mono_config(total_bitrate_kbps)))
            }
            BweMode::Stereo => {
                if total_bitrate_kbps > 128 {
                    return Ok(None);
                }
                Ok(Some(stereo_config(total_bitrate_kbps)))
            }
            BweMode::Multichannel { non_lfe_channels } => {
                if non_lfe_channels == 0 {
                    return Err(CodecError::InvalidData(
                        "multichannel BWE requires at least one non-LFE channel",
                    ));
                }
                // Equivalent stereo rate = total / nonLfeChannels * 2. Keep it rational to avoid
                // threshold changes from integer/floating-point rounding.
                let numerator = u64::from(total_bitrate_kbps).saturating_mul(2);
                let denominator = u64::from(non_lfe_channels);
                if numerator > 128_u64.saturating_mul(denominator) {
                    return Ok(None);
                }
                Ok(Some(multichannel_config(numerator, denominator)))
            }
        }
    }

    pub fn parse_side_info(
        self,
        bytes: &[u8],
        bit_offset: usize,
    ) -> Result<BweSideInfo, CodecError> {
        parse_bwe_side_info_at(bytes, bit_offset, self.num_sfb, self.num_tiles)
    }
}

/// Parse table-27 `DecodeBweSideBits()` once `numSfb` and `numTiles` have been selected by the
/// bitrate configuration tables.
pub fn parse_bwe_side_info_at(
    bytes: &[u8],
    bit_offset: usize,
    num_sfb: u8,
    num_tiles: u8,
) -> Result<BweSideInfo, CodecError> {
    if usize::from(num_sfb) > MAX_SFB || usize::from(num_tiles) > MAX_TILES {
        return Err(CodecError::InvalidData(
            "BWE SFB/tile count exceeds normative tables",
        ));
    }

    let mut reader = BitReader::with_bit_position(bytes, bit_offset)?;
    let mut envelope_indices = [None; MAX_SFB];
    for slot in envelope_indices.iter_mut().take(usize::from(num_sfb)) {
        *slot = Some(reader.read_bits(7)? as u8);
    }

    let mut whitening_levels = [None; MAX_TILES];
    for slot in whitening_levels.iter_mut().take(usize::from(num_tiles)) {
        let enabled = reader.read_bit()?;
        *slot = Some(if !enabled {
            WhiteningLevel::Off
        } else if reader.read_bit()? {
            WhiteningLevel::High
        } else {
            WhiteningLevel::Mid
        });
    }

    Ok(BweSideInfo {
        envelope_indices,
        whitening_levels,
        next_bit_offset: reader.position_bits(),
    })
}

fn mono_config(rate: u32) -> BweConfig {
    if rate <= 32 {
        make_config(
            BweMode::Mono,
            &[352, 416, 480, 544, 608, 672, 768],
            &[352, 480, 608, 768],
            &[64, 96, 144],
            &[0, 2, 4, 6],
        )
    } else if rate <= 56 {
        make_config(
            BweMode::Mono,
            &[448, 496, 544, 608, 672, 736, 832],
            &[448, 544, 672, 832],
            &[96, 144, 192],
            &[0, 2, 4, 6],
        )
    } else if rate <= 72 {
        make_config(
            BweMode::Mono,
            &[544, 608, 672, 736, 832],
            &[544, 672, 832],
            &[144, 192],
            &[0, 2, 4],
        )
    } else {
        make_config(
            BweMode::Mono,
            &[672, 736, 832],
            &[672, 832],
            &[192],
            &[0, 2],
        )
    }
}

fn stereo_config(rate: u32) -> BweConfig {
    if rate <= 64 {
        make_config(
            BweMode::Stereo,
            &[352, 416, 480, 544, 608, 672, 768],
            &[352, 480, 608, 768],
            &[64, 96, 144],
            &[0, 2, 4, 6],
        )
    } else if rate <= 96 {
        make_config(
            BweMode::Stereo,
            &[544, 608, 672, 736, 832],
            &[544, 672, 832],
            &[144, 192],
            &[0, 2, 4],
        )
    } else {
        make_config(
            BweMode::Stereo,
            &[672, 736, 832],
            &[672, 832],
            &[192],
            &[0, 2],
        )
    }
}

fn multichannel_config(rate_num: u64, rate_den: u64) -> BweConfig {
    let mode = BweMode::Multichannel {
        non_lfe_channels: u16::try_from(rate_den).unwrap_or(u16::MAX),
    };
    if rate_num <= 56 * rate_den {
        make_config(
            mode,
            &[352, 400, 448, 512, 576, 672, 768],
            &[352, 448, 576, 768],
            &[64, 96, 144],
            &[0, 2, 4, 6],
        )
    } else if rate_num <= 75 * rate_den {
        make_config(
            mode,
            &[400, 448, 512, 576, 672, 768],
            &[400, 512, 672, 768],
            &[64, 96, 144],
            &[0, 2, 4, 5],
        )
    } else if rate_num <= 108 * rate_den {
        make_config(
            mode,
            &[544, 608, 672, 736, 832],
            &[544, 672, 832],
            &[144, 192],
            &[0, 2, 4],
        )
    } else {
        make_config(
            mode,
            &[672, 736, 832],
            &[672, 832],
            &[192],
            &[0, 2],
        )
    }
}

fn make_config(
    mode: BweMode,
    sfb: &[u16],
    target: &[u16],
    source: &[u16],
    target_sfb: &[u8],
) -> BweConfig {
    debug_assert!(sfb.len() >= 2 && sfb.len() <= MAX_SFB_BOUNDARIES);
    debug_assert!(target.len() >= 2 && target.len() <= MAX_TILE_BOUNDARIES);
    debug_assert_eq!(source.len() + 1, target.len());
    debug_assert_eq!(target_sfb.len(), target.len());

    let mut sfb_boundaries = [None; MAX_SFB_BOUNDARIES];
    let mut target_tiles = [None; MAX_TILE_BOUNDARIES];
    let mut source_tiles = [None; MAX_TILES];
    let mut target_tile_sfb = [None; MAX_TILE_BOUNDARIES];
    for (slot, &value) in sfb_boundaries.iter_mut().zip(sfb) {
        *slot = Some(value);
    }
    for (slot, &value) in target_tiles.iter_mut().zip(target) {
        *slot = Some(value);
    }
    for (slot, &value) in source_tiles.iter_mut().zip(source) {
        *slot = Some(value);
    }
    for (slot, &value) in target_tile_sfb.iter_mut().zip(target_sfb) {
        *slot = Some(value);
    }

    BweConfig {
        mode,
        num_sfb: (sfb.len() - 1) as u8,
        num_tiles: source.len() as u8,
        sfb_boundaries,
        target_tiles,
        source_tiles,
        target_tile_sfb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_tables_follow_normative_rate_bands() {
        let low = BweConfig::for_bitrate(BweMode::Mono, 32)
            .unwrap()
            .unwrap();
        assert_eq!(low.num_sfb, 6);
        assert_eq!(low.num_tiles, 3);
        assert_eq!(low.sfb_boundaries[0], Some(352));
        assert_eq!(low.sfb_boundaries[6], Some(768));

        let high = BweConfig::for_bitrate(BweMode::Mono, 96)
            .unwrap()
            .unwrap();
        assert_eq!(high.num_sfb, 2);
        assert_eq!(high.num_tiles, 1);
        assert_eq!(high.target_tiles[0], Some(672));
        assert!(BweConfig::for_bitrate(BweMode::Mono, 97).unwrap().is_none());
    }

    #[test]
    fn stereo_disables_above_128_kbps() {
        assert!(BweConfig::for_bitrate(BweMode::Stereo, 128).unwrap().is_some());
        assert!(BweConfig::for_bitrate(BweMode::Stereo, 129).unwrap().is_none());
    }

    #[test]
    fn multichannel_uses_exact_equivalent_stereo_ratio() {
        // 832 kb/s 7.1.4 has 11 non-LFE channels: 832*2/11 ~=151.3 kb/s, so BWE is off.
        let mode = BweMode::Multichannel {
            non_lfe_channels: 11,
        };
        assert!(BweConfig::for_bitrate(mode, 832).unwrap().is_none());

        // 704*2/11 = 128 exactly: enabled at the normative boundary.
        let edge = BweConfig::for_bitrate(mode, 704).unwrap().unwrap();
        assert_eq!(edge.num_sfb, 2);
        assert_eq!(edge.num_tiles, 1);
    }

    #[test]
    fn parses_envelopes_and_three_whitening_levels() {
        // Packed bitstream: env=3 (7), env=65 (7), OFF=0, MID=10, HIGH=11.
        // 0000011 1000001 0 10 11 -> 00000111 00000101 01100000.
        let bits = [0b0000_0111, 0b0000_0101, 0b0110_0000];
        let info = parse_bwe_side_info_at(&bits, 0, 2, 3).unwrap();
        assert_eq!(info.envelope_indices[0], Some(3));
        assert_eq!(info.envelope_indices[1], Some(65));
        assert_eq!(info.whitening_levels[0], Some(WhiteningLevel::Off));
        assert_eq!(info.whitening_levels[1], Some(WhiteningLevel::Mid));
        assert_eq!(info.whitening_levels[2], Some(WhiteningLevel::High));
        assert_eq!(info.next_bit_offset, 19);
    }
}
