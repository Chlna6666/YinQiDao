use crate::model::SpatialSettings;
use yinqidao_audio_spatial::EnvironmentSettings;

const MAX_ENVIRONMENT_MIX: f32 = 0.20;

/// Convert user-facing spatial controls into the acoustic environment shared by stereo virtualization
/// and authored native multichannel rendering. Disabling spatial effects keeps the mandatory native
/// binaural renderer alive, but removes synthetic early/late room energy.
pub(crate) fn spatial_environment_settings(settings: &SpatialSettings) -> EnvironmentSettings {
    let room_size = settings.room_size.clamp(0.0, 1.0);
    let damping = (0.34
        + room_size * 0.28
        + settings.distance.clamp(0.0, 1.0) * 0.18)
        .clamp(0.0, 1.0);
    let mix = if settings.enabled {
        (settings.depth.clamp(0.0, 1.0) * 0.09
            + room_size * 0.08
            + settings.immersive_3d.clamp(0.0, 1.0) * 0.05)
            .clamp(0.0, MAX_ENVIRONMENT_MIX)
    } else {
        0.0
    };

    EnvironmentSettings {
        mix,
        room_size,
        damping,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::SpatialPreset;

    #[test]
    fn disabled_spatial_keeps_geometry_but_removes_synthetic_room_energy() {
        let mut settings = SpatialPreset::Immersive3d.settings();
        settings.enabled = false;
        let environment = spatial_environment_settings(&settings);
        assert_eq!(environment.mix, 0.0);
        assert!(environment.room_size >= 0.0);
        assert!(environment.damping > 0.0);
    }

    #[test]
    fn immersive_settings_request_bounded_room_energy() {
        let settings = SpatialPreset::Immersive3d.settings();
        let environment = spatial_environment_settings(&settings);
        assert!(environment.mix > 0.0);
        assert!(environment.mix <= MAX_ENVIRONMENT_MIX);
        assert!((0.0..=1.0).contains(&environment.room_size));
        assert!((0.0..=1.0).contains(&environment.damping));
    }
}
