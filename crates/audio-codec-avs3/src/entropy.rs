use yinqidao_codec_core::CodecError;

use crate::{
    BitRange, CONTEXT_RANGE_MODEL_COUNT, RangeByteWindow, RangeDecoder, context_range_model,
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
    decode_context_latents_into(packet, range, latent_positions, latent_channels, &mut output)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
