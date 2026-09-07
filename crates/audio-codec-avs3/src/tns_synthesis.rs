use yinqidao_codec_core::CodecError;

use crate::{TnsSideInfo, TransformType, reflection_coefficient};

const MDCT_LINES: usize = 1024;
const SHORT_BLOCKS: usize = 8;
const SHORT_LINES: usize = MDCT_LINES / SHORT_BLOCKS;
const MAX_ORDER: usize = 8;

// GY/T 363-2023 7.10.3.3: [660 Hz, 5400 Hz] and [5400 Hz, 20000 Hz]. At the normative
// 48-kHz/1024-line transform, floor(2 * 1024 * f / 48000) gives these line boundaries.
const TNS_FILTER_RANGES: [(usize, usize); 2] = [(28, 230), (230, 853)];

/// Reusable short-window reorder scratch for inverse TNS.
#[derive(Debug)]
pub struct TnsSynthesisWorkspace {
    reordered: [f32; MDCT_LINES],
}

impl TnsSynthesisWorkspace {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for TnsSynthesisWorkspace {
    fn default() -> Self {
        Self {
            reordered: [0.0; MDCT_LINES],
        }
    }
}

fn short_deinterleave(spectrum: &mut [f32], scratch: &mut [f32; MDCT_LINES]) {
    for block in 0..SHORT_BLOCKS {
        for line in 0..SHORT_LINES {
            scratch[block * SHORT_LINES + line] = spectrum[block + SHORT_BLOCKS * line];
        }
    }
    spectrum.copy_from_slice(scratch);
}

fn short_interleave(spectrum: &mut [f32], scratch: &mut [f32; MDCT_LINES]) {
    for block in 0..SHORT_BLOCKS {
        for line in 0..SHORT_LINES {
            scratch[block + SHORT_BLOCKS * line] = spectrum[block * SHORT_LINES + line];
        }
    }
    spectrum.copy_from_slice(scratch);
}

#[inline]
fn inverse_lattice_sample(
    mut value: f32,
    coefficients: &[f32; MAX_ORDER],
    order: usize,
    state: &mut [f32; MAX_ORDER],
) -> f32 {
    value -= coefficients[order - 1] * state[order - 1];
    for index in (0..order - 1).rev() {
        value -= coefficients[index] * state[index];
        state[index + 1] = coefficients[index].mul_add(value, state[index]);
    }
    state[0] = value;
    value
}

/// Apply AVS3 inverse temporal-noise-shaping to one 1024-line MDCT spectrum.
///
/// The two filter groups are processed from the high-frequency band to the low-frequency band with
/// one zero-initialized lattice history, matching the decoder filter cascade. For short windows the
/// spectrum is deinterleaved before TNS and interleaved again afterwards. No heap allocation occurs.
pub fn apply_inverse_tns(
    side: &TnsSideInfo,
    transform_type: TransformType,
    spectrum: &mut [f32],
    workspace: &mut TnsSynthesisWorkspace,
) -> Result<(), CodecError> {
    if spectrum.len() != MDCT_LINES {
        return Err(CodecError::InvalidData(
            "inverse TNS requires a 1024-line MDCT spectrum",
        ));
    }

    let is_short = transform_type == TransformType::Short;
    if is_short {
        short_deinterleave(spectrum, &mut workspace.reordered);
    }

    let mut state = [0.0_f32; MAX_ORDER];
    let mut coefficients = [0.0_f32; MAX_ORDER];

    for filter_index in (0..side.filters.len()).rev() {
        let filter = side.filters[filter_index];
        if !filter.enabled {
            continue;
        }
        let order = usize::from(filter.order);
        if !(1..=MAX_ORDER).contains(&order) {
            return Err(CodecError::InvalidData(
                "enabled TNS filter order must be in 1..=8",
            ));
        }

        coefficients.fill(0.0);
        for (dimension, coefficient) in coefficients.iter_mut().take(order).enumerate() {
            let index = filter.quant_indices[dimension].ok_or(CodecError::InvalidData(
                "enabled TNS filter is missing a reflection-coefficient index",
            ))?;
            *coefficient = reflection_coefficient(index)?;
        }

        let (start, end) = TNS_FILTER_RANGES[filter_index];
        for value in &mut spectrum[start..end] {
            *value = inverse_lattice_sample(*value, &coefficients, order, &mut state);
        }
    }

    if is_short {
        short_interleave(spectrum, &mut workspace.reordered);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TnsFilterSideInfo;

    fn disabled_filter() -> TnsFilterSideInfo {
        TnsFilterSideInfo {
            enabled: false,
            order: 0,
            quant_indices: [None; MAX_ORDER],
        }
    }

    #[test]
    fn disabled_tns_is_bit_exact_noop_for_long_and_short() {
        let side = TnsSideInfo {
            filters: [disabled_filter(), disabled_filter()],
            next_bit_offset: 0,
        };
        let input: [f32; MDCT_LINES] = std::array::from_fn(|i| i as f32 * 0.125 - 31.0);
        for transform in [TransformType::Long, TransformType::Short] {
            let mut spectrum = input;
            let mut workspace = TnsSynthesisWorkspace::new();
            apply_inverse_tns(&side, transform, &mut spectrum, &mut workspace).unwrap();
            assert_eq!(spectrum, input);
        }
    }

    #[test]
    fn order_one_filter_matches_standard_iir_recurrence() {
        let mut low = disabled_filter();
        low.enabled = true;
        low.order = 1;
        low.quant_indices[0] = Some(10); // B.33: +0.18374951...
        let side = TnsSideInfo {
            filters: [low, disabled_filter()],
            next_bit_offset: 0,
        };
        let coefficient = reflection_coefficient(10).unwrap();
        let mut spectrum = [0.0_f32; MDCT_LINES];
        spectrum[28] = 1.0;
        spectrum[29] = 0.5;
        let mut workspace = TnsSynthesisWorkspace::new();
        apply_inverse_tns(&side, TransformType::Long, &mut spectrum, &mut workspace).unwrap();
        assert_eq!(spectrum[28], 1.0);
        assert!((spectrum[29] - (0.5 - coefficient)).abs() < 1.0e-6);
    }

    #[test]
    fn rejects_malformed_enabled_filter_without_index() {
        let mut low = disabled_filter();
        low.enabled = true;
        low.order = 1;
        let side = TnsSideInfo {
            filters: [low, disabled_filter()],
            next_bit_offset: 0,
        };
        let mut spectrum = [0.0_f32; MDCT_LINES];
        let mut workspace = TnsSynthesisWorkspace::new();
        assert!(apply_inverse_tns(
            &side,
            TransformType::Long,
            &mut spectrum,
            &mut workspace,
        )
        .is_err());
    }
}
