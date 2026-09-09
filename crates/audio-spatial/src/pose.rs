pub const DEFAULT_HEAD_RADIUS_M: f32 = 0.0875;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);
    pub const FORWARD: Self = Self::new(0.0, 0.0, 1.0);
    pub const BACK: Self = Self::new(0.0, 0.0, -1.0);
    pub const UP: Self = Self::new(0.0, 1.0, 0.0);
    pub const RIGHT: Self = Self::new(1.0, 0.0, 0.0);
    pub const LEFT: Self = Self::new(-1.0, 0.0, 0.0);

    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[inline]
    pub fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    #[inline]
    pub fn cross(self, other: Self) -> Self {
        Self::new(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    #[inline]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    #[inline]
    pub fn normalized_or(self, fallback: Self) -> Self {
        let length = self.length();
        if length > 1.0e-8 {
            Self::new(self.x / length, self.y / length, self.z / length)
        } else {
            fallback
        }
    }

    #[inline]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self::new(
            self.x + (other.x - self.x) * t,
            self.y + (other.y - self.y) * t,
            self.z + (other.z - self.z) * t,
        )
    }
}

impl std::ops::Add for Vec3 {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self::Output {
        Self::new(self.x + rhs.x, self.y + rhs.y, self.z + rhs.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        Self::new(self.x - rhs.x, self.y - rhs.y, self.z - rhs.z)
    }
}

impl std::ops::Mul<f32> for Vec3 {
    type Output = Self;

    #[inline]
    fn mul(self, rhs: f32) -> Self::Output {
        Self::new(self.x * rhs, self.y * rhs, self.z * rhs)
    }
}

impl std::ops::Mul<Vec3> for f32 {
    type Output = Vec3;

    #[inline]
    fn mul(self, rhs: Vec3) -> Self::Output {
        rhs * self
    }
}

#[inline]
fn orthonormal_basis(forward: Vec3, up: Vec3) -> (Vec3, Vec3, Vec3) {
    let forward = forward.normalized_or(Vec3::FORWARD);
    let up_hint = up.normalized_or(Vec3::UP);
    let right = up_hint.cross(forward).normalized_or(Vec3::RIGHT);
    let up = forward.cross(right).normalized_or(Vec3::UP);
    (right, up, forward)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SourcePose {
    pub position: Vec3,
    pub velocity: Vec3,
    pub gain: f32,
    pub spread: f32,
}

impl SourcePose {
    pub const fn new(position: Vec3) -> Self {
        Self {
            position,
            velocity: Vec3::ZERO,
            gain: 1.0,
            spread: 0.0,
        }
    }

    #[inline]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            position: self.position.lerp(other.position, t),
            velocity: self.velocity.lerp(other.velocity, t),
            gain: self.gain + (other.gain - self.gain) * t,
            spread: self.spread + (other.spread - self.spread) * t,
        }
    }
}

impl Default for SourcePose {
    fn default() -> Self {
        Self::new(Vec3::FORWARD)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerPose {
    pub position: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}

impl ListenerPose {
    pub const fn identity() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::FORWARD,
            up: Vec3::UP,
        }
    }

    /// Return an orthonormal listener-local `(right, up, forward)` basis.
    #[inline]
    pub fn basis(self) -> (Vec3, Vec3, Vec3) {
        orthonormal_basis(self.forward, self.up)
    }

    /// Return the physical left/right ear reference positions used by the parametric binaural model.
    #[inline]
    pub fn ear_positions(self) -> (Vec3, Vec3) {
        self.ear_positions_with_radius(DEFAULT_HEAD_RADIUS_M)
    }

    #[inline]
    pub fn ear_positions_with_radius(self, radius_m: f32) -> (Vec3, Vec3) {
        let (right, _, _) = self.basis();
        let radius_m = if radius_m.is_finite() {
            radius_m.max(0.0)
        } else {
            DEFAULT_HEAD_RADIUS_M
        };
        (
            self.position - right * radius_m,
            self.position + right * radius_m,
        )
    }
}

impl Default for ListenerPose {
    fn default() -> Self {
        Self::identity()
    }
}

/// World-space transform of the rectangular acoustic room.
///
/// The room is deliberately independent from `ListenerPose`: turning or moving the listener must not
/// rotate or translate the walls. This becomes the stable coordinate frame for image-source geometry,
/// while direct binaural cues continue to use the listener's own head basis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomPose {
    pub position: Vec3,
    pub forward: Vec3,
    pub up: Vec3,
}

impl RoomPose {
    pub const fn identity() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::FORWARD,
            up: Vec3::UP,
        }
    }

    /// Compatibility pose reproducing the previous listener-centered room exactly.
    pub const fn from_listener(listener: ListenerPose) -> Self {
        Self {
            position: listener.position,
            forward: listener.forward,
            up: listener.up,
        }
    }

    #[inline]
    pub fn basis(self) -> (Vec3, Vec3, Vec3) {
        orthonormal_basis(self.forward, self.up)
    }

    #[inline]
    pub fn world_to_local(self, point: Vec3) -> Vec3 {
        let relative = point - self.position;
        let (right, up, forward) = self.basis();
        Vec3::new(
            relative.dot(right),
            relative.dot(up),
            relative.dot(forward),
        )
    }

    #[inline]
    pub fn local_to_world(self, point: Vec3) -> Vec3 {
        let (right, up, forward) = self.basis();
        self.position + right * point.x + up * point.y + forward * point.z
    }
}

impl Default for RoomPose {
    fn default() -> Self {
        Self::identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_listener_basis_is_right_up_forward() {
        let (right, up, forward) = ListenerPose::identity().basis();
        assert!((right.x - 1.0).abs() < 1.0e-6);
        assert!((up.y - 1.0).abs() < 1.0e-6);
        assert!((forward.z - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn default_ear_positions_share_the_renderer_head_radius() {
        let listener = ListenerPose::identity();
        let (left, right) = listener.ear_positions();
        assert!((left.x + DEFAULT_HEAD_RADIUS_M).abs() < 1.0e-6);
        assert!((right.x - DEFAULT_HEAD_RADIUS_M).abs() < 1.0e-6);
        assert!(((right - left).length() - DEFAULT_HEAD_RADIUS_M * 2.0).abs() < 1.0e-6);
    }

    #[test]
    fn room_pose_round_trips_world_coordinates() {
        let room = RoomPose {
            position: Vec3::new(2.0, 0.5, -1.0),
            forward: Vec3::RIGHT,
            up: Vec3::UP,
        };
        let world = Vec3::new(4.0, 1.25, 3.0);
        let local = room.world_to_local(world);
        let restored = room.local_to_world(local);
        assert!((restored.x - world.x).abs() < 1.0e-5);
        assert!((restored.y - world.y).abs() < 1.0e-5);
        assert!((restored.z - world.z).abs() < 1.0e-5);
    }

    #[test]
    fn listener_rotation_does_not_change_identity_room_basis() {
        let listener = ListenerPose {
            position: Vec3::ZERO,
            forward: Vec3::RIGHT,
            up: Vec3::UP,
        };
        let (_, _, listener_forward) = listener.basis();
        let (_, _, room_forward) = RoomPose::identity().basis();
        assert_eq!(listener_forward, Vec3::RIGHT);
        assert_eq!(room_forward, Vec3::FORWARD);
    }

    #[test]
    fn vector_add_and_scalar_multiply_support_geometry_without_temporaries() {
        let value = Vec3::RIGHT + Vec3::FORWARD * 2.0;
        assert_eq!(value, Vec3::new(1.0, 0.0, 2.0));
        assert_eq!(0.5 * value, Vec3::new(0.5, 0.0, 1.0));
        assert_eq!(Vec3::BACK, Vec3::new(0.0, 0.0, -1.0));
    }
}
