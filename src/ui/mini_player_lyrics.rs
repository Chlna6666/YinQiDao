use gpui::{Context, Entity, IntoElement, div, hsla, prelude::*, px};

use super::{
    app_runtime_events, mini_player_view,
    player_stage::{PlaybackProgress, PlaybackTime},
    shell::MusicApp,
    theme,
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

    let mini_player = mini_player_view::view(app, cx, playback_progress, playback_time);
    div()
        .relative()
        .child(mini_player)
        .child(
            div()
                .id("mini-plugin-command-palette")
                .absolute()
                .top(px(21.0))
                .right(px(224.0))
                .w(px(48.0))
                .h(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(hsla(0.0, 0.0, 0.0, 0.0))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_TERTIARY)
                .hover(|style| style.bg(theme::bg_hover()).text_color(theme::TEXT_PRIMARY))
                .transition(theme::press_transition())
                .active(|style| style.scale(0.94))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|app, _, _, cx| super::plugin_command_palette::open(app, cx)),
                )
                .child("命令"),
        )
        .into_any_element()
}
