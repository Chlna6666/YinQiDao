use std::sync::{OnceLock, RwLock};

use crate::{
    model::{SmartAudioSettings, TrackTransitionSettings},
    settings::AppConfig,
};

#[derive(Clone, Debug, Default)]
pub(crate) struct AudioRuntimePolicy {
    pub smart_audio: SmartAudioSettings,
    pub transition: TrackTransitionSettings,
}

static AUDIO_RUNTIME_POLICY: OnceLock<RwLock<AudioRuntimePolicy>> = OnceLock::new();

pub(crate) fn set_audio_runtime_policy(policy: AudioRuntimePolicy) {
    let slot = AUDIO_RUNTIME_POLICY.get_or_init(|| RwLock::new(AudioRuntimePolicy::default()));
    if let Ok(mut current) = slot.write() {
        *current = policy;
    }
}

pub(crate) fn audio_runtime_policy() -> AudioRuntimePolicy {
    AUDIO_RUNTIME_POLICY
        .get()
        .and_then(|slot| slot.read().ok().map(|policy| policy.clone()))
        .unwrap_or_default()
}

pub(crate) fn policy_from_config(config: &AppConfig) -> AudioRuntimePolicy {
    AudioRuntimePolicy {
        smart_audio: config.smart_audio.clone(),
        transition: config.transition.clone(),
    }
}
