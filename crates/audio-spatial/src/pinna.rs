use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PinnaCueTelemetry {
    pub center_hz: f32,
    pub q: f32,
    pub depth_db: f32,
    pub ear_lateral: f32,
    pub left_center_hz: f32,
    pub right_center_hz: f32,
    pub left_depth_db: f32,
    pub right_depth_db: f32,
    pub shoulder_center_hz: f32,
    pub shoulder_q: f32,
    pub shoulder_gain_db: f32,
    pub left_shoulder_center_hz: f32,
    pub right_shoulder_center_hz: f32,
    pub left_shoulder_gain_db: f32,
    pub right_shoulder_gain_db: f32,
    pub ridge_center_hz: f32,
    pub ridge_q: f32,
    pub ridge_gain_db: f32,
    pub left_ridge_center_hz: f32,
    pub right_ridge_center_hz: f32,
    pub left_ridge_gain_db: f32,
    pub right_ridge_gain_db: f32,
    /// 0..1 normalized strength of the generic spectral cue. This is not a perceptual score.
    pub cue_strength: f32,
}

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
    /// Primary direction-dependent notch. Kept as `left/right` so existing renderer diagnostics and
    /// cache tests continue to describe the dominant pinna cue.
    pub(crate) left: BiquadCoefficients,
    pub(crate) right: BiquadCoefficients,
    /// Secondary broad spectral shoulder.
    pub(crate) left_shoulder: BiquadCoefficients,
    pub(crate) right_shoulder: BiquadCoefficients,
    /// Third lower-frequency sagittal landmark. A signed gain distinguishes above/front from
    /// below/rear without relying on another high-frequency notch.
    pub(crate) left_ridge: BiquadCoefficients,
    pub(crate) right_ridge: BiquadCoefficients,
}

impl StereoPinnaCoefficients {
    pub(crate) const IDENTITY: Self = Self {
        left: BiquadCoefficients::IDENTITY,
        right: BiquadCoefficients::IDENTITY,
        left_shoulder: BiquadCoefficients::IDENTITY,
        right_shoulder: BiquadCoefficients::IDENTITY,
        left_ridge: BiquadCoefficients::IDENTITY,
        right_ridge: BiquadCoefficients::IDENTITY,
    };

    #[inline]
    pub(crate) fn step_to(self, end: Self, frames: usize) -> StereoPinnaCoefficientStep {
        StereoPinnaCoefficientStep {
            left: self.left.step_to(end.left, frames),
            right: self.right.step_to(end.right, frames),
            left_shoulder: self.left_shoulder.step_to(end.left_shoulder, frames),
            right_shoulder: self.right_shoulder.step_to(end.right_shoulder, frames),
            left_ridge: self.left_ridge.step_to(end.left_ridge, frames),
            right_ridge: self.right_ridge.step_to(end.right_ridge, frames),
        }
    }

    #[inline]
    pub(crate) fn advance(&mut self, step: StereoPinnaCoefficientStep) {
        self.left.advance(step.left);
        self.right.advance(step.right);
        self.left_shoulder.advance(step.left_shoulder);
        self.right_shoulder.advance(step.right_shoulder);
        self.left_ridge.advance(step.left_ridge);
        self.right_ridge.advance(step.right_ridge);
    }

    /// Advance only the primary notch. Early reflections use this reduced path so four sagittal
    /// taps do not pay for the shoulder/ridge stages on every sample.
    #[inline]
    pub(crate) fn advance_primary(&mut self, step: StereoPinnaCoefficientStep) {
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
    left_shoulder: BiquadCoefficientStep,
    right_shoulder: BiquadCoefficientStep,
    left_ridge: BiquadCoefficientStep,
    right_ridge: BiquadCoefficientStep,
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

/// Fixed-state direct-path pinna layer. Three cascaded generic biquads provide a primary notch,
/// broad shoulder and lower-frequency sagittal ridge. This is not a measured HRTF and does not
/// depend on SOFA/KEMAR data.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StereoPinnaState {
    left_notch: BiquadState,
    right_notch: BiquadState,
    left_shoulder: BiquadState,
    right_shoulder: BiquadState,
    left_ridge: BiquadState,
    right_ridge: BiquadState,
}

impl StereoPinnaState {
    #[inline]
    pub(crate) fn process(
        &mut self,
        left: f32,
        right: f32,
        coefficients: StereoPinnaCoefficients,
    ) -> (f32, f32) {
        let left = self.left_notch.process(left, coefficients.left);
        let right = self.right_notch.process(right, coefficients.right);
        let left = self.left_shoulder.process(left, coefficients.left_shoulder);
        let right = self.right_shoulder.process(right, coefficients.right_shoulder);
        (
            self.left_ridge.process(left, coefficients.left_ridge),
            self.right_ridge.process(right, coefficients.right_ridge),
        )
    }

    /// Process only the primary direction-dependent notch. Used by selected early-reflection taps
    /// to retain front/rear/elevation identity without multiplying the full direct-path filter cost.
    #[inline]
    pub(crate) fn process_primary(
        &mut self,
        left: f32,
        right: f32,
        coefficients: StereoPinnaCoefficients,
    ) -> (f32, f32) {
        (
            self.left_notch.process(left, coefficients.left),
            self.right_notch.process(right, coefficients.right),
        )
    }

    pub(crate) fn reset(&mut self) {
        self.left_notch.reset();
        self.right_notch.reset();
        self.left_shoulder.reset();
        self.right_shoulder.reset();
        self.left_ridge.reset();
        self.right_ridge.reset();
    }
}

#[derive(Clone, Copy, Debug)]
struct PinnaCueShape {
    center_hz: f32,
    q: f32,
    depth_db: f32,
    shoulder_center_hz: f32,
    shoulder_q: f32,
    shoulder_gain_db: f32,
    ridge_center_hz: f32,
    ridge_q: f32,
    ridge_gain_db: f32,
    ear_lateral: f32,
}

/// Public read-only projection of the exact generic pinna cue used by the realtime direct path.
/// Angles are expressed in degrees to match SpatialDebugSource telemetry.
pub fn pinna_cue_telemetry(
    azimuth_degrees: f32,
    elevation_degrees: f32,
    spread: f32,
) -> PinnaCueTelemetry {
    telemetry_from_shape(cue_shape(
        finite_or_zero(azimuth_degrees).to_radians(),
        finite_or_zero(elevation_degrees).to_radians(),
        spread,
    ))
}

/// Build generic pinna notch + shoulder + ridge cues from listener-local direction.
///
/// `azimuth_radians > 0` means source-right. The high-frequency notch and broad shoulder retain
/// strong rear/elevation separation, while the lower ridge gives the sagittal plane a third
/// independent landmark. This remains a generic parametric approximation, not a personal HRTF.
pub(crate) fn coefficients_for_direction(
    sample_rate: f32,
    azimuth_radians: f32,
    elevation_radians: f32,
    spread: f32,
) -> StereoPinnaCoefficients {
    let cue = telemetry_from_shape(cue_shape(
        azimuth_radians,
        elevation_radians,
        spread,
    ));

    StereoPinnaCoefficients {
        left: peaking_coefficients(sample_rate, cue.left_center_hz, cue.q, -cue.left_depth_db),
        right: peaking_coefficients(sample_rate, cue.right_center_hz, cue.q, -cue.right_depth_db),
        left_shoulder: peaking_coefficients(
            sample_rate,
            cue.left_shoulder_center_hz,
            cue.shoulder_q,
            cue.left_shoulder_gain_db,
        ),
        right_shoulder: peaking_coefficients(
            sample_rate,
            cue.right_shoulder_center_hz,
            cue.shoulder_q,
            cue.right_shoulder_gain_db,
        ),
        left_ridge: peaking_coefficients(
            sample_rate,
            cue.left_ridge_center_hz,
            cue.ridge_q,
            cue.left_ridge_gain_db,
        ),
        right_ridge: peaking_coefficients(
            sample_rate,
            cue.right_ridge_center_hz,
            cue.ridge_q,
            cue.right_ridge_gain_db,
        ),
    }
}

/// Lightweight spectral cue for first-order room reflections. Only the primary notch is populated,
/// at roughly one third of the direct-path depth; the renderer applies it only to front/rear/floor/
/// ceiling image sources, where ITD/ILD alone are weakest at preserving sagittal identity.
pub(crate) fn reflection_coefficients_for_direction(
    sample_rate: f32,
    azimuth_radians: f32,
    elevation_radians: f32,
    spread: f32,
) -> StereoPinnaCoefficients {
    const REFLECTION_DEPTH_SCALE: f32 = 0.32;
    let cue = telemetry_from_shape(cue_shape(
        azimuth_radians,
        elevation_radians,
        spread,
    ));
    StereoPinnaCoefficients {
        left: peaking_coefficients(
            sample_rate,
            cue.left_center_hz,
            cue.q,
            -cue.left_depth_db * REFLECTION_DEPTH_SCALE,
        ),
        right: peaking_coefficients(
            sample_rate,
            cue.right_center_hz,
            cue.q,
            -cue.right_depth_db * REFLECTION_DEPTH_SCALE,
        ),
        ..StereoPinnaCoefficients::IDENTITY
    }
}

#[inline]
fn telemetry_from_shape(shape: PinnaCueShape) -> PinnaCueTelemetry {
    let lateral = shape.ear_lateral;
    let left_center_hz = shape.center_hz * (1.0 - lateral * 0.018);
    let right_center_hz = shape.center_hz * (1.0 + lateral * 0.018);
    let left_depth_db = shape.depth_db * (1.0 + lateral * 0.08);
    let right_depth_db = shape.depth_db * (1.0 - lateral * 0.08);
    let left_shoulder_center_hz = shape.shoulder_center_hz * (1.0 - lateral * 0.012);
    let right_shoulder_center_hz = shape.shoulder_center_hz * (1.0 + lateral * 0.012);
    let left_shoulder_gain_db = shape.shoulder_gain_db * (1.0 - lateral * 0.05);
    let right_shoulder_gain_db = shape.shoulder_gain_db * (1.0 + lateral * 0.05);
    let left_ridge_center_hz = shape.ridge_center_hz * (1.0 - lateral * 0.010);
    let right_ridge_center_hz = shape.ridge_center_hz * (1.0 + lateral * 0.010);
    let left_ridge_gain_db = shape.ridge_gain_db * (1.0 - lateral * 0.04);
    let right_ridge_gain_db = shape.ridge_gain_db * (1.0 + lateral * 0.04);
    PinnaCueTelemetry {
        center_hz: shape.center_hz,
        q: shape.q,
        depth_db: shape.depth_db,
        ear_lateral: shape.ear_lateral,
        left_center_hz,
        right_center_hz,
        left_depth_db,
        right_depth_db,
        shoulder_center_hz: shape.shoulder_center_hz,
        shoulder_q: shape.shoulder_q,
        shoulder_gain_db: shape.shoulder_gain_db,
        left_shoulder_center_hz,
        right_shoulder_center_hz,
        left_shoulder_gain_db,
        right_shoulder_gain_db,
        ridge_center_hz: shape.ridge_center_hz,
        ridge_q: shape.ridge_q,
        ridge_gain_db: shape.ridge_gain_db,
        left_ridge_center_hz,
        right_ridge_center_hz,
        left_ridge_gain_db,
        right_ridge_gain_db,
        cue_strength: ((shape.depth_db / 6.0) * 0.62
            + (shape.shoulder_gain_db / 2.8) * 0.23
            + (shape.ridge_gain_db.abs() / 2.0) * 0.15)
            .clamp(0.0, 1.0),
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
    let elevation_up = elevation_sin.max(0.0);
    let elevation_down = (-elevation_sin).max(0.0);
    // Sagittal cues are naturally least useful at the extreme lateral poles, but attenuating them
    // as aggressively as before made elevation disappear whenever a moving source passed the ears.
    let sagittal_weight = (1.0 - lateral * 0.48).clamp(0.46, 1.0);
    let spread_weight = 1.0 - spread * 0.72;

    PinnaCueShape {
        // Wider front/rear + elevation separation is the primary remedy for front/back confusion.
        // Values remain inside a conservative generic pinna band and are clamped again against the
        // actual sample-rate in peaking_coefficients().
        center_hz: (9_800.0 + elevation_sin * 2_600.0 - rear * 2_700.0)
            .clamp(5_000.0, 12_400.0),
        q: (1.10 + rear * 0.65 + elevation_sin.abs() * 0.38).clamp(0.90, 2.20),
        depth_db: ((0.55
            + rear * 3.85
            + elevation_sin.abs() * 2.10
            + elevation_down * 0.35)
            * sagittal_weight
            * spread_weight)
            .clamp(0.0, 6.0),
        // The broad shoulder occupies a lower band and moves with elevation/rear amount.
        shoulder_center_hz: (5_000.0 + elevation_sin * 1_600.0 - rear * 900.0)
            .clamp(2_800.0, 7_400.0),
        shoulder_q: (0.70 + rear * 0.20 + elevation_sin.abs() * 0.25).clamp(0.62, 1.30),
        shoulder_gain_db: ((0.20
            + rear * 0.90
            + elevation_up * 1.80
            + elevation_down * 0.62)
            * sagittal_weight
            * spread_weight)
            .clamp(0.0, 2.8),
        // A third, substantially lower landmark intentionally uses signed gain. Above-front tends
        // toward a gentle presence lift; below and rear directions become a shallow dip. The cue is
        // kept below 2 dB so it reinforces localization instead of becoming an obvious tone control.
        ridge_center_hz: (2_650.0 + elevation_sin * 850.0 - rear * 450.0)
            .clamp(1_450.0, 3_900.0),
        ridge_q: (0.62 + rear * 0.18 + elevation_sin.abs() * 0.20).clamp(0.55, 1.15),
        ridge_gain_db: ((elevation_up * 1.65 - elevation_down * 1.10 - rear * 1.25)
            * sagittal_weight
            * spread_weight)
            .clamp(-2.0, 1.8),
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
    let gain_db = finite_or_zero(gain_db).clamp(-6.0, 3.0);
    if gain_db.abs() <= 1.0e-4 {
        return BiquadCoefficients::IDENTITY;
    }

    let max_center = (sample_rate * 0.45).max(sample_rate * 0.10);
    let min_center = 900.0_f32.min(max_center * 0.50);
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

    fn coefficient_distance_from_identity(coefficients: BiquadCoefficients) -> f32 {
        (coefficients.b0 - 1.0).abs()
            + coefficients.b1.abs()
            + coefficients.b2.abs()
            + coefficients.a1.abs()
            + coefficients.a2.abs()
    }

    #[test]
    fn rear_cue_is_deeper_and_lower_than_front() {
        let front = cue_shape(0.0, 0.0, 0.0);
        let rear = cue_shape(PI, 0.0, 0.0);
        assert!(rear.depth_db > front.depth_db + 3.0);
        assert!(rear.center_hz < front.center_hz - 2_000.0);
        assert!(rear.shoulder_gain_db > front.shoulder_gain_db + 0.7);
        assert!(rear.ridge_center_hz < front.ridge_center_hz - 300.0);
        assert!(rear.ridge_gain_db < front.ridge_gain_db - 1.0);
    }

    #[test]
    fn elevation_moves_all_three_sagittal_features() {
        let above = cue_shape(0.0, PI * 0.35, 0.0);
        let below = cue_shape(0.0, -PI * 0.35, 0.0);
        assert!(above.center_hz > below.center_hz + 3_000.0);
        assert!(above.shoulder_center_hz > below.shoulder_center_hz + 2_000.0);
        assert!(above.shoulder_gain_db > below.shoulder_gain_db + 0.8);
        assert!(above.ridge_center_hz > below.ridge_center_hz + 1_000.0);
        assert!(above.ridge_gain_db > below.ridge_gain_db + 2.0);
        assert!(above.depth_db > 1.0);
        assert!(below.depth_db > 1.0);
    }

    #[test]
    fn spread_reduces_all_pinna_cues() {
        let focused = cue_shape(PI, PI * 0.20, 0.0);
        let diffuse = cue_shape(PI, PI * 0.20, 1.0);
        assert!(diffuse.depth_db < focused.depth_db);
        assert!(diffuse.shoulder_gain_db < focused.shoulder_gain_db);
        assert!(diffuse.ridge_gain_db.abs() < focused.ridge_gain_db.abs());
    }

    #[test]
    fn lateral_source_gets_per_ear_spectral_asymmetry() {
        let coefficients = coefficients_for_direction(48_000.0, PI * 0.5, PI * 0.15, 0.0);
        assert_ne!(coefficients.left, coefficients.right);
        assert_ne!(coefficients.left_shoulder, coefficients.right_shoulder);
        assert_ne!(coefficients.left_ridge, coefficients.right_ridge);
    }

    #[test]
    fn reflection_pinna_keeps_only_a_weaker_primary_notch() {
        let direct = coefficients_for_direction(48_000.0, PI, PI * 0.20, 0.0);
        let reflection = reflection_coefficients_for_direction(48_000.0, PI, PI * 0.20, 0.0);
        assert_ne!(reflection.left, BiquadCoefficients::IDENTITY);
        assert_ne!(reflection.right, BiquadCoefficients::IDENTITY);
        assert_eq!(reflection.left_shoulder, BiquadCoefficients::IDENTITY);
        assert_eq!(reflection.right_shoulder, BiquadCoefficients::IDENTITY);
        assert_eq!(reflection.left_ridge, BiquadCoefficients::IDENTITY);
        assert_eq!(reflection.right_ridge, BiquadCoefficients::IDENTITY);
        assert!(
            coefficient_distance_from_identity(reflection.left)
                < coefficient_distance_from_identity(direct.left)
        );
        assert!(
            coefficient_distance_from_identity(reflection.right)
                < coefficient_distance_from_identity(direct.right)
        );
    }

    #[test]
    fn public_telemetry_matches_runtime_shape() {
        let telemetry = pinna_cue_telemetry(180.0, 0.0, 0.0);
        let rear = cue_shape(PI, 0.0, 0.0);
        assert!((telemetry.center_hz - rear.center_hz).abs() < f32::EPSILON);
        assert!((telemetry.depth_db - rear.depth_db).abs() < f32::EPSILON);
        assert!((telemetry.shoulder_center_hz - rear.shoulder_center_hz).abs() < f32::EPSILON);
        assert!((telemetry.shoulder_gain_db - rear.shoulder_gain_db).abs() < f32::EPSILON);
        assert!((telemetry.ridge_center_hz - rear.ridge_center_hz).abs() < f32::EPSILON);
        assert!((telemetry.ridge_gain_db - rear.ridge_gain_db).abs() < f32::EPSILON);
        assert!(telemetry.cue_strength > 0.4);
    }

    #[test]
    fn coefficient_ramp_reaches_next_block_boundary() {
        let start = coefficients_for_direction(48_000.0, 0.0, 0.0, 0.0);
        let end = coefficients_for_direction(48_000.0, PI, PI * 0.25, 0.0);
        let step = start.step_to(end, 64);
        let mut current = start;
        for _ in 0..64 {
            current.advance(step);
        }
        assert!((current.left.b0 - end.left.b0).abs() < 1.0e-5);
        assert!((current.left.a2 - end.left.a2).abs() < 1.0e-5);
        assert!((current.right.b1 - end.right.b1).abs() < 1.0e-5);
        assert!((current.left_shoulder.b0 - end.left_shoulder.b0).abs() < 1.0e-5);
        assert!((current.right_shoulder.a2 - end.right_shoulder.a2).abs() < 1.0e-5);
        assert!((current.left_ridge.b0 - end.left_ridge.b0).abs() < 1.0e-5);
        assert!((current.right_ridge.a2 - end.right_ridge.a2).abs() < 1.0e-5);
    }

    #[test]
    fn filter_state_isolates_non_finite_input_across_three_stages() {
        let mut state = StereoPinnaState::default();
        let coefficients = coefficients_for_direction(48_000.0, PI, PI * 0.2, 0.0);
        let (left, right) = state.process(f32::NAN, f32::INFINITY, coefficients);
        assert_eq!(left, 0.0);
        assert_eq!(right, 0.0);
        let (left, right) = state.process(0.25, -0.25, coefficients);
        assert!(left.is_finite());
        assert!(right.is_finite());
    }

    #[test]
    fn low_sample_rate_keeps_all_biquad_stages_finite() {
        let coefficients = coefficients_for_direction(4_000.0, PI * 0.75, PI * 0.25, 0.0);
        let reflection =
            reflection_coefficients_for_direction(4_000.0, PI * 0.75, PI * 0.25, 0.0);
        assert!(coefficients_finite(coefficients.left));
        assert!(coefficients_finite(coefficients.right));
        assert!(coefficients_finite(coefficients.left_shoulder));
        assert!(coefficients_finite(coefficients.right_shoulder));
        assert!(coefficients_finite(coefficients.left_ridge));
        assert!(coefficients_finite(coefficients.right_ridge));
        assert!(coefficients_finite(reflection.left));
        assert!(coefficients_finite(reflection.right));
    }
}
