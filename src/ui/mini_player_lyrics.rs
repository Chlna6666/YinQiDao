use gpui::{Context, Entity, IntoElement, div, hsla, prelude::*, px, rgb};

use super::{
    player_legacy::{self, PlaybackProgress, PlaybackTime},
    shell::MusicApp,
    theme,
};

/// Active mini-player facade. The normal player no longer embeds lyric text; the only integration
/// is a NetEase-style `词` button that controls the independent desktop lyric window.
pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> impl IntoElement {
    let base = player_legacy::mini_player(app, cx, playback_progress, playback_time);
    let visible = app.config.desktop_lyrics.visible;

    div()
        .w_full()
        .relative()
        .child(base)
        // The legacy mini player already reserves this slot for its captions/stage button. Draw
        // the desktop-lyrics control above that slot so the active facade owns the behavior without
        // duplicating the complete player layout.
        .child(
            div()
                .id("mini-desktop-lyrics-btn")
                .absolute()
                .top(px(23.0))
                .right(px(172.0))
                .size(px(30.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(if visible {
                    theme::accent_red_muted()
                } else {
                    hsla(0.0, 0.0, 0.0, 0.0)
                })
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if visible {
                    rgb(0xff_3b_5c)
                } else {
                    rgb(0x78_7f_8c)
                })
                .hover(|style| style.bg(theme::bg_hover()))
                .active(|style| style.scale(0.92))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_desktop_lyrics_visible(cx);
                    }),
                )
                .child("词"),
        )
}
