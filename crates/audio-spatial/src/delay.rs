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
        let max_delay = length.saturating_sub(4) as f32;
        let delay = delay_samples.clamp(1.0, max_delay.max(1.0));
        let whole = delay.floor() as usize;
        let fraction = delay - whole as f32;
        let newest = if self.write_cursor == 0 { length - 1 } else { self.write_cursor - 1 };
        let sample_at = |age: usize| {
            let age = age % length;
            self.samples[(newest + length - age) % length]
        };
        let x_minus_one = sample_at(whole.saturating_sub(1));
        let x_zero = sample_at(whole);
        let x_one = sample_at(whole.saturating_add(1));
        let x_two = sample_at(whole.saturating_add(2));
        let mu = fraction;
        let c_minus_one = -mu * (mu - 1.0) * (mu - 2.0) / 6.0;
        let c_zero = (mu + 1.0) * (mu - 1.0) * (mu - 2.0) * 0.5;
        let c_one = -(mu + 1.0) * mu * (mu - 2.0) * 0.5;
        let c_two = (mu + 1.0) * mu * (mu - 1.0) / 6.0;
        x_minus_one * c_minus_one + x_zero * c_zero + x_one * c_one + x_two * c_two
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
        assert!((delay.read(2.0) - 5.0).abs() < 1.0e-6);
        assert!((delay.read(3.0) - 4.0).abs() < 1.0e-6);
    }
    #[test]
    fn fractional_delay_is_continuous_between_adjacent_samples() {
        let mut delay = CubicDelayLine::new(16);
        for value in 0..8 { delay.push(value as f32); }
        let value = delay.read(2.5);
        assert!(value > 4.0 && value < 6.0);
    }
}
