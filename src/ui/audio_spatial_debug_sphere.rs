use std::f32::consts::PI;

const LATITUDE_DEGREES: [f32; 5] = [-60.0, -30.0, 0.0, 30.0, 60.0];
const MERIDIAN_COUNT: usize = 6;
const RING_SEGMENTS: usize = 32;

/// Listener-centric spherical reference field used by the GPU spatial debugger.
///
/// This module deliberately produces only line segments. The caller owns the actual GPU mesh
/// builder, so the field stays independent from GPUI and can be unit-tested without a graphics
/// context. The rectangular room is an acoustics implementation detail and must not define the
/// primary source-space visualization.
pub(super) fn for_each_spherical_segment(
    radius: f32,
    mut visit: impl FnMut([f32; 3], [f32; 3], bool),
) {
    let radius = finite_or(radius, 1.8).max(0.05);

    for latitude_degrees in LATITUDE_DEGREES {
        let latitude = latitude_degrees.to_radians();
        let y = radius * latitude.sin();
        let ring_radius = radius * latitude.cos();
        let mut previous = latitude_point(ring_radius, y, 0.0);
        for segment in 1..=RING_SEGMENTS {
            let phase = PI * 2.0 * segment as f32 / RING_SEGMENTS as f32;
            let current = latitude_point(ring_radius, y, phase);
            visit(previous, current, latitude_degrees == 0.0);
            previous = current;
        }
    }

    // A meridian and its PI-shifted counterpart describe the same great circle, so [0, PI) is
    // sufficient. Six great circles plus five latitude rings keep the sphere immediately readable
    // while staying cheap enough for the 30 Hz diagnostic mesh rebuild cadence.
    for meridian in 0..MERIDIAN_COUNT {
        let azimuth = PI * meridian as f32 / MERIDIAN_COUNT as f32;
        let mut previous = meridian_point(radius, azimuth, 0.0);
        for segment in 1..=RING_SEGMENTS {
            let phase = PI * 2.0 * segment as f32 / RING_SEGMENTS as f32;
            let current = meridian_point(radius, azimuth, phase);
            visit(previous, current, false);
            previous = current;
        }
    }
}

/// Choose the visible listener-centric shell from the actual direct-source geometry. Reflection
/// bounce positions are intentionally excluded: a large rectangular room should never shrink the
/// direct spherical field to a tiny object in the debugger.
pub(super) fn direct_field_radius<'a>(
    minimum: f32,
    source_positions: impl IntoIterator<Item = &'a [f32; 3]>,
) -> f32 {
    let mut radius = finite_or(minimum, 1.8).max(0.05);
    for position in source_positions {
        radius = radius.max(length3(*position) * 1.14);
    }
    radius
}

/// Room-reflection paths may extend beyond the direct shell. They are allowed to enlarge the camera
/// fit only within a bounded multiple of the primary field, preserving direct-source readability.
pub(super) fn bounded_fit_radius(field_radius: f32, acoustic_extent: f32) -> f32 {
    let field_radius = finite_or(field_radius, 1.8).max(0.05);
    let acoustic_extent = finite_or(acoustic_extent, field_radius).max(field_radius);
    (field_radius * 1.22).max(acoustic_extent.min(field_radius * 1.72) * 1.04)
}

#[inline]
fn latitude_point(radius: f32, y: f32, phase: f32) -> [f32; 3] {
    let (sin, cos) = phase.sin_cos();
    [radius * sin, y, radius * cos]
}

#[inline]
fn meridian_point(radius: f32, azimuth: f32, phase: f32) -> [f32; 3] {
    let (phase_sin, phase_cos) = phase.sin_cos();
    let (azimuth_sin, azimuth_cos) = azimuth.sin_cos();
    let horizontal = radius * phase_cos;
    [
        horizontal * azimuth_sin,
        radius * phase_sin,
        horizontal * azimuth_cos,
    ]
}

#[inline]
fn length3(value: [f32; 3]) -> f32 {
    (value[0] * value[0] + value[1] * value[1] + value[2] * value[2]).sqrt()
}

#[inline]
fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spherical_grid_stays_on_requested_shell() {
        let radius = 2.0;
        let mut count = 0usize;
        for_each_spherical_segment(radius, |start, end, _| {
            count += 1;
            assert!((length3(start) - radius).abs() < 1.0e-4);
            assert!((length3(end) - radius).abs() < 1.0e-4);
        });
        assert_eq!(
            count,
            (LATITUDE_DEGREES.len() + MERIDIAN_COUNT) * RING_SEGMENTS
        );
    }

    #[test]
    fn direct_sources_define_field_without_room_extent() {
        let positions = [[0.0, 0.0, 1.0], [0.0, 1.1, 0.0], [1.6, 0.0, 0.0]];
        let radius = direct_field_radius(1.0, positions.iter());
        assert!((radius - 1.6 * 1.14).abs() < 1.0e-5);
    }

    #[test]
    fn reflection_extent_is_bounded_relative_to_direct_field() {
        let fit = bounded_fit_radius(2.0, 20.0);
        assert!(fit >= 2.0 * 1.22);
        assert!(fit <= 2.0 * 1.72 * 1.04 + 1.0e-5);
    }
}
