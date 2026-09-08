use crate::{
    ChannelLayout, EarlyReflectionNetwork, EnvironmentSettings, ListenerPose, SourcePose,
    SpatialError, SpeakerLayout, Trajectory, renderer::CpuRenderer,
};

pub const DEFAULT_BLOCK_FRAMES: usize = 64;
pub const DEFAULT_MAX_SOURCES: usize = 32;

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

#[derive(Clone, Debug)]
pub struct SpatialEngine {
    config: EngineConfig,
    renderer: CpuRenderer,
    environment: EarlyReflectionNetwork,
    mix_left: Vec<f32>,
    mix_right: Vec<f32>,
    listener: ListenerPose,
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
        Ok(Self {
            renderer: CpuRenderer::new(
                config.sample_rate,
                config.block_frames,
                config.max_sources,
            )?,
            environment: EarlyReflectionNetwork::new(config.sample_rate, config.environment),
            mix_left: vec![0.0; config.block_frames],
            mix_right: vec![0.0; config.block_frames],
            listener: ListenerPose::identity(),
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

    pub fn set_environment(&mut self, settings: EnvironmentSettings) {
        self.config.environment = settings;
        self.environment.set_settings(settings);
    }

    pub fn reset(&mut self) {
        self.renderer.reset();
        self.environment.reset();
    }

    /// Render an interleaved native speaker layout directly to interleaved stereo.
    /// Each source reads the caller's interleaved PCM with a stride; no per-channel PCM copy exists.
    pub fn render_interleaved_layout(
        &mut self,
        input: &[f32],
        layout: ChannelLayout,
        output: &mut [f32],
    ) -> Result<usize, SpatialError> {
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

        let mut frame_offset = 0usize;
        while frame_offset < frames {
            let block_frames = (frames - frame_offset).min(self.config.block_frames);
            self.mix_left[..block_frames].fill(0.0);
            self.mix_right[..block_frames].fill(0.0);
            let block_start = frame_offset * channels;
            let block_end = block_start + block_frames * channels;
            let block = &input[block_start..block_end];
            for (source_index, speaker) in layout.speakers().iter().copied().enumerate() {
                let pose = SourcePose {
                    position: speaker.direction,
                    velocity: crate::Vec3::ZERO,
                    gain: speaker.gain,
                    spread: 0.0,
                };
                self.renderer.render_strided_source(
                    source_index,
                    block,
                    channels,
                    source_index,
                    block_frames,
                    pose,
                    pose,
                    self.listener,
                    speaker.kind,
                    &mut self.mix_left,
                    &mut self.mix_right,
                )?;
            }
            self.environment.process_planar(
                &mut self.mix_left[..block_frames],
                &mut self.mix_right[..block_frames],
            );
            let normalization = layout.normalization();
            for frame in 0..block_frames {
                let output_index = (frame_offset + frame) * 2;
                output[output_index] = self.mix_left[frame] * normalization;
                output[output_index + 1] = self.mix_right[frame] * normalization;
            }
            frame_offset += block_frames;
        }
        Ok(frames)
    }

    /// Render the authored left/right channels as two independent virtual full-range sources.
    ///
    /// This is the preferred stereo virtualization primitive: it preserves the source programme's
    /// left/right information instead of collapsing it to mono before applying ITD/ILD/head-shadow.
    /// Start/end poses use end-exclusive sample-clock semantics: `end` is the state at n + frames.
    /// Internally the renderer still works in the configured fixed block size without allocations.
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
                self.listener,
                crate::SourceKind::FullRange,
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
                self.listener,
                crate::SourceKind::FullRange,
                &mut self.mix_left,
                &mut self.mix_right,
            )?;
            self.environment.process_planar(
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
        let mut frame_offset = 0usize;
        while frame_offset < frames {
            let block_frames = (frames - frame_offset).min(self.config.block_frames);
            self.mix_left[..block_frames].fill(0.0);
            self.mix_right[..block_frames].fill(0.0);
            let block = &input[frame_offset..frame_offset + block_frames];
            let (start_pose, end_pose) = trajectory.next_segment(block_frames);
            self.renderer.render_strided_source(
                0,
                block,
                1,
                0,
                block_frames,
                start_pose,
                end_pose,
                self.listener,
                crate::SourceKind::FullRange,
                &mut self.mix_left,
                &mut self.mix_right,
            )?;
            self.environment.process_planar(
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
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vec3;

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
}
