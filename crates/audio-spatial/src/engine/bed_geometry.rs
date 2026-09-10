use super::*;

const STATIC_VERTICAL_FOCUS_REFERENCE_DEGREES: f32 = 30.0;
const STATIC_HEIGHT_ENERGY_BOOST: f32 = 0.46;
const STATIC_LOWER_ENERGY_BOOST: f32 = 0.07;

/// Optional elevation remap for a synthesized speaker bed.
///
/// This transform is deliberately separate from `SpeakerLayout`: the standard/native 5.1/7.1/.2/.4
/// contracts remain authored speaker geometry. Callers may use this only when they are deliberately
/// constructing a virtual bed from stereo/mono programme and want a wider upper/lower hemisphere.
/// It does not create non-standard "floor speaker" channel roles.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpeakerBedGeometry {
    pub front_elevation_offset_degrees: f32,
    pub surround_elevation_offset_degrees: f32,
    pub rear_elevation_offset_degrees: f32,
    pub top_elevation_offset_degrees: f32,
}

impl SpeakerBedGeometry {
    pub const IDENTITY: Self = Self {
        front_elevation_offset_degrees: 0.0,
        surround_elevation_offset_degrees: 0.0,
        rear_elevation_offset_degrees: 0.0,
        top_elevation_offset_degrees: 0.0,
    };

    #[inline]
    fn sanitized(self) -> Self {
        Self {
            front_elevation_offset_degrees: sanitize_offset(
                self.front_elevation_offset_degrees,
            ),
            surround_elevation_offset_degrees: sanitize_offset(
                self.surround_elevation_offset_degrees,
            ),
            rear_elevation_offset_degrees: sanitize_offset(self.rear_elevation_offset_degrees),
            top_elevation_offset_degrees: sanitize_offset(self.top_elevation_offset_degrees),
        }
    }

    #[inline]
    fn offset_for_role(self, role: Option<ChannelRole>) -> f32 {
        match role {
            Some(ChannelRole::FrontLeft | ChannelRole::FrontRight) => {
                self.front_elevation_offset_degrees
            }
            Some(ChannelRole::SurroundLeft | ChannelRole::SurroundRight) => {
                self.surround_elevation_offset_degrees
            }
            Some(ChannelRole::RearLeft | ChannelRole::RearRight) => {
                self.rear_elevation_offset_degrees
            }
            Some(
                ChannelRole::TopFrontLeft
                | ChannelRole::TopFrontRight
                | ChannelRole::TopRearLeft
                | ChannelRole::TopRearRight,
            ) => self.top_elevation_offset_degrees,
            // Keep the centre programme anchored to the horizon. LFE is direction-independent and
            // therefore must never acquire a virtual elevation from this transform.
            Some(ChannelRole::Center | ChannelRole::Lfe) | None => 0.0,
        }
    }

    #[inline]
    fn vertical_focus(self) -> f32 {
        let max_offset = self
            .front_elevation_offset_degrees
            .abs()
            .max(self.surround_elevation_offset_degrees.abs())
            .max(self.rear_elevation_offset_degrees.abs())
            .max(self.top_elevation_offset_degrees.abs());
        (max_offset / STATIC_VERTICAL_FOCUS_REFERENCE_DEGREES).clamp(0.0, 1.0)
    }

    #[inline]
    fn energy_scale_for_role(self, role: Option<ChannelRole>) -> f32 {
        let focus = self.vertical_focus();
        match role {
            Some(
                ChannelRole::TopFrontLeft
                | ChannelRole::TopFrontRight
                | ChannelRole::TopRearLeft
                | ChannelRole::TopRearRight,
            ) => 1.0 + STATIC_HEIGHT_ENERGY_BOOST * focus,
            Some(
                ChannelRole::SurroundLeft
                | ChannelRole::SurroundRight
                | ChannelRole::RearLeft
                | ChannelRole::RearRight,
            ) => 1.0 + STATIC_LOWER_ENERGY_BOOST * focus,
            Some(ChannelRole::FrontLeft | ChannelRole::FrontRight | ChannelRole::Center | ChannelRole::Lfe)
            | None => 1.0,
        }
    }

    #[inline]
    fn remap_speaker(self, speaker: Speaker, role: Option<ChannelRole>) -> Speaker {
        if speaker.kind == crate::SourceKind::Lfe {
            return speaker;
        }
        let offset_degrees = self.offset_for_role(role);
        if offset_degrees.abs() <= f32::EPSILON {
            return speaker;
        }

        let radius = speaker.direction.length();
        let direction = speaker.direction.normalized_or(Vec3::FORWARD);
        let horizontal = (direction.x * direction.x + direction.z * direction.z).sqrt();
        let base_elevation = direction.y.atan2(horizontal);
        // Avoid the exact poles: azimuth becomes undefined there and tiny numerical changes would
        // make a virtual source jump around the listener's vertical axis.
        let target_elevation = (base_elevation + offset_degrees.to_radians())
            .clamp(-75.0_f32.to_radians(), 75.0_f32.to_radians());
        let azimuth = direction.x.atan2(direction.z);
        let (elevation_sin, elevation_cos) = target_elevation.sin_cos();
        let (azimuth_sin, azimuth_cos) = azimuth.sin_cos();
        Speaker {
            direction: Vec3::new(
                azimuth_sin * elevation_cos,
                elevation_sin,
                azimuth_cos * elevation_cos,
            ) * radius.max(1.0e-6),
            ..speaker
        }
    }
}

/// Render a synthesized speaker bed with an explicit vertical geometry remap.
///
/// `render_interleaved_layout()` remains the canonical native/authored API and calls the same core
/// with identity geometry. This variant exists so stereo-derived virtual beds can occupy both
/// hemispheres without mutating standard speaker-layout semantics. When the static remap opens a
/// large vertical aperture, height roles receive a modest energy preference and the auxiliary bed
/// is power-normalized as a group. This makes the upper hemisphere audible without increasing the
/// total nominal surround/rear/height source power or adding another EQ stage.
impl SpatialEngine {
    pub fn render_interleaved_layout_with_geometry(
        &mut self,
        input: &[f32],
        layout_kind: ChannelLayout,
        geometry: SpeakerBedGeometry,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
        self.render_interleaved_layout_geometry_impl(input, layout_kind, geometry, output)
    }

    pub(super) fn render_interleaved_layout_geometry_impl(
        &mut self,
        input: &[f32],
        layout_kind: ChannelLayout,
        geometry: SpeakerBedGeometry,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
        let geometry = geometry.sanitized();
        let debug_layout = layout_kind;
        let layout = SpeakerLayout::for_layout(layout_kind);
        let channels = layout.channels();
        if channels == 0 {
            return Err(SpatialError::UnsupportedChannelLayout);
        }
        if channels > self.renderer.source_capacity() {
            return Err(SpatialError::SourceCapacityExceeded);
        }
        if input.len() % channels != 0 {
            return Err(SpatialError::ChannelCountMismatch);
        }
        let frames = input.len() / channels;
        if output.len() < frames.saturating_mul(2) {
            return Err(SpatialError::OutputTooSmall);
        }

        if self.debug_enabled {
            analyze_interleaved_activity(input, channels, &mut self.debug_activity);
        }

        let normalization = layout.normalization();
        let block_limit = self.scene_block_frames();
        let scene_intensity = self.scene_motion.map_or(0.0, |motion| motion.intensity);
        let mut base_speakers = [Speaker::full_range(Vec3::FORWARD, 0.0); MAX_DEBUG_SOURCES];
        prepare_virtual_bed_speakers(layout, geometry, &mut base_speakers[..channels]);
        let mut latest_poses = [SourcePose::default(); MAX_DEBUG_SOURCES];
        let mut early_reflection_sources = [false; MAX_DEBUG_SOURCES];
        for source_index in 0..channels {
            let transformed = base_speakers[source_index];
            latest_poses[source_index] = static_speaker_pose(transformed);
            early_reflection_sources[source_index] =
                authored_role_uses_early_reflections(layout.role(source_index));
        }

        let mut frame_offset = 0usize;
        while frame_offset < frames {
            let block_frames = (frames - frame_offset).min(block_limit);
            let (listener_start, listener_end) = self.runtime_listener_segment();
            self.mix_left[..block_frames].fill(0.0);
            self.mix_right[..block_frames].fill(0.0);
            let block_start = frame_offset * channels;
            let block_end = block_start + block_frames * channels;
            let block = &input[block_start..block_end];
            let scene_segment = self
                .scene_trajectory
                .as_mut()
                .map(|trajectory| trajectory.next_segment(block_frames));

            for source_index in 0..channels {
                let speaker = base_speakers[source_index];
                let (mut start_pose, mut end_pose) =
                    if let Some((scene_start, scene_end)) = scene_segment {
                        (
                            scene_speaker_pose(speaker, scene_start, scene_intensity),
                            scene_speaker_pose(speaker, scene_end, scene_intensity),
                        )
                    } else {
                        let pose = static_speaker_pose(speaker);
                        (pose, pose)
                    };
                bind_segment_velocity(
                    &mut start_pose,
                    &mut end_pose,
                    block_frames,
                    self.config.sample_rate,
                    speaker,
                );
                latest_poses[source_index] = end_pose;
                self.renderer.render_strided_source(
                    source_index,
                    block,
                    channels,
                    source_index,
                    block_frames,
                    start_pose,
                    end_pose,
                    listener_start,
                    listener_end,
                    speaker.kind,
                    early_reflection_sources[source_index],
                    &mut self.mix_left,
                    &mut self.mix_right,
                )?;
            }

            for frame in 0..block_frames {
                self.mix_left[frame] *= normalization;
                self.mix_right[frame] *= normalization;
            }
            self.late_field.process_planar(
                &mut self.mix_left[..block_frames],
                &mut self.mix_right[..block_frames],
            );
            for frame in 0..block_frames {
                let output_index = (frame_offset + frame) * 2;
                output[output_index] = self.mix_left[frame];
                output[output_index + 1] = self.mix_right[frame];
            }
            frame_offset += block_frames;
        }

        if self.debug_enabled {
            self.debug_snapshot
                .begin_capture(self.listener, self.config.environment);
            self.debug_snapshot.set_layout(Some(debug_layout));
            for source_index in 0..channels {
                let speaker = base_speakers[source_index];
                self.debug_snapshot
                    .record_source(source_index, speaker.kind, latest_poses[source_index]);
                self.debug_snapshot
                    .set_source_activity(source_index, self.debug_activity[source_index]);
            }
            self.debug_snapshot.finish_capture(frames);
        }
        Ok(frames)
    }
}

#[inline]
fn prepare_virtual_bed_speakers(
    layout: SpeakerLayout,
    geometry: SpeakerBedGeometry,
    output: &mut [Speaker],
) {
    debug_assert!(output.len() >= layout.channels());
    let focus = geometry.vertical_focus();
    let mut original_auxiliary_power = 0.0_f32;
    let mut weighted_auxiliary_power = 0.0_f32;

    for (source_index, speaker) in layout.speakers().iter().copied().enumerate() {
        let role = layout.role(source_index);
        let mut transformed = geometry.remap_speaker(speaker, role);
        if is_auxiliary_role(role) {
            original_auxiliary_power += speaker.gain * speaker.gain;
            let scale = geometry.energy_scale_for_role(role);
            transformed.gain *= scale;
            weighted_auxiliary_power += transformed.gain * transformed.gain;
        }
        output[source_index] = transformed;
    }

    if focus <= 1.0e-5
        || original_auxiliary_power <= 1.0e-12
        || weighted_auxiliary_power <= 1.0e-12
    {
        return;
    }

    // Re-distribute auxiliary energy instead of increasing it. Front L/R, centre and LFE are not
    // part of this normalization, so the dry/front anchor and direction-independent bass contract
    // remain untouched while height programme becomes less masked by the lower hemisphere.
    let correction = (original_auxiliary_power / weighted_auxiliary_power).sqrt();
    for (source_index, speaker) in output[..layout.channels()].iter_mut().enumerate() {
        if is_auxiliary_role(layout.role(source_index)) {
            speaker.gain *= correction;
        }
    }
}

#[inline]
fn is_auxiliary_role(role: Option<ChannelRole>) -> bool {
    matches!(
        role,
        Some(
            ChannelRole::SurroundLeft
                | ChannelRole::SurroundRight
                | ChannelRole::RearLeft
                | ChannelRole::RearRight
                | ChannelRole::TopFrontLeft
                | ChannelRole::TopFrontRight
                | ChannelRole::TopRearLeft
                | ChannelRole::TopRearRight
        )
    )
}

#[inline]
fn sanitize_offset(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(-45.0, 45.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use super::*;

    fn immersive_virtual_geometry() -> SpeakerBedGeometry {
        SpeakerBedGeometry {
            front_elevation_offset_degrees: 8.0,
            surround_elevation_offset_degrees: -22.0,
            rear_elevation_offset_degrees: -32.0,
            top_elevation_offset_degrees: 14.0,
        }
    }

    #[test]
    fn identity_geometry_preserves_standard_layout_directions_and_gains() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut prepared = [Speaker::full_range(Vec3::FORWARD, 0.0); 12];
        prepare_virtual_bed_speakers(layout, SpeakerBedGeometry::IDENTITY, &mut prepared);
        for (index, speaker) in layout.speakers().iter().copied().enumerate() {
            assert_eq!(prepared[index], speaker);
        }
    }

    #[test]
    fn virtual_geometry_spans_upper_and_lower_hemispheres_without_moving_center_or_lfe() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let geometry = immersive_virtual_geometry();
        let transformed: [Speaker; 12] = std::array::from_fn(|index| {
            geometry.remap_speaker(layout.speakers()[index], layout.role(index))
        });
        assert!(transformed[4].direction.y < -0.45);
        assert!(transformed[6].direction.y < -0.30);
        assert!(transformed[8].direction.y > 0.80);
        assert!(transformed[10].direction.y > 0.80);
        assert!(transformed[2].direction.y.abs() < 1.0e-6);
        assert_eq!(transformed[3].direction, Vec3::FORWARD);
        assert_eq!(transformed[3].kind, crate::SourceKind::Lfe);
    }

    #[test]
    fn vertical_energy_balance_preserves_auxiliary_power_and_prioritizes_height() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let mut prepared = [Speaker::full_range(Vec3::FORWARD, 0.0); 12];
        prepare_virtual_bed_speakers(layout, immersive_virtual_geometry(), &mut prepared);

        let mut original_auxiliary_power = 0.0_f32;
        let mut prepared_auxiliary_power = 0.0_f32;
        for (index, original) in layout.speakers().iter().copied().enumerate() {
            if is_auxiliary_role(layout.role(index)) {
                original_auxiliary_power += original.gain * original.gain;
                prepared_auxiliary_power += prepared[index].gain * prepared[index].gain;
            }
        }
        assert!((original_auxiliary_power - prepared_auxiliary_power).abs() < 1.0e-5);

        // Top-front is intentionally promoted relative to the standard bed while lower horizontal
        // support remains present. Front/C/LFE must remain exact anchors.
        assert!(prepared[8].gain > layout.speakers()[8].gain * 1.10);
        assert!(prepared[10].gain > layout.speakers()[10].gain * 1.10);
        assert!(prepared[4].gain > 0.0);
        assert!(prepared[6].gain > 0.0);
        assert_eq!(prepared[0].gain, layout.speakers()[0].gain);
        assert_eq!(prepared[2].gain, layout.speakers()[2].gain);
        assert_eq!(prepared[3].gain, layout.speakers()[3].gain);
    }

    #[test]
    fn geometry_remap_preserves_azimuth_and_radius() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let speaker = layout.speakers()[4];
        let transformed = immersive_virtual_geometry().remap_speaker(speaker, layout.role(4));
        let original_azimuth = speaker.direction.x.atan2(speaker.direction.z);
        let transformed_azimuth = transformed.direction.x.atan2(transformed.direction.z);
        assert!((original_azimuth - transformed_azimuth).abs() < 1.0e-5);
        assert!((speaker.direction.length() - transformed.direction.length()).abs() < 1.0e-5);
    }

    #[test]
    fn geometry_render_publishes_actual_lower_and_upper_sources() {
        let mut config = EngineConfig::new(48_000);
        config.environment.mix = 0.0;
        let mut engine = SpatialEngine::new(config).unwrap();
        engine.set_debug_enabled(true);
        let input = vec![0.05_f32; 64 * 12];
        let mut output = vec![0.0_f32; 64 * 2];
        engine
            .render_interleaved_layout_with_geometry(
                &input,
                ChannelLayout::Surround7_1_4,
                immersive_virtual_geometry(),
                &mut output,
            )
            .unwrap();
        let snapshot = engine.debug_snapshot().expect("debug snapshot");
        assert!(snapshot.sources[4].position.y < -0.45);
        assert!(snapshot.sources[8].position.y > 0.80);
        assert!(snapshot.sources[2].position.y.abs() < 1.0e-6);
        assert_eq!(snapshot.sources[3].kind, crate::SpatialDebugSourceKind::Lfe);
        assert_eq!(snapshot.sources[3].position, Vec3::FORWARD);
        assert!(snapshot.sources[8].gain > SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4).speakers()[8].gain);
    }

    #[test]
    fn non_finite_offsets_collapse_to_identity() {
        let geometry = SpeakerBedGeometry {
            front_elevation_offset_degrees: f32::NAN,
            surround_elevation_offset_degrees: f32::INFINITY,
            rear_elevation_offset_degrees: f32::NEG_INFINITY,
            top_elevation_offset_degrees: f32::NAN,
        };
        assert_eq!(geometry.sanitized(), SpeakerBedGeometry::IDENTITY);
    }

    #[test]
    fn elevation_offsets_are_clamped_before_rendering() {
        let geometry = SpeakerBedGeometry {
            front_elevation_offset_degrees: 180.0,
            surround_elevation_offset_degrees: -180.0,
            rear_elevation_offset_degrees: -90.0,
            top_elevation_offset_degrees: 90.0,
        }
        .sanitized();
        assert_eq!(geometry.front_elevation_offset_degrees, 45.0);
        assert_eq!(geometry.surround_elevation_offset_degrees, -45.0);
        assert_eq!(geometry.rear_elevation_offset_degrees, -45.0);
        assert_eq!(geometry.top_elevation_offset_degrees, 45.0);
    }

    #[test]
    fn pole_guard_keeps_virtual_directions_finite() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let transformed = SpeakerBedGeometry {
            top_elevation_offset_degrees: 45.0,
            ..SpeakerBedGeometry::IDENTITY
        }
        .remap_speaker(layout.speakers()[8], layout.role(8));
        assert!(transformed.direction.x.is_finite());
        assert!(transformed.direction.y.is_finite());
        assert!(transformed.direction.z.is_finite());
        let elevation = transformed
            .direction
            .y
            .atan2((transformed.direction.x.powi(2) + transformed.direction.z.powi(2)).sqrt())
            .abs();
        assert!(elevation < PI * 0.5);
    }
}
