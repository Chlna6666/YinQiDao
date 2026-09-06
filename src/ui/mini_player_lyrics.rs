use gpui::{Context, Entity, IntoElement, div, prelude::*, px, rgb};

use super::{
    player_legacy::{self, PlaybackProgress, PlaybackTime},
    shell::MusicApp,
    theme,
};

pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> impl IntoElement {
    let base = player_legacy::mini_player(app, cx, playback_progress, playback_time);
    let config = &app.config.desktop_lyrics;
    let display = config
        .show_in_player
        .then(|| app.desktop_lyrics_display())
        .flatten();

    let mut root = div().w_full().flex().flex_col();
    if let Some(display) = display {
        let mut text = div()
            .min_w(px(0.0))
            .max_w(px(820.0))
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(config.active_color & 0x00ff_ffff))
            .truncate()
            .child(display.current);
        if config.show_translation
            && let Some(translation) = display.translation
        {
            text = text.child(
                div()
                    .ml_2()
                    .text_xs()
                    .font_weight(gpui::FontWeight::NORMAL)
                    .text_color(rgb(config.translation_color & 0x00ff_ffff))
                    .child(format!(" · {translation}")),
            );
        }
        root = root.child(
            div()
                .id("normal-player-current-lyric")
                .w_full()
                .h(px(30.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .px_6()
                .bg(theme::BG_CANVAS)
                .border_t_1()
                .border_color(theme::BORDER_HAIRLINE)
                .child(text),
        );
    }
    root.child(base)
}
