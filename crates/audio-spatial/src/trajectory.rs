use std::f64::consts::TAU;

use crate::{SourcePose, Vec3};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrajectoryKind {
    Orbit360,
    FigureEight,
    Pendulum,
    FrontBack,
    Planetary,
    NearEar,
    Helix,
}

#[derive(Clone, Debug)]
pub struct Trajectory {
    kind: TrajectoryKind,
    sample_rate: f64,
    speed_hz: f64,
    radius: f32,
    elevation: f32,
    direction: f64,
    sample_clock: u64,
}

impl Trajectory {
    pub fn new(
        kind: TrajectoryKind,
        sample_rate: u32,
        speed_hz: f32,
        radius: f32,
        elevation: f32,
    ) -> Self {
        Self {
            kind,
            sample_rate: f64::from(sample_rate.max(1)),
            speed_hz: f64::from(speed_hz.clamp(0.005, 2.0)),
            radius: radius.clamp(0.05, 8.0),
            elevation: elevation.clamp(-1.0, 1.0),
            direction: 1.0,
            sample_clock: 0,
        }
    }

    pub fn set_clockwise(&mut self, clockwise: bool) {
        self.direction = if clockwise { 1.0 } else { -1.0 };
    }

    pub fn sample_clock(&self) -> u64 {
        self.sample_clock
    }

    pub fn reset(&mut self) {
        self.sample_clock = 0;
    }

    /// Return start/end poses for one render block and advance the audio-owned sample clock.
    pub fn next_segment(&mut self, frames: usize) -> (SourcePose, SourcePose) {
        let start = self.pose_at(self.sample_clock);
        self.sample_clock = self.sample_clock.saturating_add(frames as u64);
        let end = self.pose_at(self.sample_clock);
        (start, end)
    }

    fn pose_at(&self, sample_clock: u64) -> SourcePose {
        let position = self.position_at(sample_clock);
        let next_clock = sample_clock.saturating_add(1);
        let velocity = if sample_clock == 0 {
            velocity_between(position, self.position_at(next_clock), self.sample_rate as f32)
        } else {
            let previous = self.position_at(sample_clock - 1);
            let next = self.position_at(next_clock);
            velocity_between(previous, next, self.sample_rate as f32 * 0.5)
        };
        SourcePose {
            position,
            velocity,
            gain: 1.0,
            spread: 0.0,
        }
    }

    fn position_at(&self, sample_clock: u64) -> Vec3 {
        let phase = sample_clock as f64 * self.speed_hz * TAU / self.sample_rate * self.direction;
        let (sin, cos) = phase.sin_cos();
        let sin = sin as f32;
        let cos = cos as f32;
        let sin2 = 2.0 * sin * cos;
        let (x, y, z, distance_scale) = match self.kind {
            TrajectoryKind::Orbit360 => (sin, self.elevation, cos, 1.0),
            TrajectoryKind::FigureEight => (
                sin,
                self.elevation + sin2 * 0.18,
                cos * sin,
                0.82 + 0.18 * cos.abs(),
            ),
            TrajectoryKind::Pendulum => (sin, self.elevation, 0.72, 0.86 + 0.14 * cos.abs()),
            TrajectoryKind::FrontBack => (sin * 0.14, self.elevation, cos, 0.90 + 0.10 * sin.abs()),
            TrajectoryKind::Planetary => (
                sin,
                self.elevation + sin2 * 0.24,
                cos,
                0.62 + 0.38 * sin2.abs(),
            ),
            TrajectoryKind::NearEar => (
                sin,
                self.elevation + cos * 0.12,
                0.22 + cos * 0.42,
                0.38 + 0.18 * sin2.abs(),
            ),
            TrajectoryKind::Helix => (
                sin,
                self.elevation + sin2 * 0.42,
                cos,
                0.88 + 0.12 * cos.abs(),
            ),
        };
        let direction = Vec3::new(x, y, z).normalized_or(Vec3::FORWARD);
        let radius = self.radius * distance_scale;
        Vec3::new(
            direction.x * radius,
            direction.y * radius,
            direction.z * radius,
        )
    }
}

#[inline]
fn velocity_between(start: Vec3, end: Vec3, scale: f32) -> Vec3 {
    Vec3::new(
        (end.x - start.x) * scale,
        (end.y - start.y) * scale,
        (end.z - start.z) * scale,
    )
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

    #[test]
    fn counter_clockwise_reverses_lateral_motion_without_changing_clock() {
        let mut clockwise = Trajectory::new(TrajectoryKind::Orbit360, 48_000, 0.5, 1.0, 0.0);
        let mut counter = clockwise.clone();
        counter.set_clockwise(false);

        let (_, clockwise_end) = clockwise.next_segment(1_200);
        let (_, counter_end) = counter.next_segment(1_200);
        assert_eq!(clockwise.sample_clock(), counter.sample_clock());
        assert!((clockwise_end.position.x + counter_end.position.x).abs() < 1.0e-5);
        assert!((clockwise_end.position.z - counter_end.position.z).abs() < 1.0e-5);
        assert!((clockwise_end.velocity.x + counter_end.velocity.x).abs() < 1.0e-3);
    }

    #[test]
    fn orbit_pose_reports_tangential_velocity_in_meters_per_second() {
        let trajectory = Trajectory::new(TrajectoryKind::Orbit360, 48_000, 0.5, 1.0, 0.0);
        let pose = trajectory.pose_at(0);
        let speed = pose.velocity.length();
        let expected = std::f32::consts::TAU * 0.5;
        assert!((speed - expected).abs() < 0.01);
        assert!(pose.position.dot(pose.velocity).abs() < 0.001);
    }

    #[test]
    fn all_motion_modes_report_finite_velocity() {
        for kind in [
            TrajectoryKind::Orbit360,
            TrajectoryKind::FigureEight,
            TrajectoryKind::Pendulum,
            TrajectoryKind::FrontBack,
            TrajectoryKind::Planetary,
            TrajectoryKind::NearEar,
            TrajectoryKind::Helix,
        ] {
            let trajectory = Trajectory::new(kind, 48_000, 0.75, 1.2, 0.1);
            let pose = trajectory.pose_at(12_345);
            assert!(pose.velocity.x.is_finite());
            assert!(pose.velocity.y.is_finite());
            assert!(pose.velocity.z.is_finite());
        }
    }
}
