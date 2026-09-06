use gpui::{Context, Entity, IntoElement};

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
) -> impl IntoElement {
    player_stage::mini_player(app, cx, playback_progress, playback_time)
}
