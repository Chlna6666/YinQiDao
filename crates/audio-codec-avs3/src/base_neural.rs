use yinqidao_audio_simd::dot_product;
use yinqidao_codec_core::CodecError;

use crate::{ConvTranspose1dParams, ConvTranspose1dSpec, NeuralActivation, conv1d_transpose_same};

pub const BASE_INPUT_POSITIONS: usize = 64;
pub const BASE_INPUT_CHANNELS: usize = 16;
pub const BASE_OUTPUT_POSITIONS: usize = 1024;

pub const BASE_LAYER_1_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: 16,
    output_channels: 8,
    kernel_size: 5,
    stride: 2,
    activation: NeuralActivation::None,
};
pub const BASE_LAYER_2_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: 8,
    output_channels: 4,
    kernel_size: 5,
    stride: 2,
    activation: NeuralActivation::None,
};
pub const BASE_LAYER_3_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: 4,
    output_channels: 2,
    kernel_size: 5,
    stride: 2,
    activation: NeuralActivation::None,
};
pub const BASE_LAYER_4_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: 2,
    output_channels: 1,
    kernel_size: 5,
    stride: 2,
    activation: NeuralActivation::None,
};

#[derive(Clone, Copy, Debug)]
pub struct IgdnParams<'a> {
    /// One beta value per output channel.
    pub beta: &'a [f32],
    /// Row-major `[output_channel][input_channel]` gamma matrix.
    pub gamma: &'a [f32],
}

#[derive(Clone, Copy, Debug)]
pub struct BaseDecoderParams<'a> {
    pub layer_1: ConvTranspose1dParams<'a>,
    pub igdn_1: IgdnParams<'a>,
    pub layer_2: ConvTranspose1dParams<'a>,
    pub igdn_2: IgdnParams<'a>,
    pub layer_3: ConvTranspose1dParams<'a>,
    pub igdn_3: IgdnParams<'a>,
    pub layer_4: ConvTranspose1dParams<'a>,
}

#[derive(Debug, Default)]
pub struct BaseDecoderWorkspace {
    layer_1: Vec<f32>,
    layer_2: Vec<f32>,
    layer_3: Vec<f32>,
}

impl BaseDecoderWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self) {
        // Every hidden stage contains exactly 1024 scalar values:
        // 128x8 -> 256x4 -> 512x2.
        self.layer_1.resize(128 * 8, 0.0);
        self.layer_2.resize(256 * 4, 0.0);
        self.layer_3.resize(512 * 2, 0.0);
    }
}

/// Apply AVS3 inverse generalized divisive normalization to one position-major tensor.
///
/// For output channel `i`, IGDN computes
/// `y_i = x_i * sqrt(beta_i + sum_j(gamma[i,j] * x_j^2))`.
/// The normative base decoder uses at most eight channels, so the squared-input scratch stays on
/// the stack and no per-frame temporary allocation or transpose is required.
pub fn apply_igdn_in_place(
    values: &mut [f32],
    channels: usize,
    params: IgdnParams<'_>,
) -> Result<(), CodecError> {
    if channels == 0 || channels > 8 {
        return Err(CodecError::InvalidData(
            "AVS3 base IGDN channel count must be between one and eight",
        ));
    }
    if values.len() % channels != 0 {
        return Err(CodecError::InvalidData(
            "AVS3 base IGDN tensor length is not divisible by channel count",
        ));
    }
    if params.beta.len() != channels || params.gamma.len() != channels * channels {
        return Err(CodecError::InvalidData(
            "AVS3 base IGDN parameter shape does not match channel count",
        ));
    }
    if params
        .beta
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
        || params.gamma.iter().any(|value| !value.is_finite())
    {
        return Err(CodecError::InvalidData(
            "AVS3 base IGDN parameters contain invalid values",
        ));
    }

    let mut squared = [0.0_f32; 8];
    for row in values.chunks_exact_mut(channels) {
        if row.iter().any(|value| !value.is_finite()) {
            return Err(CodecError::InvalidData(
                "AVS3 base IGDN input contains a non-finite value",
            ));
        }
        for channel in 0..channels {
            squared[channel] = row[channel] * row[channel];
        }
        for channel in 0..channels {
            let gamma_row = &params.gamma[channel * channels..(channel + 1) * channels];
            let normalization = params.beta[channel] + dot_product(&squared[..channels], gamma_row);
            if !normalization.is_finite() || normalization < 0.0 {
                return Err(CodecError::InvalidData(
                    "AVS3 base IGDN normalization is negative or non-finite",
                ));
            }
            row[channel] *= normalization.sqrt();
        }
    }
    Ok(())
}

/// Execute the normative basic-profile base decoding neural network geometry from table 16:
/// `64x16 -> 128x8 -> 256x4 -> 512x2 -> 1024x1`.
///
/// B.10..B.23 provide the kernels, biases, beta and gamma values. They are supplied explicitly by
/// the caller so this execution path remains Pure Rust and does not depend on an opaque model blob.
pub fn decode_base_network(
    input: &[f32],
    params: BaseDecoderParams<'_>,
    workspace: &mut BaseDecoderWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if params.layer_1.spec != BASE_LAYER_1_SPEC
        || params.layer_2.spec != BASE_LAYER_2_SPEC
        || params.layer_3.spec != BASE_LAYER_3_SPEC
        || params.layer_4.spec != BASE_LAYER_4_SPEC
    {
        return Err(CodecError::InvalidData(
            "base decoder layer spec does not match AVS3 table 16",
        ));
    }
    if input.len() != BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS {
        return Err(CodecError::InvalidData(
            "base decoder input must be sixty-four positions by sixteen channels",
        ));
    }
    if output.len() != BASE_OUTPUT_POSITIONS {
        return Err(CodecError::InvalidData(
            "base decoder output must contain 1024 MDCT coefficients",
        ));
    }

    workspace.prepare();
    conv1d_transpose_same(
        input,
        BASE_INPUT_POSITIONS,
        params.layer_1,
        &mut workspace.layer_1,
    )?;
    apply_igdn_in_place(&mut workspace.layer_1, 8, params.igdn_1)?;

    conv1d_transpose_same(
        &workspace.layer_1,
        128,
        params.layer_2,
        &mut workspace.layer_2,
    )?;
    apply_igdn_in_place(&mut workspace.layer_2, 4, params.igdn_2)?;

    conv1d_transpose_same(
        &workspace.layer_2,
        256,
        params.layer_3,
        &mut workspace.layer_3,
    )?;
    apply_igdn_in_place(&mut workspace.layer_3, 2, params.igdn_3)?;

    conv1d_transpose_same(&workspace.layer_3, 512, params.layer_4, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn igdn_identity_parameters_leave_tensor_unchanged() {
        let mut values = [1.0_f32, -2.0, 3.0, -4.0];
        let beta = [1.0_f32, 1.0];
        let gamma = [0.0_f32; 4];
        apply_igdn_in_place(
            &mut values,
            2,
            IgdnParams {
                beta: &beta,
                gamma: &gamma,
            },
        )
        .unwrap();
        assert_eq!(values, [1.0, -2.0, 3.0, -4.0]);
    }

    #[test]
    fn igdn_uses_cross_channel_squared_energy() {
        let mut values = [3.0_f32, 4.0];
        let beta = [1.0_f32, 1.0];
        let gamma = [1.0_f32, 0.0, 0.0, 1.0];
        apply_igdn_in_place(
            &mut values,
            2,
            IgdnParams {
                beta: &beta,
                gamma: &gamma,
            },
        )
        .unwrap();
        assert!((values[0] - 3.0 * 10.0_f32.sqrt()).abs() < 1.0e-6);
        assert!((values[1] - 4.0 * 17.0_f32.sqrt()).abs() < 1.0e-6);
    }

    #[test]
    fn base_decoder_reaches_normative_1024_point_mdct_geometry() {
        let kernel_1 = vec![0.0_f32; 5 * 8 * 16];
        let kernel_2 = vec![0.0_f32; 5 * 4 * 8];
        let kernel_3 = vec![0.0_f32; 5 * 2 * 4];
        let kernel_4 = vec![0.0_f32; 5 * 1 * 2];
        let bias_1 = [1.0_f32; 8];
        let bias_2 = [2.0_f32; 4];
        let bias_3 = [3.0_f32; 2];
        let bias_4 = [4.0_f32; 1];
        let beta_1 = [1.0_f32; 8];
        let beta_2 = [1.0_f32; 4];
        let beta_3 = [1.0_f32; 2];
        let gamma_1 = [0.0_f32; 64];
        let gamma_2 = [0.0_f32; 16];
        let gamma_3 = [0.0_f32; 4];
        let params = BaseDecoderParams {
            layer_1: ConvTranspose1dParams {
                spec: BASE_LAYER_1_SPEC,
                kernel: &kernel_1,
                bias: &bias_1,
            },
            igdn_1: IgdnParams {
                beta: &beta_1,
                gamma: &gamma_1,
            },
            layer_2: ConvTranspose1dParams {
                spec: BASE_LAYER_2_SPEC,
                kernel: &kernel_2,
                bias: &bias_2,
            },
            igdn_2: IgdnParams {
                beta: &beta_2,
                gamma: &gamma_2,
            },
            layer_3: ConvTranspose1dParams {
                spec: BASE_LAYER_3_SPEC,
                kernel: &kernel_3,
                bias: &bias_3,
            },
            igdn_3: IgdnParams {
                beta: &beta_3,
                gamma: &gamma_3,
            },
            layer_4: ConvTranspose1dParams {
                spec: BASE_LAYER_4_SPEC,
                kernel: &kernel_4,
                bias: &bias_4,
            },
        };
        let input = vec![0.0_f32; BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS];
        let mut output = vec![0.0_f32; BASE_OUTPUT_POSITIONS];
        let mut workspace = BaseDecoderWorkspace::new();
        decode_base_network(&input, params, &mut workspace, &mut output).unwrap();
        assert!(output.iter().all(|&value| value == 4.0));
    }

    #[test]
    fn base_decoder_rejects_non_normative_layer_geometry() {
        let mut wrong = BASE_LAYER_1_SPEC;
        wrong.stride = 1;
        let kernel_1 = vec![0.0_f32; 5 * 8 * 16];
        let kernel_2 = vec![0.0_f32; 5 * 4 * 8];
        let kernel_3 = vec![0.0_f32; 5 * 2 * 4];
        let kernel_4 = vec![0.0_f32; 5 * 1 * 2];
        let bias_1 = [0.0_f32; 8];
        let bias_2 = [0.0_f32; 4];
        let bias_3 = [0.0_f32; 2];
        let bias_4 = [0.0_f32; 1];
        let beta_1 = [1.0_f32; 8];
        let beta_2 = [1.0_f32; 4];
        let beta_3 = [1.0_f32; 2];
        let gamma_1 = [0.0_f32; 64];
        let gamma_2 = [0.0_f32; 16];
        let gamma_3 = [0.0_f32; 4];
        let params = BaseDecoderParams {
            layer_1: ConvTranspose1dParams {
                spec: wrong,
                kernel: &kernel_1,
                bias: &bias_1,
            },
            igdn_1: IgdnParams {
                beta: &beta_1,
                gamma: &gamma_1,
            },
            layer_2: ConvTranspose1dParams {
                spec: BASE_LAYER_2_SPEC,
                kernel: &kernel_2,
                bias: &bias_2,
            },
            igdn_2: IgdnParams {
                beta: &beta_2,
                gamma: &gamma_2,
            },
            layer_3: ConvTranspose1dParams {
                spec: BASE_LAYER_3_SPEC,
                kernel: &kernel_3,
                bias: &bias_3,
            },
            igdn_3: IgdnParams {
                beta: &beta_3,
                gamma: &gamma_3,
            },
            layer_4: ConvTranspose1dParams {
                spec: BASE_LAYER_4_SPEC,
                kernel: &kernel_4,
                bias: &bias_4,
            },
        };
        let input = vec![0.0_f32; BASE_INPUT_POSITIONS * BASE_INPUT_CHANNELS];
        let mut output = vec![0.0_f32; BASE_OUTPUT_POSITIONS];
        let mut workspace = BaseDecoderWorkspace::new();
        assert!(decode_base_network(&input, params, &mut workspace, &mut output).is_err());
    }
}
