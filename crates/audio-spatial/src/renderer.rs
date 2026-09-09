use std::f32::consts::PI;

use crate::{
    EnvironmentSettings, ListenerPose, SourceKind, SourcePose, SpatialError, Vec3,
    delay::CubicDelayLine,
    environment::{EARLY_REFLECTION_TAP_COUNT, ReflectionWall},
    image_source::{MAX_REFLECTION_DELAY_SECONDS, source_reflection_descriptors},
    pinna::{
        StereoPinnaCoefficientStep, StereoPinnaCoefficients, StereoPinnaState,
        coefficients_for_direction, reflection_coefficients_for_direction,
    },
};

const SPEED_OF_SOUND_M_S: f32 = 343.0;
const HEAD_RADIUS_M: f32 = 0.0875;
const COMMON_CAUSAL_DELAY_SAMPLES: f32 = 2.0;
const MAX_DISTANCE_METERS: f32 = 32.0;
const NEAR_FIELD_FULL_METERS: f32 = 0.25;
const NEAR_FIELD_FADE_METERS: f32 = 1.20;
const AIR_ABSORPTION_START_METERS: f32 = 1.0;
const REFLECTION_EPSILON: f32 = 1.0e-5;

#[derive(Clone, Copy, Debug, Default)]
struct RenderParameters {
    left_delay: f32,
    right_delay: f32,
    left_gain: f32,
    right_gain: f32,
    left_filter_alpha: f32,
    right_filter_alpha: f32,
}

impl RenderParameters {
    #[inline]
    fn step_to(self, end: Self, frames: usize) -> RenderParameterStep {
        if frames == 0 {
            return RenderParameterStep::default();
        }
        // `end` is the state at n + frames. Use an end-exclusive ramp so the realtime loop consumes
        // exactly `frames` sample-clock intervals before reaching the next block boundary.
        let scale = 1.0 / frames as f32;
        RenderParameterStep {
            left_delay: (end.left_delay - self.left_delay) * scale,
            right_delay: (end.right_delay - self.right_delay) * scale,
            left_gain: (end.left_gain - self.left_gain) * scale,
            right_gain: (end.right_gain - self.right_gain) * scale,
            left_filter_alpha: (end.left_filter_alpha - self.left_filter_alpha) * scale,
            right_filter_alpha: (end.right_filter_alpha - self.right_filter_alpha) * scale,
        }
    }

    #[inline]
    fn advance(&mut self, step: RenderParameterStep) {
        self.left_delay += step.left_delay;
        self.right_delay += step.right_delay;
        self.left_gain += step.left_gain;
        self.right_gain += step.right_gain;
        self.left_filter_alpha += step.left_filter_alpha;
        self.right_filter_alpha += step.right_filter_alpha;
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RenderParameterStep {
    left_delay: f32,
    right_delay: f32,
    left_gain: f32,
    right_gain: f32,
    left_filter_alpha: f32,
    right_filter_alpha: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct ReflectionRenderParameters {
    path: RenderParameters,
    pinna: StereoPinnaCoefficients,
    pinna_enabled: bool,
}

impl ReflectionRenderParameters {
    #[inline]
    fn step_to(self, end: Self, frames: usize) -> ReflectionRenderParameterStep {
        ReflectionRenderParameterStep {
            path: self.path.step_to(end.path, frames),
            pinna: self.pinna.step_to(end.pinna, frames),
        }
    }

    #[inline]
    fn advance(&mut self, step: ReflectionRenderParameterStep) {
        self.path.advance(step.path);
        if self.pinna_enabled {
            self.pinna.advance_primary(step.pinna);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ReflectionRenderParameterStep {
    path: RenderParameterStep,
    pinna: StereoPinnaCoefficientStep,
}

#[derive(Clone, Debug)]
struct SourceState {
    delay: CubicDelayLine,
    filter_left: f32,
    filter_right: f32,
    pinna: StereoPinnaState,
    reflection_filter_left: [f32; EARLY_REFLECTION_TAP_COUNT],
    reflection_filter_right: [f32; EARLY_REFLECTION_TAP_COUNT],
    reflection_pinna: [StereoPinnaState; EARLY_REFLECTION_TAP_COUNT],
    lfe_state: f32,
    scratch_left: Vec<f32>,
    scratch_right: Vec<f32>,
    cached_pose: Option<SourcePose>,
    cached_listener: Option<ListenerPose>,
    cached_parameters: RenderParameters,
    cached_pinna_pose: Option<SourcePose>,
    cached_pinna_listener: Option<ListenerPose>,
    cached_pinna_coefficients: StereoPinnaCoefficients,
    cached_reflection_pose: Option<SourcePose>,
    cached_reflection_listener: Option<ListenerPose>,
    cached_reflection_environment: Option<EnvironmentSettings>,
    cached_reflection_parameters: [ReflectionRenderParameters; EARLY_REFLECTION_TAP_COUNT],
}

impl SourceState {
    fn new(delay_capacity: usize, block_frames: usize) -> Self {
        Self {
            delay: CubicDelayLine::new(delay_capacity),
            filter_left: 0.0,
            filter_right: 0.0,
            pinna: StereoPinnaState::default(),
            reflection_filter_left: [0.0; EARLY_REFLECTION_TAP_COUNT],
            reflection_filter_right: [0.0; EARLY_REFLECTION_TAP_COUNT],
            reflection_pinna: [StereoPinnaState::default(); EARLY_REFLECTION_TAP_COUNT],
            lfe_state: 0.0,
            scratch_left: vec![0.0; block_frames],
            scratch_right: vec![0.0; block_frames],
            cached_pose: None,
            cached_listener: None,
            cached_parameters: RenderParameters::default(),
            cached_pinna_pose: None,
            cached_pinna_listener: None,
            cached_pinna_coefficients: StereoPinnaCoefficients::IDENTITY,
            cached_reflection_pose: None,
            cached_reflection_listener: None,
            cached_reflection_environment: None,
            cached_reflection_parameters: [
                ReflectionRenderParameters::default();
                EARLY_REFLECTION_TAP_COUNT
            ],
        }
    }

    #[inline]
    fn parameters_for(
        &mut self,
        sample_rate: f32,
        pose: SourcePose,
        listener: ListenerPose,
    ) -> RenderParameters {
        if self.cached_pose == Some(pose) && self.cached_listener == Some(listener) {
            return self.cached_parameters;
        }
        let parameters = parameters_for_pose(sample_rate, pose, listener);
        self.cached_pose = Some(pose);
        self.cached_listener = Some(listener);
        self.cached_parameters = parameters;
        parameters
    }

    #[inline]
    fn pinna_for(
        &mut self,
        sample_rate: f32,
        pose: SourcePose,
        listener: ListenerPose,
    ) -> StereoPinnaCoefficients {
        if self.cached_pinna_pose == Some(pose) && self.cached_pinna_listener == Some(listener) {
            return self.cached_pinna_coefficients;
        }

        let (azimuth, elevation) = listener_direction_angles(pose.position, listener);
        let coefficients = coefficients_for_direction(
            sample_rate,
            azimuth,
            elevation,
            finite_or_zero(pose.spread).clamp(0.0, 1.0),
        );

        self.cached_pinna_pose = Some(pose);
        self.cached_pinna_listener = Some(listener);
        self.cached_pinna_coefficients = coefficients;
        coefficients
    }

    #[inline]
    fn reflection_parameters_for(
        &mut self,
        sample_rate: f32,
        pose: SourcePose,
        listener: ListenerPose,
        environment: EnvironmentSettings,
    ) -> [ReflectionRenderParameters; EARLY_REFLECTION_TAP_COUNT] {
        if environment.mix <= REFLECTION_EPSILON {
            return [ReflectionRenderParameters::default(); EARLY_REFLECTION_TAP_COUNT];
        }
        if self.cached_reflection_pose == Some(pose)
            && self.cached_reflection_listener == Some(listener)
            && self.cached_reflection_environment == Some(environment)
        {
            return self.cached_reflection_parameters;
        }

        let descriptors = source_reflection_descriptors(sample_rate, pose, listener, environment);
        let parameters = std::array::from_fn(|index| {
            let reflection = descriptors[index];
            let reflected_pose = SourcePose {
                position: reflection.image_position,
                velocity: pose.velocity,
                gain: finite_or_zero(pose.gain).clamp(0.0, 4.0)
                    * environment.mix
                    * reflection.wall_reflectance,
                spread: finite_or_zero(pose.spread).clamp(0.0, 1.0),
            };
            let mut path = parameters_for_pose(sample_rate, reflected_pose, listener);
            // Direct rendering intentionally omits absolute propagation latency. The image-source
            // solver therefore contributes only the excess path delay, while the virtual image
            // position still drives binaural direction, distance attenuation and air absorption.
            path.left_delay += reflection.excess_delay_samples;
            path.right_delay += reflection.excess_delay_samples;
            let wall_alpha = one_pole_alpha(sample_rate, reflection.damping_cutoff_hz);
            path.left_filter_alpha = path.left_filter_alpha.min(wall_alpha);
            path.right_filter_alpha = path.right_filter_alpha.min(wall_alpha);

            // Lateral wall reflections already have strong ITD/ILD. Spend the extra biquad only on
            // front/rear/floor/ceiling image sources, where preserving sagittal spectral identity
            // materially improves depth/elevation without multiplying the full room-path cost.
            let pinna_enabled = matches!(
                reflection.wall,
                ReflectionWall::Front
                    | ReflectionWall::Rear
                    | ReflectionWall::Floor
                    | ReflectionWall::Ceiling
            );
            let pinna = if pinna_enabled {
                let (azimuth, elevation) =
                    listener_direction_angles(reflected_pose.position, listener);
                reflection_coefficients_for_direction(
                    sample_rate,
                    azimuth,
                    elevation,
                    reflected_pose.spread,
                )
            } else {
                StereoPinnaCoefficients::IDENTITY
            };

            ReflectionRenderParameters {
                path,
                pinna,
                pinna_enabled,
            }
        });

        self.cached_reflection_pose = Some(pose);
        self.cached_reflection_listener = Some(listener);
        self.cached_reflection_environment = Some(environment);
        self.cached_reflection_parameters = parameters;
        parameters
    }

    fn invalidate_environment(&mut self) {
        self.reflection_filter_left.fill(0.0);
        self.reflection_filter_right.fill(0.0);
        for pinna in &mut self.reflection_pinna {
            pinna.reset();
        }
        self.cached_reflection_pose = None;
        self.cached_reflection_listener = None;
        self.cached_reflection_environment = None;
        self.cached_reflection_parameters = [
            ReflectionRenderParameters::default();
            EARLY_REFLECTION_TAP_COUNT
        ];
    }

    fn reset(&mut self) {
        self.delay.reset();
        self.filter_left = 0.0;
        self.filter_right = 0.0;
        self.pinna.reset();
        self.lfe_state = 0.0;
        self.scratch_left.fill(0.0);
        self.scratch_right.fill(0.0);
        self.cached_pose = None;
        self.cached_listener = None;
        self.cached_parameters = RenderParameters::default();
        self.cached_pinna_pose = None;
        self.cached_pinna_listener = None;
        self.cached_pinna_coefficients = StereoPinnaCoefficients::IDENTITY;
        self.invalidate_environment();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CpuRenderer {
    sample_rate: f32,
    block_frames: usize,
    sources: Vec<SourceState>,
    lfe_alpha: f32,
    environment: EnvironmentSettings,
}

impl CpuRenderer {
    pub(crate) fn new(
        sample_rate: u32,
        block_frames: usize,
        max_sources: usize,
    ) -> Result<Self, SpatialError> {
        if sample_rate == 0 {
            return Err(SpatialError::InvalidSampleRate);
        }
        if block_frames == 0 {
            return Err(SpatialError::InvalidBlockFrames);
        }
        if max_sources == 0 {
            return Err(SpatialError::InvalidSourceCapacity);
        }
        let sample_rate_f32 = sample_rate as f32;
        let maximum_itd_seconds = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * (PI * 0.5 + 1.0);
        // One ring per source now serves direct ITD and all first-order reflections. This avoids a
        // second PCM copy/ring for the room path while keeping the render-time storage fixed.
        let delay_capacity = (sample_rate_f32
            * (MAX_REFLECTION_DELAY_SECONDS + maximum_itd_seconds + 0.0035))
            .ceil() as usize
            + 8;
        let lfe_alpha = 1.0 - (-2.0 * PI * 120.0 / sample_rate_f32).exp();
        let mut sources = Vec::with_capacity(max_sources);
        for _ in 0..max_sources {
            sources.push(SourceState::new(delay_capacity, block_frames));
        }
        Ok(Self {
            sample_rate: sample_rate_f32,
            block_frames,
            sources,
            lfe_alpha,
            environment: EnvironmentSettings::default(),
        })
    }

    pub(crate) fn source_capacity(&self) -> usize {
        self.sources.len()
    }

    pub(crate) fn set_environment(&mut self, settings: EnvironmentSettings) {
        let settings = sanitize_environment(settings);
        if settings == self.environment {
            return;
        }
        self.environment = settings;
        for source in &mut self.sources {
            source.invalidate_environment();
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_strided_source(
        &mut self,
        source_index: usize,
        input: &[f32],
        input_stride: usize,
        input_channel: usize,
        frames: usize,
        start_pose: SourcePose,
        end_pose: SourcePose,
        listener: ListenerPose,
        kind: SourceKind,
        mix_left: &mut [f32],
        mix_right: &mut [f32],
    ) -> Result<(), SpatialError> {
        if source_index >= self.sources.len() {
            return Err(SpatialError::SourceCapacityExceeded);
        }
        debug_assert!(frames <= self.block_frames);
        debug_assert!(frames <= mix_left.len());
        debug_assert!(frames <= mix_right.len());
        let lfe_alpha = self.lfe_alpha;
        let sample_rate = self.sample_rate;
        let environment = self.environment;
        let reflections_enabled = environment.mix > REFLECTION_EPSILON;
        let state = &mut self.sources[source_index];

        match kind {
            SourceKind::FullRange => {
                let start = state.parameters_for(sample_rate, start_pose, listener);
                let end = if start_pose == end_pose {
                    start
                } else {
                    state.parameters_for(sample_rate, end_pose, listener)
                };
                let mut parameters = start;
                let parameter_step = start.step_to(end, frames);

                let pinna_start = state.pinna_for(sample_rate, start_pose, listener);
                let pinna_end = if start_pose == end_pose {
                    pinna_start
                } else {
                    state.pinna_for(sample_rate, end_pose, listener)
                };
                let mut pinna_coefficients = pinna_start;
                let pinna_step = pinna_start.step_to(pinna_end, frames);

                let reflection_start = if reflections_enabled {
                    state.reflection_parameters_for(sample_rate, start_pose, listener, environment)
                } else {
                    [ReflectionRenderParameters::default(); EARLY_REFLECTION_TAP_COUNT]
                };
                let reflection_end = if !reflections_enabled || start_pose == end_pose {
                    reflection_start
                } else {
                    state.reflection_parameters_for(sample_rate, end_pose, listener, environment)
                };
                let mut reflection_parameters = reflection_start;
                let reflection_steps: [
                    ReflectionRenderParameterStep;
                    EARLY_REFLECTION_TAP_COUNT
                ] = std::array::from_fn(|index| {
                    reflection_start[index].step_to(reflection_end[index], frames)
                });

                let mut input_index = input_channel;
                for frame in 0..frames {
                    let sample = sanitize_sample(input[input_index]);
                    input_index += input_stride;
                    state.delay.push(sample);

                    let (delayed_left, delayed_right) = state
                        .delay
                        .read_pair(parameters.left_delay, parameters.right_delay);
                    state.filter_left +=
                        parameters.left_filter_alpha * (delayed_left - state.filter_left);
                    state.filter_right +=
                        parameters.right_filter_alpha * (delayed_right - state.filter_right);
                    let direct_left = state.filter_left * parameters.left_gain;
                    let direct_right = state.filter_right * parameters.right_gain;
                    let (mut output_left, mut output_right) =
                        state.pinna.process(direct_left, direct_right, pinna_coefficients);

                    if reflections_enabled {
                        for tap in 0..EARLY_REFLECTION_TAP_COUNT {
                            let reflection = reflection_parameters[tap];
                            let path = reflection.path;
                            let (reflected_left, reflected_right) =
                                state.delay.read_pair(path.left_delay, path.right_delay);
                            state.reflection_filter_left[tap] += path.left_filter_alpha
                                * (reflected_left - state.reflection_filter_left[tap]);
                            state.reflection_filter_right[tap] += path.right_filter_alpha
                                * (reflected_right - state.reflection_filter_right[tap]);
                            let mut reflected_left =
                                state.reflection_filter_left[tap] * path.left_gain;
                            let mut reflected_right =
                                state.reflection_filter_right[tap] * path.right_gain;
                            if reflection.pinna_enabled {
                                (reflected_left, reflected_right) = state.reflection_pinna[tap]
                                    .process_primary(
                                        reflected_left,
                                        reflected_right,
                                        reflection.pinna,
                                    );
                            }
                            output_left += reflected_left;
                            output_right += reflected_right;
                            reflection_parameters[tap].advance(reflection_steps[tap]);
                        }
                    }

                    state.scratch_left[frame] = output_left;
                    state.scratch_right[frame] = output_right;
                    parameters.advance(parameter_step);
                    pinna_coefficients.advance(pinna_step);
                }
            }
            SourceKind::Lfe => {
                // LFE remains direction-independent. First-order directional room reflections are
                // intentionally skipped here; the future diffuse/FDN low-frequency field owns that
                // responsibility without inventing a localized LFE wall image.
                let mut gain = finite_or_zero(start_pose.gain).clamp(0.0, 4.0);
                let end_gain = finite_or_zero(end_pose.gain).clamp(0.0, 4.0);
                let gain_step = if frames == 0 {
                    0.0
                } else {
                    (end_gain - gain) / frames as f32
                };
                let mut input_index = input_channel;
                for frame in 0..frames {
                    let sample = sanitize_sample(input[input_index]);
                    input_index += input_stride;
                    state.delay.push(sample);
                    let delayed = state.delay.read(COMMON_CAUSAL_DELAY_SAMPLES);
                    state.lfe_state += lfe_alpha * (delayed - state.lfe_state);
                    let value = state.lfe_state * gain;
                    state.scratch_left[frame] = value;
                    state.scratch_right[frame] = value;
                    gain += gain_step;
                }
            }
        }

        yinqidao_audio_simd::mix_accumulate(
            &mut mix_left[..frames],
            &state.scratch_left[..frames],
            1.0,
        );
        yinqidao_audio_simd::mix_accumulate(
            &mut mix_right[..frames],
            &state.scratch_right[..frames],
            1.0,
        );
        Ok(())
    }

    pub(crate) fn reset(&mut self) {
        for state in &mut self.sources {
            state.reset();
        }
    }
}

#[inline]
fn listener_direction_angles(position: Vec3, listener: ListenerPose) -> (f32, f32) {
    let raw_relative = position - listener.position;
    let relative = Vec3::new(
        finite_or_zero(raw_relative.x),
        finite_or_zero(raw_relative.y),
        finite_or_zero(raw_relative.z),
    );
    let direction = relative.normalized_or(Vec3::FORWARD);
    let (right, up, forward) = listener.basis();
    let local_right = direction.dot(right);
    let local_up = direction.dot(up);
    let local_forward = direction.dot(forward);
    let azimuth = local_right.atan2(local_forward);
    let horizontal = (local_right * local_right + local_forward * local_forward).sqrt();
    (azimuth, local_up.atan2(horizontal))
}

#[inline]
fn parameters_for_pose(
    sample_rate: f32,
    pose: SourcePose,
    listener: ListenerPose,
) -> RenderParameters {
    let raw_relative = pose.position - listener.position;
    let relative = Vec3::new(
        finite_or_zero(raw_relative.x),
        finite_or_zero(raw_relative.y),
        finite_or_zero(raw_relative.z),
    );
    let distance = relative.length().clamp(0.05, MAX_DISTANCE_METERS);
    let direction = relative.normalized_or(Vec3::FORWARD);
    let (right, up, forward) = listener.basis();
    let local_right = direction.dot(right);
    let local_up = direction.dot(up);
    let local_forward = direction.dot(forward);
    let azimuth = local_right.atan2(local_forward);
    let horizontal = (local_right * local_right + local_forward * local_forward).sqrt();
    let elevation = local_up.atan2(horizontal);
    let spread = finite_or_zero(pose.spread).clamp(0.0, 1.0);
    let lateral = azimuth.sin().abs() * (1.0 - spread * 0.72);
    let front = azimuth.cos();
    let rear = (-front).max(0.0);
    let elevation_sin = elevation.sin();
    let elevation_up = elevation_sin.max(0.0);
    let elevation_down = (-elevation_sin).max(0.0);

    let theta = azimuth.abs().clamp(0.0, PI);
    let path_term = if theta <= PI * 0.5 {
        theta + theta.sin()
    } else {
        PI - theta + theta.sin()
    };
    let far_itd_samples = HEAD_RADIUS_M / SPEED_OF_SOUND_M_S * path_term * sample_rate;

    let local_right_m = relative.dot(right);
    let local_up_m = relative.dot(up);
    let local_forward_m = relative.dot(forward);
    let left_ear_distance = ((local_right_m + HEAD_RADIUS_M).powi(2)
        + local_up_m * local_up_m
        + local_forward_m * local_forward_m)
        .sqrt()
        .max(0.03);
    let right_ear_distance = ((local_right_m - HEAD_RADIUS_M).powi(2)
        + local_up_m * local_up_m
        + local_forward_m * local_forward_m)
        .sqrt()
        .max(0.03);
    let geometric_itd_samples =
        (left_ear_distance - right_ear_distance).abs() / SPEED_OF_SOUND_M_S * sample_rate;
    let near_field_amount = 1.0
        - smoothstep01(
            (distance - NEAR_FIELD_FULL_METERS)
                / (NEAR_FIELD_FADE_METERS - NEAR_FIELD_FULL_METERS),
        );
    let itd_samples = far_itd_samples
        + (geometric_itd_samples - far_itd_samples) * (near_field_amount * 0.35);
    let (left_delay, right_delay) = if azimuth >= 0.0 {
        (
            COMMON_CAUSAL_DELAY_SAMPLES + itd_samples,
            COMMON_CAUSAL_DELAY_SAMPLES,
        )
    } else {
        (
            COMMON_CAUSAL_DELAY_SAMPLES,
            COMMON_CAUSAL_DELAY_SAMPLES + itd_samples,
        )
    };

    let base_far_ear_attenuation =
        (1.0 - lateral * (0.18 + 0.10 / distance.max(0.35))).clamp(0.62, 1.0);
    let (near_ear_distance, far_ear_distance) = if azimuth >= 0.0 {
        (right_ear_distance, left_ear_distance)
    } else {
        (left_ear_distance, right_ear_distance)
    };
    let geometric_far_ear_attenuation =
        (near_ear_distance / far_ear_distance).clamp(0.30, 1.0).sqrt();
    let far_ear_attenuation = base_far_ear_attenuation
        * (1.0
            + (geometric_far_ear_attenuation - 1.0) * (near_field_amount * 0.55));

    let distance_gain = if distance <= 1.0 {
        1.0
    } else {
        1.0 / (1.0 + (distance - 1.0) * 0.34)
    };
    let air_amount = smoothstep01(
        (distance - AIR_ABSORPTION_START_METERS)
            / (MAX_DISTANCE_METERS - AIR_ABSORPTION_START_METERS),
    );
    let rear_gain = 1.0 - rear * 0.06;
    let elevation_gain = 1.0 - elevation_down * 0.025;
    let common_gain = finite_or_zero(pose.gain).clamp(0.0, 4.0)
        * distance_gain
        * rear_gain
        * elevation_gain;

    let near_cutoff = (20_000.0
        - rear * 4_200.0
        - elevation_up * 900.0
        - elevation_down * 1_700.0
        - air_amount * 6_500.0)
        .clamp(4_500.0, 20_000.0);
    let far_cutoff = (near_cutoff - lateral * 9_500.0).clamp(3_200.0, 20_000.0);
    let near_alpha = one_pole_alpha(sample_rate, near_cutoff);
    let far_alpha = one_pole_alpha(sample_rate, far_cutoff);

    if azimuth >= 0.0 {
        RenderParameters {
            left_delay,
            right_delay,
            left_gain: common_gain * far_ear_attenuation,
            right_gain: common_gain,
            left_filter_alpha: far_alpha,
            right_filter_alpha: near_alpha,
        }
    } else {
        RenderParameters {
            left_delay,
            right_delay,
            left_gain: common_gain,
            right_gain: common_gain * far_ear_attenuation,
            left_filter_alpha: near_alpha,
            right_filter_alpha: far_alpha,
        }
    }
}

#[inline]
fn one_pole_alpha(sample_rate: f32, cutoff_hz: f32) -> f32 {
    1.0 - (-2.0 * PI * cutoff_hz.min(sample_rate * 0.45) / sample_rate).exp()
}

#[inline]
fn sanitize_environment(settings: EnvironmentSettings) -> EnvironmentSettings {
    EnvironmentSettings {
        mix: finite_or_zero(settings.mix).clamp(0.0, 0.45),
        room_size: finite_or_zero(settings.room_size).clamp(0.0, 1.0),
        damping: finite_or_zero(settings.damping).clamp(0.0, 1.0),
    }
}

#[inline]
fn smoothstep01(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

#[inline]
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

#[inline]
fn sanitize_sample(sample: f32) -> f32 {
    finite_or_zero(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_source_delays_and_attenuates_far_left_ear() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::RIGHT),
            ListenerPose::identity(),
        );
        assert!(parameters.left_delay > parameters.right_delay);
        assert!(parameters.left_gain < parameters.right_gain);
        assert!(parameters.left_filter_alpha < parameters.right_filter_alpha);
    }

    #[test]
    fn front_source_is_symmetric() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::FORWARD),
            ListenerPose::identity(),
        );
        assert!((parameters.left_delay - parameters.right_delay).abs() < 1.0e-6);
        assert!((parameters.left_gain - parameters.right_gain).abs() < 1.0e-6);

        let mut state = SourceState::new(4_096, 64);
        let pinna = state.pinna_for(
            48_000.0,
            SourcePose::new(Vec3::FORWARD),
            ListenerPose::identity(),
        );
        assert_eq!(pinna.left, pinna.right);
    }

    #[test]
    fn near_lateral_source_has_stronger_ild_than_far_lateral_source() {
        let near = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.22, 0.0, 0.0)),
            ListenerPose::identity(),
        );
        let far = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(4.0, 0.0, 0.0)),
            ListenerPose::identity(),
        );
        let near_ratio = near.left_gain / near.right_gain.max(1.0e-8);
        let far_ratio = far.left_gain / far.right_gain.max(1.0e-8);
        assert!(near_ratio < far_ratio);
    }

    #[test]
    fn distant_front_source_has_more_air_absorption() {
        let near = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.0, 0.0, 1.0)),
            ListenerPose::identity(),
        );
        let far = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::new(0.0, 0.0, 24.0)),
            ListenerPose::identity(),
        );
        assert!(far.left_filter_alpha < near.left_filter_alpha);
        assert!(far.right_filter_alpha < near.right_filter_alpha);
    }

    #[test]
    fn source_parameter_cache_tracks_pose_and_listener() {
        let mut state = SourceState::new(4_096, 64);
        let listener = ListenerPose::identity();
        let pose = SourcePose::new(Vec3::RIGHT);
        let first = state.parameters_for(48_000.0, pose, listener);
        assert_eq!(state.cached_pose, Some(pose));
        assert_eq!(state.cached_listener, Some(listener));
        let second = state.parameters_for(48_000.0, pose, listener);
        assert!((first.left_delay - second.left_delay).abs() < f32::EPSILON);
        assert!((first.right_gain - second.right_gain).abs() < f32::EPSILON);

        let moved = SourcePose::new(Vec3::LEFT);
        let third = state.parameters_for(48_000.0, moved, listener);
        assert_eq!(state.cached_pose, Some(moved));
        assert!(third.right_delay > third.left_delay);
    }

    #[test]
    fn pinna_parameter_cache_tracks_pose_and_listener() {
        let mut state = SourceState::new(4_096, 64);
        let listener = ListenerPose::identity();
        let pose = SourcePose::new(Vec3::RIGHT);
        let first = state.pinna_for(48_000.0, pose, listener);
        let second = state.pinna_for(48_000.0, pose, listener);
        assert_eq!(first, second);
        assert_eq!(state.cached_pinna_pose, Some(pose));
        assert_eq!(state.cached_pinna_listener, Some(listener));

        let moved = SourcePose::new(Vec3::BACK);
        let third = state.pinna_for(48_000.0, moved, listener);
        assert_ne!(third, first);
        assert_eq!(state.cached_pinna_pose, Some(moved));
    }

    #[test]
    fn image_source_reflections_add_excess_delay_and_respect_environment_mix() {
        let mut state = SourceState::new(4_096, 64);
        let listener = ListenerPose::identity();
        let pose = SourcePose::new(Vec3::new(0.5, 0.0, 1.0));
        let dry = state.reflection_parameters_for(
            48_000.0,
            pose,
            listener,
            EnvironmentSettings {
                mix: 0.0,
                ..EnvironmentSettings::default()
            },
        );
        assert!(dry.iter().all(|reflection| reflection.path.left_gain == 0.0));
        let wet = state.reflection_parameters_for(
            48_000.0,
            pose,
            listener,
            EnvironmentSettings {
                mix: 0.12,
                ..EnvironmentSettings::default()
            },
        );
        let direct = parameters_for_pose(48_000.0, pose, listener);
        assert!(
            wet.iter()
                .all(|reflection| reflection.path.left_delay >= direct.left_delay)
        );
        assert!(wet.iter().any(|reflection| reflection.path.left_gain > 0.0));
    }

    #[test]
    fn only_sagittal_room_reflections_pay_for_pinna_filtering() {
        let mut state = SourceState::new(4_096, 64);
        let reflections = state.reflection_parameters_for(
            48_000.0,
            SourcePose::new(Vec3::new(0.4, 0.2, 1.0)),
            ListenerPose::identity(),
            EnvironmentSettings {
                mix: 0.18,
                ..EnvironmentSettings::default()
            },
        );
        assert!(!reflections[0].pinna_enabled);
        assert!(!reflections[1].pinna_enabled);
        assert!(reflections[2..].iter().all(|reflection| reflection.pinna_enabled));
        assert_eq!(reflections[0].pinna, StereoPinnaCoefficients::IDENTITY);
        assert_eq!(reflections[1].pinna, StereoPinnaCoefficients::IDENTITY);
        assert_ne!(reflections[2].pinna, StereoPinnaCoefficients::IDENTITY);
        assert_ne!(reflections[3].pinna, StereoPinnaCoefficients::IDENTITY);
        assert_ne!(reflections[4].pinna, StereoPinnaCoefficients::IDENTITY);
        assert_ne!(reflections[5].pinna, StereoPinnaCoefficients::IDENTITY);
    }

    #[test]
    fn environment_change_invalidates_only_reflection_cache() {
        let mut renderer = CpuRenderer::new(48_000, 64, 2).unwrap();
        let pose = SourcePose::new(Vec3::FORWARD);
        let listener = ListenerPose::identity();
        let direct = renderer.sources[0].parameters_for(48_000.0, pose, listener);
        let pinna = renderer.sources[0].pinna_for(48_000.0, pose, listener);
        renderer.sources[0].reflection_parameters_for(
            48_000.0,
            pose,
            listener,
            EnvironmentSettings::default(),
        );
        renderer.set_environment(EnvironmentSettings {
            mix: 0.2,
            room_size: 0.8,
            damping: 0.7,
        });
        assert_eq!(renderer.sources[0].cached_pose, Some(pose));
        assert!(
            (renderer.sources[0].cached_parameters.left_gain - direct.left_gain).abs()
                < f32::EPSILON
        );
        assert_eq!(renderer.sources[0].cached_pinna_coefficients, pinna);
        assert!(renderer.sources[0].cached_reflection_pose.is_none());
    }

    #[test]
    fn end_exclusive_parameter_ramp_reaches_next_block_start() {
        let start = RenderParameters {
            left_delay: 2.0,
            right_delay: 8.0,
            left_gain: 0.4,
            right_gain: 1.0,
            left_filter_alpha: 0.2,
            right_filter_alpha: 0.8,
        };
        let end = RenderParameters {
            left_delay: 10.0,
            right_delay: 3.0,
            left_gain: 0.9,
            right_gain: 0.5,
            left_filter_alpha: 0.7,
            right_filter_alpha: 0.3,
        };
        let frames = 64;
        let step = start.step_to(end, frames);
        let mut current = start;
        for _ in 0..frames {
            current.advance(step);
        }

        assert!((current.left_delay - end.left_delay).abs() < 1.0e-4);
        assert!((current.right_delay - end.right_delay).abs() < 1.0e-4);
        assert!((current.left_gain - end.left_gain).abs() < 1.0e-5);
        assert!((current.right_gain - end.right_gain).abs() < 1.0e-5);
        assert!((current.left_filter_alpha - end.left_filter_alpha).abs() < 1.0e-5);
        assert!((current.right_filter_alpha - end.right_filter_alpha).abs() < 1.0e-5);
    }

    #[test]
    fn single_frame_parameter_ramp_advances_to_next_block_start_after_sample() {
        let start = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::RIGHT),
            ListenerPose::identity(),
        );
        let end = parameters_for_pose(
            48_000.0,
            SourcePose::new(Vec3::LEFT),
            ListenerPose::identity(),
        );
        let step = start.step_to(end, 1);
        let mut current = start;
        current.advance(step);
        assert!((current.left_delay - end.left_delay).abs() < 1.0e-5);
        assert!((current.right_delay - end.right_delay).abs() < 1.0e-5);
        assert!((current.left_gain - end.left_gain).abs() < 1.0e-5);
        assert!((current.right_gain - end.right_gain).abs() < 1.0e-5);
    }

    #[test]
    fn non_finite_pcm_isolated_before_delay_state() {
        assert_eq!(sanitize_sample(f32::NAN), 0.0);
        assert_eq!(sanitize_sample(f32::INFINITY), 0.0);
        assert_eq!(sanitize_sample(f32::NEG_INFINITY), 0.0);
        assert_eq!(sanitize_sample(0.25), 0.25);
    }

    #[test]
    fn non_finite_pose_does_not_produce_non_finite_parameters() {
        let parameters = parameters_for_pose(
            48_000.0,
            SourcePose {
                position: Vec3::new(f32::NAN, f32::INFINITY, 1.0),
                gain: f32::NAN,
                spread: f32::NAN,
                ..SourcePose::default()
            },
            ListenerPose::identity(),
        );
        assert!(parameters.left_delay.is_finite());
        assert!(parameters.right_delay.is_finite());
        assert!(parameters.left_gain.is_finite());
        assert!(parameters.right_gain.is_finite());
        assert!(parameters.left_filter_alpha.is_finite());
        assert!(parameters.right_filter_alpha.is_finite());
    }
}
