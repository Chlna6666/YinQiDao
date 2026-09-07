use crate::fd_lsf::{LSF_MEAN, LsfCodebook, LsfCodebooks};

pub const FD_LSF_TABLE_VALUES: usize = 10_992;
pub const FD_LSF_TABLE_BYTES: usize = FD_LSF_TABLE_VALUES * 4;
pub const FD_LSF_TABLE_FNV1A: u64 = 0x9ce2_64f0_19b7_5cc4;

const FD_CHUNK_BYTES: usize = 5_400;
const FD_LAST_CHUNK_BYTES: usize = 768;

const FD_CHUNK_0: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_0.bin");
const FD_CHUNK_1: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_1.bin");
const FD_CHUNK_2: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_2.bin");
const FD_CHUNK_3: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_3.bin");
const FD_CHUNK_4: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_4.bin");
const FD_CHUNK_5: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_5.bin");
const FD_CHUNK_6: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_6.bin");
const FD_CHUNK_7: &[u8; FD_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_7.bin");
const FD_CHUNK_8: &[u8; FD_LAST_CHUNK_BYTES] = include_bytes!("../assets/avs3_fd_tables_8.bin");

const HBR_STAGE1_CB1: usize = 16;
const HBR_STAGE1_CB2: usize = 2_320;
const HBR_STAGE2_CB1: usize = 4_112;
const HBR_STAGE2_CB2: usize = 4_496;
const HBR_STAGE2_CB3: usize = 4_880;
const HBR_STAGE2_CB4: usize = 5_072;
const HBR_STAGE2_CB5: usize = 5_168;
const LBR_STAGE1_CB1: usize = 5_296;
const LBR_STAGE1_CB2: usize = 7_600;
const LBR_STAGE2_CB1: usize = 9_392;
const LBR_STAGE2_CB2: usize = 10_032;
const LBR_STAGE2_CB3: usize = 10_544;

#[inline(always)]
const fn fd_asset_byte(index: usize) -> u8 {
    let chunk = index / FD_CHUNK_BYTES;
    let offset = index % FD_CHUNK_BYTES;
    match chunk {
        0 => FD_CHUNK_0[offset],
        1 => FD_CHUNK_1[offset],
        2 => FD_CHUNK_2[offset],
        3 => FD_CHUNK_3[offset],
        4 => FD_CHUNK_4[offset],
        5 => FD_CHUNK_5[offset],
        6 => FD_CHUNK_6[offset],
        7 => FD_CHUNK_7[offset],
        8 => FD_CHUNK_8[offset],
        _ => 0,
    }
}

const fn decode_fd_lsf_tables() -> [f32; FD_LSF_TABLE_VALUES] {
    let mut output = [0.0_f32; FD_LSF_TABLE_VALUES];
    let mut index = 0usize;
    while index < FD_LSF_TABLE_VALUES {
        let byte = index * 4;
        let bits = u32::from_le_bytes([
            fd_asset_byte(byte),
            fd_asset_byte(byte + 1),
            fd_asset_byte(byte + 2),
            fd_asset_byte(byte + 3),
        ]);
        output[index] = f32::from_bits(bits);
        index += 1;
    }
    output
}

/// GY/T 363-2023 Annex-B frequency-domain LSF codebooks, materialized as native `f32` once at
/// compile time from the checked little-endian normative byte asset.
///
/// The binary chunks only exist to make the repository transport robust. They are concatenated in
/// numeric filename order by `fd_asset_byte`; no chunk dispatch, byte conversion or allocation is
/// executed while decoding frames.
static FD_LSF_TABLES: [f32; FD_LSF_TABLE_VALUES] = decode_fd_lsf_tables();

#[inline]
pub fn normative_lsf_codebooks() -> LsfCodebooks<'static> {
    LsfCodebooks {
        high_stage1: [
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE1_CB1..HBR_STAGE1_CB2],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE1_CB2..HBR_STAGE2_CB1],
            },
        ],
        high_stage2: [
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE2_CB1..HBR_STAGE2_CB2],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE2_CB2..HBR_STAGE2_CB3],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE2_CB3..HBR_STAGE2_CB4],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE2_CB4..HBR_STAGE2_CB5],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[HBR_STAGE2_CB5..LBR_STAGE1_CB1],
            },
        ],
        low_stage1: [
            LsfCodebook {
                values: &FD_LSF_TABLES[LBR_STAGE1_CB1..LBR_STAGE1_CB2],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[LBR_STAGE1_CB2..LBR_STAGE2_CB1],
            },
        ],
        low_stage2: [
            LsfCodebook {
                values: &FD_LSF_TABLES[LBR_STAGE2_CB1..LBR_STAGE2_CB2],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[LBR_STAGE2_CB2..LBR_STAGE2_CB3],
            },
            LsfCodebook {
                values: &FD_LSF_TABLES[LBR_STAGE2_CB3..FD_LSF_TABLE_VALUES],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset_fnv1a() -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for index in 0..FD_LSF_TABLE_BYTES {
            hash ^= u64::from(fd_asset_byte(index));
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        hash
    }

    #[test]
    fn bundled_fd_asset_has_exact_geometry_and_fingerprint() {
        assert_eq!(FD_LSF_TABLE_BYTES, 43_968);
        assert_eq!(FD_LSF_TABLE_VALUES, 10_992);
        assert_eq!(asset_fnv1a(), FD_LSF_TABLE_FNV1A);
        assert!(FD_LSF_TABLES.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn bundled_mean_matches_annex_b46_constants_bit_for_bit() {
        for index in 0..LSF_MEAN.len() {
            assert_eq!(FD_LSF_TABLES[index].to_bits(), LSF_MEAN[index].to_bits());
        }
    }

    #[test]
    fn normative_codebooks_match_all_annex_b_split_vq_geometries() {
        let tables = normative_lsf_codebooks();
        assert_eq!(tables.high_stage1[0].values.len(), 256 * 9);
        assert_eq!(tables.high_stage1[1].values.len(), 256 * 7);
        assert_eq!(tables.high_stage2[0].values.len(), 128 * 3);
        assert_eq!(tables.high_stage2[1].values.len(), 128 * 3);
        assert_eq!(tables.high_stage2[2].values.len(), 64 * 3);
        assert_eq!(tables.high_stage2[3].values.len(), 32 * 3);
        assert_eq!(tables.high_stage2[4].values.len(), 32 * 4);
        assert_eq!(tables.low_stage1[0].values.len(), 256 * 9);
        assert_eq!(tables.low_stage1[1].values.len(), 256 * 7);
        assert_eq!(tables.low_stage2[0].values.len(), 128 * 5);
        assert_eq!(tables.low_stage2[1].values.len(), 128 * 4);
        assert_eq!(tables.low_stage2[2].values.len(), 64 * 7);
    }
}
