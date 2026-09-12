use gpui::{Context, Entity, IntoElement, div};

use super::{
    app_runtime_events,
    mini_player_view,
    player_stage::{PlaybackProgress, PlaybackTime},
    shell::MusicApp,
};

/// Active mini-player facade. Runtime event binding is initialized before the Stage-cover short
/// circuit so audio structural events keep flowing even while the mini-player is not materialized.
pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> gpui::AnyElement {
    app_runtime_events::ensure_audio_runtime(app, cx);

    if app.stage_open && !app.stage_animating && app.stage_progress >= 0.999 {
        return div().into_any_element();
    }

    mini_player_view::view(app, cx, playback_progress, playback_time).into_any_element()
}
