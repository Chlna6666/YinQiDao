use std::time::Duration;

use gpui::{Context, IntoElement, div, prelude::*};

use crate::gpu::AppleFluidView;

use super::{player_stage, shell::MusicApp};

pub(super) use super::mini_player_lyrics::mini_player;
pub(super) use super::player_stage::{NowPlaying, PlaybackProgress, PlaybackTime};

const STAGE_CHROME_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

/// Render the immersive stage and reserve the first explicit click for waking hidden chrome.
///
/// The original stage listens on its root, but a zero-opacity dock and the scrollable lyrics
/// viewport can remain the frontmost hit target while clean mode is active. A dedicated surface
/// above the stage makes wake-up independent of event bubbling and disappears immediately after
/// `wake_stage_controls_immediately` restores the controls.
pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    fluid_background: gpui::Entity<AppleFluidView>,
) -> gpui::AnyElement {
    let idle_hidden = app.stage_open
        && app.stage_last_user_activity.elapsed() >= STAGE_CHROME_IDLE_TIMEOUT
        && !app.seeking
        && !app.volume_dragging
        && !app.stage_controls_hovered;
    let needs_wake_surface = app.stage_suppress_wake_until.is_some() || idle_hidden;
    let stage = player_stage::render(app, cx, fluid_background);

    let mut root = div().size_full().relative().child(stage);
    if needs_wake_surface {
        root = root.child(
            div()
                .id("stage-explicit-wake-surface")
                .absolute()
                .inset_0()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.wake_stage_controls_immediately(cx);
                    }),
                ),
        );
    }

    root.into_any_element()
}
