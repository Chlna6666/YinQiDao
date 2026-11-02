use std::time::Duration;

use super::{
    shell::MusicApp,
    theme::{self, SampledMotion},
};

/// Shared timeline for stage metadata, titlebar and transport controls.
pub(super) fn fade_motion(visible: bool) -> SampledMotion {
    SampledMotion::new(
        theme::DURATION_EMPHASIZED,
        theme::ease_in_out_cubic,
        visible,
    )
}

/// Retarget once per semantic visibility change and publish a monotonically increasing epoch.
/// Epoch comparisons also catch a fast hide-then-show that returns to the same boolean target.
pub(super) fn retarget_motion(motion: &mut SampledMotion, epoch: &mut u64, visible: bool) {
    if motion.set_target(visible) {
        *epoch = epoch.wrapping_add(1);
    }
}

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

/// Pointer motion may keep *visible* chrome active, but must never reveal
/// manually hidden or timeout-hidden chrome. Revealing needs a deliberate click.
#[inline]
pub(super) fn pointer_motion_may_refresh_activity(app: &MusicApp) -> bool {
    app.stage_open && !needs_wake_surface(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_chrome_reverse_advances_shared_epoch() {
        let mut motion = fade_motion(false);
        let mut epoch = 0;
        retarget_motion(&mut motion, &mut epoch, false);
        assert_eq!(epoch, 0);
        retarget_motion(&mut motion, &mut epoch, true);
        assert_eq!(epoch, 1);
        retarget_motion(&mut motion, &mut epoch, false);
        assert_eq!(epoch, 2);
        retarget_motion(&mut motion, &mut epoch, true);
        assert_eq!(epoch, 3);
        assert_eq!(motion.target_value(), 1.0);
    }
}
