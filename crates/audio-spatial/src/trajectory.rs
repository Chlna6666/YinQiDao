use std::f64::consts::TAU;

use crate::{SourcePose, Vec3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrajectoryKind { Orbit360, FigureEight, Pendulum, FrontBack, Planetary, NearEar, Helix }

#[derive(Clone, Debug)]
pub struct Trajectory {
    kind: TrajectoryKind, sample_rate: f64, speed_hz: f64, radius: f32, elevation: f32, sample_clock: u64,
}

impl Trajectory {
    pub fn new(kind: TrajectoryKind, sample_rate: u32, speed_hz: f32, radius: f32, elevation: f32) -> Self {
        Self {
            kind, sample_rate: f64::from(sample_rate.max(1)), speed_hz: f64::from(speed_hz.clamp(0.005, 2.0)),
            radius: radius.clamp(0.05, 8.0), elevation: elevation.clamp(-1.0, 1.0), sample_clock: 0,
        }
    }
    pub fn sample_clock(&self) -> u64 { self.sample_clock }
    pub fn reset(&mut self) { self.sample_clock = 0; }

    /// Return start/end poses for one render block and advance the audio-owned sample clock.
    pub fn next_segment(&mut self, frames: usize) -> (SourcePose, SourcePose) {
        let start = self.pose_at(self.sample_clock);
        self.sample_clock = self.sample_clock.saturating_add(frames as u64);
        let end = self.pose_at(self.sample_clock);
        (start, end)
    }

    fn pose_at(&self, sample_clock: u64) -> SourcePose {
        let phase = sample_clock as f64 * self.speed_hz * TAU / self.sample_rate;
        let (sin, cos) = phase.sin_cos();
        let sin = sin as f32; let cos = cos as f32; let sin2 = 2.0 * sin * cos;
        let (x, y, z, distance_scale) = match self.kind {
            TrajectoryKind::Orbit360 => (sin, self.elevation, cos, 1.0),
            TrajectoryKind::FigureEight => (sin, self.elevation + sin2 * 0.18, cos * sin, 0.82 + 0.18 * cos.abs()),
            TrajectoryKind::Pendulum => (sin, self.elevation, 0.72, 0.86 + 0.14 * cos.abs()),
            TrajectoryKind::FrontBack => (sin * 0.14, self.elevation, cos, 0.90 + 0.10 * sin.abs()),
            TrajectoryKind::Planetary => (sin, self.elevation + sin2 * 0.24, cos, 0.62 + 0.38 * sin2.abs()),
            TrajectoryKind::NearEar => (sin, self.elevation + cos * 0.12, 0.22 + cos * 0.42, 0.38 + 0.18 * sin2.abs()),
            TrajectoryKind::Helix => (sin, self.elevation + sin2 * 0.42, cos, 0.88 + 0.12 * cos.abs()),
        };
        let direction = Vec3::new(x, y, z).normalized_or(Vec3::FORWARD);
        let radius = self.radius * distance_scale;
        SourcePose {
            position: Vec3::new(direction.x * radius, direction.y * radius, direction.z * radius),
            velocity: Vec3::ZERO, gain: 1.0, spread: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trajectory_is_audio_clock_driven() {
        let mut trajectory = Trajectory::new(TrajectoryKind::Orbit360, 48_000, 0.5, 1.0, 0.0);
        let (_, first_end) = trajectory.next_segment(480);
        let (_, second_end) = trajectory.next_segment(480);
        assert_eq!(trajectory.sample_clock(), 960);
        assert_ne!(first_end.position, second_end.position);
    }
}
