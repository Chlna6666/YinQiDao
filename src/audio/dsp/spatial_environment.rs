use crate::model::SpatialSettings;
use yinqidao_audio_spatial::EnvironmentSettings;

const MAX_ENVIRONMENT_MIX: f32 = 0.16;

/// Convert user-facing spatial controls into the acoustic environment shared by stereo virtualization
/// and authored native multichannel rendering. The listener-centric spherical direct field is the
/// primary localization layer; rectangular early reflections and the late field are deliberately
/// kept as a lower-level room-acoustics layer so six room planes cannot dominate the perceived 3D
/// geometry. Disabling spatial effects keeps the mandatory native binaural renderer alive, but
/// removes synthetic early/late room energy.
pub(crate) fn spatial_environment_settings(settings: &SpatialSettings) -> EnvironmentSettings {
    let room_size = settings.room_size.clamp(0.0, 1.0);
    // Large rooms used to drive the FDN and wall filters too dark: the 0.34 base plus a 0.28
    // room-size slope pushed Concert/Immersive scenes toward an obviously closed-in top end.
    // Room size should primarily control geometry/decay. Keep a smaller distance-dependent air
    // loss while preserving enough upper-mid/high-frequency energy for binaural externalization.
    let damping = (0.20
        + room_size * 0.18
        + settings.distance.clamp(0.0, 1.0) * 0.10)
        .clamp(0.0, 1.0);
    let mix = if settings.enabled {
        (settings.depth.clamp(0.0, 1.0) * 0.085
            + room_size * 0.072
            + settings.immersive_3d.clamp(0.0, 1.0) * 0.024)
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
    fn immersive_settings_keep_room_below_the_primary_spherical_field() {
        let settings = SpatialPreset::Immersive3d.settings();
        let environment = spatial_environment_settings(&settings);
        assert!(environment.mix > 0.0);
        assert!(environment.mix <= MAX_ENVIRONMENT_MIX);
        assert!(environment.mix < settings.mix * 0.30);
        assert!((0.0..=1.0).contains(&environment.room_size));
        assert!((0.0..=1.0).contains(&environment.damping));
    }

    #[test]
    fn professional_rooms_keep_an_open_high_frequency_decay() {
        let concert = spatial_environment_settings(&SpatialPreset::ConcertHall.settings());
        let immersive = spatial_environment_settings(&SpatialPreset::Immersive3d.settings());
        assert!(concert.damping < 0.40);
        assert!(immersive.damping < 0.35);
    }
}
