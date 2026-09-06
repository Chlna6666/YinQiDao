use gpui::Context;

use crate::{
    audio::{EqPreset, PlayerCommand, SpatialPreset, clamp_eq, clamp_spatial},
    audio_policy::{policy_from_config, set_audio_runtime_policy},
    model::{SpatialSettings, TransitionMode},
    ui::MusicApp,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum SpatialControl {
    Width,
    Depth,
    Distance,
    Mix,
    Crossfeed,
    Room,
    Immersive3d,
    MotionSpeed,
    MotionRadius,
    MotionIntensity,
}

impl MusicApp {
    fn publish_audio_policy(&self) {
        set_audio_runtime_policy(policy_from_config(&self.config));
    }

    fn persist_audio_preferences(&mut self) {
        self.publish_audio_policy();
        self.save_config();
    }

    fn disable_smart_audio_for_manual_tuning(&mut self) {
        if self.config.smart_audio.enabled {
            self.config.smart_audio.enabled = false;
            self.send(PlayerCommand::SetSmartAudio(self.config.smart_audio.clone()));
        }
    }

    pub(crate) fn toggle_smart_audio(&mut self, cx: &mut Context<Self>) {
        self.config.smart_audio.enabled = !self.config.smart_audio.enabled;
        self.send(PlayerCommand::SetSmartAudio(self.config.smart_audio.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn adjust_smart_audio_intensity(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.config.smart_audio.intensity =
            (self.config.smart_audio.intensity + delta).clamp(0.0, 1.0);
        self.send(PlayerCommand::SetSmartAudio(self.config.smart_audio.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn set_manual_eq_preset(&mut self, preset: EqPreset, cx: &mut Context<Self>) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.eq = preset.settings();
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.persist_audio_preferences();
        self.status = format!("EQ 已切换为 {preset:?}");
        cx.notify();
    }

    pub(crate) fn toggle_manual_eq(&mut self, cx: &mut Context<Self>) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.eq.enabled = !self.config.eq.enabled;
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn adjust_manual_eq_band(
        &mut self,
        index: usize,
        delta_db: f32,
        cx: &mut Context<Self>,
    ) {
        self.disable_smart_audio_for_manual_tuning();
        if let Some(band) = self.config.eq.bands_db.get_mut(index) {
            *band += delta_db;
            self.config.eq.enabled = true;
            self.config.eq = clamp_eq(self.config.eq.clone());
            self.send(PlayerCommand::SetEq(self.config.eq.clone()));
            self.persist_audio_preferences();
        }
        cx.notify();
    }

    pub(crate) fn set_manual_eq_band_ratio(
        &mut self,
        index: usize,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        let db = -12.0 + ratio.clamp(0.0, 1.0) * 24.0;
        let quantized = (db * 2.0).round() * 0.5;
        let current = self.config.eq.bands_db.get(index).copied().unwrap_or_default();
        self.adjust_manual_eq_band(index, quantized - current, cx);
    }

    pub(crate) fn adjust_manual_eq_preamp(&mut self, delta_db: f32, cx: &mut Context<Self>) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.eq.preamp_db += delta_db;
        self.config.eq.enabled = true;
        self.config.eq = clamp_eq(self.config.eq.clone());
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn set_manual_spatial_preset(
        &mut self,
        preset: SpatialPreset,
        cx: &mut Context<Self>,
    ) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.spatial = preset.settings();
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.persist_audio_preferences();
        self.status = format!("空间音频已切换为 {preset:?}");
        cx.notify();
    }

    pub(crate) fn toggle_manual_spatial(&mut self, cx: &mut Context<Self>) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.spatial.enabled = !self.config.spatial.enabled;
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn adjust_manual_spatial(
        &mut self,
        control: SpatialControl,
        delta: f32,
        cx: &mut Context<Self>,
    ) {
        self.disable_smart_audio_for_manual_tuning();
        match control {
            SpatialControl::Width => self.config.spatial.width += delta,
            SpatialControl::Depth => self.config.spatial.depth += delta,
            SpatialControl::Distance => self.config.spatial.distance += delta,
            SpatialControl::Mix => self.config.spatial.mix += delta,
            SpatialControl::Crossfeed => self.config.spatial.crossfeed += delta,
            SpatialControl::Room => self.config.spatial.room_size += delta,
            SpatialControl::Immersive3d => self.config.spatial.immersive_3d += delta,
            SpatialControl::MotionSpeed => self.config.spatial.motion_speed_hz += delta,
            SpatialControl::MotionRadius => self.config.spatial.motion_radius += delta,
            SpatialControl::MotionIntensity => self.config.spatial.motion_intensity += delta,
        }
        self.config.spatial.enabled = true;
        self.config.spatial = clamp_spatial(self.config.spatial.clone());
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn toggle_spatial_direction(&mut self, cx: &mut Context<Self>) {
        self.disable_smart_audio_for_manual_tuning();
        self.config.spatial.clockwise = !self.config.spatial.clockwise;
        self.config.spatial.enabled = true;
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn toggle_track_transition(&mut self, cx: &mut Context<Self>) {
        self.config.transition.enabled = !self.config.transition.enabled;
        self.send(PlayerCommand::SetTransition(self.config.transition.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn set_track_transition_mode(
        &mut self,
        mode: TransitionMode,
        cx: &mut Context<Self>,
    ) {
        self.config.transition.mode = mode;
        self.config.transition.enabled = true;
        self.send(PlayerCommand::SetTransition(self.config.transition.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn adjust_track_transition_duration(
        &mut self,
        delta_ms: i64,
        cx: &mut Context<Self>,
    ) {
        let next = (self.config.transition.duration_ms as i64 + delta_ms).clamp(250, 12_000);
        self.config.transition.duration_ms = next as u64;
        self.send(PlayerCommand::SetTransition(self.config.transition.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn toggle_transition_smart_cue(&mut self, cx: &mut Context<Self>) {
        self.config.transition.smart_cue = !self.config.transition.smart_cue;
        self.send(PlayerCommand::SetTransition(self.config.transition.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn adjust_transition_max_cue(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        let next = (self.config.transition.max_smart_cue_ms as i64 + delta_ms).clamp(0, 8_000);
        self.config.transition.max_smart_cue_ms = next as u64;
        self.send(PlayerCommand::SetTransition(self.config.transition.clone()));
        self.persist_audio_preferences();
        cx.notify();
    }

    pub(crate) fn effective_spatial_settings(&self) -> SpatialSettings {
        self.config.spatial.clone()
    }
}
