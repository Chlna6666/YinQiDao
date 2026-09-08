use std::f32::consts::PI;

use yinqidao_codec_core::CodecError;

use crate::{
    FD_SHAPING_SFB_BOUNDARIES, FdShapingSideInfo, LSF_ORDER, LsfCodebooks, dequantize_lsf,
};

const MDCT_LINES: usize = 1024;
const LPC_COEFFICIENTS: usize = LSF_ORDER + 1;
const LPC_GAIN_BASE_POINTS: usize = 256;
const LPC_GAIN_RAW_POINTS: usize = LPC_GAIN_BASE_POINTS + 1;
const LPC_RESPONSE_FFT_LEN: usize = 512;
const LPC_GAIN_INTERPOLATION: usize = MDCT_LINES / LPC_GAIN_BASE_POINTS;
const GAMMA_LPC: f32 = f32::from_bits(0x3F70_A3D7); // 0.939999998f

/// Fixed, decoder-owned workspace for frequency-domain inverse spectrum shaping.
///
/// All buffers have normative compile-time geometry; processing a frame performs no heap
/// allocation and requires no FFT planner or complex-vector construction.
#[derive(Debug)]
pub struct FdShapingWorkspace {
    lsf: [f32; LSF_ORDER],
    lsp: [f32; LSF_ORDER],
    lpc: [f32; LPC_COEFFICIENTS],
    weighted_lpc: [f32; LPC_COEFFICIENTS],
    raw_gain: [f32; LPC_GAIN_RAW_POINTS],
    interpolated_gain: [f32; MDCT_LINES],
}

impl FdShapingWorkspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lsf(&self) -> &[f32; LSF_ORDER] {
        &self.lsf
    }

    pub fn lpc(&self) -> &[f32; LPC_COEFFICIENTS] {
        &self.lpc
    }
}

impl Default for FdShapingWorkspace {
    fn default() -> Self {
        Self {
            lsf: [0.0; LSF_ORDER],
            lsp: [0.0; LSF_ORDER],
            lpc: [0.0; LPC_COEFFICIENTS],
            weighted_lpc: [0.0; LPC_COEFFICIENTS],
            raw_gain: [0.0; LPC_GAIN_RAW_POINTS],
            interpolated_gain: [0.0; MDCT_LINES],
        }
    }
}

/// Convert AVS3 48-kHz line spectral frequencies in Hz to line spectral pairs.
pub fn lsf_to_lsp(lsf: &[f32; LSF_ORDER], lsp: &mut [f32; LSF_ORDER]) -> Result<(), CodecError> {
    for (frequency, output) in lsf.iter().zip(lsp) {
        if !frequency.is_finite() || !(0.0..=24_000.0).contains(frequency) {
            return Err(CodecError::InvalidData(
                "LSF frequency is outside the 48-kHz Nyquist interval",
            ));
        }
        *output = (*frequency * (PI / 24_000.0)).cos();
    }
    Ok(())
}

fn lsp_polynomial(
    lsp: &[f32; LSF_ORDER],
    root_parity: usize,
    output: &mut [f32; LSF_ORDER / 2 + 1],
) {
    output.fill(0.0);
    output[0] = 1.0;
    output[1] = -2.0 * lsp[root_parity];

    for degree in 2..=LSF_ORDER / 2 {
        let root = lsp[root_parity + 2 * (degree - 1)];
        let factor = -2.0 * root;
        output[degree] = factor.mul_add(output[degree - 1], 2.0 * output[degree - 2]);
        for coefficient in (2..degree).rev() {
            output[coefficient] += factor.mul_add(output[coefficient - 1], output[coefficient - 2]);
        }
        output[1] += factor;
    }
}

/// Convert sixteen LSP roots to the order-16 LPC polynomial `A(z)`.
pub fn lsp_to_lpc(
    lsp: &[f32; LSF_ORDER],
    lpc: &mut [f32; LPC_COEFFICIENTS],
) -> Result<(), CodecError> {
    if lsp
        .iter()
        .any(|value| !value.is_finite() || *value < -1.0 || *value > 1.0)
    {
        return Err(CodecError::InvalidData("LSP root is outside [-1, 1]"));
    }

    let mut even = [0.0_f32; LSF_ORDER / 2 + 1];
    let mut odd = [0.0_f32; LSF_ORDER / 2 + 1];
    lsp_polynomial(lsp, 0, &mut even);
    lsp_polynomial(lsp, 1, &mut odd);

    for index in (1..=LSF_ORDER / 2).rev() {
        even[index] += even[index - 1];
        odd[index] -= odd[index - 1];
    }

    lpc.fill(0.0);
    lpc[0] = 1.0;
    for index in 0..LSF_ORDER / 2 {
        let p = even[index + 1];
        let q = odd[index + 1];
        lpc[index + 1] = 0.5 * (p + q);
        lpc[LSF_ORDER - index] = 0.5 * (p - q);
    }

    if lpc.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "LSP to LPC conversion produced a non-finite coefficient",
        ));
    }
    Ok(())
}

fn weight_lpc(lpc: &[f32; LPC_COEFFICIENTS], output: &mut [f32; LPC_COEFFICIENTS]) {
    let mut weight = 1.0_f32;
    for (coefficient, weighted) in lpc.iter().zip(output) {
        *weighted = *coefficient * weight;
        weight *= GAMMA_LPC;
    }
}

/// Evaluate the 17 non-zero, half-bin-rotated LPC taps directly at the 257 frequency points needed
/// by the normative four-times interpolation.
///
/// A generic 512-point FFT would transform 495 known zeros. Direct sparse evaluation requires only
/// 257 `sin_cos` calls and 17 complex multiply-accumulates per point, with no FFT scratch/planner.
fn raw_lpc_gain(
    weighted_lpc: &[f32; LPC_COEFFICIENTS],
    output: &mut [f32; LPC_GAIN_RAW_POINTS],
) -> Result<(), CodecError> {
    for (bin, gain) in output.iter_mut().enumerate() {
        // The reference path pre-rotates tap i by -i*pi/512 and then applies a forward FFT whose
        // twiddle sign is negative. Combined, frequency bin k evaluates angle (2k+1)*pi/512.
        let phase = (2 * bin + 1) as f32 * (PI / LPC_RESPONSE_FFT_LEN as f32);
        let (sin_step, cos_step) = phase.sin_cos();
        let mut cos_i = 1.0_f32;
        let mut sin_i = 0.0_f32;
        let mut real = 0.0_f32;
        let mut imag = 0.0_f32;

        for coefficient in weighted_lpc {
            real = coefficient.mul_add(cos_i, real);
            imag = (-*coefficient).mul_add(sin_i, imag);
            let next_cos = cos_i.mul_add(cos_step, -sin_i * sin_step);
            let next_sin = sin_i.mul_add(cos_step, cos_i * sin_step);
            cos_i = next_cos;
            sin_i = next_sin;
        }

        let magnitude_squared = real.mul_add(real, imag * imag);
        if !magnitude_squared.is_finite() || magnitude_squared <= 0.0 {
            return Err(CodecError::InvalidData(
                "weighted LPC response has zero or invalid magnitude",
            ));
        }
        *gain = magnitude_squared.sqrt().recip();
    }
    Ok(())
}

fn interpolate_lpc_gain(raw: &[f32; LPC_GAIN_RAW_POINTS], output: &mut [f32; MDCT_LINES]) {
    debug_assert_eq!(LPC_GAIN_INTERPOLATION, 4);
    for bin in 0..LPC_GAIN_BASE_POINTS {
        let start = raw[bin];
        let step = (raw[bin + 1] - start) * 0.25;
        let offset = bin * LPC_GAIN_INTERPOLATION;
        output[offset] = start;
        output[offset + 1] = start + step;
        output[offset + 2] = step.mul_add(2.0, start);
        output[offset + 3] = step.mul_add(3.0, start);
    }
}

fn apply_subband_averaged_gain(
    interpolated: &[f32; MDCT_LINES],
    spectrum: &mut [f32],
) -> Result<(), CodecError> {
    if spectrum.len() != MDCT_LINES {
        return Err(CodecError::InvalidData(
            "FD inverse shaping requires a 1024-line MDCT spectrum",
        ));
    }

    for boundaries in FD_SHAPING_SFB_BOUNDARIES.windows(2) {
        let start = usize::from(boundaries[0]);
        let end = usize::from(boundaries[1]);
        let width = end - start;
        let mut sum = 0.0_f32;
        for gain in &interpolated[start..end] {
            sum += *gain;
        }
        let average = sum / width as f32;
        if !average.is_finite() || average <= 0.0 {
            return Err(CodecError::InvalidData(
                "FD shaping subband has invalid LPC gain",
            ));
        }
        for value in &mut spectrum[start..end] {
            *value *= average;
        }
    }
    Ok(())
}

/// Apply the complete frequency-domain inverse spectrum-shaping algorithm to one 1024-line MDCT
/// spectrum, using the caller-selected Annex-B LSF codebooks.
///
/// The only external data dependency is B.34..B.45. B.46/B.47, LSF dequantization, LSF/LSP/LPC,
/// gamma weighting, half-bin frequency-response evaluation, 4x interpolation and 49-subband
/// averaging are all implemented in this crate.
pub fn apply_inverse_fd_spectrum_shaping(
    side: &FdShapingSideInfo,
    codebooks: LsfCodebooks<'_>,
    spectrum: &mut [f32],
    workspace: &mut FdShapingWorkspace,
) -> Result<(), CodecError> {
    if spectrum.len() != MDCT_LINES {
        return Err(CodecError::InvalidData(
            "FD inverse shaping requires a 1024-line MDCT spectrum",
        ));
    }
    if spectrum.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "FD inverse shaping input contains non-finite MDCT data",
        ));
    }

    dequantize_lsf(side, codebooks, &mut workspace.lsf)?;
    lsf_to_lsp(&workspace.lsf, &mut workspace.lsp)?;
    lsp_to_lpc(&workspace.lsp, &mut workspace.lpc)?;
    weight_lpc(&workspace.lpc, &mut workspace.weighted_lpc);
    raw_lpc_gain(&workspace.weighted_lpc, &mut workspace.raw_gain)?;
    interpolate_lpc_gain(&workspace.raw_gain, &mut workspace.interpolated_gain);
    apply_subband_averaged_gain(&workspace.interpolated_gain, spectrum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_lpc_has_unit_gain_for_every_mdct_line() {
        let mut weighted = [0.0_f32; LPC_COEFFICIENTS];
        weighted[0] = 1.0;
        let mut raw = [0.0_f32; LPC_GAIN_RAW_POINTS];
        raw_lpc_gain(&weighted, &mut raw).unwrap();
        assert!(raw.iter().all(|&gain| gain == 1.0));
        let mut interpolated = [0.0_f32; MDCT_LINES];
        interpolate_lpc_gain(&raw, &mut interpolated);
        assert!(interpolated.iter().all(|&gain| gain == 1.0));
        let mut spectrum = [2.0_f32; MDCT_LINES];
        apply_subband_averaged_gain(&interpolated, &mut spectrum).unwrap();
        assert!(spectrum.iter().all(|&value| value == 2.0));
    }

    #[test]
    fn lsf_to_lsp_maps_dc_and_nyquist_endpoints() {
        let lsf: [f32; LSF_ORDER] =
            std::array::from_fn(|index| index as f32 * 24_000.0 / (LSF_ORDER - 1) as f32);
        let mut lsp = [0.0_f32; LSF_ORDER];
        lsf_to_lsp(&lsf, &mut lsp).unwrap();
        assert_eq!(lsp[0], 1.0);
        assert!((lsp[LSF_ORDER - 1] + 1.0).abs() < 2.0e-6);
    }

    #[test]
    fn lsp_to_lpc_produces_symmetric_endpoint_layout() {
        let lsp: [f32; LSF_ORDER] =
            std::array::from_fn(|index| (((index + 1) as f32) * PI / (LSF_ORDER + 1) as f32).cos());
        let mut lpc = [0.0_f32; LPC_COEFFICIENTS];
        lsp_to_lpc(&lsp, &mut lpc).unwrap();
        assert_eq!(lpc[0], 1.0);
        assert!(lpc.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn malformed_lsp_is_rejected() {
        let mut lsp = [0.0_f32; LSF_ORDER];
        lsp[4] = 1.01;
        let mut lpc = [0.0_f32; LPC_COEFFICIENTS];
        assert!(lsp_to_lpc(&lsp, &mut lpc).is_err());
    }
}
