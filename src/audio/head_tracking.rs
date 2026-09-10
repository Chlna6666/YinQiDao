use std::f32::consts::{FRAC_PI_2, PI};

use yinqidao_audio_spatial::{
    ListenerPose, Vec3, reset_runtime_listener_pose, set_runtime_listener_pose,
};

/// One realtime-capable head-tracking source.
///
/// Implementations are deliberately pull-based: a device thread, OpenTrack adapter, IMU bridge or
/// debug/mouse provider can expose only its latest pose without enqueueing a backlog of stale head
/// samples. The returned pose uses YinQiDao's world convention: +X right, +Y up, +Z forward.
pub trait HeadTrackingProvider: Send {
    fn poll_pose(&mut self) -> Option<ListenerPose>;

    fn reset(&mut self) {}
}

/// Human/device-friendly orientation sample converted to the full listener basis expected by the
/// spatial renderer. Angles are radians; positive yaw turns right and positive pitch looks up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadTrackingEulerPose {
    pub position_meters: Vec3,
    pub yaw_radians: f32,
    pub pitch_radians: f32,
    pub roll_radians: f32,
}

impl HeadTrackingEulerPose {
    pub const fn identity() -> Self {
        Self {
            position_meters: Vec3::ZERO,
            yaw_radians: 0.0,
            pitch_radians: 0.0,
            roll_radians: 0.0,
        }
    }

    /// Convert yaw/pitch/roll to an orthogonal forward/up pair without allocating.
    pub fn listener_pose(self) -> ListenerPose {
        let yaw = finite_or(self.yaw_radians, 0.0).rem_euclid(PI * 2.0);
        let pitch = finite_or(self.pitch_radians, 0.0)
            .clamp(-FRAC_PI_2 + 1.0e-4, FRAC_PI_2 - 1.0e-4);
        let roll = finite_or(self.roll_radians, 0.0).rem_euclid(PI * 2.0);
        let (yaw_sin, yaw_cos) = yaw.sin_cos();
        let (pitch_sin, pitch_cos) = pitch.sin_cos();
        let (roll_sin, roll_cos) = roll.sin_cos();

        let forward = Vec3::new(
            yaw_sin * pitch_cos,
            pitch_sin,
            yaw_cos * pitch_cos,
        )
        .normalized_or(Vec3::FORWARD);
        let right_without_roll = Vec3::new(yaw_cos, 0.0, -yaw_sin).normalized_or(Vec3::RIGHT);
        let up_without_roll = forward
            .cross(right_without_roll)
            .normalized_or(Vec3::UP);
        let up = (up_without_roll * roll_cos - right_without_roll * roll_sin)
            .normalized_or(up_without_roll);

        ListenerPose {
            position: sanitize_vec3(self.position_meters, Vec3::ZERO),
            forward,
            up,
        }
    }
}

impl Default for HeadTrackingEulerPose {
    fn default() -> Self {
        Self::identity()
    }
}

/// Re-centering transform shared by every tracking backend. A provider remains free to use its own
/// coordinate system internally; once a neutral ListenerPose is captured, subsequent poses are
/// expressed in that neutral listener-local basis so the neutral orientation maps to +Z/+Y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadTrackingCalibration {
    neutral: ListenerPose,
}

impl HeadTrackingCalibration {
    pub const fn identity() -> Self {
        Self {
            neutral: ListenerPose::identity(),
        }
    }

    pub fn from_neutral(neutral: ListenerPose) -> Self {
        Self {
            neutral: sanitize_listener_pose(neutral),
        }
    }

    pub fn neutral(self) -> ListenerPose {
        self.neutral
    }

    pub fn apply(self, raw: ListenerPose) -> ListenerPose {
        let raw = sanitize_listener_pose(raw);
        let neutral = sanitize_listener_pose(self.neutral);
        let (right, up, forward) = neutral.basis();
        let displacement = raw.position - neutral.position;

        ListenerPose {
            position: Vec3::new(
                displacement.dot(right),
                displacement.dot(up),
                displacement.dot(forward),
            ),
            forward: Vec3::new(
                raw.forward.dot(right),
                raw.forward.dot(up),
                raw.forward.dot(forward),
            )
            .normalized_or(Vec3::FORWARD),
            up: Vec3::new(raw.up.dot(right), raw.up.dot(up), raw.up.dot(forward))
                .normalized_or(Vec3::UP),
        }
    }
}

impl Default for HeadTrackingCalibration {
    fn default() -> Self {
        Self::identity()
    }
}

/// Latest-only manual provider used by the Audio Laboratory and suitable for mouse/debug input.
/// Repeated pushes overwrite the pending pose instead of forming a queue, matching the realtime
/// listener slot's semantics and preventing stale cursor motion from lagging behind audio.
#[derive(Clone, Debug, Default)]
pub struct ManualHeadTrackingProvider {
    pending: Option<ListenerPose>,
}

impl ManualHeadTrackingProvider {
    pub fn push_pose(&mut self, pose: ListenerPose) {
        self.pending = Some(sanitize_listener_pose(pose));
    }

    pub fn push_euler(&mut self, pose: HeadTrackingEulerPose) {
        self.push_pose(pose.listener_pose());
    }
}

impl HeadTrackingProvider for ManualHeadTrackingProvider {
    fn poll_pose(&mut self) -> Option<ListenerPose> {
        self.pending.take()
    }

    fn reset(&mut self) {
        self.pending = None;
    }
}

/// Provider-to-DSP bridge. It performs calibration outside the realtime render loop and publishes
/// only the newest complete ListenerPose into the lock-free spatial control slot.
pub struct HeadTrackingBridge<P> {
    provider: P,
    calibration: HeadTrackingCalibration,
    last_raw_pose: Option<ListenerPose>,
}

impl<P: HeadTrackingProvider> HeadTrackingBridge<P> {
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            calibration: HeadTrackingCalibration::identity(),
            last_raw_pose: None,
        }
    }

    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn provider_mut(&mut self) -> &mut P {
        &mut self.provider
    }

    pub fn calibration(&self) -> HeadTrackingCalibration {
        self.calibration
    }

    /// Pull one latest device sample and publish it. Call this from the provider/input thread; the
    /// audio thread only reads the lock-free ListenerPose slot once per internal DSP block.
    pub fn poll_and_publish(&mut self) -> Option<ListenerPose> {
        let raw = self.provider.poll_pose()?;
        self.last_raw_pose = Some(raw);
        let calibrated = self.calibration.apply(raw);
        set_runtime_listener_pose(calibrated);
        Some(calibrated)
    }

    /// Treat the most recently observed raw pose as the new neutral forward/up/position origin.
    /// This does not reset provider/device state and therefore works for continuously streaming IMUs.
    pub fn recenter_to_last_pose(&mut self) -> bool {
        let Some(raw) = self.last_raw_pose else {
            return false;
        };
        self.calibration = HeadTrackingCalibration::from_neutral(raw);
        reset_runtime_listener_pose();
        true
    }

    pub fn clear_recenter(&mut self) {
        self.calibration = HeadTrackingCalibration::identity();
        reset_runtime_listener_pose();
    }

    pub fn reset(&mut self) {
        self.provider.reset();
        self.last_raw_pose = None;
        self.calibration = HeadTrackingCalibration::identity();
        reset_runtime_listener_pose();
    }
}

#[inline]
fn sanitize_listener_pose(pose: ListenerPose) -> ListenerPose {
    ListenerPose {
        position: sanitize_vec3(pose.position, Vec3::ZERO),
        forward: sanitize_vec3(pose.forward, Vec3::FORWARD),
        up: sanitize_vec3(pose.up, Vec3::UP),
    }
}

#[inline]
fn sanitize_vec3(value: Vec3, fallback: Vec3) -> Vec3 {
    Vec3::new(
        finite_or(value.x, fallback.x),
        finite_or(value.y, fallback.y),
        finite_or(value.z, fallback.z),
    )
}

#[inline]
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euler_identity_matches_listener_identity() {
        assert_eq!(
            HeadTrackingEulerPose::identity().listener_pose(),
            ListenerPose::identity()
        );
    }

    #[test]
    fn positive_yaw_turns_listener_to_the_right() {
        let pose = HeadTrackingEulerPose {
            yaw_radians: FRAC_PI_2,
            ..HeadTrackingEulerPose::identity()
        }
        .listener_pose();
        assert!((pose.forward.x - 1.0).abs() < 1.0e-5);
        assert!(pose.forward.y.abs() < 1.0e-5);
        assert!(pose.forward.z.abs() < 1.0e-5);
    }

    #[test]
    fn positive_pitch_raises_listener_forward_vector() {
        let pose = HeadTrackingEulerPose {
            pitch_radians: 0.40,
            ..HeadTrackingEulerPose::identity()
        }
        .listener_pose();
        assert!(pose.forward.y > 0.38);
        let (right, up, forward) = pose.basis();
        assert!(right.dot(up).abs() < 1.0e-5);
        assert!(right.dot(forward).abs() < 1.0e-5);
        assert!(up.dot(forward).abs() < 1.0e-5);
    }

    #[test]
    fn roll_changes_up_without_changing_forward() {
        let base = HeadTrackingEulerPose::identity().listener_pose();
        let rolled = HeadTrackingEulerPose {
            roll_radians: 0.5,
            ..HeadTrackingEulerPose::identity()
        }
        .listener_pose();
        assert_eq!(rolled.forward, base.forward);
        assert_ne!(rolled.up, base.up);
    }

    #[test]
    fn recenter_maps_neutral_pose_to_identity() {
        let neutral = HeadTrackingEulerPose {
            position_meters: Vec3::new(0.3, -0.1, 0.7),
            yaw_radians: 0.55,
            pitch_radians: -0.20,
            roll_radians: 0.12,
        }
        .listener_pose();
        let calibrated = HeadTrackingCalibration::from_neutral(neutral).apply(neutral);
        assert!(calibrated.position.length() < 1.0e-5);
        assert!((calibrated.forward - Vec3::FORWARD).length() < 1.0e-5);
        assert!((calibrated.up - Vec3::UP).length() < 1.0e-5);
    }

    #[test]
    fn manual_provider_keeps_only_the_latest_pose() {
        let mut provider = ManualHeadTrackingProvider::default();
        provider.push_euler(HeadTrackingEulerPose {
            yaw_radians: 0.1,
            ..HeadTrackingEulerPose::identity()
        });
        let newest = HeadTrackingEulerPose {
            yaw_radians: 0.8,
            ..HeadTrackingEulerPose::identity()
        }
        .listener_pose();
        provider.push_pose(newest);
        assert_eq!(provider.poll_pose(), Some(newest));
        assert_eq!(provider.poll_pose(), None);
    }

    #[test]
    fn non_finite_euler_input_cannot_escape_into_listener_pose() {
        let pose = HeadTrackingEulerPose {
            position_meters: Vec3::new(f32::NAN, 0.2, f32::INFINITY),
            yaw_radians: f32::NAN,
            pitch_radians: f32::INFINITY,
            roll_radians: f32::NEG_INFINITY,
        }
        .listener_pose();
        assert_eq!(pose.position, Vec3::new(0.0, 0.2, 0.0));
        assert!(pose.forward.x.is_finite());
        assert!(pose.forward.y.is_finite());
        assert!(pose.forward.z.is_finite());
        assert!(pose.up.x.is_finite());
        assert!(pose.up.y.is_finite());
        assert!(pose.up.z.is_finite());
    }
}
