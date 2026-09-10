mod eq;
mod limiter;
mod spatial;
mod trajectory_spatial;

pub use eq::{EqPreset, clamp_eq};
pub use spatial::{SpatialPreset, clamp_spatial};

use std::cell::Cell;

use crate::model::{EqSettings, SourceLayoutOverride, SpatialSettings, VirtualBedMode};
use yinqidao_audio_spatial::{
    ChannelLayout, EngineConfig as NativeSpatialConfig, EnvironmentSettings, SpeakerLayout,
    SpatialDebugSnapshot, SpatialEngine,
};

use super::debug::{
    AudioDebugMonitorMode, audio_debug_enabled, audio_debug_monitor_mode, capture_audio_debug_frame,
};
use super::spatial_debug::{clear_spatial_debug_snapshot, publish_spatial_debug_snapshot};
use eq::EqProcessor;
use limiter::StereoPeakLimiter;
use spatial::Spatializer;
use trajectory_spatial::{
    StereoSpatializer, apply_scene_motion_settings, spatial_environment_settings,
};

std::thread_local! {
    static TRANSPORT_RESET_GENERATION: Cell<u64> = const { Cell::new(1) };
}

pub(crate) fn request_transport_reset() {
    TRANSPORT_RESET_GENERATION.with(|generation| {
        generation.set(generation.get().wrapping_add(1));
    });
}

#[inline]
fn transport_reset_generation() -> u64 {
    TRANSPORT_RESET_GENERATION.with(Cell::get)
}

#[derive(Clone, Debug)]
struct StreamingLinearResampler {
    input_rate: u32,
    output_rate: u32,
    input_frames: u64,
    next_source_position: f64,
    previous_frame: Option<[f32; 2]>,
}

impl StreamingLinearResampler {
    fn new(output_rate: u32) -> Self {
        Self {
            input_rate: 0,
            output_rate: output_rate.max(1),
            input_frames: 0,
            next_source_position: 0.0,
            previous_frame: None,
        }
    }

    fn reset(&mut self) {
        self.input_rate = 0;
        self.input_frames = 0;
        self.next_source_position = 0.0;
        self.previous_frame = None;
    }

    fn configure(&mut self, input_rate: u32, output_rate: u32) {
        let input_rate = input_rate.max(1);
        let output_rate = output_rate.max(1);
        if self.input_rate != input_rate || self.output_rate != output_rate {
            self.input_rate = input_rate;
            self.output_rate = output_rate;
            self.input_frames = 0;
            self.next_source_position = 0.0;
            self.previous_frame = None;
        }
    }

    fn process_into(
        &mut self,
        input: &[f32],
        input_rate: u32,
        output_rate: u32,
        output: &mut Vec<f32>,
    ) {
        output.clear();
        self.process_append(input, input_rate, output_rate, output);
    }

    /// Append one contiguous stereo segment to an existing output buffer while preserving the
    /// resampler timeline. Native multichannel rendering feeds fixed-size spatial blocks through
    /// this path so decoder chunk size no longer dictates a second full-chunk stereo workspace.
    fn process_append(
        &mut self,
        input: &[f32],
        input_rate: u32,
        output_rate: u32,
        output: &mut Vec<f32>,
    ) {
        let input_rate = input_rate.max(1);
        let output_rate = output_rate.max(1);
        if input_rate == output_rate {
            self.reset();
            self.output_rate = output_rate;
            output.extend_from_slice(input);
            return;
        }

        self.configure(input_rate, output_rate);

        let frames = input.len() / 2;
        if frames == 0 {
            return;
        }
        let estimated_frames = ((frames as u64)
            .saturating_mul(u64::from(output_rate))
            .saturating_add(u64::from(input_rate) - 1)
            / u64::from(input_rate))
        .saturating_add(2) as usize;
        output.reserve(estimated_frames.saturating_mul(2));

        let base_frame = self.input_frames;
        let last_frame = base_frame.saturating_add(frames as u64 - 1);
        let source_step = f64::from(input_rate) / f64::from(output_rate);
        const EPSILON: f64 = 1.0e-9;

        while self.next_source_position <= last_frame as f64 + EPSILON {
            let source_floor = self.next_source_position.floor();
            let source_index = source_floor.max(0.0) as u64;
            let fraction = (self.next_source_position - source_floor).clamp(0.0, 1.0);

            let Some(first) = self.frame_at(input, base_frame, source_index) else {
                break;
            };
            let second = if fraction <= EPSILON {
                first
            } else {
                let Some(next) = self.frame_at(input, base_frame, source_index.saturating_add(1))
                else {
                    break;
                };
                next
            };

            output.push(first[0] + (second[0] - first[0]) * fraction as f32);
            output.push(first[1] + (second[1] - first[1]) * fraction as f32);
            self.next_source_position += source_step;
        }

        let last_index = (frames - 1) * 2;
        self.previous_frame = Some([input[last_index], input[last_index + 1]]);
        self.input_frames = self.input_frames.saturating_add(frames as u64);
    }

    fn frame_at(&self, input: &[f32], base_frame: u64, absolute_index: u64) -> Option<[f32; 2]> {
        if absolute_index < base_frame {
            return (absolute_index.saturating_add(1) == base_frame)
                .then_some(self.previous_frame)
                .flatten();
        }
        let relative = absolute_index.saturating_sub(base_frame) as usize;
        let sample_index = relative.checked_mul(2)?;
        Some([*input.get(sample_index)?, *input.get(sample_index + 1)?])
    }
}

#[derive(Clone, Debug)]
pub struct AudioProcessor {
    pub(crate) eq: EqProcessor,
    pub(crate) spatial: Spatializer,
    volume: f32,
    limiter: StereoPeakLimiter,
    stereo_scratch: Vec<f32>,
    stereo_spatial: Option<StereoSpatializer>,
    native_spatial_scratch: Vec<f32>,
    native_spatial: Option<SpatialEngine>,
    native_spatial_rate: u32,
    native_spatial_environment: Option<EnvironmentSettings>,
    source_debug_scratch: Vec<f32>,
    eq_debug_scratch: Vec<f32>,
    resampler: StreamingLinearResampler,
    transport_reset_generation: u64,
    spatial_debug_published_frames: Option<u64>,
}

impl AudioProcessor {
    pub fn new(sample_rate: u32, eq: EqSettings, spatial: SpatialSettings, volume: f32) -> Self {
        let sample_rate = sample_rate.max(1);
        let native_block_samples = NativeSpatialConfig::new(sample_rate)
            .block_frames
            .saturating_mul(2);
        Self {
            eq: EqProcessor::new(sample_rate, eq),
            spatial: Spatializer::new(sample_rate, spatial),
            volume: volume.clamp(0.0, 1.0),
            limiter: StereoPeakLimiter::new(sample_rate),
            stereo_scratch: Vec::new(),
            stereo_spatial: StereoSpatializer::new(sample_rate),
            native_spatial_scratch: vec![0.0; native_block_samples],
            native_spatial: None,
            native_spatial_rate: 0,
            native_spatial_environment: None,
            source_debug_scratch: Vec::new(),
            eq_debug_scratch: Vec::new(),
            resampler: StreamingLinearResampler::new(sample_rate),
            transport_reset_generation: transport_reset_generation(),
            spatial_debug_published_frames: None,
        }
    }

    pub(crate) fn reset_transport(&mut self) {
        self.eq.reset_state();
        self.spatial.reset_transport();
        self.limiter.reset();
        if let Some(engine) = self.stereo_spatial.as_mut() {
            engine.reset();
        }
        if let Some(engine) = self.native_spatial.as_mut() {
            engine.reset();
        }
        self.resampler.reset();
        self.spatial_debug_published_frames = None;
    }

    #[inline]
    fn consume_transport_reset(&mut self) {
        let generation = transport_reset_generation();
        let parameters_changed = self.eq.take_transport_reset_request();
        if parameters_changed || generation != self.transport_reset_generation {
            self.reset_transport();
            self.transport_reset_generation = generation;
        }
    }

    #[cfg(test)]
    pub fn process(&mut self, input: &[f32], input_rate: u32, input_channels: u16) -> Vec<f32> {
        self.process_with_layout(input, input_rate, input_channels, None)
    }

    #[cfg(test)]
    pub fn process_with_layout(
        &mut self,
        input: &[f32],
        input_rate: u32,
        input_channels: u16,
        spatial_layout_hint: Option<ChannelLayout>,
    ) -> Vec<f32> {
        let mut output = Vec::new();
        self.process_into_with_layout(
            input,
            input_rate,
            input_channels,
            spatial_layout_hint,
            &mut output,
        );
        output
    }

    pub fn process_into(
        &mut self,
        input: &[f32],
        input_rate: u32,
        input_channels: u16,
        output: &mut Vec<f32>,
    ) {
        self.process_into_with_layout(input, input_rate, input_channels, None, output);
    }

    pub fn process_into_with_layout(
        &mut self,
        input: &[f32],
        input_rate: u32,
        input_channels: u16,
        spatial_layout_hint: Option<ChannelLayout>,
        output: &mut Vec<f32>,
    ) {
        self.consume_transport_reset();

        let output_rate = self.eq.sample_rate();
        let authored_multichannel = input_channels > 2;
        let debug_enabled = audio_debug_enabled();
        let spatial_settings = self.spatial.settings().clone();
        let native_layout = resolved_native_spatial_layout(
            input_channels,
            spatial_layout_hint,
            &spatial_settings,
        );
        if let Some(renderer) = self.stereo_spatial.as_mut() {
            renderer.set_debug_enabled(debug_enabled);
        }
        if let Some(engine) = self.native_spatial.as_mut() {
            engine.set_debug_enabled(debug_enabled);
        }
        if !debug_enabled && self.spatial_debug_published_frames.is_some() {
            clear_spatial_debug_snapshot();
            self.spatial_debug_published_frames = None;
        }

        let mut native_spatial_used = false;
        if let Some(layout) = native_layout {
            if self.render_native_spatial_into(
                input,
                input_rate,
                output_rate,
                layout,
                debug_enabled,
                &spatial_settings,
                output,
            ) {
                native_spatial_used = true;
            } else {
                // A verified/explicit bed that could not initialize the native renderer must not be
                // re-labelled as another height layout. Fall back to a layout-neutral fold.
                to_stereo_into(input, input_channels, &mut self.stereo_scratch);
                self.resampler
                    .process_into(&self.stereo_scratch, input_rate, output_rate, output);
            }
        } else {
            // Missing/discrete/custom metadata is intentionally not guessed from 6/8/10/12 alone.
            // A neutral Mid/Side fold preserves common programme plus inter-channel difference;
            // VirtualBedMode is a separate explicit synthesis choice and never declares the input
            // channel semantics by itself.
            to_stereo_into(input, input_channels, &mut self.stereo_scratch);
            self.resampler
                .process_into(&self.stereo_scratch, input_rate, output_rate, output);
        }

        if debug_enabled {
            copy_reuse(output, &mut self.source_debug_scratch);
        }

        self.eq.process(output);
        if debug_enabled {
            copy_reuse(output, &mut self.eq_debug_scratch);
        }

        // Source Layout Override and Virtual Bed are intentionally independent. If metadata is
        // absent and no exact native source declaration resolved, a separately selected explicit
        // Virtual Bed may still synthesize a new bed from the conservative stereo fold.
        let allow_explicit_virtual_fallback = authored_multichannel
            && !native_spatial_used
            && spatial_layout_hint.is_none()
            && explicit_virtual_bed_requested(&spatial_settings);
        let allow_stereo_spatial = !authored_multichannel || allow_explicit_virtual_fallback;
        let mut stereo_spatial_used = false;
        if allow_stereo_spatial {
            let handled = self
                .stereo_spatial
                .as_mut()
                .is_some_and(|renderer| renderer.process_in_place(output, &spatial_settings));
            stereo_spatial_used = handled;
            if !handled {
                self.spatial.process(output);
            }
        }

        if debug_enabled {
            let spatial_snapshot = if native_spatial_used {
                self.native_spatial
                    .as_ref()
                    .and_then(SpatialEngine::debug_snapshot)
            } else if stereo_spatial_used {
                self.stereo_spatial
                    .as_ref()
                    .and_then(StereoSpatializer::debug_snapshot)
            } else {
                None
            };
            self.publish_spatial_debug_if_due(spatial_snapshot);

            capture_audio_debug_frame(
                &self.source_debug_scratch,
                &self.eq_debug_scratch,
                output,
                output_rate,
            );

            match audio_debug_monitor_mode() {
                AudioDebugMonitorMode::Source => {
                    output.clear();
                    output.extend_from_slice(&self.source_debug_scratch);
                }
                AudioDebugMonitorMode::PostEq => {
                    output.clear();
                    output.extend_from_slice(&self.eq_debug_scratch);
                }
                AudioDebugMonitorMode::PostSpatial => {}
            }
        }

        let gain = perceptual_volume_gain(self.volume);
        self.limiter.process_interleaved_stereo(output, gain);
        // The limiter ceiling sits below full scale. Keep the SIMD clamp only as a final invariant
        // guard for unexpected arithmetic faults; normal finite audio should never reach it.
        yinqidao_audio_simd::gain_clamp_in_place(output, 1.0);
    }

    fn publish_spatial_debug_if_due(&mut self, snapshot: Option<SpatialDebugSnapshot>) {
        let Some(snapshot) = snapshot else {
            if self.spatial_debug_published_frames.take().is_some() {
                clear_spatial_debug_snapshot();
            }
            return;
        };
        let interval_frames = (u64::from(snapshot.sample_rate.max(1)) / 30).max(1);
        let due = match self.spatial_debug_published_frames {
            None => true,
            Some(previous) if snapshot.rendered_frames < previous => true,
            Some(previous) => snapshot.rendered_frames.saturating_sub(previous) >= interval_frames,
        };
        if due {
            publish_spatial_debug_snapshot(snapshot);
            self.spatial_debug_published_frames = Some(snapshot.rendered_frames);
        }
    }

    fn render_native_spatial_into(
        &mut self,
        input: &[f32],
        input_rate: u32,
        output_rate: u32,
        layout: ChannelLayout,
        debug_enabled: bool,
        spatial_settings: &SpatialSettings,
        output: &mut Vec<f32>,
    ) -> bool {
        let input_rate = input_rate.max(1);
        let output_rate = output_rate.max(1);
        let environment = spatial_environment_settings(spatial_settings);
        if self.native_spatial_rate != input_rate || self.native_spatial.is_none() {
            let mut config = NativeSpatialConfig::new(input_rate);
            config.environment = environment;
            match SpatialEngine::new(config) {
                Ok(mut engine) => {
                    engine.set_debug_enabled(debug_enabled);
                    let required_samples = engine.config().block_frames.saturating_mul(2);
                    if self.native_spatial_scratch.len() < required_samples {
                        // Engine setup/reconfiguration is outside the steady-state render loop.
                        // Decoder chunk size can no longer grow this workspace.
                        self.native_spatial_scratch.resize(required_samples, 0.0);
                    }
                    self.native_spatial = Some(engine);
                    self.native_spatial_rate = input_rate;
                    self.native_spatial_environment = Some(environment);
                }
                Err(_) => {
                    self.native_spatial = None;
                    self.native_spatial_rate = input_rate;
                    self.native_spatial_environment = None;
                    return false;
                }
            }
        } else if self.native_spatial_environment != Some(environment) {
            if let Some(engine) = self.native_spatial.as_mut() {
                engine.set_environment(environment);
            }
            self.native_spatial_environment = Some(environment);
        }

        let channels = match layout {
            ChannelLayout::Surround5_1 => 6,
            ChannelLayout::Surround7_1 => 8,
            ChannelLayout::Surround5_1_2 => 8,
            ChannelLayout::Surround5_1_4 => 10,
            ChannelLayout::Surround7_1_2 => 10,
            ChannelLayout::Surround7_1_4 => 12,
            ChannelLayout::Stereo => return false,
        };
        if input.len() % channels != 0 {
            return false;
        }
        let total_frames = input.len() / channels;
        let Some(engine) = self.native_spatial.as_mut() else {
            return false;
        };
        let block_frames = engine.config().block_frames.max(1);
        let required_samples = block_frames.saturating_mul(2);
        if self.native_spatial_scratch.len() < required_samples {
            return false;
        }

        engine.set_debug_enabled(debug_enabled);
        apply_scene_motion_settings(engine, spatial_settings);
        output.clear();
        let estimated_output_frames = ((total_frames as u64)
            .saturating_mul(u64::from(output_rate))
            .saturating_add(u64::from(input_rate) - 1)
            / u64::from(input_rate))
        .saturating_add(2) as usize;
        output.reserve(estimated_output_frames.saturating_mul(2));

        let mut frame_offset = 0usize;
        while frame_offset < total_frames {
            let frames = (total_frames - frame_offset).min(block_frames);
            let input_start = frame_offset.saturating_mul(channels);
            let input_end = input_start.saturating_add(frames.saturating_mul(channels));
            let scratch_samples = frames.saturating_mul(2);
            if engine
                .render_interleaved_layout(
                    &input[input_start..input_end],
                    layout,
                    &mut self.native_spatial_scratch[..scratch_samples],
                )
                .is_err()
            {
                engine.reset();
                self.resampler.reset();
                output.clear();
                return false;
            }
            self.resampler.process_append(
                &self.native_spatial_scratch[..scratch_samples],
                input_rate,
                output_rate,
                output,
            );
            frame_offset += frames;
        }
        true
    }
}

#[inline]
fn validated_native_spatial_layout(
    channels: u16,
    hint: Option<ChannelLayout>,
) -> Option<ChannelLayout> {
    let layout = hint?;
    if !matches!(
        layout,
        ChannelLayout::Surround5_1
            | ChannelLayout::Surround7_1
            | ChannelLayout::Surround5_1_2
            | ChannelLayout::Surround5_1_4
            | ChannelLayout::Surround7_1_2
            | ChannelLayout::Surround7_1_4
    ) {
        return None;
    }
    (SpeakerLayout::for_layout(layout).channels() == usize::from(channels)).then_some(layout)
}

#[inline]
fn explicit_virtual_bed_layout(mode: VirtualBedMode) -> Option<ChannelLayout> {
    match mode {
        VirtualBedMode::Off | VirtualBedMode::Auto => None,
        VirtualBedMode::Surround5_1 => Some(ChannelLayout::Surround5_1),
        VirtualBedMode::Surround7_1 => Some(ChannelLayout::Surround7_1),
        VirtualBedMode::Surround5_1_2 => Some(ChannelLayout::Surround5_1_2),
        VirtualBedMode::Surround5_1_4 => Some(ChannelLayout::Surround5_1_4),
        VirtualBedMode::Surround7_1_2 => Some(ChannelLayout::Surround7_1_2),
        VirtualBedMode::Surround7_1_4 => Some(ChannelLayout::Surround7_1_4),
    }
}

#[inline]
fn source_layout_override_layout(mode: SourceLayoutOverride) -> Option<ChannelLayout> {
    match mode {
        SourceLayoutOverride::None => None,
        SourceLayoutOverride::Surround5_1 => Some(ChannelLayout::Surround5_1),
        SourceLayoutOverride::Surround7_1 => Some(ChannelLayout::Surround7_1),
        SourceLayoutOverride::Surround5_1_2 => Some(ChannelLayout::Surround5_1_2),
        SourceLayoutOverride::Surround5_1_4 => Some(ChannelLayout::Surround5_1_4),
        SourceLayoutOverride::Surround7_1_2 => Some(ChannelLayout::Surround7_1_2),
        SourceLayoutOverride::Surround7_1_4 => Some(ChannelLayout::Surround7_1_4),
    }
}

/// Resolve a native authored/declared bed without channel-count guessing.
///
/// Reliable codec/container metadata wins. Only when metadata is completely absent may the
/// independent Source Layout Override declare speaker semantics, and then only if its exact speaker
/// count matches decoded PCM. The declaration is intentionally independent from spatial enable/mix
/// so HiFi Direct can still preserve a metadata-less native bed without enabling synthetic room or
/// Scene Motion.
#[inline]
fn resolved_native_spatial_layout(
    channels: u16,
    hint: Option<ChannelLayout>,
    settings: &SpatialSettings,
) -> Option<ChannelLayout> {
    if let Some(layout) = validated_native_spatial_layout(channels, hint) {
        return Some(layout);
    }
    if hint.is_some() {
        return None;
    }
    let layout = source_layout_override_layout(settings.source_layout_override)?;
    (SpeakerLayout::for_layout(layout).channels() == usize::from(channels)).then_some(layout)
}

#[inline]
fn explicit_virtual_bed_requested(settings: &SpatialSettings) -> bool {
    settings.enabled && explicit_virtual_bed_layout(settings.virtual_bed).is_some()
}

#[inline]
fn copy_reuse(source: &[f32], destination: &mut Vec<f32>) {
    destination.clear();
    destination.extend_from_slice(source);
}

pub(crate) fn perceptual_volume_gain(volume: f32) -> f32 {
    let volume = volume.clamp(0.0, 1.0);
    volume * volume
}

fn to_stereo_into(input: &[f32], channels: u16, output: &mut Vec<f32>) {
    let channels = channels.max(1) as usize;
    let frames = input.len() / channels;
    output.clear();
    output.reserve(frames.saturating_mul(2));

    match channels {
        1 => {
            for sample in input.iter().take(frames) {
                output.push(*sample);
                output.push(*sample);
            }
        }
        2 => output.extend_from_slice(&input[..frames.saturating_mul(2)]),
        _ => layout_neutral_multichannel_fold_into(input, channels, output),
    }
}

/// Fold an unknown/discrete multichannel stream without assigning speaker semantics that the
/// decoder did not provide. The average is the layout-neutral common programme (Mid); a zero-sum
/// alternating projection retains some inter-channel difference as Side. A separately selected
/// Virtual Bed may then distribute both components around the full sphere without pretending that
/// 8ch is definitely 7.1 or that 10ch is definitely 5.1.4/7.1.2.
fn layout_neutral_multichannel_fold_into(input: &[f32], channels: usize, output: &mut Vec<f32>) {
    let scale = 1.0 / channels.max(1) as f32;
    for frame in input.chunks_exact(channels) {
        let mut common = 0.0_f32;
        let mut difference = 0.0_f32;
        for (index, sample) in frame.iter().copied().enumerate() {
            let sample = if sample.is_finite() { sample } else { 0.0 };
            common += sample;
            difference += if index & 1 == 0 { sample } else { -sample };
        }
        let mid = common * scale;
        let side = difference * scale;
        output.push((mid + side).clamp(-1.35, 1.35));
        output.push((mid - side).clamp(-1.35, 1.35));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_spatial_and_limiter_keep_output_bounded() {
        let mut eq = EqPreset::Rock.settings();
        eq.preamp_db = 99.0;
        let spatial = SpatialPreset::Immersive3d.settings();
        let mut processor = AudioProcessor::new(48_000, clamp_eq(eq), spatial, 1.0);
        let output = processor.process(&vec![8.0; 192], 48_000, 2);
        assert!(output.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn process_into_reuses_allocations() {
        let mut processor = AudioProcessor::new(
            48_000,
            EqSettings::default(),
            SpatialSettings::default(),
            1.0,
        );
        let input = vec![0.25; 96];
        let mut output = Vec::new();
        processor.process_into(&input, 24_000, 1, &mut output);
        let output_ptr = output.as_ptr();
        let output_capacity = output.capacity();
        let scratch_capacity = processor.stereo_scratch.capacity();
        processor.process_into(&input, 24_000, 1, &mut output);
        assert_eq!(output.as_ptr(), output_ptr);
        assert_eq!(output.capacity(), output_capacity);
        assert_eq!(processor.stereo_scratch.capacity(), scratch_capacity);
    }

    #[test]
    fn native_layout_requires_explicit_matching_metadata() {
        assert_eq!(
            validated_native_spatial_layout(6, Some(ChannelLayout::Surround5_1)),
            Some(ChannelLayout::Surround5_1)
        );
        assert_eq!(
            validated_native_spatial_layout(8, Some(ChannelLayout::Surround7_1)),
            Some(ChannelLayout::Surround7_1)
        );
        assert_eq!(
            validated_native_spatial_layout(8, Some(ChannelLayout::Surround5_1_2)),
            Some(ChannelLayout::Surround5_1_2)
        );
        assert_eq!(
            validated_native_spatial_layout(10, Some(ChannelLayout::Surround5_1_4)),
            Some(ChannelLayout::Surround5_1_4)
        );
        assert_eq!(
            validated_native_spatial_layout(10, Some(ChannelLayout::Surround7_1_2)),
            Some(ChannelLayout::Surround7_1_2)
        );
        assert_eq!(
            validated_native_spatial_layout(12, Some(ChannelLayout::Surround7_1_4)),
            Some(ChannelLayout::Surround7_1_4)
        );
        assert_eq!(validated_native_spatial_layout(6, None), None);
        assert_eq!(validated_native_spatial_layout(8, None), None);
        assert_eq!(validated_native_spatial_layout(10, None), None);
        assert_eq!(
            validated_native_spatial_layout(6, Some(ChannelLayout::Surround7_1)),
            None
        );
        assert_eq!(
            validated_native_spatial_layout(10, Some(ChannelLayout::Surround5_1_2)),
            None
        );
        assert_eq!(
            validated_native_spatial_layout(10, Some(ChannelLayout::Surround7_1_4)),
            None
        );
        assert_eq!(
            validated_native_spatial_layout(12, Some(ChannelLayout::Surround5_1_4)),
            None
        );
    }

    #[test]
    fn matching_source_layout_override_declares_metadata_less_multichannel() {
        let mut settings = SpatialPreset::Studio.settings();
        settings.source_layout_override = SourceLayoutOverride::Surround5_1_2;
        assert_eq!(
            resolved_native_spatial_layout(8, None, &settings),
            Some(ChannelLayout::Surround5_1_2)
        );
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_2;
        assert_eq!(
            resolved_native_spatial_layout(10, None, &settings),
            Some(ChannelLayout::Surround7_1_2)
        );
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_4;
        assert_eq!(
            resolved_native_spatial_layout(12, None, &settings),
            Some(ChannelLayout::Surround7_1_4)
        );
    }

    #[test]
    fn source_layout_override_is_independent_from_spatial_enable() {
        let mut settings = SpatialPreset::Hifi.settings();
        assert!(!settings.enabled);
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_2;
        assert_eq!(
            resolved_native_spatial_layout(10, None, &settings),
            Some(ChannelLayout::Surround7_1_2)
        );
    }

    #[test]
    fn source_layout_override_never_beats_verified_or_conflicting_metadata() {
        let mut settings = SpatialPreset::Studio.settings();
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_2;
        assert_eq!(
            resolved_native_spatial_layout(10, Some(ChannelLayout::Surround5_1_4), &settings),
            Some(ChannelLayout::Surround5_1_4)
        );
        assert_eq!(
            resolved_native_spatial_layout(10, Some(ChannelLayout::Surround7_1_4), &settings),
            None
        );
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_4;
        assert_eq!(resolved_native_spatial_layout(10, None, &settings), None);
    }

    #[test]
    fn virtual_bed_selection_does_not_declare_unknown_multichannel_layout() {
        let mut settings = SpatialPreset::Studio.settings();
        settings.virtual_bed = VirtualBedMode::Surround7_1_2;
        settings.source_layout_override = SourceLayoutOverride::None;
        assert_eq!(resolved_native_spatial_layout(10, None, &settings), None);
    }

    #[test]
    fn ambiguous_ten_channel_pcm_without_hint_stays_out_of_native_renderer() {
        let input = vec![0.05_f32; 10 * 64];
        let mut processor = AudioProcessor::new(
            48_000,
            EqPreset::Flat.settings(),
            SpatialSettings::default(),
            1.0,
        );
        let output = processor.process(&input, 48_000, 10);
        assert_eq!(output.len(), 128);
        assert!(processor.native_spatial.is_none());
    }

    #[test]
    fn unknown_multichannel_auto_stays_conservative() {
        let input = (0..128)
            .flat_map(|frame| {
                (0..10).map(move |channel| ((frame * 7 + channel * 13) as f32 * 0.013).sin() * 0.1)
            })
            .collect::<Vec<_>>();
        let settings = SpatialPreset::Orbit360.settings();
        assert_eq!(settings.virtual_bed, VirtualBedMode::Auto);
        assert_eq!(settings.source_layout_override, SourceLayoutOverride::None);
        let mut processor = AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process(&input, 48_000, 10);
        assert_eq!(output.len(), 256);
        assert!(processor.native_spatial.is_none());
        assert_eq!(
            processor
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            None
        );
    }

    #[test]
    fn unknown_multichannel_explicit_virtual_bed_uses_spherical_fallback() {
        let input = (0..128)
            .flat_map(|frame| {
                (0..10).map(move |channel| ((frame * 11 + channel * 5) as f32 * 0.017).sin() * 0.12)
            })
            .collect::<Vec<_>>();
        let mut settings = SpatialPreset::Orbit360.settings();
        settings.virtual_bed = VirtualBedMode::Surround7_1_4;
        settings.source_layout_override = SourceLayoutOverride::None;
        let mut processor = AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process(&input, 48_000, 10);
        assert_eq!(output.len(), 256);
        assert!(processor.native_spatial.is_none());
        assert_eq!(
            processor
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            Some(128)
        );
        assert!(output.iter().any(|sample| sample.abs() > 1.0e-4));
    }

    #[test]
    fn metadata_less_ten_channel_override_preserves_native_pcm_path() {
        let mut settings = SpatialPreset::Orbit360.settings();
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_2;
        let input = vec![0.04_f32; 10 * 128];
        let mut processor = AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process(&input, 48_000, 10);
        assert_eq!(output.len(), 256);
        assert!(processor.native_spatial.is_some());
        assert_eq!(
            processor
                .native_spatial
                .as_ref()
                .and_then(SpatialEngine::scene_motion_sample_clock),
            Some(128)
        );
        assert_eq!(
            processor
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            None
        );
    }

    #[test]
    fn hifi_override_preserves_native_layout_without_scene_motion() {
        let mut settings = SpatialPreset::Hifi.settings();
        settings.source_layout_override = SourceLayoutOverride::Surround7_1_2;
        let input = vec![0.04_f32; 10 * 64];
        let mut processor = AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process(&input, 48_000, 10);
        assert_eq!(output.len(), 128);
        let engine = processor.native_spatial.as_ref().expect("native engine");
        assert_eq!(engine.scene_motion_sample_clock(), None);
        assert_eq!(engine.config().environment.mix, 0.0);
    }

    #[test]
    fn verified_native_layout_never_uses_virtual_fallback_even_when_explicit() {
        let mut settings = SpatialPreset::Orbit8d.settings();
        settings.virtual_bed = VirtualBedMode::Surround7_1_4;
        settings.source_layout_override = SourceLayoutOverride::Surround5_1_4;
        let input = vec![0.04_f32; 12 * 128];
        let mut processor = AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process_with_layout(
            &input,
            48_000,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        assert_eq!(output.len(), 256);
        assert!(processor.native_spatial.is_some());
        assert_eq!(
            processor
                .native_spatial
                .as_ref()
                .and_then(SpatialEngine::scene_motion_sample_clock),
            Some(128)
        );
        assert_eq!(
            processor
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            None
        );
    }

    #[test]
    fn layout_neutral_fold_keeps_common_programme_centered() {
        let input = vec![0.25_f32; 10];
        let mut output = Vec::new();
        to_stereo_into(&input, 10, &mut output);
        assert_eq!(output, vec![0.25, 0.25]);
    }

    #[test]
    fn avs3_five_one_uses_self_owned_native_renderer() {
        let mut input = vec![0.0_f32; 6 * 64];
        for frame in input.chunks_exact_mut(6) {
            frame[0] = 0.30;
            frame[1] = -0.15;
            frame[2] = 0.18;
            frame[4] = 0.22;
            frame[5] = -0.12;
        }
        let mut processor = AudioProcessor::new(
            48_000,
            EqPreset::Flat.settings(),
            SpatialSettings::default(),
            1.0,
        );
        let output = processor.process_with_layout(
            &input,
            48_000,
            6,
            Some(ChannelLayout::Surround5_1),
        );
        assert_eq!(output.len(), 128);
        assert_eq!(processor.native_spatial_rate, 48_000);
        assert!(processor.native_spatial.is_some());
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
    }

    #[test]
    fn avs3_seven_one_two_uses_self_owned_native_renderer() {
        let mut input = vec![0.0_f32; 10 * 64];
        for frame in input.chunks_exact_mut(10) {
            frame[0] = 0.30;
            frame[1] = -0.15;
            frame[2] = 0.18;
            frame[4] = 0.22;
            frame[6] = 0.10;
            frame[8] = 0.16;
            frame[9] = -0.12;
        }
        let mut processor = AudioProcessor::new(
            48_000,
            EqPreset::Flat.settings(),
            SpatialSettings::default(),
            1.0,
        );
        let output = processor.process_with_layout(
            &input,
            48_000,
            10,
            Some(ChannelLayout::Surround7_1_2),
        );
        assert_eq!(output.len(), 128);
        assert_eq!(processor.native_spatial_rate, 48_000);
        assert!(processor.native_spatial.is_some());
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
    }

    #[test]
    fn avs3_seven_one_four_uses_self_owned_native_renderer() {
        let mut input = vec![0.0_f32; 12 * 64];
        for frame in input.chunks_exact_mut(12) {
            frame[0] = 0.30;
            frame[1] = -0.15;
            frame[2] = 0.18;
            frame[4] = 0.22;
            frame[8] = 0.14;
            frame[11] = -0.12;
        }

        let mut processor = AudioProcessor::new(
            48_000,
            EqPreset::Flat.settings(),
            SpatialSettings::default(),
            1.0,
        );
        let output = processor.process_with_layout(
            &input,
            48_000,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        assert_eq!(output.len(), 128);
        assert_eq!(processor.native_spatial_rate, 48_000);
        assert!(processor.native_spatial.is_some());
        assert!(output.iter().any(|sample| sample.abs() > 0.001));
    }

    #[test]
    fn native_spatial_scratch_is_bounded_by_engine_block_size() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut processor =
            AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let scratch_ptr = processor.native_spatial_scratch.as_ptr();
        let scratch_len = processor.native_spatial_scratch.len();
        let scratch_capacity = processor.native_spatial_scratch.capacity();
        let input = vec![0.03_f32; 12 * 8_192];
        let output = processor.process_with_layout(
            &input,
            44_100,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );

        assert!(!output.is_empty());
        assert_eq!(processor.native_spatial_scratch.as_ptr(), scratch_ptr);
        assert_eq!(processor.native_spatial_scratch.len(), scratch_len);
        assert_eq!(processor.native_spatial_scratch.capacity(), scratch_capacity);
        assert_eq!(
            processor
                .native_spatial
                .as_ref()
                .and_then(SpatialEngine::scene_motion_sample_clock),
            Some(8_192)
        );
    }

    #[test]
    fn authored_seven_one_family_uses_audio_clock_scene_motion() {
        let cases = [
            (ChannelLayout::Surround7_1, 8_u16),
            (ChannelLayout::Surround7_1_2, 10_u16),
            (ChannelLayout::Surround7_1_4, 12_u16),
        ];
        for (layout, channels) in cases {
            let settings = SpatialPreset::Orbit8d.settings();
            let mut processor =
                AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
            let input = vec![0.05_f32; usize::from(channels) * 128];
            let output =
                processor.process_with_layout(&input, 48_000, channels, Some(layout));
            assert_eq!(output.len(), 256);
            assert_eq!(
                processor
                    .native_spatial
                    .as_ref()
                    .and_then(SpatialEngine::scene_motion_sample_clock),
                Some(128)
            );
        }
    }

    #[test]
    fn authored_other_native_layouts_share_the_same_scene_motion_path() {
        let cases = [
            (ChannelLayout::Surround5_1, 6_u16),
            (ChannelLayout::Surround5_1_2, 8_u16),
            (ChannelLayout::Surround5_1_4, 10_u16),
        ];
        for (layout, channels) in cases {
            let settings = SpatialPreset::Orbit360.settings();
            let mut processor =
                AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
            let input = vec![0.03_f32; usize::from(channels) * 96];
            let _ = processor.process_with_layout(&input, 48_000, channels, Some(layout));
            assert_eq!(
                processor
                    .native_spatial
                    .as_ref()
                    .and_then(SpatialEngine::scene_motion_sample_clock),
                Some(96)
            );
        }
    }

    #[test]
    fn native_multichannel_uses_shared_room_without_second_stereo_motion() {
        let mut input = vec![0.0_f32; 12 * 64];
        for frame in input.chunks_exact_mut(12) {
            frame[0] = 0.30;
            frame[1] = -0.15;
            frame[4] = 0.22;
            frame[8] = 0.18;
            frame[11] = -0.12;
        }

        let immersive = SpatialPreset::Immersive3d.settings();
        let expected_environment = spatial_environment_settings(&immersive);
        let mut enabled =
            AudioProcessor::new(48_000, EqPreset::Flat.settings(), immersive, 1.0);
        let _ = enabled.process_with_layout(
            &input,
            48_000,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        assert_eq!(enabled.native_spatial_environment, Some(expected_environment));
        assert_eq!(
            enabled
                .native_spatial
                .as_ref()
                .expect("native engine")
                .config()
                .environment,
            expected_environment
        );
        assert!(expected_environment.mix > 0.0);
        assert_eq!(
            enabled
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            None
        );

        let mut disabled_settings = SpatialPreset::Immersive3d.settings();
        disabled_settings.enabled = false;
        let mut disabled =
            AudioProcessor::new(48_000, EqPreset::Flat.settings(), disabled_settings, 1.0);
        let _ = disabled.process_with_layout(
            &input,
            48_000,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        assert_eq!(
            disabled
                .native_spatial_environment
                .expect("native environment")
                .mix,
            0.0
        );
    }

    #[test]
    fn stereo_motion_uses_self_owned_audio_clock_renderer() {
        let settings = SpatialPreset::Orbit360.settings();
        let input = vec![0.2_f32; 128 * 2];
        let mut processor =
            AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings, 1.0);
        let output = processor.process(&input, 48_000, 2);
        assert_eq!(output.len(), input.len());
        assert_eq!(
            processor
                .stereo_spatial
                .as_ref()
                .and_then(StereoSpatializer::sample_clock),
            Some(128)
        );
    }

    #[test]
    fn transport_reset_matches_fresh_native_renderer_resampler_and_eq() {
        let mut input = vec![0.0_f32; 12 * 97];
        for (frame_index, frame) in input.chunks_exact_mut(12).enumerate() {
            let phase = frame_index as f32 * 0.031;
            frame[0] = phase.sin() * 0.30;
            frame[1] = phase.cos() * -0.17;
            frame[2] = 0.08;
            frame[4] = phase.sin() * 0.12;
            frame[8] = phase.cos() * 0.09;
            frame[11] = phase.sin() * -0.07;
        }
        let make_processor = || {
            AudioProcessor::new(
                48_000,
                EqPreset::Rock.settings(),
                SpatialSettings::default(),
                1.0,
            )
        };

        let mut reused = make_processor();
        let _dirty = reused.process_with_layout(
            &input,
            44_100,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        reused.reset_transport();
        let actual = reused.process_with_layout(
            &input,
            44_100,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );

        let mut fresh = make_processor();
        let expected = fresh.process_with_layout(
            &input,
            44_100,
            12,
            Some(ChannelLayout::Surround7_1_4),
        );
        assert_eq!(actual.len(), expected.len());
        assert!(
            actual
                .iter()
                .zip(expected.iter())
                .all(|(left, right)| (left - right).abs() < 1.0e-5)
        );
    }

    #[test]
    fn transport_reset_clears_stereo_virtual_source_history() {
        let settings = SpatialPreset::Orbit360.settings();
        let input = (0..512)
            .flat_map(|index| {
                let phase = index as f32 * 0.019;
                [phase.sin() * 0.25, phase.cos() * 0.20]
            })
            .collect::<Vec<_>>();
        let make_processor = || {
            AudioProcessor::new(48_000, EqPreset::Flat.settings(), settings.clone(), 1.0)
        };

        let mut reused = make_processor();
        let _dirty = reused.process(&input, 48_000, 2);
        reused.reset_transport();
        let actual = reused.process(&input, 48_000, 2);

        let mut fresh = make_processor();
        let expected = fresh.process(&input, 48_000, 2);
        assert_eq!(actual.len(), expected.len());
        assert!(
            actual
                .iter()
                .zip(expected.iter())
                .all(|(left, right)| (left - right).abs() < 1.0e-5)
        );
    }

    #[test]
    fn worker_local_transport_generation_is_consumed_once() {
        let input = vec![0.1_f32; 128];
        let mut processor = AudioProcessor::new(
            48_000,
            EqPreset::Rock.settings(),
            SpatialPreset::Orbit360.settings(),
            1.0,
        );
        let _ = processor.process(&input, 48_000, 2);
        let before = processor.transport_reset_generation;
        request_transport_reset();
        assert_ne!(transport_reset_generation(), before);
        let _ = processor.process(&input, 48_000, 2);
        assert_eq!(processor.transport_reset_generation, transport_reset_generation());
    }

    #[test]
    fn streaming_resampler_matches_one_shot_timeline_across_chunk_boundaries() {
        let mut input = Vec::new();
        for frame in 0..257 {
            let value = frame as f32 / 257.0;
            input.extend_from_slice(&[value, -value]);
        }

        let mut one_shot = StreamingLinearResampler::new(48_000);
        let mut expected = Vec::new();
        one_shot.process_into(&input, 44_100, 48_000, &mut expected);

        let mut streaming = StreamingLinearResampler::new(48_000);
        let mut actual = Vec::new();
        for range in [0..74, 74..161, 161..257] {
            streaming.process_append(
                &input[range.start * 2..range.end * 2],
                44_100,
                48_000,
                &mut actual,
            );
        }

        assert_eq!(actual.len(), expected.len());
        assert!(
            actual
                .iter()
                .zip(expected.iter())
                .all(|(left, right)| (left - right).abs() < 1.0e-5)
        );
    }

    #[test]
    fn perceptual_volume_curve_preserves_low_level_headroom() {
        assert_eq!(perceptual_volume_gain(0.0), 0.0);
        assert!((perceptual_volume_gain(0.5) - 0.25).abs() < f32::EPSILON);
        assert_eq!(perceptual_volume_gain(1.0), 1.0);
    }
}
