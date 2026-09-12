use gpui::{Context, Entity, IntoElement, div};

use super::{
    player_stage::{self, PlaybackProgress, PlaybackTime},
    shell::MusicApp,
};

/// Active mini-player facade. The desktop lyrics control now lives in the original right-side
/// player slot, so this facade must not add a second absolute-positioned button/hitbox.
pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> gpui::AnyElement {
    // Once the immersive Stage is fully open, this player is completely covered and its owning
    // main-page content is already replaced by an empty retained node. Avoid rebuilding cover,
    // shuffle/repeat/volume controls and their listeners on every root transport poll. The moment a
    // close transition starts `stage_open` becomes false, so the underlying mini-player is restored
    // before the Stage translates away.
    if app.stage_open && !app.stage_animating && app.stage_progress >= 0.999 {
        return div().id("mini-player-stage-covered").into_any_element();
    }

    player_stage::mini_player(app, cx, playback_progress, playback_time).into_any_element()
}
