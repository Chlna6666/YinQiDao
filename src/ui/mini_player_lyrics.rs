use gpui::{Context, Entity, IntoElement, div};

use super::{
    mini_player_view,
    player_stage::{PlaybackProgress, PlaybackTime},
    shell::MusicApp,
};

/// Active mini-player facade. Once Stage fully covers the main surface, keep even the retained
/// mini-player Entity out of layout/paint. Closing Stage restores it before the sampled drawer moves.
pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> gpui::AnyElement {
    if app.stage_open && !app.stage_animating && app.stage_progress >= 0.999 {
        return div().into_any_element();
    }

    mini_player_view::view(app, cx, playback_progress, playback_time).into_any_element()
}
