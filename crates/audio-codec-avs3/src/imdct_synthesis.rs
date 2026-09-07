use std::{f32::consts::PI, fmt, sync::Arc};

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use yinqidao_codec_core::CodecError;

use crate::{TransformType, synthesis::{apply_window_in_place, overlap_add}};

const FRAME_LINES: usize = 1024;
const LONG_TIME_LEN: usize = FRAME_LINES * 2;
const SHORT_BLOCKS: usize = 8;
const SHORT_LINES: usize = FRAME_LINES / SHORT_BLOCKS;
const SHORT_TIME_LEN: usize = SHORT_LINES * 2;
const LONG_IFFT_LEN: usize = LONG_TIME_LEN / 4;
const SHORT_IFFT_LEN: usize = SHORT_TIME_LEN / 4;
const TRANSITION_PADDING: usize = 448;

/// Decoder-owned AVS3 IMDCT/window/overlap-add state for one coded channel.
///
/// FFT plans, FFT scratch, sine windows and all transform buffers are retained across frames.
/// The hot path performs no heap allocation. Short-window synthesis consumes the 8-way
/// frequency-interleaved spectrum directly, avoiding the reference decoder's extra 1024-float
/// `MdctSpectrumDeinterleave` copy before the eight 128-line transforms.
pub struct Avs3SynthesisWorkspace {
    long_ifft: Arc<dyn Fft<f32>>,
    short_ifft: Arc<dyn Fft<f32>>,
    fft_buffer: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    long_time: [f32; LONG_TIME_LEN],
    short_spectrum: [f32; SHORT_LINES],
    short_time: [f32; SHORT_TIME_LEN],
    short_output: [f32; FRAME_LINES],
    short_overlap: [f32; SHORT_LINES],
    overlap: [f32; FRAME_LINES],
    long_left: [f32; FRAME_LINES],
    long_right: [f32; FRAME_LINES],
    short_left: [f32; SHORT_LINES],
    short_right: [f32; SHORT_LINES],
}

impl Avs3SynthesisWorkspace {
    pub fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let long_ifft = planner.plan_fft_inverse(LONG_IFFT_LEN);
        let short_ifft = planner.plan_fft_inverse(SHORT_IFFT_LEN);
        let scratch_len = long_ifft
            .get_inplace_scratch_len()
            .max(short_ifft.get_inplace_scratch_len());
        let long_left = std::array::from_fn(|index| {
            ((PI / (2.0 * FRAME_LINES as f32)) * (index as f32 + 0.5)).sin()
        });
        let short_left = std::array::from_fn(|index| {
            ((PI / (2.0 * SHORT_LINES as f32)) * (index as f32 + 0.5)).sin()
        });
        let long_right = std::array::from_fn(|index| long_left[FRAME_LINES - 1 - index]);
        let short_right = std::array::from_fn(|index| short_left[SHORT_LINES - 1 - index]);

        Self {
            long_ifft,
            short_ifft,
            fft_buffer: vec![Complex::new(0.0, 0.0); LONG_IFFT_LEN],
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            long_time: [0.0; LONG_TIME_LEN],
            short_spectrum: [0.0; SHORT_LINES],
            short_time: [0.0; SHORT_TIME_LEN],
            short_output: [0.0; FRAME_LINES],
            short_overlap: [0.0; SHORT_LINES],
            overlap: [0.0; FRAME_LINES],
            long_left,
            long_right,
            short_left,
            short_right,
        }
    }

    pub fn reset(&mut self) {
        self.overlap.fill(0.0);
        self.short_overlap.fill(0.0);
    }

    pub fn overlap(&self) -> &[f32; FRAME_LINES] {
        &self.overlap
    }

    fn synthesize_long_like(
        &mut self,
        transform_type: TransformType,
        spectrum: &[f32],
        output: &mut [f32],
    ) -> Result<(), CodecError> {
        inverse_mdct_into(
            spectrum,
            &mut self.long_time,
            &self.long_ifft,
            &mut self.fft_buffer,
            &mut self.fft_scratch,
        )?;

        match transform_type {
            TransformType::Long => {
                apply_window_in_place(&mut self.long_time[..FRAME_LINES], &self.long_left);
                apply_window_in_place(&mut self.long_time[FRAME_LINES..], &self.long_right);
            }
            TransformType::CutIn => {
                apply_window_in_place(&mut self.long_time[..FRAME_LINES], &self.long_left);
                let short_start = FRAME_LINES + TRANSITION_PADDING;
                let short_end = short_start + SHORT_LINES;
                apply_window_in_place(&mut self.long_time[short_start..short_end], &self.short_right);
                self.long_time[short_end..].fill(0.0);
            }
            TransformType::CutOut => {
                self.long_time[..TRANSITION_PADDING].fill(0.0);
                let short_end = TRANSITION_PADDING + SHORT_LINES;
                apply_window_in_place(
                    &mut self.long_time[TRANSITION_PADDING..short_end],
                    &self.short_left,
                );
                apply_window_in_place(&mut self.long_time[FRAME_LINES..], &self.long_right);
            }
            TransformType::Short => {
                return Err(CodecError::Internal(
                    "short-window frame entered long IMDCT synthesis".into(),
                ));
            }
        }

        output.copy_from_slice(&self.long_time[..FRAME_LINES]);
        overlap_add(output, &self.overlap, 1.0);
        self.overlap.copy_from_slice(&self.long_time[FRAME_LINES..]);
        Ok(())
    }

    fn synthesize_short(&mut self, spectrum: &[f32], output: &mut [f32]) -> Result<(), CodecError> {
        self.short_output.fill(0.0);
        self.short_overlap
            .copy_from_slice(&self.overlap[TRANSITION_PADDING..TRANSITION_PADDING + SHORT_LINES]);

        for block in 0..SHORT_BLOCKS {
            for line in 0..SHORT_LINES {
                self.short_spectrum[line] = spectrum[block + SHORT_BLOCKS * line];
            }
            inverse_mdct_into(
                &self.short_spectrum,
                &mut self.short_time,
                &self.short_ifft,
                &mut self.fft_buffer,
                &mut self.fft_scratch,
            )?;
            apply_window_in_place(&mut self.short_time[..SHORT_LINES], &self.short_left);
            apply_window_in_place(&mut self.short_time[SHORT_LINES..], &self.short_right);
            overlap_add(
                &mut self.short_time[..SHORT_LINES],
                &self.short_overlap,
                1.0,
            );
            self.short_overlap
                .copy_from_slice(&self.short_time[SHORT_LINES..]);
            self.short_output[block * SHORT_LINES..(block + 1) * SHORT_LINES]
                .copy_from_slice(&self.short_time[..SHORT_LINES]);
        }

        output[..TRANSITION_PADDING].copy_from_slice(&self.overlap[..TRANSITION_PADDING]);
        output[TRANSITION_PADDING..]
            .copy_from_slice(&self.short_output[..FRAME_LINES - TRANSITION_PADDING]);

        self.overlap[..TRANSITION_PADDING]
            .copy_from_slice(&self.short_output[FRAME_LINES - TRANSITION_PADDING..]);
        self.overlap[TRANSITION_PADDING..TRANSITION_PADDING + SHORT_LINES]
            .copy_from_slice(&self.short_overlap);
        self.overlap[TRANSITION_PADDING + SHORT_LINES..].fill(0.0);
        Ok(())
    }
}

impl Default for Avs3SynthesisWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Avs3SynthesisWorkspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Avs3SynthesisWorkspace")
            .field("long_ifft_len", &LONG_IFFT_LEN)
            .field("short_ifft_len", &SHORT_IFFT_LEN)
            .field("fft_scratch_values", &self.fft_scratch.len())
            .finish_non_exhaustive()
    }
}

/// Run AVS3 inverse MDCT, transform-specific sine windowing and overlap-add for one channel.
///
/// `spectrum` must contain 1024 MDCT lines. Short-window input remains in the codec's eight-way
/// frequency-interleaved layout; long and transition windows use the ordinary contiguous layout.
pub fn synthesize_mdct_frame(
    transform_type: TransformType,
    spectrum: &[f32],
    output: &mut [f32],
    workspace: &mut Avs3SynthesisWorkspace,
) -> Result<(), CodecError> {
    if spectrum.len() != FRAME_LINES || output.len() != FRAME_LINES {
        return Err(CodecError::InvalidData(
            "AVS3 synthesis requires 1024 MDCT lines and 1024 output samples",
        ));
    }
    if spectrum.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "AVS3 synthesis input contains non-finite MDCT data",
        ));
    }

    if transform_type == TransformType::Short {
        workspace.synthesize_short(spectrum, output)?;
    } else {
        workspace.synthesize_long_like(transform_type, spectrum, output)?;
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err(CodecError::InvalidData(
            "AVS3 synthesis produced non-finite PCM data",
        ));
    }
    Ok(())
}

fn inverse_mdct_into(
    spectrum: &[f32],
    time: &mut [f32],
    ifft: &Arc<dyn Fft<f32>>,
    fft_buffer: &mut [Complex<f32>],
    scratch: &mut [Complex<f32>],
) -> Result<(), CodecError> {
    let n = spectrum
        .len()
        .checked_mul(2)
        .ok_or(CodecError::InvalidData("IMDCT length overflow"))?;
    if !matches!(spectrum.len(), FRAME_LINES | SHORT_LINES) || time.len() != n {
        return Err(CodecError::InvalidData(
            "AVS3 IMDCT supports only 1024-line long or 128-line short transforms",
        ));
    }
    let fft_len = n / 4;
    if ifft.len() != fft_len || fft_buffer.len() < fft_len {
        return Err(CodecError::Internal("IMDCT FFT workspace geometry mismatch".into()));
    }
    let scratch_len = ifft.get_inplace_scratch_len();
    if scratch.len() < scratch_len {
        return Err(CodecError::Internal("IMDCT FFT scratch buffer is too small".into()));
    }

    let frequency = 2.0 * PI / n as f32;
    let (sin_step, cos_step) = frequency.sin_cos();
    let (mut sin_phase, mut cos_phase) = (frequency * 0.125).sin_cos();

    for index in 0..fft_len {
        let real_input = -spectrum[2 * index];
        let imag_input = spectrum[spectrum.len() - 1 - 2 * index];
        fft_buffer[index].re = real_input.mul_add(cos_phase, -imag_input * sin_phase);
        fft_buffer[index].im = imag_input.mul_add(cos_phase, real_input * sin_phase);

        let next_cos = cos_phase.mul_add(cos_step, -sin_phase * sin_step);
        let next_sin = sin_phase.mul_add(cos_step, cos_phase * sin_step);
        cos_phase = next_cos;
        sin_phase = next_sin;
    }

    ifft.process_with_scratch(
        &mut fft_buffer[..fft_len],
        &mut scratch[..scratch_len],
    );

    time.fill(0.0);
    let post_scale = 0.5 * (n as f32).sqrt() / fft_len as f32;
    let (mut sin_phase, mut cos_phase) = (frequency * 0.125).sin_cos();
    for index in 0..fft_len {
        let value = fft_buffer[index];
        let real = post_scale * value.re.mul_add(cos_phase, -value.im * sin_phase);
        let imag = post_scale * value.im.mul_add(cos_phase, value.re * sin_phase);

        time[n / 2 + n / 4 - 1 - 2 * index] = real;
        if index < n / 8 {
            time[n / 2 + n / 4 + 2 * index] = real;
        } else {
            time[2 * index - n / 4] = -real;
        }

        time[n / 4 + 2 * index] = imag;
        if index < n / 8 {
            time[n / 4 - 1 - 2 * index] = -imag;
        } else {
            time[n / 4 + n - 1 - 2 * index] = imag;
        }

        let next_cos = cos_phase.mul_add(cos_step, -sin_phase * sin_step);
        let next_sin = sin_phase.mul_add(cos_step, cos_phase * sin_step);
        cos_phase = next_cos;
        sin_phase = next_sin;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inverse_dft_normalized(input: &[Complex<f32>], output: &mut [Complex<f32>]) {
        let len = input.len();
        for (k, destination) in output.iter_mut().enumerate() {
            let mut sum = Complex::new(0.0, 0.0);
            for (index, value) in input.iter().enumerate() {
                let angle = 2.0 * PI * (k * index) as f32 / len as f32;
                let (sin, cos) = angle.sin_cos();
                sum.re += value.re * cos - value.im * sin;
                sum.im += value.re * sin + value.im * cos;
            }
            *destination = sum / len as f32;
        }
    }

    fn reference_short_imdct(spectrum: &[f32; SHORT_LINES]) -> [f32; SHORT_TIME_LEN] {
        let n = SHORT_TIME_LEN;
        let fft_len = SHORT_IFFT_LEN;
        let frequency = 2.0 * PI / n as f32;
        let mut pre = [Complex::new(0.0, 0.0); SHORT_IFFT_LEN];
        for index in 0..fft_len {
            let phase = frequency * (index as f32 + 0.125);
            let (sin, cos) = phase.sin_cos();
            let real_input = -spectrum[2 * index];
            let imag_input = spectrum[spectrum.len() - 1 - 2 * index];
            pre[index] = Complex::new(
                real_input * cos - imag_input * sin,
                imag_input * cos + real_input * sin,
            );
        }
        let mut transformed = [Complex::new(0.0, 0.0); SHORT_IFFT_LEN];
        inverse_dft_normalized(&pre, &mut transformed);
        let mut time = [0.0_f32; SHORT_TIME_LEN];
        let scale = 0.5 * (n as f32).sqrt();
        for index in 0..fft_len {
            let phase = frequency * (index as f32 + 0.125);
            let (sin, cos) = phase.sin_cos();
            let real = scale * (transformed[index].re * cos - transformed[index].im * sin);
            let imag = scale * (transformed[index].im * cos + transformed[index].re * sin);
            time[n / 2 + n / 4 - 1 - 2 * index] = real;
            if index < n / 8 {
                time[n / 2 + n / 4 + 2 * index] = real;
            } else {
                time[2 * index - n / 4] = -real;
            }
            time[n / 4 + 2 * index] = imag;
            if index < n / 8 {
                time[n / 4 - 1 - 2 * index] = -imag;
            } else {
                time[n / 4 + n - 1 - 2 * index] = imag;
            }
        }
        time
    }

    #[test]
    fn short_imdct_matches_direct_inverse_dft_oracle() {
        let spectrum: [f32; SHORT_LINES] =
            std::array::from_fn(|index| ((index as f32 * 0.37).sin() * 3.0) + index as f32 * 0.001);
        let expected = reference_short_imdct(&spectrum);
        let mut workspace = Avs3SynthesisWorkspace::new();
        let mut actual = [0.0_f32; SHORT_TIME_LEN];
        inverse_mdct_into(
            &spectrum,
            &mut actual,
            &workspace.short_ifft,
            &mut workspace.fft_buffer,
            &mut workspace.fft_scratch,
        )
        .unwrap();
        for (left, right) in actual.iter().zip(expected) {
            assert!((*left - right).abs() < 2.0e-3, "{left} != {right}");
        }
    }

    #[test]
    fn windows_match_normative_sine_geometry() {
        let workspace = Avs3SynthesisWorkspace::new();
        assert!(workspace.long_left[0] > 0.0);
        assert!(workspace.long_left[0] < workspace.long_left[FRAME_LINES - 1]);
        assert_eq!(workspace.long_right[0], workspace.long_left[FRAME_LINES - 1]);
        assert_eq!(workspace.short_right[SHORT_LINES - 1], workspace.short_left[0]);
    }

    #[test]
    fn zero_long_frame_is_zero_and_keeps_zero_overlap() {
        let mut workspace = Avs3SynthesisWorkspace::new();
        let spectrum = [0.0_f32; FRAME_LINES];
        let mut output = [1.0_f32; FRAME_LINES];
        synthesize_mdct_frame(TransformType::Long, &spectrum, &mut output, &mut workspace).unwrap();
        assert!(output.iter().all(|value| *value == 0.0));
        assert!(workspace.overlap.iter().all(|value| *value == 0.0));
    }

    #[test]
    fn short_stride_layout_routes_each_frequency_interleaved_block() {
        let mut interleaved = [0.0_f32; FRAME_LINES];
        for block in 0..SHORT_BLOCKS {
            for line in 0..SHORT_LINES {
                interleaved[block + SHORT_BLOCKS * line] = (block * 1000 + line) as f32;
            }
        }
        let block = 5;
        let mut gathered = [0.0_f32; SHORT_LINES];
        for line in 0..SHORT_LINES {
            gathered[line] = interleaved[block + SHORT_BLOCKS * line];
        }
        for (line, value) in gathered.into_iter().enumerate() {
            assert_eq!(value, (block * 1000 + line) as f32);
        }
    }
}
