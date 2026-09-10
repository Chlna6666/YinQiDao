use crate::{
    ChannelLayout, ChannelRole, EnvironmentSettings, ListenerPose, MAX_DEBUG_SOURCES, SourceActivity,
    SourcePose, SpatialDebugSnapshot, SpatialError, Speaker, SpeakerLayout, Trajectory,
    TrajectoryKind, Vec3, analyze_interleaved_activity, late_field::LateDiffuseField,
    listener_control::latest_runtime_listener_pose, renderer::CpuRenderer,
};

pub const DEFAULT_BLOCK_FRAMES: usize = 64;
pub const DEFAULT_MAX_SOURCES: usize = 32;

const MAX_SCENE_SEGMENT_DEGREES: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EngineConfig {
    pub sample_rate: u32,
    pub block_frames: usize,
    pub max_sources: usize,
    pub environment: EnvironmentSettings,
}

impl EngineConfig {
    pub const fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            block_frames: DEFAULT_BLOCK_FRAMES,
            max_sources: DEFAULT_MAX_SOURCES,
            environment: EnvironmentSettings {
                mix: 0.10,
                room_size: 0.30,
                damping: 0.45,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SceneMotionSignature {
    kind: TrajectoryKind,
    speed_hz: f32,
    radius_meters: f32,
    intensity: f32,
    clockwise: bool,
}

#[derive(Clone, Debug)]
pub struct SpatialEngine {
    config: EngineConfig,
    renderer: CpuRenderer,
    late_field: LateDiffuseField,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    listener: ListenerPose,
    scene_motion: Option<SceneMotionSignature>,
    scene_trajectory: Option<Trajectory>,
    debug_enabled: bool,
    debug_snapshot: SpatialDebugSnapshot,
    debug_activity: [SourceActivity; MAX_DEBUG_SOURCES],
}

impl SpatialEngine {
    pub fn new(config: EngineConfig) -> Result<Self, SpatialError> {
        if config.sample_rate == 0 {
            return Err(SpatialError::InvalidSampleRate);
        }
        if config.block_frames == 0 {
            return Err(SpatialError::InvalidBlockFrames);
        }
        if config.max_sources == 0 {
            return Err(SpatialError::InvalidSourceCapacity);
        }
        let mut renderer = CpuRenderer::new(
            config.sample_rate,
            config.block_frames,
            config.max_sources,
        )?;
        renderer.set_environment(config.environment);
        let late_field = LateDiffuseField::new(config.sample_rate, config.environment);
        Ok(Self {
            renderer,
            late_field,
            mix_left: vec![0.0; config.block_frames],
            mix_right: vec![0.0; config.block_frames],
            listener: ListenerPose::identity(),
            scene_motion: None,
            scene_trajectory: None,
            debug_enabled: false,
            debug_snapshot: SpatialDebugSnapshot::new(config.sample_rate),
            debug_activity: [SourceActivity::default(); MAX_DEBUG_SOURCES],
            config,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn simd_backend(&self) -> yinqidao_audio_simd::SimdBackend {
        yinqidao_audio_simd::best_backend()
    }

    pub fn set_listener(&mut self, listener: ListenerPose) {
        self.listener = listener;
    }

    /// Capture one coherent listener segment for the next DSP block. The previous block endpoint is
    /// the new block start; a freshly published head pose becomes the end. Renderer parameter ramps
    /// then smooth ITD/ILD/pinna/reflection changes sample-by-sample without solving head geometry
    /// in the inner sample loop.
    #[inline]
    fn runtime_listener_segment(&mut self) -> (ListenerPose, ListenerPose) {
        let start = self.listener;
        let end = latest_runtime_listener_pose().unwrap_or(start);
        self.listener = end;
        (start, end)
    }

    pub fn set_environment(&mut self, settings: EnvironmentSettings) {
        self.config.environment = settings;
        self.renderer.set_environment(settings);
        self.late_field.set_environment(settings);
    }

    /// Configure rigid listener-centric scene motion for authored speaker beds.
    ///
    /// Every full-range authored channel keeps its own PCM slot and relative speaker geometry. The
    /// whole bed is rotated on a spherical shell from the audio-owned trajectory; LFE remains
    /// direction-independent. Passing `None` disables scene motion without disabling native
    /// binaural rendering.
    #[allow(clippy::too_many_arguments)]
    pub fn set_scene_motion(
        &mut self,
        kind: Option<TrajectoryKind>,
        speed_hz: f32,
        radius_meters: f32,
        intensity: f32,
        clockwise: bool,
    ) {
        let intensity = finite_or(intensity, 0.0).clamp(0.0, 1.0);
        let signature = kind.and_then(|kind| {
            if intensity <= 0.001 {
                return None;
            }
            Some(SceneMotionSignature {
                kind,
                speed_hz: finite_or(speed_hz, 0.10).abs().clamp(0.005, 2.0),
                radius_meters: finite_or(radius_meters, 1.0).clamp(0.05, 8.0),
                intensity,
                clockwise,
            })
        });
        if self.scene_motion == signature {
            return;
        }

        let trajectory_changed = match (self.scene_motion, signature) {
            (Some(previous), Some(next)) => {
                previous.kind != next.kind
                    || previous.speed_hz != next.speed_hz
                    || previous.radius_meters != next.radius_meters
                    || previous.clockwise != next.clockwise
            }
            (None, None) => false,
            _ => true,
        };
        self.scene_motion = signature;
        if !trajectory_changed {
            // Intensity is a transform amount, not a timeline parameter. Preserve phase/sample-clock
            // continuity when the user adjusts only this control.
            return;
        }

        self.scene_trajectory = signature.map(|signature| {
            let mut trajectory = Trajectory::new(
                signature.kind,
                self.config.sample_rate,
                signature.speed_hz,
                signature.radius_meters,
                0.0,
            );
            trajectory.set_clockwise(signature.clockwise);
            trajectory
        });

        // Changing the trajectory invalidates source-position-dependent filters and reflection
        // histories. Configuration changes are infrequent; render calls with an unchanged signature
        // remain allocation-free and do not reset the hot path.
        self.renderer.reset();
        self.late_field.reset();
        if self.debug_enabled {
            self.debug_snapshot
                .reset_timeline(self.listener, self.config.environment);
        }
    }

    pub fn scene_motion_sample_clock(&self) -> Option<u64> {
        self.scene_trajectory.as_ref().map(Trajectory::sample_clock)
    }

    /// Enable the fixed-size spatial scene snapshot. Disabled is the default production path.
    /// Enabling this never allocates in render calls; the snapshot is stored inside the engine.
    pub fn set_debug_enabled(&mut self, enabled: bool) {
        if self.debug_enabled == enabled {
            return;
        }
        self.debug_enabled = enabled;
        self.debug_activity.fill(SourceActivity::default());
        if enabled {
            self.debug_snapshot
                .reset_timeline(self.listener, self.config.environment);
        }
    }

    pub fn debug_enabled(&self) -> bool {
        self.debug_enabled
    }

    /// Return a by-value fixed-size snapshot. Source activity is already bound to each source slot,
    /// so geometry and Peak/RMS are published atomically by downstream scene publishers.
    pub fn debug_snapshot(&self) -> Option<SpatialDebugSnapshot> {
        self.debug_enabled.then_some(self.debug_snapshot)
    }

    /// Transitional diagnostic accessor retained for benchmark/tests while UI consumes activity
    /// directly from `SpatialDebugSource`.
    pub fn debug_source_activity(&self) -> Option<[SourceActivity; MAX_DEBUG_SOURCES]> {
        self.debug_enabled.then_some(self.debug_activity)
    }

    pub fn reset(&mut self) {
        self.renderer.reset();
        self.late_field.reset();
        if let Some(trajectory) = self.scene_trajectory.as_mut() {
            trajectory.reset();
        }
        self.debug_activity.fill(SourceActivity::default());
        if self.debug_enabled {
            self.debug_snapshot
                .reset_timeline(self.listener, self.config.environment);
        }
    }

    /// Render an interleaved native speaker layout directly to interleaved stereo.
    ///
    /// With scene motion enabled, the authored bed is transformed as one rigid spherical scene.
    /// Each source still reads the caller's interleaved PCM with a stride, so there is no
    /// intermediate per-channel copy and no downmix before binaural rendering.
    pub fn render_interleaved_layout(
        &mut self,
        input: &[f32],
        layout: ChannelLayout,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
        let debug_layout = layout;
        let layout = SpeakerLayout::for_layout(layout);
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
        let output_samples = frames.saturating_mul(2);
        if output.len() < output_samples {
            return Err(SpatialError::OutputTooSmall);
        }

        if self.debug_enabled {
            analyze_interleaved_activity(input, channels, &mut self.debug_activity);
        }

        let normalization = layout.normalization();
        let block_limit = self.scene_block_frames();
        let scene_intensity = self.scene_motion.map_or(0.0, |motion| motion.intensity);
        let mut latest_poses = [SourcePose::default(); MAX_DEBUG_SOURCES];
        let mut early_reflection_sources = [false; MAX_DEBUG_SOURCES];
        for (source_index, speaker) in layout.speakers().iter().copied().enumerate() {
            latest_poses[source_index] = static_speaker_pose(speaker);
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

            for (source_index, speaker) in layout.speakers().iter().copied().enumerate() {
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
            for (source_index, speaker) in layout.speakers().iter().copied().enumerate() {
                self.debug_snapshot
                    .record_source(source_index, speaker.kind, latest_poses[source_index]);
                self.debug_snapshot
                    .set_source_activity(source_index, self.debug_activity[source_index]);
            }
            self.debug_snapshot.finish_capture(frames);
        }
        Ok(frames)
    }

    fn scene_block_frames(&self) -> usize {
        let block_frames = self.config.block_frames.max(1);
        let Some(motion) = self.scene_motion else {
            return block_frames;
        };
        let frames_for_limit = (self.config.sample_rate.max(1) as f32
            * MAX_SCENE_SEGMENT_DEGREES
            / (motion.speed_hz * 360.0))
            .floor()
            .max(1.0) as usize;
        block_frames.min(frames_for_limit.max(1))
    }

    /// Render the authored left/right channels as two independent virtual full-range sources.
    /// Start/end poses use end-exclusive sample-clock semantics: `end` is the state at n + frames.
    #[allow(clippy::too_many_arguments)]
    pub fn render_interleaved_stereo_pair(
        &mut self,
        input: &[f32],
        left_start: SourcePose,
        left_end: SourcePose,
        right_start: SourcePose,
        right_end: SourcePose,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
        if self.renderer.source_capacity() < 2 {
            return Err(SpatialError::SourceCapacityExceeded);
        }
        if input.len() % 2 != 0 {
            return Err(SpatialError::ChannelCountMismatch);
        }
        let frames = input.len() / 2;
        if output.len() < frames.saturating_mul(2) {
            return Err(SpatialError::OutputTooSmall);
        }
        if frames == 0 {
            return Ok(0);
        }

        let denominator = frames as f32;
        let mut frame_offset = 0usize;
        while frame_offset < frames {
            let block_frames = (frames - frame_offset).min(self.config.block_frames);
            let (listener_start, listener_end) = self.runtime_listener_segment();
            let block_end_exclusive = frame_offset + block_frames;
            let start_t = frame_offset as f32 / denominator;
            let end_t = block_end_exclusive as f32 / denominator;
            let left_block_start = left_start.lerp(left_end, start_t);
            let left_block_end = left_start.lerp(left_end, end_t);
            let right_block_start = right_start.lerp(right_end, start_t);
            let right_block_end = right_start.lerp(right_end, end_t);

            self.mix_left[..block_frames].fill(0.0);
            self.mix_right[..block_frames].fill(0.0);
            let block_start = frame_offset * 2;
            let block_end = block_start + block_frames * 2;
            let block = &input[block_start..block_end];

            self.renderer.render_strided_source(
                0,
                block,
                2,
                0,
                block_frames,
                left_block_start,
                left_block_end,
                listener_start,
                listener_end,
                crate::SourceKind::FullRange,
                true,
                &mut self.mix_left,
                &mut self.mix_right,
            )?;
            self.renderer.render_strided_source(
                1,
                block,
                2,
                1,
                block_frames,
                right_block_start,
                right_block_end,
                listener_start,
                listener_end,
                crate::SourceKind::FullRange,
                true,
                &mut self.mix_left,
                &mut self.mix_right,
            )?;
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
            analyze_interleaved_activity(input, 2, &mut self.debug_activity);
            self.debug_snapshot
                .begin_capture(self.listener, self.config.environment);
            self.debug_snapshot.set_layout(Some(ChannelLayout::Stereo));
            self.debug_snapshot
                .record_source(0, crate::SourceKind::FullRange, left_end);
            self.debug_snapshot
                .set_source_activity(0, self.debug_activity[0]);
            self.debug_snapshot
                .record_source(1, crate::SourceKind::FullRange, right_end);
            self.debug_snapshot
                .set_source_activity(1, self.debug_activity[1]);
            self.debug_snapshot.finish_capture(frames);
        }
        Ok(frames)
    }

    /// Render one mono source along an audio-clock-driven trajectory for 360/8D-style effects.
    pub fn render_mono_trajectory(
        &mut self,
        input: &[f32],
        trajectory: &mut Trajectory,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
        let frames = input.len();
        if output.len() < frames.saturating_mul(2) {
            return Err(SpatialError::OutputTooSmall);
        }
        let mut latest_pose = None;
        let mut frame_offset = 0usize;
        while frame_offset < frames {
            let block_frames = (frames - frame_offset).min(self.config.block_frames);
            let (listener_start, listener_end) = self.runtime_listener_segment();
            self.mix_left[..block_frames].fill(0.0);
            self.mix_right[..block_frames].fill(0.0);
            let block = &input[frame_offset..frame_offset + block_frames];
            let (start_pose, end_pose) = trajectory.next_segment(block_frames);
            latest_pose = Some(end_pose);
            self.renderer.render_strided_source(
                0,
                block,
                1,
                0,
                block_frames,
                start_pose,
                end_pose,
                listener_start,
                listener_end,
                crate::SourceKind::FullRange,
                true,
                &mut self.mix_left,
                &mut self.mix_right,
            )?;
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
        if self.debug_enabled && let Some(pose) = latest_pose {
            analyze_interleaved_activity(input, 1, &mut self.debug_activity);
            self.debug_snapshot
                .begin_capture(self.listener, self.config.environment);
            self.debug_snapshot
                .record_source(0, crate::SourceKind::FullRange, pose);
            self.debug_snapshot
                .set_source_activity(0, self.debug_activity[0]);
            self.debug_snapshot.finish_capture(frames);
        }
        Ok(frames)
    }
}

/// First-order image-source reflections are a room-acoustics layer, not the spherical source model.
/// In dense height beds, keep direct HRTF/pinna processing for every top channel but let the
/// horizontal bed own localized early wall reflections. The summed field still enters the shared
/// late diffuse processor, so height programme remains present in the room tail without paying six
/// moving image-source taps per top speaker.
#[inline]
fn authored_role_uses_early_reflections(role: Option<ChannelRole>) -> bool {
    matches!(
        role,
        Some(
            ChannelRole::FrontLeft
                | ChannelRole::FrontRight
                | ChannelRole::Center
                | ChannelRole::SurroundLeft
                | ChannelRole::SurroundRight
                | ChannelRole::RearLeft
                | ChannelRole::RearRight
        )
    )
}

#[inline]
fn static_speaker_pose(speaker: Speaker) -> SourcePose {
    SourcePose {
        position: speaker.direction,
        velocity: Vec3::ZERO,
        gain: speaker.gain,
        spread: 0.0,
    }
}

#[inline]
fn scene_speaker_pose(speaker: Speaker, scene: SourcePose, intensity: f32) -> SourcePose {
    if speaker.kind == crate::SourceKind::Lfe {
        return static_speaker_pose(speaker);
    }

    let scene_direction = scene.position.normalized_or(Vec3::FORWARD);
    let horizontal =
        (scene_direction.x * scene_direction.x + scene_direction.z * scene_direction.z).sqrt();
    let yaw = scene_direction.x.atan2(scene_direction.z) * intensity;
    let pitch = scene_direction.y.atan2(horizontal) * intensity;
    let (pitch_sin, pitch_cos) = pitch.sin_cos();
    let (yaw_sin, yaw_cos) = yaw.sin_cos();

    // Pitch first, then yaw, so the transformed reference forward vector lands exactly on the
    // requested azimuth/elevation while every authored speaker undergoes the same rigid rotation.
    let pitched = rotate_x(speaker.direction, pitch_sin, pitch_cos);
    let direction = rotate_y(pitched, yaw_sin, yaw_cos).normalized_or(speaker.direction);

    let scene_radius = finite_or(scene.position.length(), 1.0).clamp(0.05, 8.0);
    let radius = 1.0 + (scene_radius - 1.0) * intensity;
    SourcePose {
        position: direction * radius,
        velocity: Vec3::ZERO,
        gain: speaker.gain,
        spread: 0.0,
    }
}

#[inline]
fn bind_segment_velocity(
    start: &mut SourcePose,
    end: &mut SourcePose,
    frames: usize,
    sample_rate: u32,
    speaker: Speaker,
) {
    if speaker.kind == crate::SourceKind::Lfe || frames == 0 {
        start.velocity = Vec3::ZERO;
        end.velocity = Vec3::ZERO;
        return;
    }
    let seconds = frames as f32 / sample_rate.max(1) as f32;
    if seconds <= 0.0 {
        return;
    }
    let velocity = (end.position - start.position) * (1.0 / seconds);
    start.velocity = velocity;
    end.velocity = velocity;
}

#[inline]
fn rotate_y(position: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        position.x * cos + position.z * sin,
        position.y,
        -position.x * sin + position.z * cos,
    )
}

#[inline]
fn rotate_x(position: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(
        position.x,
        position.y * cos + position.z * sin,
        -position.y * sin + position.z * cos,
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
    fn seven_one_four_renders_without_intermediate_channel_copy() {
        let mut config = EngineConfig::new(48_000);
        config.environment.mix = 0.0;
        let mut engine = SpatialEngine::new(config).unwrap();
        let frames = 64;
        let input = vec![0.0; frames * 12];
        let mut output = vec![0.0; frames * 2];
        assert_eq!(
            engine
                .render_interleaved_layout(&input, ChannelLayout::Surround7_1_4, &mut output)
                .unwrap(),
            frames
        );
        assert!(output.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn height_roles_keep_direct_spatialization_but_skip_localized_early_reflections() {
        for role in [
            ChannelRole::TopFrontLeft,
            ChannelRole::TopFrontRight,
            ChannelRole::TopRearLeft,
            ChannelRole::TopRearRight,
            ChannelRole::Lfe,
        ] {
            assert!(!authored_role_uses_early_reflections(Some(role)));
        }
        for role in [
            ChannelRole::FrontLeft,
            ChannelRole::FrontRight,
            ChannelRole::Center,
            ChannelRole::SurroundLeft,
            ChannelRole::SurroundRight,
            ChannelRole::RearLeft,
            ChannelRole::RearRight,
        ] {
            assert!(authored_role_uses_early_reflections(Some(role)));
        }
        assert!(!authored_role_uses_early_reflections(None));
    }

    #[test]
    fn seven_one_four_reduces_first_order_reflection_sources_from_eleven_to_seven() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let reflection_sources = layout
            .roles()
            .iter()
            .copied()
            .filter(|role| authored_role_uses_early_reflections(Some(*role)))
            .count();
        assert_eq!(reflection_sources, 7);
        assert_eq!(layout.channels(), 12);
    }

    #[test]
    fn authored_bed_scene_motion_advances_audio_clock_and_keeps_lfe_unlocalized() {
        let mut config = EngineConfig::new(48_000);
        config.environment.mix = 0.0;
        let mut engine = SpatialEngine::new(config).unwrap();
        engine.set_debug_enabled(true);
        engine.set_scene_motion(
            Some(TrajectoryKind::FigureEight),
            0.5,
            1.2,
            1.0,
            true,
        );

        let frames = 4_800;
        let input = vec![0.05_f32; frames * 12];
        let mut output = vec![0.0_f32; frames * 2];
        engine
            .render_interleaved_layout(&input, ChannelLayout::Surround7_1_4, &mut output)
            .unwrap();

        assert_eq!(engine.scene_motion_sample_clock(), Some(frames as u64));
        let snapshot = engine.debug_snapshot().expect("debug snapshot");
        assert_eq!(snapshot.layout, Some(ChannelLayout::Surround7_1_4));
        assert_eq!(snapshot.source_count, 12);
        assert!(
            (snapshot.sources[0].position - SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4)
                .speakers()[0]
                .direction)
                .length()
                > 0.01
        );
        assert_eq!(snapshot.sources[3].kind, crate::SpatialDebugSourceKind::Lfe);
        assert_eq!(snapshot.sources[3].position, Vec3::FORWARD);
    }

    #[test]
    fn scene_rotation_preserves_authored_angular_separation() {
        let layout = SpeakerLayout::for_layout(ChannelLayout::Surround7_1_4);
        let scene = SourcePose::new(Vec3::new(0.7, 0.35, -0.9));
        let left = scene_speaker_pose(layout.speakers()[0], scene, 1.0);
        let right = scene_speaker_pose(layout.speakers()[1], scene, 1.0);
        let original_dot = layout.speakers()[0]
            .direction
            .dot(layout.speakers()[1].direction);
        let transformed_dot = left
            .position
            .normalized_or(Vec3::FORWARD)
            .dot(right.position.normalized_or(Vec3::FORWARD));
        assert!((original_dot - transformed_dot).abs() < 1.0e-5);
    }

    #[test]
    fn reset_rewinds_scene_motion_clock_without_disabling_motion() {
        let mut engine = SpatialEngine::new(EngineConfig::new(48_000)).unwrap();
        engine.set_scene_motion(
            Some(TrajectoryKind::Orbit360),
            0.5,
            1.0,
            0.8,
            false,
        );
        let input = vec![0.0_f32; 64 * 8];
        let mut output = vec![0.0_f32; 64 * 2];
        engine
            .render_interleaved_layout(&input, ChannelLayout::Surround7_1, &mut output)
            .unwrap();
        assert_eq!(engine.scene_motion_sample_clock(), Some(64));
        engine.reset();
        assert_eq!(engine.scene_motion_sample_clock(), Some(0));
    }

    #[test]
    fn stereo_pair_preserves_independent_authored_channels() {
        let mut config = EngineConfig::new(48_000);
        config.environment.mix = 0.0;
        let mut engine = SpatialEngine::new(config).unwrap();
        let frames = 64;
        let mut input = vec![0.0_f32; frames * 2];
        for frame in input.as_chunks_mut::<2>().0 {
            frame[0] = 0.25;
            frame[1] = -0.10;
        }
        let mut output = vec![0.0_f32; frames * 2];
        let left = SourcePose {
            position: Vec3::new(-0.5, 0.0, 0.866_025_4),
            gain: std::f32::consts::FRAC_1_SQRT_2,
            ..SourcePose::default()
        };
        let right = SourcePose {
            position: Vec3::new(0.5, 0.0, 0.866_025_4),
            gain: std::f32::consts::FRAC_1_SQRT_2,
            ..SourcePose::default()
        };
        assert_eq!(
            engine
                .render_interleaved_stereo_pair(&input, left, left, right, right, &mut output)
                .unwrap(),
            frames
        );
        assert!(output.iter().all(|sample| sample.is_finite()));
        assert!(output.iter().any(|sample| sample.abs() > 1.0e-4));
    }

    #[test]
    fn stereo_pair_block_pose_span_is_end_exclusive() {
        let start = SourcePose::new(Vec3::FORWARD);
        let end = SourcePose::new(Vec3::RIGHT);
        let frames = 64usize;
        let denominator = frames as f32;
        let first_t = 0.0_f32 / denominator;
        let next_block_t = frames as f32 / denominator;
        assert_eq!(start.lerp(end, first_t).position, start.position);
        assert_eq!(start.lerp(end, next_block_t).position, end.position);
    }

    #[test]
    fn debug_snapshot_binds_authored_activity_to_sources() {
        let mut config = EngineConfig::new(48_000);
        config.environment.mix = 0.12;
        let mut engine = SpatialEngine::new(config).unwrap();
        assert!(engine.debug_snapshot().is_none());
        engine.set_debug_enabled(true);

        let frames = 64;
        let input = vec![0.1_f32; frames * 12];
        let mut output = vec![0.0_f32; frames * 2];
        engine
            .render_interleaved_layout(&input, ChannelLayout::Surround7_1_4, &mut output)
            .unwrap();

        let snapshot = engine.debug_snapshot().expect("enabled snapshot");
        assert_eq!(snapshot.layout, Some(ChannelLayout::Surround7_1_4));
        assert_eq!(snapshot.source_count, 12);
        assert_eq!(snapshot.rendered_frames, frames as u64);
        assert_eq!(snapshot.sequence, 1);
        assert!((snapshot.environment_contribution - 0.12).abs() < 1.0e-6);
        assert!(snapshot.sources[..snapshot.source_count].iter().all(|source| {
            source.active
                && (source.input_peak - 0.1).abs() < 1.0e-6
                && (source.input_rms - 0.1).abs() < 1.0e-6
        }));
    }
}
