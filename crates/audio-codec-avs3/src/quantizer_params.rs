// Scalar-quantizer medians used by the interoperable AVS3 hyper-prior model.
//
// GY/T 363-2023 specifies linear scalar inverse quantization, but Annex B does not publish a
// quantile-median table. These values are therefore kept separate from the normative Annex-B
// neural constants. They are the explicit binary32 parameters serialized by the public reference
// hyper-prior model, stored here so the Pure-Rust decoder never depends on its opaque model blob.

pub const BASE_QUANTILE_MEDIANS: [f32; 16] = [0.0; 16];

pub const CONTEXT_QUANTILE_MEDIANS: [f32; 16] = [
    f32::from_bits(0xBEFF_3FE2),
    f32::from_bits(0xBE5B_AFE1),
    f32::from_bits(0xBEDB_47D4),
    f32::from_bits(0xBEE2_C65B),
    f32::from_bits(0xBF60_55BB),
    f32::from_bits(0x3E3C_9387),
    f32::from_bits(0x3ECA_BDB3),
    f32::from_bits(0xBD86_89B8),
    f32::from_bits(0x3E5B_0399),
    f32::from_bits(0x3C1C_9BC7),
    f32::from_bits(0x3DF2_A98F),
    f32::from_bits(0xBDBD_BF72),
    f32::from_bits(0x3D04_0240),
    f32::from_bits(0x3DF5_5644),
    f32::from_bits(0x3DC2_B418),
    f32::from_bits(0xBB6C_6969),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_quantizer_medians_are_exact_zero() {
        assert!(
            BASE_QUANTILE_MEDIANS
                .iter()
                .all(|value| value.to_bits() == 0)
        );
    }

    #[test]
    fn context_quantizer_median_sentinels_keep_binary32_bits() {
        assert_eq!(CONTEXT_QUANTILE_MEDIANS.len(), 16);
        assert_eq!(CONTEXT_QUANTILE_MEDIANS[0].to_bits(), 0xBEFF_3FE2);
        assert_eq!(CONTEXT_QUANTILE_MEDIANS[15].to_bits(), 0xBB6C_6969);
    }
}
