#[derive(Clone, Debug)]
pub(crate) struct CubicDelayLine {
    samples: Vec<f32>,
    write_cursor: usize,
}

impl CubicDelayLine {
    pub(crate) fn new(capacity: usize) -> Self {
        Self { samples: vec![0.0; capacity.max(8)], write_cursor: 0 }
    }

    #[inline]
    pub(crate) fn push(&mut self, sample: f32) {
        self.samples[self.write_cursor] = sample;
        self.write_cursor += 1;
        if self.write_cursor == self.samples.len() { self.write_cursor = 0; }
    }

    /// Four-point Lagrange interpolation over past samples.
    #[inline]
    pub(crate) fn read(&self, delay_samples: f32) -> f32 {
        let length = self.samples.len();
        let newest = if self.write_cursor == 0 { length - 1 } else { self.write_cursor - 1 };
        self.read_with_context(delay_samples, length, newest)
    }

    /// Read both ears from one ring snapshot so length/cursor resolution is not duplicated.
    #[inline]
    pub(crate) fn read_pair(&self, left_delay: f32, right_delay: f32) -> (f32, f32) {
        let length = self.samples.len();
        let newest = if self.write_cursor == 0 { length - 1 } else { self.write_cursor - 1 };
        if left_delay == right_delay {
            let sample = self.read_with_context(left_delay, length, newest);
            return (sample, sample);
        }
        (
            self.read_with_context(left_delay, length, newest),
            self.read_with_context(right_delay, length, newest),
        )
    }

    #[inline]
    fn read_with_context(&self, delay_samples: f32, length: usize, newest: usize) -> f32 {
        let max_delay = length.saturating_sub(4) as f32;
        let delay = delay_samples.clamp(1.0, max_delay.max(1.0));
        // Delay is positive after clamping, so integer truncation is identical to floor() here.
        let whole = delay as usize;
        let fraction = delay - whole as f32;

        // Near-ear and LFE paths commonly use an exact two-sample causal delay. Avoid evaluating
        // four Lagrange taps and cubic coefficients when the requested position is integral.
        if fraction == 0.0 {
            return self.sample_at_age(newest, whole, length);
        }

        let x_minus_one = self.sample_at_age(newest, whole - 1, length);
        let x_zero = self.sample_at_age(newest, whole, length);
        let x_one = self.sample_at_age(newest, whole + 1, length);
        let x_two = self.sample_at_age(newest, whole + 2, length);
        let mu = fraction;
        let c_minus_one = -mu * (mu - 1.0) * (mu - 2.0) / 6.0;
        let c_zero = (mu + 1.0) * (mu - 1.0) * (mu - 2.0) * 0.5;
        let c_one = -(mu + 1.0) * mu * (mu - 2.0) * 0.5;
        let c_two = (mu + 1.0) * mu * (mu - 1.0) / 6.0;
        x_minus_one * c_minus_one + x_zero * c_zero + x_one * c_one + x_two * c_two
    }

    #[inline]
    fn sample_at_age(&self, newest: usize, age: usize, length: usize) -> f32 {
        debug_assert!(age < length);
        // `newest + length - age` is in [1, 2*length). One conditional subtraction replaces the
        // integer modulo that previously ran for every Lagrange tap in the realtime hot path.
        let index = newest + length - age;
        let wrapped = if index >= length { index - length } else { index };
        self.samples[wrapped]
    }

    pub(crate) fn reset(&mut self) {
        self.samples.fill(0.0);
        self.write_cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_delay_selects_exact_history_sample() {
        let mut delay = CubicDelayLine::new(16);
        for value in 0..8 { delay.push(value as f32); }
        assert_eq!(delay.read(2.0), 5.0);
        assert_eq!(delay.read(3.0), 4.0);
    }

    #[test]
    fn fractional_delay_is_continuous_between_adjacent_samples() {
        let mut delay = CubicDelayLine::new(16);
        for value in 0..8 { delay.push(value as f32); }
        let value = delay.read(2.5);
        assert!(value > 4.0 && value < 6.0);
    }

    #[test]
    fn paired_reads_match_independent_reads() {
        let mut delay = CubicDelayLine::new(16);
        for value in 0..24 { delay.push(value as f32 * 0.25); }
        let (left, right) = delay.read_pair(2.0, 3.375);
        assert!((left - delay.read(2.0)).abs() < f32::EPSILON);
        assert!((right - delay.read(3.375)).abs() < f32::EPSILON);
    }

    #[test]
    fn wrapped_ring_preserves_age_addressing() {
        let mut delay = CubicDelayLine::new(8);
        for value in 0..20 { delay.push(value as f32); }
        assert_eq!(delay.read(1.0), 18.0);
        assert_eq!(delay.read(2.0), 17.0);
        assert_eq!(delay.read(4.0), 15.0);
    }
}
