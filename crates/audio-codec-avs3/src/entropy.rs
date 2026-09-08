use yinqidao_codec_core::CodecError;

use crate::{
    BASE_RANGE_MODEL_COUNT, BitRange, CONTEXT_RANGE_MODEL_COUNT, RangeByteWindow, RangeDecoder,
    base_range_model, context_range_model,
};

/// Decode the context/hyper-prior latent in the normative flattened order:
/// `latent_position * latent_channels + channel`.
///
/// The context model has exactly 16 range distributions (table B.1), selected directly by latent
/// channel index. The caller supplies the actual latent geometry from the neural model; this layer
/// only performs entropy decoding and does not depend on CNN weights or quantizer storage.
pub fn decode_context_latents(
    packet: &[u8],
    range: BitRange,
    latent_positions: usize,
    latent_channels: usize,
) -> Result<Vec<i32>, CodecError> {
    let len = latent_positions
        .checked_mul(latent_channels)
        .ok_or(CodecError::InvalidData(
            "context latent geometry overflows address space",
        ))?;
    let mut output = vec![0_i32; len];
    decode_context_latents_into(
        packet,
        range,
        latent_positions,
        latent_channels,
        &mut output,
    )?;
    Ok(output)
}

/// Allocation-free variant for reusable decoder workspaces.
pub fn decode_context_latents_into(
    packet: &[u8],
    range: BitRange,
    latent_positions: usize,
    latent_channels: usize,
    output: &mut [i32],
) -> Result<(), CodecError> {
    if latent_channels == 0 || latent_channels > CONTEXT_RANGE_MODEL_COUNT {
        return Err(CodecError::InvalidData(
            "context latent channel count must be between one and sixteen",
        ));
    }
    let expected = latent_positions
        .checked_mul(latent_channels)
        .ok_or(CodecError::InvalidData(
            "context latent geometry overflows address space",
        ))?;
    if output.len() != expected {
        return Err(CodecError::InvalidData(
            "context latent output length does not match geometry",
        ));
    }

    let input = RangeByteWindow::new(packet, range)?;
    let mut decoder = RangeDecoder::new(input);
    for position in 0..latent_positions {
        let row = &mut output[position * latent_channels..(position + 1) * latent_channels];
        for (channel, value) in row.iter_mut().enumerate() {
            *value = decoder.decode_value(context_range_model(channel)?)?;
        }
    }
    Ok(())
}

/// Decode the base/VAE latent stream with the per-value B.9 row indices predicted by the context
/// decoder and table-B.8 selector.
pub fn decode_base_latents(
    packet: &[u8],
    range: BitRange,
    model_indices: &[u8],
) -> Result<Vec<i32>, CodecError> {
    let mut output = vec![0_i32; model_indices.len()];
    decode_base_latents_into(packet, range, model_indices, &mut output)?;
    Ok(output)
}

/// Allocation-free base entropy decoder for reusable frame workspaces.
pub fn decode_base_latents_into(
    packet: &[u8],
    range: BitRange,
    model_indices: &[u8],
    output: &mut [i32],
) -> Result<(), CodecError> {
    if output.len() != model_indices.len() {
        return Err(CodecError::InvalidData(
            "base latent output length does not match model-index tensor",
        ));
    }
    if model_indices
        .iter()
        .any(|&index| usize::from(index) >= BASE_RANGE_MODEL_COUNT)
    {
        return Err(CodecError::InvalidData(
            "base latent model index exceeds table B.9",
        ));
    }

    let input = RangeByteWindow::new(packet, range)?;
    let mut decoder = RangeDecoder::new(input);
    for (slot, &index) in output.iter_mut().zip(model_indices) {
        *slot = decoder.decode_value(base_range_model(usize::from(index))?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BASE_RANGE_MODEL_OFFSETS;

    #[test]
    fn zero_extended_stream_decodes_first_symbol_of_each_context_model() {
        let decoded = decode_context_latents(
            &[],
            BitRange {
                bit_offset: 0,
                bit_len: 0,
            },
            1,
            16,
        )
        .unwrap();

        let mut expected = vec![-1; 16];
        expected[4] = -19;
        expected[12] = -7;
        assert_eq!(decoded, expected);
    }

    #[test]
    fn repeats_context_model_selection_for_each_latent_position() {
        let mut decoded = [0_i32; 8];
        decode_context_latents_into(
            &[],
            BitRange {
                bit_offset: 0,
                bit_len: 0,
            },
            2,
            4,
            &mut decoded,
        )
        .unwrap();
        assert_eq!(decoded, [-1; 8]);
    }

    #[test]
    fn validates_latent_geometry_before_range_decode() {
        let range = BitRange {
            bit_offset: 0,
            bit_len: 0,
        };
        assert!(decode_context_latents(&[], range, 1, 0).is_err());
        assert!(decode_context_latents(&[], range, 1, 17).is_err());

        let mut output = [0_i32; 3];
        assert!(decode_context_latents_into(&[], range, 1, 4, &mut output).is_err());
    }

    #[test]
    fn zero_extended_base_stream_uses_exact_b9_offsets() {
        let indices = [0_u8, 10, 57, 59, 63];
        let decoded = decode_base_latents(
            &[],
            BitRange {
                bit_offset: 0,
                bit_len: 0,
            },
            &indices,
        )
        .unwrap();
        let expected = indices.map(|index| BASE_RANGE_MODEL_OFFSETS[usize::from(index)]);
        assert_eq!(decoded.as_slice(), &expected);
    }

    #[test]
    fn base_decoder_validates_shape_and_model_indices_before_decode() {
        let range = BitRange {
            bit_offset: 0,
            bit_len: 0,
        };
        let mut wrong_len = [0_i32; 1];
        assert!(decode_base_latents_into(&[], range, &[0, 1], &mut wrong_len).is_err());

        let mut output = [0_i32; 1];
        assert!(decode_base_latents_into(&[], range, &[64], &mut output).is_err());
    }
}
