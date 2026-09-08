use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use yinqidao_codec_core::CodecError;

pub const HOA_TRANSFORM_LEN: usize = 1_024;
pub const HOA_TRANSFORM_BINS: usize = HOA_TRANSFORM_LEN / 2;
const HOA_COMPLEX_FFT_LEN: usize = HOA_TRANSFORM_LEN / 4;
const TWIDDLE_OFFSET: f32 = 0.125;
const HOA_FORWARD_SCALE: f64 = 32.0;
const HOA_INVERSE_SCALE: f32 = 0.0625;

/// Reusable 1024-sample forward/inverse MDCT pair used by the HOA 512-hop post filter.
///
/// AVS3's HOA post stage uses a half-frame transform distinct from the ordinary 2048-point core
/// IMDCT. FFT plans, scratch and fixed twiddle seeds are retained across frames; transform calls do
/// not allocate, call trigonometric functions or compute square roots.
pub struct HoaTransformWorkspace {
    forward_fft: Arc<dyn Fft<f32>>,
    inverse_fft: Arc<dyn Fft<f32>>,
    work: [Complex<f32>; HOA_COMPLEX_FFT_LEN],
    scratch: Vec<Complex<f32>>,
    cosine_step: f32,
    sine_step: f32,
    first_cosine: f32,
    first_sine: f32,
}

impl HoaTransformWorkspace {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let forward_fft = planner.plan_fft_forward(HOA_COMPLEX_FFT_LEN);
        let inverse_fft = planner.plan_fft_inverse(HOA_COMPLEX_FFT_LEN);
        let scratch_len = forward_fft
            .get_inplace_scratch_len()
            .max(inverse_fft.get_inplace_scratch_len());
        let frequency = 2.0_f32 * core::f32::consts::PI / HOA_TRANSFORM_LEN as f32;
        let first_angle = frequency * TWIDDLE_OFFSET;
        Self {
            forward_fft,
            inverse_fft,
            work: [Complex::new(0.0, 0.0); HOA_COMPLEX_FFT_LEN],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            cosine_step: f64::from(frequency).cos() as f32,
            sine_step: f64::from(frequency).sin() as f32,
            first_cosine: f64::from(first_angle).cos() as f32,
            first_sine: f64::from(first_angle).sin() as f32,
        }
    }

    pub fn forward(&mut self, signal: &[f32], output: &mut [f32]) -> Result<(), CodecError> {
        if signal.len() != HOA_TRANSFORM_LEN || output.len() != HOA_TRANSFORM_BINS {
            return Err(CodecError::InvalidData(
                "HOA forward MDCT requires 1024 samples and 512 coefficients",
            ));
        }
        if signal.iter().any(|value| !value.is_finite()) {
            return Err(CodecError::InvalidData(
                "HOA forward MDCT input contains non-finite samples",
            ));
        }

        let mut cosine = self.first_cosine;
        let mut sine = self.first_sine;
        for (index, value) in self.work.iter_mut().enumerate() {
            let folded = HOA_TRANSFORM_LEN / 2 - 1 - 2 * index;
            let real = if index < HOA_TRANSFORM_LEN / 8 {
                signal[HOA_TRANSFORM_LEN / 4 + folded]
                    + signal[HOA_TRANSFORM_LEN + HOA_TRANSFORM_LEN / 4 - 1 - folded]
            } else {
                signal[HOA_TRANSFORM_LEN / 4 + folded] - signal[HOA_TRANSFORM_LEN / 4 - 1 - folded]
            };
            let unfolded = 2 * index;
            let imaginary = if index < HOA_TRANSFORM_LEN / 8 {
                signal[HOA_TRANSFORM_LEN / 4 + unfolded]
                    - signal[HOA_TRANSFORM_LEN / 4 - 1 - unfolded]
            } else {
                signal[HOA_TRANSFORM_LEN / 4 + unfolded]
                    + signal[HOA_TRANSFORM_LEN + HOA_TRANSFORM_LEN / 4 - 1 - unfolded]
            };
            value.re = real.mul_add(cosine, imaginary * sine);
            value.im = imaginary.mul_add(cosine, -real * sine);

            let old_cosine = cosine;
            cosine = cosine.mul_add(self.cosine_step, -sine * self.sine_step);
            sine = sine.mul_add(self.cosine_step, old_cosine * self.sine_step);
        }

        let scratch_len = self.forward_fft.get_inplace_scratch_len();
        self.forward_fft
            .process_with_scratch(&mut self.work, &mut self.scratch[..scratch_len]);

        cosine = self.first_cosine;
        sine = self.first_sine;
        for (index, value) in self.work.iter().enumerate() {
            let real = 2.0_f32 * value.re.mul_add(cosine, value.im * sine);
            let imaginary = 2.0_f32 * value.im.mul_add(cosine, -value.re * sine);
            output[2 * index] = (-f64::from(real) / HOA_FORWARD_SCALE) as f32;
            output[HOA_TRANSFORM_BINS - 1 - 2 * index] =
                (f64::from(imaginary) / HOA_FORWARD_SCALE) as f32;

            let old_cosine = cosine;
            cosine = cosine.mul_add(self.cosine_step, -sine * self.sine_step);
            sine = sine.mul_add(self.cosine_step, old_cosine * self.sine_step);
        }
        Ok(())
    }

    pub fn inverse(&mut self, coefficients: &[f32], signal: &mut [f32]) -> Result<(), CodecError> {
        if coefficients.len() != HOA_TRANSFORM_BINS || signal.len() != HOA_TRANSFORM_LEN {
            return Err(CodecError::InvalidData(
                "HOA inverse MDCT requires 512 coefficients and 1024 samples",
            ));
        }
        if coefficients.iter().any(|value| !value.is_finite()) {
            return Err(CodecError::InvalidData(
                "HOA inverse MDCT input contains non-finite coefficients",
            ));
        }

        let mut cosine = self.first_cosine;
        let mut sine = self.first_sine;
        for (index, value) in self.work.iter_mut().enumerate() {
            let real = -coefficients[2 * index];
            let imaginary = coefficients[HOA_TRANSFORM_BINS - 1 - 2 * index];
            value.re = real.mul_add(cosine, -imaginary * sine);
            value.im = imaginary.mul_add(cosine, real * sine);

            let old_cosine = cosine;
            cosine = cosine.mul_add(self.cosine_step, -sine * self.sine_step);
            sine = sine.mul_add(self.cosine_step, old_cosine * self.sine_step);
        }

        let scratch_len = self.inverse_fft.get_inplace_scratch_len();
        self.inverse_fft
            .process_with_scratch(&mut self.work, &mut self.scratch[..scratch_len]);

        signal.fill(0.0);
        cosine = self.first_cosine;
        sine = self.first_sine;
        for (index, value) in self.work.iter().enumerate() {
            let real = HOA_INVERSE_SCALE * value.re.mul_add(cosine, -value.im * sine);
            let imaginary = HOA_INVERSE_SCALE * value.im.mul_add(cosine, value.re * sine);

            signal[HOA_TRANSFORM_LEN / 2 + HOA_TRANSFORM_LEN / 4 - 1 - 2 * index] = real;
            if index < HOA_TRANSFORM_LEN / 8 {
                signal[HOA_TRANSFORM_LEN / 2 + HOA_TRANSFORM_LEN / 4 + 2 * index] = real;
            } else {
                signal[2 * index - HOA_TRANSFORM_LEN / 4] = -real;
            }

            signal[HOA_TRANSFORM_LEN / 4 + 2 * index] = imaginary;
            if index < HOA_TRANSFORM_LEN / 8 {
                signal[HOA_TRANSFORM_LEN / 4 - 1 - 2 * index] = -imaginary;
            } else {
                signal[HOA_TRANSFORM_LEN / 4 + HOA_TRANSFORM_LEN - 1 - 2 * index] = imaginary;
            }

            let old_cosine = cosine;
            cosine = cosine.mul_add(self.cosine_step, -sine * self.sine_step);
            sine = sine.mul_add(self.cosine_step, old_cosine * self.sine_step);
        }
        if signal.iter().any(|value| !value.is_finite()) {
            return Err(CodecError::InvalidData(
                "HOA inverse MDCT produced non-finite samples",
            ));
        }
        Ok(())
    }
}

impl Default for HoaTransformWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for HoaTransformWorkspace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HoaTransformWorkspace")
            .field("fft_len", &HOA_COMPLEX_FFT_LEN)
            .field("scratch_values", &self.scratch.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_signal_stays_zero_through_both_hoa_transforms() {
        let mut workspace = HoaTransformWorkspace::new();
        let signal = [0.0_f32; HOA_TRANSFORM_LEN];
        let mut coefficients = [1.0_f32; HOA_TRANSFORM_BINS];
        workspace.forward(&signal, &mut coefficients).unwrap();
        assert!(coefficients.iter().all(|value| *value == 0.0));

        let mut reconstructed = [1.0_f32; HOA_TRANSFORM_LEN];
        workspace
            .inverse(&coefficients, &mut reconstructed)
            .unwrap();
        assert!(reconstructed.iter().all(|value| *value == 0.0));
    }
}
