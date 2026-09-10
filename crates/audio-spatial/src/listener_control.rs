use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::{ListenerPose, Vec3};

const READ_RETRIES: usize = 4;

struct RuntimeListenerPoseSlot {
    enabled: AtomicBool,
    epoch: AtomicU64,
    words: [AtomicU32; 9],
}

impl RuntimeListenerPoseSlot {
    const fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            words: [
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(1.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
                AtomicU32::new(1.0_f32.to_bits()),
                AtomicU32::new(0.0_f32.to_bits()),
            ],
        }
    }

    fn store(&self, pose: ListenerPose) {
        let pose = sanitize_listener_pose(pose);
        let mut epoch = self.epoch.load(Ordering::Acquire);
        loop {
            if epoch & 1 != 0 {
                std::hint::spin_loop();
                epoch = self.epoch.load(Ordering::Acquire);
                continue;
            }
            match self.epoch.compare_exchange_weak(
                epoch,
                epoch.wrapping_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => epoch = current,
            }
        }

        for (word, value) in self.words.iter().zip([
            pose.position.x,
            pose.position.y,
            pose.position.z,
            pose.forward.x,
            pose.forward.y,
            pose.forward.z,
            pose.up.x,
            pose.up.y,
            pose.up.z,
        ]) {
            word.store(value.to_bits(), Ordering::Relaxed);
        }

        self.epoch.store(epoch.wrapping_add(2), Ordering::Release);
        self.enabled.store(true, Ordering::Release);
    }

    #[inline]
    fn load(&self) -> Option<ListenerPose> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }

        for _ in 0..READ_RETRIES {
            let start = self.epoch.load(Ordering::Acquire);
            if start & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            let mut values = [0.0_f32; 9];
            for (destination, word) in values.iter_mut().zip(&self.words) {
                *destination = f32::from_bits(word.load(Ordering::Relaxed));
            }

            let end = self.epoch.load(Ordering::Acquire);
            if start == end && end & 1 == 0 && self.enabled.load(Ordering::Acquire) {
                return Some(ListenerPose {
                    position: Vec3::new(values[0], values[1], values[2]),
                    forward: Vec3::new(values[3], values[4], values[5]),
                    up: Vec3::new(values[6], values[7], values[8]),
                });
            }
            std::hint::spin_loop();
        }
        None
    }
}

static RUNTIME_LISTENER_POSE: RuntimeListenerPoseSlot = RuntimeListenerPoseSlot::new();

/// Publish the latest listener/head pose without routing high-rate tracking updates through the
/// player command queue. Writers may run on a UI or tracking thread; realtime renderers read this
/// slot once per internal DSP block and keep the previous pose if a write is in progress.
pub fn set_runtime_listener_pose(pose: ListenerPose) {
    RUNTIME_LISTENER_POSE.store(pose);
}

/// Return listener tracking to the identity/head-forward pose while keeping the lock-free runtime
/// pose channel active for every current SpatialEngine instance.
pub fn reset_runtime_listener_pose() {
    RUNTIME_LISTENER_POSE.store(ListenerPose::identity());
}

#[inline]
pub(crate) fn latest_runtime_listener_pose() -> Option<ListenerPose> {
    RUNTIME_LISTENER_POSE.load()
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
    fn fresh_slot_is_inactive_until_first_runtime_pose() {
        let slot = RuntimeListenerPoseSlot::new();
        assert_eq!(slot.load(), None);
    }

    #[test]
    fn slot_round_trips_position_forward_and_up_as_one_snapshot() {
        let slot = RuntimeListenerPoseSlot::new();
        let pose = ListenerPose {
            position: Vec3::new(1.25, -0.40, 2.50),
            forward: Vec3::new(0.60, 0.20, 0.77),
            up: Vec3::new(-0.10, 0.97, 0.20),
        };
        slot.store(pose);
        assert_eq!(slot.load(), Some(pose));
    }

    #[test]
    fn non_finite_components_are_blocked_before_realtime_state() {
        let slot = RuntimeListenerPoseSlot::new();
        slot.store(ListenerPose {
            position: Vec3::new(f32::NAN, 1.0, f32::INFINITY),
            forward: Vec3::new(f32::NEG_INFINITY, 0.0, 1.0),
            up: Vec3::new(0.0, f32::NAN, 0.0),
        });
        let pose = slot.load().expect("runtime pose");
        assert_eq!(pose.position, Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(pose.forward, Vec3::FORWARD);
        assert_eq!(pose.up, Vec3::UP);
        assert!(pose.position.x.is_finite());
        assert!(pose.position.y.is_finite());
        assert!(pose.position.z.is_finite());
    }
}
