use yinqidao_audio_simd::dot_product;
use yinqidao_codec_core::CodecError;

pub const CONTEXT_CHANNELS: usize = 16;
pub const CONTEXT_INPUT_POSITIONS: usize = 16;
pub const CONTEXT_OUTPUT_POSITIONS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeuralActivation {
    None,
    Relu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConvTranspose1dSpec {
    pub input_channels: usize,
    pub output_channels: usize,
    pub kernel_size: usize,
    pub stride: usize,
    pub activation: NeuralActivation,
}

#[derive(Clone, Copy, Debug)]
pub struct ConvTranspose1dParams<'a> {
    pub spec: ConvTranspose1dSpec,
    /// Position-major kernel layout: `[kernel_position][output_channel][input_channel]`.
    pub kernel: &'a [f32],
    pub bias: &'a [f32],
}

impl ConvTranspose1dSpec {
    pub fn output_positions(self, input_positions: usize) -> Result<usize, CodecError> {
        if self.kernel_size == 0 || self.kernel_size & 1 == 0 {
            return Err(CodecError::InvalidData(
                "transpose CNN requires a non-zero odd kernel size",
            ));
        }
        if self.stride == 0 {
            return Err(CodecError::InvalidData(
                "transpose CNN stride must be non-zero",
            ));
        }
        input_positions
            .checked_mul(self.stride)
            .ok_or(CodecError::InvalidData(
                "transpose CNN output geometry overflows address space",
            ))
    }

    fn expected_kernel_len(self) -> Result<usize, CodecError> {
        self.kernel_size
            .checked_mul(self.output_channels)
            .and_then(|value| value.checked_mul(self.input_channels))
            .ok_or(CodecError::InvalidData(
                "transpose CNN kernel geometry overflows address space",
            ))
    }
}

/// Execute one SAME-padded 1D transposed convolution.
///
/// Feature tensors are position-major (`position * channels + channel`). The inner channel
/// reduction uses the shared runtime-dispatched SIMD dot-product kernel, keeping architecture
/// intrinsics outside the codec crate. For an odd kernel the SAME crop is centered around
/// `kernel_size / 2`; stride two therefore doubles the feature dimension exactly.
pub fn conv1d_transpose_same(
    input: &[f32],
    input_positions: usize,
    params: ConvTranspose1dParams<'_>,
    output: &mut [f32],
) -> Result<(), CodecError> {
    let spec = params.spec;
    if spec.input_channels == 0 || spec.output_channels == 0 {
        return Err(CodecError::InvalidData(
            "transpose CNN channel count must be non-zero",
        ));
    }
    let expected_input = input_positions
        .checked_mul(spec.input_channels)
        .ok_or(CodecError::InvalidData(
            "transpose CNN input geometry overflows address space",
        ))?;
    if input.len() != expected_input {
        return Err(CodecError::InvalidData(
            "transpose CNN input length does not match geometry",
        ));
    }
    if params.kernel.len() != spec.expected_kernel_len()? {
        return Err(CodecError::InvalidData(
            "transpose CNN kernel length does not match geometry",
        ));
    }
    if params.bias.len() != spec.output_channels {
        return Err(CodecError::InvalidData(
            "transpose CNN bias length does not match output channels",
        ));
    }

    let output_positions = spec.output_positions(input_positions)?;
    let expected_output = output_positions
        .checked_mul(spec.output_channels)
        .ok_or(CodecError::InvalidData(
            "transpose CNN output geometry overflows address space",
        ))?;
    if output.len() != expected_output {
        return Err(CodecError::InvalidData(
            "transpose CNN output length does not match geometry",
        ));
    }

    for row in output.chunks_exact_mut(spec.output_channels) {
        row.copy_from_slice(params.bias);
    }

    let padding = spec.kernel_size / 2;
    for input_position in 0..input_positions {
        let input_row_start = input_position * spec.input_channels;
        let input_row = &input[input_row_start..input_row_start + spec.input_channels];
        let base_output = input_position
            .checked_mul(spec.stride)
            .ok_or(CodecError::InvalidData(
                "transpose CNN output position overflows address space",
            ))?;

        for kernel_position in 0..spec.kernel_size {
            let Some(output_position) = base_output
                .checked_add(kernel_position)
                .and_then(|value| value.checked_sub(padding))
            else {
                continue;
            };
            if output_position >= output_positions {
                continue;
            }

            let kernel_plane = kernel_position * spec.output_channels * spec.input_channels;
            let output_row = &mut output
                [output_position * spec.output_channels..(output_position + 1) * spec.output_channels];
            for (output_channel, value) in output_row.iter_mut().enumerate() {
                let kernel_start = kernel_plane + output_channel * spec.input_channels;
                let kernel_row = &params.kernel[kernel_start..kernel_start + spec.input_channels];
                *value += dot_product(input_row, kernel_row);
            }
        }
    }

    if spec.activation == NeuralActivation::Relu {
        for value in output {
            *value = value.max(0.0);
        }
    }
    Ok(())
}

pub const CONTEXT_LAYER_1_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: CONTEXT_CHANNELS,
    output_channels: CONTEXT_CHANNELS,
    kernel_size: 3,
    stride: 2,
    activation: NeuralActivation::Relu,
};
pub const CONTEXT_LAYER_2_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: CONTEXT_CHANNELS,
    output_channels: CONTEXT_CHANNELS,
    kernel_size: 3,
    stride: 2,
    activation: NeuralActivation::Relu,
};
pub const CONTEXT_LAYER_3_SPEC: ConvTranspose1dSpec = ConvTranspose1dSpec {
    input_channels: CONTEXT_CHANNELS,
    output_channels: CONTEXT_CHANNELS,
    kernel_size: 3,
    stride: 1,
    activation: NeuralActivation::None,
};

#[derive(Clone, Copy, Debug)]
pub struct ContextDecoderParams<'a> {
    pub layer_1: ConvTranspose1dParams<'a>,
    pub layer_2: ConvTranspose1dParams<'a>,
    pub layer_3: ConvTranspose1dParams<'a>,
}

#[derive(Debug, Default)]
pub struct ContextDecoderWorkspace {
    layer_1: Vec<f32>,
    layer_2: Vec<f32>,
}

impl ContextDecoderWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self) {
        self.layer_1.resize(32 * CONTEXT_CHANNELS, 0.0);
        self.layer_2.resize(CONTEXT_OUTPUT_POSITIONS * CONTEXT_CHANNELS, 0.0);
    }
}

/// Execute the normative three-layer AVS3 context decoder geometry: 16x16 -> 32x16 -> 64x16 ->
/// 64x16. B.2..B.7 supply the actual kernels/biases; this function intentionally owns no model
/// blob and performs no native-code calls.
pub fn decode_context_network(
    input: &[f32],
    params: ContextDecoderParams<'_>,
    workspace: &mut ContextDecoderWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if params.layer_1.spec != CONTEXT_LAYER_1_SPEC
        || params.layer_2.spec != CONTEXT_LAYER_2_SPEC
        || params.layer_3.spec != CONTEXT_LAYER_3_SPEC
    {
        return Err(CodecError::InvalidData(
            "context decoder layer spec does not match AVS3 table 15",
        ));
    }
    if input.len() != CONTEXT_INPUT_POSITIONS * CONTEXT_CHANNELS {
        return Err(CodecError::InvalidData(
            "context decoder input must be 16 positions by 16 channels",
        ));
    }
    if output.len() != CONTEXT_OUTPUT_POSITIONS * CONTEXT_CHANNELS {
        return Err(CodecError::InvalidData(
            "context decoder output must be 64 positions by 16 channels",
        ));
    }

    workspace.prepare();
    conv1d_transpose_same(
        input,
        CONTEXT_INPUT_POSITIONS,
        params.layer_1,
        &mut workspace.layer_1,
    )?;
    conv1d_transpose_same(
        &workspace.layer_1,
        32,
        params.layer_2,
        &mut workspace.layer_2,
    )?;
    conv1d_transpose_same(
        &workspace.layer_2,
        CONTEXT_OUTPUT_POSITIONS,
        params.layer_3,
        output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stride_two_same_geometry_has_defined_impulse_mapping() {
        let spec = ConvTranspose1dSpec {
            input_channels: 1,
            output_channels: 1,
            kernel_size: 3,
            stride: 2,
            activation: NeuralActivation::None,
        };
        let params = ConvTranspose1dParams {
            spec,
            kernel: &[1.0, 2.0, 3.0],
            bias: &[0.0],
        };
        let mut output = [0.0_f32; 4];
        conv1d_transpose_same(&[4.0, 5.0], 2, params, &mut output).unwrap();
        assert_eq!(output, [8.0, 17.0, 10.0, 15.0]);
    }

    #[test]
    fn relu_is_applied_after_bias_and_accumulation() {
        let spec = ConvTranspose1dSpec {
            input_channels: 1,
            output_channels: 1,
            kernel_size: 3,
            stride: 1,
            activation: NeuralActivation::Relu,
        };
        let params = ConvTranspose1dParams {
            spec,
            kernel: &[0.0; 3],
            bias: &[-2.0],
        };
        let mut output = [9.0_f32; 2];
        conv1d_transpose_same(&[1.0, -1.0], 2, params, &mut output).unwrap();
        assert_eq!(output, [0.0, 0.0]);
    }

    #[test]
    fn context_network_produces_exact_normative_geometry() {
        let kernel = vec![0.0_f32; 3 * CONTEXT_CHANNELS * CONTEXT_CHANNELS];
        let bias_1 = vec![1.0_f32; CONTEXT_CHANNELS];
        let bias_2 = vec![2.0_f32; CONTEXT_CHANNELS];
        let bias_3 = vec![3.0_f32; CONTEXT_CHANNELS];
        let params = ContextDecoderParams {
            layer_1: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_1_SPEC,
                kernel: &kernel,
                bias: &bias_1,
            },
            layer_2: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_2_SPEC,
                kernel: &kernel,
                bias: &bias_2,
            },
            layer_3: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_3_SPEC,
                kernel: &kernel,
                bias: &bias_3,
            },
        };
        let input = vec![0.0_f32; CONTEXT_INPUT_POSITIONS * CONTEXT_CHANNELS];
        let mut output = vec![0.0_f32; CONTEXT_OUTPUT_POSITIONS * CONTEXT_CHANNELS];
        let mut workspace = ContextDecoderWorkspace::new();
        decode_context_network(&input, params, &mut workspace, &mut output).unwrap();
        assert!(output.iter().all(|&value| value == 3.0));
    }
}
