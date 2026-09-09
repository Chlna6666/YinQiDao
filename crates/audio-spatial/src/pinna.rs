use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BiquadCoefficients {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl BiquadCoefficients {
    pub(crate) const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    #[inline]
    pub(crate) fn step_to(self, end: Self, frames: usize) -> BiquadCoefficientStep {
        if frames == 0 {
            return BiquadCoefficientStep::default();
        }
        let scale = 1.0 / frames as f32;
        BiquadCoefficientStep {
            b0: (end.b0 - self.b0) * scale,
            b1: (end.b1 - self.b1) * scale,
            b2: (end.b2 - self.b2) * scale,
            a1: (end.a1 - self.a1) * scale,
            a2: (end.a2 - self.a2) * scale,
        }
    }

    #[inline]
    pub(crate) fn advance(&mut self, step: BiquadCoefficientStep) {
        self.b0 += step.b0;
        self.b1 += step.b1;
        self.b2 += step.b2;
        self.a1 += step.a1;
        self.a2 += step.a2;
    }
}

impl Default for BiquadCoefficients {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BiquadCoefficientStep {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StereoPinnaCoefficients {
    pub(crate) left: BiquadCoefficients,
    pub(crate) right: BiquadCoefficients,
}

impl StereoPinnaCoefficients {
    pub(crate) const IDENTITY: Self = Self {
        left: BiquadCoefficients::IDENTITY,
        right: BiquadCoefficients::IDENTITY,
    };

    #[inline]
    pub(crate) fn step_to(self, end: Self, frames: usize) -> StereoPinnaCoefficientStep {
        StereoPinnaCoefficientStep {
            left: self.left.step_to(end.left, frames),
            right: self.right.step_to(end.right, frames),
        }
    }

    #[inline]
    pub(crate) fn advance(&mut self, step: StereoPinnaCoefficientStep) {
        self.left.advance(step.left);
        self.right.advance(step.right);
    }
}

impl Default for StereoPinnaCoefficients {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StereoPinnaCoefficientStep {
    left: BiquadCoefficientStep,
    right: BiquadCoefficientStep,
}

#[derive(Clone, Copy, Debug, Default)]
struct BiquadState {
    z1: f32,
    z2: f32,
}

impl BiquadState {
    #[inline]
    fn process(&mut self, input: f32, coefficients: BiquadCoefficients) -> f32 {
        if !input.is_finite() {
            self.reset();
            return 0.0;
        }

        let output = coefficients.b0.mul_add(input, self.z1);
        let z1 = coefficients
            .b1
            .mul_add(input, (-coefficients.a1).mul_add(output, self.z2));
        let z2 = coefficients.b2.mul_add(input, -coefficients.a2 * output);

        if output.is_finite() && z1.is_finite() && z2.is_finite() {
            self.z1 = flush_denormal(z1);
            self.z2 = flush_denormal(z2);
            output
        } else {
            self.reset();
            0.0
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Fixed-state direct-path pinna layer. The filter is deliberately generic and parametric; it is
/// not a measured HRTF and does not depend on SOFA/KEMAR data.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StereoPinnaState {
    left: BiquadState,
    right: BiquadState,
}

impl StereoPinnaState {
    #[inline]
    pub(crate) fn process(
        &mut self,
        left: f32,
        right: f32,
        coefficients: StereoPinnaCoefficients,
    ) -> (f32, f32) {
        (
            self.left.process(left, coefficients.left),
            self.right.process(right, coefficients.right),
        )
    }

    pub(crate) fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
    }
}

#[derive(Clone, Copy, Debug)]
struct PinnaCueShape {
    center_hz: f32,
    q: f32,
    depth_db: f32,
    ear_lateral: f32,
}

/// Build a conservative generic pinna notch from listener-local direction.
///
/// `azimuth_radians > 0` means source-right. Rear sources move the notch downward and deepen it;
/// elevation moves the notch upward/downward; lateral/spread reduce sagittal coloration. A small
/// per-ear asymmetry prevents the pinna layer from collapsing back to identical L/R spectra.
pub(crate) fn coefficients_for_direction(
    sample_rate: f32,
    azimuth_radians: f32,
    elevation_radians: f32,
    spread: f32,
) -> StereoPinnaCoefficients {
    let shape = cue_shape(azimuth_radians, elevation_radians, spread);
    let lateral = shape.ear_lateral;

    let left_center = shape.center_hz * (1.0 - lateral * 0.018);
    let right_center = shape.center_hz * (1.0 + lateral * 0.018);
    let left_depth = shape.depth_db * (1.0 + lateral * 0.08);
    let right_depth = shape.depth_db * (1.0 - lateral * 0.08);

    StereoPinnaCoefficients {
        left: peaking_coefficients(sample_rate, left_center, shape.q, -left_depth),
        right: peaking_coefficients(sample_rate, right_center, shape.q, -right_depth),
    }
}

#[inline]
fn cue_shape(azimuth_radians: f32, elevation_radians: f32, spread: f32) -> PinnaCueShape {
    let azimuth = finite_or_zero(azimuth_radians).clamp(-PI, PI);
    let elevation = finite_or_zero(elevation_radians).clamp(-PI * 0.5, PI * 0.5);
    let spread = finite_or_zero(spread).clamp(0.0, 1.0);

    let lateral_signed = azimuth.sin();
    let lateral = lateral_signed.abs();
    let rear = (-azimuth.cos()).max(0.0);
    let elevation_sin = elevation.sin();
    let sagittal_weight = (1.0 - lateral * 0.58).clamp(0.38, 1.0);
    let spread_weight = 1.0 - spread * 0.60;

    PinnaCueShape {
        center_hz: (9_200.0 + elevation_sin * 1_700.0 - rear * 1_800.0)
            .clamp(5_400.0, 11_200.0),
        q: (1.05 + rear * 0.55 + elevation_sin.abs() * 0.25).clamp(0.90, 1.90),
        depth_db: ((0.65 + rear * 2.90 + elevation_sin.abs() * 1.25)
            * sagittal_weight
            * spread_weight)
            .clamp(0.0, 4.8),
        ear_lateral: lateral_signed * (1.0 - spread * 0.70),
    }
}

#[inline]
fn peaking_coefficients(
    sample_rate: f32,
    center_hz: f32,
    q: f32,
    gain_db: f32,
) -> BiquadCoefficients {
    let sample_rate = finite_or_zero(sample_rate).max(1.0);
    let gain_db = finite_or_zero(gain_db).clamp(-6.0, 0.0);
    if gain_db.abs() <= 1.0e-4 {
        return BiquadCoefficients::IDENTITY;
    }

    let max_center = (sample_rate * 0.45).max(sample_rate * 0.10);
    let min_center = 3_000.0_f32.min(max_center * 0.50);
    let center_hz = finite_or_zero(center_hz).clamp(min_center, max_center);
    let q = finite_or_zero(q).clamp(0.55, 3.0);

    let omega = 2.0 * PI * center_hz / sample_rate;
    let (sin_omega, cos_omega) = omega.sin_cos();
    let a = 10.0_f32.powf(gain_db / 40.0);
    let alpha = sin_omega / (2.0 * q);
    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_omega;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_omega;
    let a2 = 1.0 - alpha / a;
    let reciprocal_a0 = a0.recip();

    let coefficients = BiquadCoefficients {
        b0: b0 * reciprocal_a0,
        b1: b1 * reciprocal_a0,
        b2: b2 * reciprocal_a0,
        a1: a1 * reciprocal_a0,
        a2: a2 * reciprocal_a0,
    };
    if coefficients_finite(coefficients) {
        coefficients
    } else {
        BiquadCoefficients::IDENTITY
    }
}

#[inline]
fn coefficients_finite(coefficients: BiquadCoefficients) -> bool {
    coefficients.b0.is_finite()
        && coefficients.b1.is_finite()
        && coefficients.b2.is_finite()
        && coefficients.a1.is_finite()
        && coefficients.a2.is_finite()
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[inline]
fn flush_denormal(value: f32) -> f32 {
    if value.abs() < 1.0e-20 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rear_cue_is_deeper_and_lower_than_front() {
        let front = cue_shape(0.0, 0.0, 0.0);
        let rear = cue_shape(PI, 0.0, 0.0);
        assert!(rear.depth_db > front.depth_db + 2.0);
        assert!(rear.center_hz < front.center_hz);
    }

    #[test]
    fn elevation_moves_sagittal_notch_frequency() {
        let above = cue_shape(0.0, PI * 0.35, 0.0);
        let below = cue_shape(0.0, -PI * 0.35, 0.0);
        assert!(above.center_hz > below.center_hz);
        assert!(above.depth_db > 1.0);
        assert!(below.depth_db > 1.0);
    }

    #[test]
    fn spread_reduces_pinna_coloration() {
        let focused = cue_shape(PI, 0.0, 0.0);
        let diffuse = cue_shape(PI, 0.0, 1.0);
        assert!(diffuse.depth_db < focused.depth_db);
    }

    #[test]
    fn lateral_source_gets_per_ear_spectral_asymmetry() {
        let coefficients = coefficients_for_direction(48_000.0, PI * 0.5, 0.0, 0.0);
        assert_ne!(coefficients.left, coefficients.right);
    }

    #[test]
    fn coefficient_ramp_reaches_next_block_boundary() {
        let start = coefficients_for_direction(48_000.0, 0.0, 0.0, 0.0);
        let end = coefficients_for_direction(48_000.0, PI, 0.0, 0.0);
        let step = start.step_to(end, 64);
        let mut current = start;
        for _ in 0..64 {
            current.advance(step);
        }
        assert!((current.left.b0 - end.left.b0).abs() < 1.0e-5);
        assert!((current.left.a2 - end.left.a2).abs() < 1.0e-5);
        assert!((current.right.b1 - end.right.b1).abs() < 1.0e-5);
    }

    #[test]
    fn filter_state_isolates_non_finite_input() {
        let mut state = StereoPinnaState::default();
        let coefficients = coefficients_for_direction(48_000.0, PI, 0.0, 0.0);
        let (left, right) = state.process(f32::NAN, f32::INFINITY, coefficients);
        assert_eq!(left, 0.0);
        assert_eq!(right, 0.0);
        let (left, right) = state.process(0.25, -0.25, coefficients);
        assert!(left.is_finite());
        assert!(right.is_finite());
    }
}