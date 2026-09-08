use crate::model::{SpatialMotionMode, SpatialSettings};
use yinqidao_audio_spatial::{EngineConfig, SpatialEngine, Trajectory, TrajectoryKind};

const MIN_TRAJECTORY_RADIUS_METERS: f32 = 0.35;
const TRAJECTORY_RADIUS_RANGE_METERS: f32 = 0.65;
const MAX_TRAJECTORY_BLEND: f32 = 0.55;

#[derive(Clone, Copy, Debug, PartialEq)]
struct TrajectorySignature {
    kind: TrajectoryKind,
    speed_hz: f32,
    radius_meters: f32,
    clockwise: bool,
}

impl TrajectorySignature {
    fn from_settings(settings: &SpatialSettings) -> Option<Self> {
        if !settings.enabled || settings.motion_intensity <= 0.001 {
            return None;
        }
        let kind = match settings.motion_mode {
            SpatialMotionMode::Static => return None,
            SpatialMotionMode::Orbit8d => TrajectoryKind::FigureEight,
            SpatialMotionMode::Orbit360 => TrajectoryKind::Orbit360,
            SpatialMotionMode::Pendulum => TrajectoryKind::Pendulum,
            SpatialMotionMode::FrontBack => TrajectoryKind::FrontBack,
            SpatialMotionMode::Planetary => TrajectoryKind::Planetary,
            SpatialMotionMode::NearEar => TrajectoryKind::NearEar,
        };
        Some(Self {
            kind,
            speed_hz: settings.motion_speed_hz,
            radius_meters: MIN_TRAJECTORY_RADIUS_METERS
                + settings.motion_radius.clamp(0.0, 1.0) * TRAJECTORY_RADIUS_RANGE_METERS,
            clockwise: settings.clockwise,
        })
    }
}

/// Root-player bridge for the self-owned audio-clock trajectory renderer.
///
/// The authored stereo image remains in the legacy static stage for now. Only its centre component
/// is sent through `SpatialEngine::render_mono_trajectory`, then mixed back with a bounded wet gain.
/// This migrates motion ownership without collapsing the whole stereo programme to mono.
#[derive(Clone, Debug)]
pub(crate) struct TrajectorySpatializer {
    sample_rate: u32,
    engine: SpatialEngine,
    trajectory: Option<Trajectory>,
    signature: Option<TrajectorySignature>,
    mono_scratch: Vec<f32>,
    wet_scratch: Vec<f32>,
}

impl TrajectorySpatializer {
    pub(crate) fn new(sample_rate: u32) -> Option<Self> {
        let sample_rate = sample_rate.max(1);
        let mut config = EngineConfig::new(sample_rate);
        // The legacy static stage still owns width/crossfeed/room during this migration. Adding the
        // new engine environment here would spatialize the same ambience twice.
        config.environment.mix = 0.0;
        Some(Self {
            sample_rate,
            engine: SpatialEngine::new(config).ok()?,
            trajectory: None,
            signature: None,
            mono_scratch: Vec::new(),
            wet_scratch: Vec::new(),
        })
    }

    pub(crate) fn reset(&mut self) {
        self.engine.reset();
        if let Some(trajectory) = self.trajectory.as_mut() {
            trajectory.reset();
        }
    }

    pub(crate) fn process_in_place(
        &mut self,
        samples: &mut [f32],
        settings: &SpatialSettings,
    ) -> bool {
        let Some(signature) = TrajectorySignature::from_settings(settings) else {
            return false;
        };
        self.ensure_trajectory(signature);

        let frames = samples.len() / 2;
        if frames == 0 {
            return true;
        }
        self.mono_scratch.resize(frames, 0.0);
        for (destination, frame) in self
            .mono_scratch
            .iter_mut()
            .zip(samples.as_chunks::<2>().0.iter())
        {
            *destination = (frame[0] + frame[1]) * 0.5;
        }
        self.wet_scratch.resize(frames.saturating_mul(2), 0.0);

        let Some(trajectory) = self.trajectory.as_mut() else {
            return false;
        };
        if self
            .engine
            .render_mono_trajectory(
                &self.mono_scratch,
                trajectory,
                &mut self.wet_scratch,
            )
            .is_err()
        {
            return false;
        }

        // Old motion was bounded to 55% of the spatial wet field. Preserve that safety property,
        // while also respecting the user's overall spatial mix so low-mix presets stay subtle.
        let blend = (settings.motion_intensity.clamp(0.0, 1.0)
            * settings.mix.clamp(0.0, 1.0)
            * MAX_TRAJECTORY_BLEND)
            .clamp(0.0, MAX_TRAJECTORY_BLEND);
        let dry = 1.0 - blend;
        for (sample, wet) in samples.iter_mut().zip(self.wet_scratch.iter().copied()) {
            *sample = *sample * dry + wet * blend;
        }
        true
    }

    fn ensure_trajectory(&mut self, signature: TrajectorySignature) {
        if self.signature == Some(signature) && self.trajectory.is_some() {
            return;
        }
        let mut trajectory = Trajectory::new(
            signature.kind,
            self.sample_rate,
            signature.speed_hz,
            signature.radius_meters,
            0.0,
        );
        trajectory.set_clockwise(signature.clockwise);
        self.engine.reset();
        self.trajectory = Some(trajectory);
        self.signature = Some(signature);
    }

    #[cfg(test)]
    pub(crate) fn sample_clock(&self) -> Option<u64> {
        self.trajectory.as_ref().map(Trajectory::sample_clock)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::SpatialPreset;

    #[test]
    fn orbit8d_maps_to_audio_clock_figure_eight() {
        let settings = SpatialPreset::Orbit8d.settings();
        let signature = TrajectorySignature::from_settings(&settings).expect("dynamic");
        assert_eq!(signature.kind, TrajectoryKind::FigureEight);
    }

    #[test]
    fn processing_advances_and_reset_rewinds_trajectory_clock() {
        let settings = SpatialPreset::Orbit360.settings();
        let mut spatializer = TrajectorySpatializer::new(48_000).expect("engine");
        let mut samples = vec![0.25_f32; 128 * 2];
        assert!(spatializer.process_in_place(&mut samples, &settings));
        assert_eq!(spatializer.sample_clock(), Some(128));
        spatializer.reset();
        assert_eq!(spatializer.sample_clock(), Some(0));
    }

    #[test]
    fn static_settings_do_not_activate_trajectory_renderer() {
        let settings = SpatialPreset::Immersive3d.settings();
        assert!(TrajectorySignature::from_settings(&settings).is_none());
    }
}
