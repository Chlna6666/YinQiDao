mod b2;

pub use b2::CONTEXT_LAYER_1_KERNEL;

// GY/T 363-2023 / T/UWA 009.1-2023 Annex B context-decoder parameters.
//
// B.2 is kept in a dedicated data module because its kernel contains 3 * 16 * 16 exact binary32
// coefficients. Tables B.3, B.5 and B.7 are stored here as exact IEEE-754 binary32 bit patterns.
// B.4 and B.6 remain intentionally absent until their complete 768-value tables pass the same
// length and sentinel validation.

pub const CONTEXT_LAYER_1_BIAS: [f32; 16] = [
    f32::from_bits(0x3E5B_9B84), f32::from_bits(0xC031_AE93),
    f32::from_bits(0xBECC_9FE9), f32::from_bits(0xBFE3_9601),
    f32::from_bits(0xC031_F07D), f32::from_bits(0x3EA0_B437),
    f32::from_bits(0xBE7B_2150), f32::from_bits(0x3F20_33F9),
    f32::from_bits(0xBE83_CC72), f32::from_bits(0xBF75_8EEB),
    f32::from_bits(0x3D02_63EC), f32::from_bits(0xBF3B_2B07),
    f32::from_bits(0xBED8_6325), f32::from_bits(0xBF12_087B),
    f32::from_bits(0xBF87_9E36), f32::from_bits(0xBEEB_8E19),
];

pub const CONTEXT_LAYER_2_BIAS: [f32; 16] = [
    f32::from_bits(0xBFC3_28B8), f32::from_bits(0x3C1E_9004),
    f32::from_bits(0xBC95_6F95), f32::from_bits(0xBFDE_CDBA),
    f32::from_bits(0xBE1C_6600), f32::from_bits(0xBD8C_2C70),
    f32::from_bits(0x3CA0_A0E8), f32::from_bits(0x3A87_502C),
    f32::from_bits(0xBD86_4DD3), f32::from_bits(0xBF56_6A9F),
    f32::from_bits(0xBEC3_4AFE), f32::from_bits(0xBFB1_F5B7),
    f32::from_bits(0xBD36_3B87), f32::from_bits(0xBDDB_A0CB),
    f32::from_bits(0xBF58_02B6), f32::from_bits(0xBEAC_23B2),
];

pub const CONTEXT_LAYER_3_BIAS: [f32; 16] = [
    f32::from_bits(0x3DE1_4B18), f32::from_bits(0x3DE1_4A06),
    f32::from_bits(0x3DE1_4986), f32::from_bits(0x3DE1_4633),
    f32::from_bits(0x3DE1_47DB), f32::from_bits(0x3DE1_43C3),
    f32::from_bits(0x3DE1_3F7A), f32::from_bits(0x3DE1_4B2D),
    f32::from_bits(0x3DE1_42F6), f32::from_bits(0x3DE1_4313),
    f32::from_bits(0x3DE1_4BB9), f32::from_bits(0x3DE1_492C),
    f32::from_bits(0x3DE1_3E3C), f32::from_bits(0x3DE1_4C20),
    f32::from_bits(0x3DE1_4608), f32::from_bits(0x3DE1_4CCD),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_context_parameter_shapes_are_exact() {
        assert_eq!(CONTEXT_LAYER_1_KERNEL.len(), 3 * 16 * 16);
        assert_eq!(CONTEXT_LAYER_1_BIAS.len(), 16);
        assert_eq!(CONTEXT_LAYER_2_BIAS.len(), 16);
        assert_eq!(CONTEXT_LAYER_3_BIAS.len(), 16);
    }

    #[test]
    fn annex_b_context_bias_sentinels_keep_binary32_bits() {
        assert_eq!(CONTEXT_LAYER_1_BIAS[0].to_bits(), 0x3E5B_9B84);
        assert_eq!(CONTEXT_LAYER_1_BIAS[15].to_bits(), 0xBEEB_8E19);
        assert_eq!(CONTEXT_LAYER_2_BIAS[0].to_bits(), 0xBFC3_28B8);
        assert_eq!(CONTEXT_LAYER_2_BIAS[15].to_bits(), 0xBEAC_23B2);
        assert_eq!(CONTEXT_LAYER_3_BIAS[0].to_bits(), 0x3DE1_4B18);
        assert_eq!(CONTEXT_LAYER_3_BIAS[15].to_bits(), 0x3DE1_4CCD);
    }
}
