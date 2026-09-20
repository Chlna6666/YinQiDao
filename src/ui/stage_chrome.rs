use std::time::Duration;

use super::shell::MusicApp;

pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

#[inline]
fn idle_hidden(app: &MusicApp) -> bool {
    app.stage_open
        && app.stage_last_user_activity.elapsed() >= IDLE_TIMEOUT
        && !app.seeking
        && !app.volume_dragging
        && !app.stage_controls_hovered
}

/// Whether the retained stage chrome should target full visibility.
#[inline]
pub(super) fn target_visible(app: &MusicApp) -> bool {
    // During close, the whole Stage surface already owns the fade/scale transition. Keep chrome at
    // its current visual strength until that surface exits; otherwise dock/titlebar opacity is
    // multiplied by the parent fade and appears to disappear a frame group before the background.
    (app.stage_open || app.stage_animating)
        && app.stage_suppress_wake_until.is_none()
        && !idle_hidden(app)
}

/// Whether the stage should reserve the next explicit interaction for waking chrome.
#[inline]
pub(super) fn needs_wake_surface(app: &MusicApp) -> bool {
    app.stage_suppress_wake_until.is_some() || idle_hidden(app)
}
