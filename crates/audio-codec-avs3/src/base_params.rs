use crate::{
    BASE_LAYER_1_SPEC, BASE_LAYER_2_SPEC, BASE_LAYER_3_SPEC, BASE_LAYER_4_SPEC,
    BaseDecoderParams, ConvTranspose1dParams, IgdnParams,
};

pub use crate::base_b10::BASE_LAYER_1_KERNEL;

// GY/T 363-2023 Annex B, tables B.11..B.23.
//
// The normative hexadecimal binary32 values are stored by exact bit pattern so conversion through
// decimal source text cannot change coefficients. Transposed-convolution kernels use the executor's
// `[kernel_position][output_channel][input_channel]` layout, which matches the table/model ordering.
//
// Annex-B IGDN gamma tables are serialized as `[input_channel][output_channel]`; the three gamma
// constants below are transposed once at source-generation time into the runtime executor's
// row-major `[output_channel][input_channel]` layout. No runtime transpose, allocation, or copy is
// required.

pub const BASE_LAYER_1_BIAS: [f32; 8] = [
    f32::from_bits(0xBB75AFD7), f32::from_bits(0xBACFF59F), f32::from_bits(0x3C56FE7F), f32::from_bits(0x3A84F1BB), f32::from_bits(0xBBA72E20),
    f32::from_bits(0x3B181840), f32::from_bits(0x3AAC3D53), f32::from_bits(0x3B089B53),
];

pub const BASE_LAYER_1_IGDN_BETA: [f32; 8] = [
    f32::from_bits(0x403ADBD3), f32::from_bits(0x407C771F), f32::from_bits(0x406049F1), f32::from_bits(0x40591198), f32::from_bits(0x405FD2E5),
    f32::from_bits(0x406467B3), f32::from_bits(0x4071FC44), f32::from_bits(0x405CCF5D),
];

pub const BASE_LAYER_1_IGDN_GAMMA: [f32; 64] = [
    f32::from_bits(0x34211F54), f32::from_bits(0x35F25EF6), f32::from_bits(0x3529CF57), f32::from_bits(0x00000000),
    f32::from_bits(0x33B14365), f32::from_bits(0x363073E4), f32::from_bits(0x30D93F8A), f32::from_bits(0x00000000),
    f32::from_bits(0x3587DDDC), f32::from_bits(0x35C324EB), f32::from_bits(0x35173C4B), f32::from_bits(0x362B4E91),
    f32::from_bits(0x350F2849), f32::from_bits(0x3621EEA2), f32::from_bits(0x35AD8B63), f32::from_bits(0x00000000),
    f32::from_bits(0x32991D6B), f32::from_bits(0x35077312), f32::from_bits(0x35C4F866), f32::from_bits(0x360F5439),
    f32::from_bits(0x3584FFC4), f32::from_bits(0x360D57DD), f32::from_bits(0x331D672B), f32::from_bits(0x35702C05),
    f32::from_bits(0x35958B9E), f32::from_bits(0x355C4FDC), f32::from_bits(0x358D813D), f32::from_bits(0x35E09AB8),
    f32::from_bits(0x353A1EB1), f32::from_bits(0x35CE5798), f32::from_bits(0x00000000), f32::from_bits(0x00000000),
    f32::from_bits(0x326A03CF), f32::from_bits(0x352A5708), f32::from_bits(0x33DB4B41), f32::from_bits(0x00000000),
    f32::from_bits(0x35503FD6), f32::from_bits(0x35BB66E5), f32::from_bits(0x00000000), f32::from_bits(0x34FA5971),
    f32::from_bits(0x35B8D894), f32::from_bits(0x35FF0198), f32::from_bits(0x35715396), f32::from_bits(0x361DCFF1),
    f32::from_bits(0x340E687F), f32::from_bits(0x359BE141), f32::from_bits(0x00000000), f32::from_bits(0x35849F7E),
    f32::from_bits(0x00000000), f32::from_bits(0x34C2267B), f32::from_bits(0x356AAB25), f32::from_bits(0x359533F4),
    f32::from_bits(0x359B8ED1), f32::from_bits(0x348DBDAD), f32::from_bits(0x35F1FFD8), f32::from_bits(0x35D68101),
    f32::from_bits(0x34E160F3), f32::from_bits(0x34998770), f32::from_bits(0x332352AE), f32::from_bits(0x347B97B9),
    f32::from_bits(0x33EFF960), f32::from_bits(0x36306725), f32::from_bits(0x2F683F95), f32::from_bits(0x35D6B07D),
];

pub const BASE_LAYER_2_KERNEL: [f32; 160] = [
    f32::from_bits(0xBF40404B), f32::from_bits(0xBC579A18), f32::from_bits(0xBE8EE010), f32::from_bits(0x3D06DC09), f32::from_bits(0x3D9F00FA),
    f32::from_bits(0x3E191BE4), f32::from_bits(0xBD228CE0), f32::from_bits(0x3DC43AFE), f32::from_bits(0xBF346A3C), f32::from_bits(0xBE0365DB),
    f32::from_bits(0xBE8D61A7), f32::from_bits(0x3E77B0B5), f32::from_bits(0x3E1971F3), f32::from_bits(0x3E6D4BBF), f32::from_bits(0x3E69E4D1),
    f32::from_bits(0x3E553D97), f32::from_bits(0xBD903D73), f32::from_bits(0x3D910CF1), f32::from_bits(0x3DD65CBB), f32::from_bits(0xBED27800),
    f32::from_bits(0xBDCA60C1), f32::from_bits(0xBEB8C1EB), f32::from_bits(0xBEC705E8), f32::from_bits(0xBE65DE2F), f32::from_bits(0x3E60E2D1),
    f32::from_bits(0xBE340734), f32::from_bits(0xBD26E1C8), f32::from_bits(0xBD375BEC), f32::from_bits(0x3D91A68D), f32::from_bits(0xBE7E684A),
    f32::from_bits(0x3E47420C), f32::from_bits(0x3EE6C68E), f32::from_bits(0xBEF74C5D), f32::from_bits(0x3EBA6950), f32::from_bits(0x3B828BA2),
    f32::from_bits(0xBE58A44E), f32::from_bits(0xBF182B69), f32::from_bits(0x3E874C98), f32::from_bits(0xBF16E9B6), f32::from_bits(0xBF073517),
    f32::from_bits(0xBE1148E3), f32::from_bits(0xBE172A8B), f32::from_bits(0x3E69ED13), f32::from_bits(0x3F0094B5), f32::from_bits(0x3EBB33BF),
    f32::from_bits(0xBF6A5410), f32::from_bits(0xBEAA541D), f32::from_bits(0x3E98E850), f32::from_bits(0x3E8E9F60), f32::from_bits(0x3D8D3DFF),
    f32::from_bits(0x3E9F25D3), f32::from_bits(0x3F2C65A6), f32::from_bits(0xBEDF3296), f32::from_bits(0x3E81A4BE), f32::from_bits(0x3D8620C3),
    f32::from_bits(0xBE210B3D), f32::from_bits(0x3E8C0ED6), f32::from_bits(0xBD97CEDB), f32::from_bits(0xBE0F70FA), f32::from_bits(0xBF55441E),
    f32::from_bits(0x3E154470), f32::from_bits(0xBE525680), f32::from_bits(0x3CE21275), f32::from_bits(0x3E16698B), f32::from_bits(0x3DF5E795),
    f32::from_bits(0x3F04F0E9), f32::from_bits(0xBE97D8DE), f32::from_bits(0x3D2697A1), f32::from_bits(0xBEBE593E), f32::from_bits(0x3DDED25F),
    f32::from_bits(0xBE4FFE9C), f32::from_bits(0x3ED99531), f32::from_bits(0xBE2DD276), f32::from_bits(0xBDA1ECBD), f32::from_bits(0x3D0F727F),
    f32::from_bits(0xBDED6159), f32::from_bits(0x3EC519F4), f32::from_bits(0xBE89EB3A), f32::from_bits(0x3EF4DFC0), f32::from_bits(0xBF3CE92F),
    f32::from_bits(0xBE6B2907), f32::from_bits(0xBE84AEB7), f32::from_bits(0x3D990837), f32::from_bits(0x3D9E7086), f32::from_bits(0xBF3A3470),
    f32::from_bits(0xBF0231A8), f32::from_bits(0x3EB1DAF3), f32::from_bits(0xBD29EFC7), f32::from_bits(0x3E6F986A), f32::from_bits(0xBE0553DB),
    f32::from_bits(0xBF63121F), f32::from_bits(0x3DAB1470), f32::from_bits(0xBEB0369F), f32::from_bits(0xBEB974A8), f32::from_bits(0x3DF16CD8),
    f32::from_bits(0xBE44AEDD), f32::from_bits(0x3EB71695), f32::from_bits(0xBED30340), f32::from_bits(0xBEBA6F0E), f32::from_bits(0x3E300D76),
    f32::from_bits(0x3E5B8651), f32::from_bits(0x3E8F0EAE), f32::from_bits(0xBF0CB732), f32::from_bits(0xBE754C41), f32::from_bits(0xBE0200C0),
    f32::from_bits(0x3F0D0613), f32::from_bits(0xBE00B60A), f32::from_bits(0x3DEAC5D9), f32::from_bits(0x3ECCB3E5), f32::from_bits(0x3DD471AA),
    f32::from_bits(0x3E7EB664), f32::from_bits(0x3E3B7DC9), f32::from_bits(0x3D862B79), f32::from_bits(0x3EF4EF2E), f32::from_bits(0xBE22EA55),
    f32::from_bits(0xBB9861A8), f32::from_bits(0xBE9BED21), f32::from_bits(0xBEAE90CC), f32::from_bits(0x3E67B943), f32::from_bits(0x3E3BDDB6),
    f32::from_bits(0x3E48AF20), f32::from_bits(0x3F3A767C), f32::from_bits(0xBE8F951B), f32::from_bits(0x3D957FC2), f32::from_bits(0x3EE699F2),
    f32::from_bits(0xBE21C7F7), f32::from_bits(0x3DAA4359), f32::from_bits(0xBEF763D1), f32::from_bits(0x3D9B9D02), f32::from_bits(0xBE57355A),
    f32::from_bits(0xBD12C4BF), f32::from_bits(0xBC2A1BFF), f32::from_bits(0x3CE18481), f32::from_bits(0x3D587E7E), f32::from_bits(0xBE079190),
    f32::from_bits(0xBE133CA9), f32::from_bits(0xBCDEE191), f32::from_bits(0x3751CF5A), f32::from_bits(0xBD4C35CB), f32::from_bits(0x3B7EF711),
    f32::from_bits(0xBB9C48F6), f32::from_bits(0x3C498D2A), f32::from_bits(0x3D65BB9C), f32::from_bits(0x3D913898), f32::from_bits(0xBC8F2EB0),
    f32::from_bits(0xBDFCBBA0), f32::from_bits(0xBD672585), f32::from_bits(0x3C28C4C3), f32::from_bits(0x3C64FEBE), f32::from_bits(0x3D9944CC),
    f32::from_bits(0xBB87426D), f32::from_bits(0x3D97FBC9), f32::from_bits(0x3D14FA24), f32::from_bits(0xBD56291A), f32::from_bits(0xBDEE8715),
    f32::from_bits(0x3CA6DD29), f32::from_bits(0x3DD81779), f32::from_bits(0x3D7F09F8), f32::from_bits(0xBCAA78AB), f32::from_bits(0xBD59BE7C),
];

pub const BASE_LAYER_2_BIAS: [f32; 4] = [
    f32::from_bits(0x3BBE23F3), f32::from_bits(0xBBBE47A2), f32::from_bits(0x3C1E1773), f32::from_bits(0x3BA8C901),
];

pub const BASE_LAYER_2_IGDN_BETA: [f32; 4] = [
    f32::from_bits(0x4179BBD6), f32::from_bits(0x4199F3FA), f32::from_bits(0x4188B602), f32::from_bits(0x4199E4B2),
];

pub const BASE_LAYER_2_IGDN_GAMMA: [f32; 16] = [
    f32::from_bits(0x33C69C6E), f32::from_bits(0x00000000), f32::from_bits(0x315AD406), f32::from_bits(0x00000000),
    f32::from_bits(0x00000000), f32::from_bits(0x33D7FB94), f32::from_bits(0x00000000), f32::from_bits(0x00000000),
    f32::from_bits(0x3543D51C), f32::from_bits(0x00000000), f32::from_bits(0x00000000), f32::from_bits(0x00000000),
    f32::from_bits(0x2EC31A0E), f32::from_bits(0x00000000), f32::from_bits(0x308A70BE), f32::from_bits(0x34A0565B),
];

pub const BASE_LAYER_3_KERNEL: [f32; 40] = [
    f32::from_bits(0xBE5C740F), f32::from_bits(0xBD2CAC67), f32::from_bits(0xBE8AC218), f32::from_bits(0xBEC5405A), f32::from_bits(0x3F024A58),
    f32::from_bits(0xBEF9B5CF), f32::from_bits(0x3F6EC4C2), f32::from_bits(0x3F7FBAB1), f32::from_bits(0xBFAE48B9), f32::from_bits(0xBE82DA5A),
    f32::from_bits(0x3F1D68F7), f32::from_bits(0xBE9B5F2F), f32::from_bits(0x3F54FD42), f32::from_bits(0xBFCDF29E), f32::from_bits(0x3C617289),
    f32::from_bits(0xBEF3F54A), f32::from_bits(0x3F302998), f32::from_bits(0x3F141E68), f32::from_bits(0x3F3C61B5), f32::from_bits(0xBF4B9FCC),
    f32::from_bits(0xBC50670E), f32::from_bits(0xBE4DB87B), f32::from_bits(0x3ED38F9A), f32::from_bits(0xBEC472AD), f32::from_bits(0x3D2BF4D2),
    f32::from_bits(0xBE31EF5B), f32::from_bits(0xBD54AFA3), f32::from_bits(0x3E05FE38), f32::from_bits(0x3DD00451), f32::from_bits(0xBC2EF0FC),
    f32::from_bits(0xBE924944), f32::from_bits(0x3E97A14F), f32::from_bits(0xBE314DA2), f32::from_bits(0x3BABD152), f32::from_bits(0x3D62034F),
    f32::from_bits(0x3E8CA2A7), f32::from_bits(0x3D920A09), f32::from_bits(0xBD43FB17), f32::from_bits(0xBDA9EC1E), f32::from_bits(0x3D1C5608),
];

pub const BASE_LAYER_3_BIAS: [f32; 2] = [
    f32::from_bits(0xBDA4D2FC), f32::from_bits(0xBD3751C7),
];

pub const BASE_LAYER_3_IGDN_BETA: [f32; 2] = [
    f32::from_bits(0x4381E53F), f32::from_bits(0x439C2C07),
];

pub const BASE_LAYER_3_IGDN_GAMMA: [f32; 4] = [
    f32::from_bits(0x2EF5DF32), f32::from_bits(0x00000000), f32::from_bits(0x2DAF37F6), f32::from_bits(0x00000000),
];

pub const BASE_LAYER_4_KERNEL: [f32; 10] = [
    f32::from_bits(0xC002293D), f32::from_bits(0xBDA6059A), f32::from_bits(0x3DD14361), f32::from_bits(0xBFC39666), f32::from_bits(0xBE1D2A5C),
    f32::from_bits(0x3C14AF53), f32::from_bits(0x3E97C19D), f32::from_bits(0x3EED70EE), f32::from_bits(0x3B75E044), f32::from_bits(0xBDE79DF1),
];

pub const BASE_LAYER_4_BIAS: [f32; 1] = [
    f32::from_bits(0x3ED0954F),
];

/// Return the complete normative basic-profile base decoder parameters from Annex B tables
/// B.10..B.23. All returned slices point directly at static binary32 tables.
pub fn base_decoder_params() -> BaseDecoderParams<'static> {
    BaseDecoderParams {
        layer_1: ConvTranspose1dParams {
            spec: BASE_LAYER_1_SPEC,
            kernel: &BASE_LAYER_1_KERNEL,
            bias: &BASE_LAYER_1_BIAS,
        },
        igdn_1: IgdnParams {
            beta: &BASE_LAYER_1_IGDN_BETA,
            gamma: &BASE_LAYER_1_IGDN_GAMMA,
        },
        layer_2: ConvTranspose1dParams {
            spec: BASE_LAYER_2_SPEC,
            kernel: &BASE_LAYER_2_KERNEL,
            bias: &BASE_LAYER_2_BIAS,
        },
        igdn_2: IgdnParams {
            beta: &BASE_LAYER_2_IGDN_BETA,
            gamma: &BASE_LAYER_2_IGDN_GAMMA,
        },
        layer_3: ConvTranspose1dParams {
            spec: BASE_LAYER_3_SPEC,
            kernel: &BASE_LAYER_3_KERNEL,
            bias: &BASE_LAYER_3_BIAS,
        },
        igdn_3: IgdnParams {
            beta: &BASE_LAYER_3_IGDN_BETA,
            gamma: &BASE_LAYER_3_IGDN_GAMMA,
        },
        layer_4: ConvTranspose1dParams {
            spec: BASE_LAYER_4_SPEC,
            kernel: &BASE_LAYER_4_KERNEL,
            bias: &BASE_LAYER_4_BIAS,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_parameter_shapes_match_base_decoder_geometry() {
        assert_eq!(BASE_LAYER_1_KERNEL.len(), 5 * 8 * 16);
        assert_eq!(BASE_LAYER_1_BIAS.len(), 8);
        assert_eq!(BASE_LAYER_1_IGDN_BETA.len(), 8);
        assert_eq!(BASE_LAYER_1_IGDN_GAMMA.len(), 8 * 8);
        assert_eq!(BASE_LAYER_2_KERNEL.len(), 5 * 4 * 8);
        assert_eq!(BASE_LAYER_2_BIAS.len(), 4);
        assert_eq!(BASE_LAYER_2_IGDN_BETA.len(), 4);
        assert_eq!(BASE_LAYER_2_IGDN_GAMMA.len(), 4 * 4);
        assert_eq!(BASE_LAYER_3_KERNEL.len(), 5 * 2 * 4);
        assert_eq!(BASE_LAYER_3_BIAS.len(), 2);
        assert_eq!(BASE_LAYER_3_IGDN_BETA.len(), 2);
        assert_eq!(BASE_LAYER_3_IGDN_GAMMA.len(), 2 * 2);
        assert_eq!(BASE_LAYER_4_KERNEL.len(), 5 * 1 * 2);
        assert_eq!(BASE_LAYER_4_BIAS.len(), 1);
    }

    #[test]
    fn annex_b_binary32_sentinels_are_exact() {
        assert_eq!(BASE_LAYER_1_KERNEL[0].to_bits(), 0x3A2C_F468);
        assert_eq!(BASE_LAYER_1_KERNEL[639].to_bits(), 0x3AA8_2E4B);
        assert_eq!(BASE_LAYER_1_BIAS[0].to_bits(), 0xBB75_AFD7);
        assert_eq!(BASE_LAYER_1_IGDN_BETA[7].to_bits(), 0x405C_CF5D);
        assert_eq!(BASE_LAYER_1_IGDN_GAMMA[0].to_bits(), 0x3421_1F54);
        assert_eq!(BASE_LAYER_2_KERNEL[0].to_bits(), 0xBF40_404B);
        assert_eq!(BASE_LAYER_2_KERNEL[159].to_bits(), 0xBD59_BE7C);
        assert_eq!(BASE_LAYER_2_IGDN_GAMMA[15].to_bits(), 0x34A0_565B);
        assert_eq!(BASE_LAYER_3_KERNEL[0].to_bits(), 0xBE5C_740F);
        assert_eq!(BASE_LAYER_3_KERNEL[39].to_bits(), 0x3D1C_5608);
        assert_eq!(BASE_LAYER_3_IGDN_GAMMA[2].to_bits(), 0x2DAF_37F6);
        assert_eq!(BASE_LAYER_4_KERNEL[0].to_bits(), 0xC002_293D);
        assert_eq!(BASE_LAYER_4_KERNEL[9].to_bits(), 0xBDE7_9DF1);
        assert_eq!(BASE_LAYER_4_BIAS[0].to_bits(), 0x3ED0_954F);
    }

    #[test]
    fn complete_annex_b_constructor_uses_only_static_tables() {
        let params = base_decoder_params();
        assert_eq!(params.layer_1.kernel.as_ptr(), BASE_LAYER_1_KERNEL.as_ptr());
        assert_eq!(params.layer_2.kernel.as_ptr(), BASE_LAYER_2_KERNEL.as_ptr());
        assert_eq!(params.layer_3.kernel.as_ptr(), BASE_LAYER_3_KERNEL.as_ptr());
        assert_eq!(params.layer_4.kernel.as_ptr(), BASE_LAYER_4_KERNEL.as_ptr());
    }
}
