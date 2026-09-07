use yinqidao_codec_core::CodecError;

use crate::{
    BASE_INPUT_CHANNELS, BASE_INPUT_POSITIONS, BASE_OUTPUT_POSITIONS, BaseDecoderParams,
    BaseDecoderWorkspace, BitRange, GroupSideInfo, basic_feature_scale, decode_base_latents_into,
    decode_base_network, inverse_scale_in_place, noise_filling_parameter,
};

const BASE_LATENT_VALUES: usize = BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS;
const SHORT_BLOCKS: usize = 8;
const BASE_DECODER_TOTAL_STRIDE: usize = 16;

/// Explicit decoder-local PRNG for AVS3 latent noise filling.
///
/// GY/T 363-2023 specifies uniformly distributed base noise in `[-1, 1]` but does not mandate a
/// particular pseudo-random generator. Keeping the state in the decoder workspace avoids global
/// RNG locks and makes frame-level behavior reproducible for tests and diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct NoiseFillingRng {
    state: u64,
}

impl NoiseFillingRng {
    pub fn new(seed: u64) -> Self {
        // xorshift64* cannot use the all-zero state.
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
        }
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    #[inline]
    pub fn next_symmetric_f32(&mut self) -> f32 {
        // Use the upper 24 bits so every result is exactly representable as f32 before mapping.
        let unit = ((self.next_u64() >> 40) as u32) as f32 * (1.0 / 16_777_215.0);
        unit.mul_add(2.0, -1.0)
    }
}

impl Default for NoiseFillingRng {
    fn default() -> Self {
        Self::new(0x4156_5333_4E46_524E)
    }
}

#[derive(Debug, Default)]
pub struct BasePipelineWorkspace {
    quantized: Vec<i32>,
    dequantized: Vec<f32>,
    neural: BaseDecoderWorkspace,
}

impl BasePipelineWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self) {
        self.quantized.resize(BASE_LATENT_VALUES, 0);
        self.dequantized.resize(BASE_LATENT_VALUES, 0.0);
    }

    pub fn quantized_latents(&self) -> &[i32] {
        &self.quantized
    }

    pub fn dequantized_latents(&self) -> &[f32] {
        &self.dequantized
    }
}

/// Inverse scalar quantization for the basic-profile base latent tensor.
///
/// The entropy decoder emits position-major `[position][channel]` values. The normative quantizer
/// is channel-wise, so the inverse operation is simply `q + median[channel]`; no transpose or
/// intermediate channel-major buffer is required.
pub fn dequantize_base_latents_into(
    quantized: &[i32],
    quantile_medians: &[f32],
    output: &mut [f32],
) -> Result<(), CodecError> {
    if quantized.len() != BASE_LATENT_VALUES || output.len() != BASE_LATENT_VALUES {
        return Err(CodecError::InvalidData(
            "base latent tensor must be sixty-four positions by sixteen channels",
        ));
    }
    if quantile_medians.len() != BASE_INPUT_CHANNELS {
        return Err(CodecError::InvalidData(
            "base quantizer requires sixteen channel medians",
        ));
    }
    if quantile_medians.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "base quantizer contains a non-finite median",
        ));
    }

    for (input_row, output_row) in quantized
        .chunks_exact(BASE_INPUT_CHANNELS)
        .zip(output.chunks_exact_mut(BASE_INPUT_CHANNELS))
    {
        for channel in 0..BASE_INPUT_CHANNELS {
            output_row[channel] = input_row[channel] as f32 + quantile_medians[channel];
        }
    }
    Ok(())
}

fn noise_fill_ranges(
    num_lines_noise_fill: usize,
    group: GroupSideInfo,
) -> Result<[(usize, usize); 2], CodecError> {
    if !(1..=2).contains(&group.num_groups) {
        return Err(CodecError::InvalidData(
            "noise filling requires one or two AVS3 groups",
        ));
    }
    let num_nf_positions = num_lines_noise_fill / BASE_DECODER_TOTAL_STRIDE;
    if num_nf_positions > BASE_INPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "noise-filling MDCT line count exceeds base latent geometry",
        ));
    }

    if group.num_groups == 1 {
        return Ok([(0, num_nf_positions), (0, 0)]);
    }

    let transient_blocks = group.group_indicator.iter().filter(|&&other| !other).count();
    let other_blocks = SHORT_BLOCKS - transient_blocks;

    // Mirror the normative short-window mapping without floating-point rounding: both source
    // expressions truncate toward zero and all operands are non-negative integers.
    let first_len = num_nf_positions * transient_blocks / SHORT_BLOCKS;
    let second_start = BASE_INPUT_POSITIONS * transient_blocks / SHORT_BLOCKS;
    let second_len = num_nf_positions * other_blocks / SHORT_BLOCKS;
    let second_end = second_start
        .checked_add(second_len)
        .ok_or(CodecError::InvalidData(
            "noise-filling group range overflows address space",
        ))?;
    if second_end > BASE_INPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "noise-filling group range exceeds base latent geometry",
        ));
    }
    Ok([(0, first_len), (second_start, second_end)])
}

/// Apply basic-profile latent noise filling in-place.
///
/// Noise is injected only where the corresponding quantized latent is exactly zero. Each selected
/// value receives a fresh uniform sample in `[-1, 1]` multiplied by its group's 3-bit NF parameter.
pub fn apply_base_noise_filling_in_place(
    quantized: &[i32],
    dequantized: &mut [f32],
    num_lines_noise_fill: usize,
    group: GroupSideInfo,
    nf_param_q_idx: [Option<u8>; 2],
    rng: &mut NoiseFillingRng,
) -> Result<(), CodecError> {
    if quantized.len() != BASE_LATENT_VALUES || dequantized.len() != BASE_LATENT_VALUES {
        return Err(CodecError::InvalidData(
            "noise-filling base latent tensor has invalid geometry",
        ));
    }
    let ranges = noise_fill_ranges(num_lines_noise_fill, group)?;

    for group_index in 0..usize::from(group.num_groups) {
        let index = nf_param_q_idx[group_index].ok_or(CodecError::InvalidData(
            "noise-filling group is missing its three-bit parameter",
        ))?;
        let amplitude = noise_filling_parameter(index)?;
        if amplitude == 0.0 {
            continue;
        }
        let (start, end) = ranges[group_index];
        for position in start..end {
            let row_start = position * BASE_INPUT_CHANNELS;
            for channel in 0..BASE_INPUT_CHANNELS {
                let offset = row_start + channel;
                if quantized[offset] == 0 {
                    dequantized[offset] += rng.next_symmetric_f32() * amplitude;
                }
            }
        }
    }
    Ok(())
}

/// Complete basic-profile inverse-QC path from one `baseBitstream` to the 1024-point neural MDCT
/// spectrum. The context decoder/B.8 selector supplies `model_indices` and the normative B.10..
/// B.23 parameters are supplied through `BaseDecoderParams`.
pub fn decode_basic_base_to_mdct(
    packet: &[u8],
    base_range: BitRange,
    model_indices: &[u8],
    quantile_medians: &[f32],
    num_lines_noise_fill: usize,
    group: GroupSideInfo,
    nf_param_q_idx: [Option<u8>; 2],
    is_feat_amplified: bool,
    scale_q_idx: u8,
    neural_params: BaseDecoderParams<'_>,
    rng: &mut NoiseFillingRng,
    workspace: &mut BasePipelineWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if model_indices.len() != BASE_LATENT_VALUES {
        return Err(CodecError::InvalidData(
            "base model-index tensor must be sixty-four positions by sixteen channels",
        ));
    }
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "basic base pipeline output must contain 1024 MDCT coefficients",
        ));
    }

    workspace.prepare();
    decode_base_latents_into(
        packet,
        base_range,
        model_indices,
        &mut workspace.quantized,
    )?;
    dequantize_base_latents_into(
        &workspace.quantized,
        quantile_medians,
        &mut workspace.dequantized,
    )?;
    apply_base_noise_filling_in_place(
        &workspace.quantized,
        &mut workspace.dequantized,
        num_lines_noise_fill,
        group,
        nf_param_q_idx,
        rng,
    )?;
    inverse_scale_in_place(
        &mut workspace.dequantized,
        basic_feature_scale(is_feat_amplified, scale_q_idx),
    )?;
    decode_base_network(
        &workspace.dequantized,
        neural_params,
        &mut workspace.neural,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BASE_LAYER_1_SPEC, BASE_LAYER_2_SPEC, BASE_LAYER_3_SPEC, BASE_LAYER_4_SPEC,
        ConvTranspose1dParams, IgdnParams,
    };

    fn identity_like_neural_params<'a>(
        kernels: [&'a [f32]; 4],
        biases: [&'a [f32]; 4],
        betas: [&'a [f32]; 3],
        gammas: [&'a [f32]; 3],
    ) -> BaseDecoderParams<'a> {
        BaseDecoderParams {
            layer_1: ConvTranspose1dParams { spec: BASE_LAYER_1_SPEC, kernel: kernels[0], bias: biases[0] },
            igdn_1: IgdnParams { beta: betas[0], gamma: gammas[0] },
            layer_2: ConvTranspose1dParams { spec: BASE_LAYER_2_SPEC, kernel: kernels[1], bias: biases[1] },
            igdn_2: IgdnParams { beta: betas[1], gamma: gammas[1] },
            layer_3: ConvTranspose1dParams { spec: BASE_LAYER_3_SPEC, kernel: kernels[2], bias: biases[2] },
            igdn_3: IgdnParams { beta: betas[2], gamma: gammas[2] },
            layer_4: ConvTranspose1dParams { spec: BASE_LAYER_4_SPEC, kernel: kernels[3], bias: biases[3] },
        }
    }

    #[test]
    fn base_inverse_quantization_is_position_major_without_transpose() {
        let mut quantized = [0_i32; BASE_LATENT_VALUES];
        quantized[0] = 3;
        quantized[1] = -2;
        quantized[BASE_INPUT_CHANNELS] = 7;
        let medians: [f32; BASE_INPUT_CHANNELS] =
            std::array::from_fn(|channel| channel as f32 * 0.25);
        let mut output = [0.0_f32; BASE_LATENT_VALUES];
        dequantize_base_latents_into(&quantized, &medians, &mut output).unwrap();
        assert_eq!(output[0], 3.0);
        assert_eq!(output[1], -1.75);
        assert_eq!(output[BASE_INPUT_CHANNELS], 7.0);
    }

    #[test]
    fn noise_filling_changes_only_quantized_zero_values() {
        let mut quantized = [0_i32; BASE_LATENT_VALUES];
        quantized[1] = 1;
        let mut values = [0.0_f32; BASE_LATENT_VALUES];
        values[1] = 1.0;
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let mut rng = NoiseFillingRng::new(1);
        apply_base_noise_filling_in_place(
            &quantized,
            &mut values,
            1024,
            group,
            [Some(7), None],
            &mut rng,
        )
        .unwrap();
        assert_eq!(values[1], 1.0);
        assert!(values[0] != 0.0);
    }

    #[test]
    fn two_group_noise_ranges_follow_short_block_partition() {
        let group = GroupSideInfo {
            num_groups: 2,
            group_indicator: [false, false, true, true, true, true, true, true],
            next_bit_offset: 0,
        };
        let ranges = noise_fill_ranges(512, group).unwrap();
        assert_eq!(ranges[0], (0, 8));
        assert_eq!(ranges[1], (16, 40));
    }

    #[test]
    fn zero_nf_index_is_an_exact_noop() {
        let quantized = [0_i32; BASE_LATENT_VALUES];
        let mut values = [2.0_f32; BASE_LATENT_VALUES];
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let mut rng = NoiseFillingRng::new(7);
        apply_base_noise_filling_in_place(
            &quantized,
            &mut values,
            1024,
            group,
            [Some(0), None],
            &mut rng,
        )
        .unwrap();
        assert_eq!(values, [2.0; BASE_LATENT_VALUES]);
    }

    #[test]
    fn complete_basic_pipeline_reaches_1024_mdct_output() {
        let k1 = vec![0.0_f32; 5 * 8 * 16];
        let k2 = vec![0.0_f32; 5 * 4 * 8];
        let k3 = vec![0.0_f32; 5 * 2 * 4];
        let k4 = vec![0.0_f32; 5 * 1 * 2];
        let b1 = [0.0_f32; 8];
        let b2 = [0.0_f32; 4];
        let b3 = [0.0_f32; 2];
        let b4 = [5.0_f32; 1];
        let beta1 = [1.0_f32; 8];
        let beta2 = [1.0_f32; 4];
        let beta3 = [1.0_f32; 2];
        let gamma1 = [0.0_f32; 64];
        let gamma2 = [0.0_f32; 16];
        let gamma3 = [0.0_f32; 4];
        let params = identity_like_neural_params(
            [&k1, &k2, &k3, &k4],
            [&b1, &b2, &b3, &b4],
            [&beta1, &beta2, &beta3],
            [&gamma1, &gamma2, &gamma3],
        );
        let model_indices = [0_u8; BASE_LATENT_VALUES];
        let medians = [0.0_f32; BASE_INPUT_CHANNELS];
        let group = GroupSideInfo {
            num_groups: 1,
            group_indicator: [false; 8],
            next_bit_offset: 0,
        };
        let mut rng = NoiseFillingRng::new(11);
        let mut workspace = BasePipelineWorkspace::new();
        let mut output = [0.0_f32; BASE_OUTPUT_POSITIONS];
        decode_basic_base_to_mdct(
            &[],
            BitRange { bit_offset: 0, bit_len: 0 },
            &model_indices,
            &medians,
            0,
            group,
            [Some(0), None],
            false,
            127,
            params,
            &mut rng,
            &mut workspace,
            &mut output,
        )
        .unwrap();
        assert!(output.iter().all(|&value| value == 5.0));
        assert_eq!(workspace.quantized_latents().len(), BASE_LATENT_VALUES);
        assert_eq!(workspace.dequantized_latents().len(), BASE_LATENT_VALUES);
    }
}
