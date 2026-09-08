use yinqidao_codec_core::CodecError;

use crate::context_params::context_decoder_params;
use crate::quantizer_params::CONTEXT_QUANTILE_MEDIANS;
use crate::{
    BitRange, CONTEXT_CHANNELS, CONTEXT_INPUT_POSITIONS, CONTEXT_OUTPUT_POSITIONS,
    ContextDecoderParams, ContextDecoderWorkspace, decode_context_latents_into,
    decode_context_network, select_base_range_model_index,
};

const CONTEXT_INPUT_VALUES: usize = CONTEXT_INPUT_POSITIONS * CONTEXT_CHANNELS;
const CONTEXT_OUTPUT_VALUES: usize = CONTEXT_OUTPUT_POSITIONS * CONTEXT_CHANNELS;

#[derive(Clone, Copy, Debug)]
pub struct ContextModelParams<'a> {
    /// Per-context-channel scalar-quantizer median. Kept separate from the Annex-B neural tables
    /// because GY/T 363-2023 does not publish this interoperable hyper-prior model parameter.
    pub quantile_medians: &'a [f32],
    pub decoder: ContextDecoderParams<'a>,
}

/// Return the production/default context model without parsing an opaque model blob.
///
/// B.2..B.7 come from the normative Annex-B static tables while the sixteen quantizer medians are
/// the explicitly stored interoperable hyper-prior parameters. Every returned slice is `'static`;
/// constructing this value performs no allocation, copy or model parsing.
pub fn default_context_model_params() -> ContextModelParams<'static> {
    ContextModelParams {
        quantile_medians: &CONTEXT_QUANTILE_MEDIANS,
        decoder: context_decoder_params(),
    }
}

#[derive(Debug, Default)]
pub struct ContextPipelineWorkspace {
    quantized: Vec<i32>,
    dequantized: Vec<f32>,
    neural: ContextDecoderWorkspace,
}

impl ContextPipelineWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    fn prepare(&mut self) {
        self.quantized.resize(CONTEXT_INPUT_VALUES, 0);
        self.dequantized.resize(CONTEXT_INPUT_VALUES, 0.0);
    }

    pub fn quantized_latents(&self) -> &[i32] {
        &self.quantized
    }

    pub fn dequantized_latents(&self) -> &[f32] {
        &self.dequantized
    }
}

/// Inverse scalar quantization for a position-major latent tensor.
///
/// The quantizer is channel-wise, so adding each channel's median directly to the position-major
/// tensor is equivalent to transposing to channel-major layout, dequantizing, then transposing
/// back. Avoiding those transposes keeps the context path allocation- and copy-free once the
/// workspace has been initialized.
pub fn dequantize_context_latents_into(
    quantized: &[i32],
    quantile_medians: &[f32],
    output: &mut [f32],
) -> Result<(), CodecError> {
    if quantile_medians.len() != CONTEXT_CHANNELS {
        return Err(CodecError::InvalidData(
            "context quantizer requires sixteen channel medians",
        ));
    }
    if quantile_medians.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "context quantizer contains a non-finite median",
        ));
    }
    if quantized.len() != CONTEXT_INPUT_VALUES || output.len() != CONTEXT_INPUT_VALUES {
        return Err(CodecError::InvalidData(
            "context latent tensor must be sixteen positions by sixteen channels",
        ));
    }

    for (input_row, output_row) in quantized
        .chunks_exact(CONTEXT_CHANNELS)
        .zip(output.chunks_exact_mut(CONTEXT_CHANNELS))
    {
        for channel in 0..CONTEXT_CHANNELS {
            output_row[channel] = input_row[channel] as f32 + quantile_medians[channel];
        }
    }
    Ok(())
}

/// Decode one context range-coded block all the way to the 64x16 standard-deviation field used by
/// table B.8. The low-level entry point keeps the model injectable for differential tests and model
/// experiments; normal decoding should use [`default_context_model_params`].
pub fn decode_context_stddev_into(
    packet: &[u8],
    context_range: BitRange,
    params: ContextModelParams<'_>,
    workspace: &mut ContextPipelineWorkspace,
    output: &mut [f32],
) -> Result<(), CodecError> {
    if output.len() != CONTEXT_OUTPUT_VALUES {
        return Err(CodecError::InvalidData(
            "context standard-deviation output must be sixty-four positions by sixteen channels",
        ));
    }

    workspace.prepare();
    decode_context_latents_into(
        packet,
        context_range,
        CONTEXT_INPUT_POSITIONS,
        CONTEXT_CHANNELS,
        &mut workspace.quantized,
    )?;
    dequantize_context_latents_into(
        &workspace.quantized,
        params.quantile_medians,
        &mut workspace.dequantized,
    )?;
    decode_context_network(
        &workspace.dequantized,
        params.decoder,
        &mut workspace.neural,
        output,
    )?;

    if output
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(CodecError::InvalidData(
            "context decoder produced an invalid base-model standard deviation",
        ));
    }
    Ok(())
}

/// Convert the context decoder's 64x16 output to the exact B.9 row index for every base latent.
pub fn select_base_range_models_into(
    context_stddev: &[f32],
    model_indices: &mut [u8],
) -> Result<(), CodecError> {
    if context_stddev.len() != CONTEXT_OUTPUT_VALUES || model_indices.len() != CONTEXT_OUTPUT_VALUES
    {
        return Err(CodecError::InvalidData(
            "base range-model index tensor must be sixty-four positions by sixteen channels",
        ));
    }

    for (stddev, index) in context_stddev.iter().zip(model_indices) {
        *index = u8::try_from(select_base_range_model_index(*stddev)?).map_err(|_| {
            CodecError::InvalidData("base range-model index exceeds six-bit domain")
        })?;
    }
    Ok(())
}

/// Low-level injectable context path. It keeps all temporary storage in `workspace` and emits only
/// the B.9 row indices required for `baseBitstream`.
pub fn decode_context_and_select_base_models(
    packet: &[u8],
    context_range: BitRange,
    params: ContextModelParams<'_>,
    workspace: &mut ContextPipelineWorkspace,
    context_stddev: &mut [f32],
    model_indices: &mut [u8],
) -> Result<(), CodecError> {
    decode_context_stddev_into(packet, context_range, params, workspace, context_stddev)?;
    select_base_range_models_into(context_stddev, model_indices)
}

/// Production/default hyper-prior path: range-decode context latents, apply the explicit
/// interoperable quantizer medians, execute Annex-B B.2..B.7, and select the exact B.9 model row for
/// every base latent. No model parameter is supplied by the caller.
pub fn decode_context_and_select_base_models_default(
    packet: &[u8],
    context_range: BitRange,
    workspace: &mut ContextPipelineWorkspace,
    context_stddev: &mut [f32],
    model_indices: &mut [u8],
) -> Result<(), CodecError> {
    decode_context_and_select_base_models(
        packet,
        context_range,
        default_context_model_params(),
        workspace,
        context_stddev,
        model_indices,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CONTEXT_LAYER_1_KERNEL, CONTEXT_LAYER_1_SPEC, CONTEXT_LAYER_2_KERNEL, CONTEXT_LAYER_2_SPEC,
        CONTEXT_LAYER_3_KERNEL, CONTEXT_LAYER_3_SPEC, ConvTranspose1dParams,
    };

    #[test]
    fn dequantizes_position_major_tensor_without_transpose() {
        let mut quantized = [0_i32; CONTEXT_INPUT_VALUES];
        for position in 0..CONTEXT_INPUT_POSITIONS {
            for channel in 0..CONTEXT_CHANNELS {
                quantized[position * CONTEXT_CHANNELS + channel] = position as i32 - channel as i32;
            }
        }
        let medians: [f32; CONTEXT_CHANNELS] = std::array::from_fn(|channel| channel as f32 * 0.25);
        let mut output = [0.0_f32; CONTEXT_INPUT_VALUES];
        dequantize_context_latents_into(&quantized, &medians, &mut output).unwrap();
        assert_eq!(output[0], 0.0);
        assert_eq!(output[1], -0.75);
        assert_eq!(output[CONTEXT_CHANNELS], 1.0);
    }

    #[test]
    fn context_pipeline_reaches_b8_model_selection() {
        let kernel = vec![0.0_f32; 3 * CONTEXT_CHANNELS * CONTEXT_CHANNELS];
        let bias_zero = vec![0.0_f32; CONTEXT_CHANNELS];
        let bias_final = vec![3.0_f32; CONTEXT_CHANNELS];
        let decoder = ContextDecoderParams {
            layer_1: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_1_SPEC,
                kernel: &kernel,
                bias: &bias_zero,
            },
            layer_2: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_2_SPEC,
                kernel: &kernel,
                bias: &bias_zero,
            },
            layer_3: ConvTranspose1dParams {
                spec: CONTEXT_LAYER_3_SPEC,
                kernel: &kernel,
                bias: &bias_final,
            },
        };
        let medians = [0.0_f32; CONTEXT_CHANNELS];
        let params = ContextModelParams {
            quantile_medians: &medians,
            decoder,
        };
        let mut workspace = ContextPipelineWorkspace::new();
        let mut stddev = vec![0.0_f32; CONTEXT_OUTPUT_VALUES];
        let mut indices = vec![0_u8; CONTEXT_OUTPUT_VALUES];
        decode_context_and_select_base_models(
            &[],
            BitRange {
                bit_offset: 0,
                bit_len: 0,
            },
            params,
            &mut workspace,
            &mut stddev,
            &mut indices,
        )
        .unwrap();

        assert!(stddev.iter().all(|&value| value == 3.0));
        let expected = select_base_range_model_index(3.0).unwrap() as u8;
        assert!(indices.iter().all(|&index| index == expected));
        assert_eq!(workspace.quantized_latents().len(), CONTEXT_INPUT_VALUES);
        assert_eq!(workspace.dequantized_latents().len(), CONTEXT_INPUT_VALUES);
    }

    #[test]
    fn default_context_model_is_fully_static() {
        let params = default_context_model_params();
        assert_eq!(params.quantile_medians, &CONTEXT_QUANTILE_MEDIANS[..]);
        assert_eq!(params.decoder.layer_1.kernel, &CONTEXT_LAYER_1_KERNEL[..]);
        assert_eq!(params.decoder.layer_2.kernel, &CONTEXT_LAYER_2_KERNEL[..]);
        assert_eq!(params.decoder.layer_3.kernel, &CONTEXT_LAYER_3_KERNEL[..]);
    }

    #[test]
    fn rejects_negative_context_standard_deviation_before_b9_selection() {
        let mut indices = [0_u8; CONTEXT_OUTPUT_VALUES];
        let mut stddev = [0.0_f32; CONTEXT_OUTPUT_VALUES];
        stddev[7] = -0.1;
        assert!(select_base_range_models_into(&stddev, &mut indices).is_err());
    }
}
