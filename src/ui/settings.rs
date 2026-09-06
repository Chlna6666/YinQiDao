use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb};

use crate::{
    desktop_lyrics::LyricsColorTarget,
    settings::DesktopLyricsAlignment,
};

use super::{
    settings_content,
    shell::MusicApp,
    theme::{
        self, ACCENT_RED, BORDER_CARD, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY,
        TEXT_TERTIARY, TEXT_WHITE, press_transition,
    },
};

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    div()
        .size_full()
        .flex()
        .bg(theme::BG_CANVAS)
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .child(settings_content::render(app, cx)),
        )
        .child(lyrics_and_shortcuts_panel(app, cx))
        .into_any_element()
}

fn lyrics_and_shortcuts_panel(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    div()
        .id("settings-lyrics-shortcuts-panel")
        .w(px(340.0))
        .h_full()
        .flex_none()
        .overflow_y_scroll()
        .border_l_1()
        .border_color(BORDER_HAIRLINE)
        .bg(rgb(0xfa_fb_fd))
        .p_5()
        .flex()
        .flex_col()
        .gap_5()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child("桌面歌词与全局快捷键"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child("窗口、样式、位置和快捷键开关均持久化到 config.toml"),
                ),
        )
        .child(desktop_lyrics_section(app, cx))
        .child(global_shortcuts_section(app, cx))
}

fn desktop_lyrics_section(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let config = &app.config.desktop_lyrics;
    section_card(
        "桌面歌词",
        "独立透明窗口与正常播放界面共享同一份歌词时间轴",
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(toggle_row(
                "显示桌面歌词",
                "独立悬浮窗口",
                "desktop-lyrics-visible",
                config.visible,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_visible(cx)),
            ))
            .child(toggle_row(
                "正常播放器显示歌词",
                "在底部播放器上方显示当前句",
                "desktop-lyrics-in-player",
                config.show_in_player,
                cx.listener(|this, _, _, cx| this.toggle_player_lyrics(cx)),
            ))
            .child(toggle_row(
                "锁定歌词窗口",
                "锁定后禁止拖动位置",
                "desktop-lyrics-lock",
                config.locked,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_lock(cx)),
            ))
            .child(toggle_row(
                "始终置顶",
                "以浮动窗口保持在普通窗口上方",
                "desktop-lyrics-topmost",
                config.always_on_top,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_topmost(cx)),
            ))
            .child(toggle_row(
                "显示翻译",
                "有翻译时显示双语歌词",
                "desktop-lyrics-translation",
                config.show_translation,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_translation(cx)),
            ))
            .child(toggle_row(
                "显示下一句",
                "双行模式显示当前句和下一句",
                "desktop-lyrics-two-line",
                config.two_line,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_two_line(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(alignment_row(app, cx))
            .child(step_row(
                "字号",
                format!("{:.0} px", config.font_size),
                "desktop-lyrics-font-dec",
                "desktop-lyrics-font-inc",
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_font(-2.0, cx)),
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_font(2.0, cx)),
            ))
            .child(step_row(
                "背景透明度",
                format!("{}%", (config.background_opacity * 100.0).round() as u32),
                "desktop-lyrics-bg-dec",
                "desktop-lyrics-bg-inc",
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_background(-0.05, cx)),
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_background(0.05, cx)),
            ))
            .child(color_row(
                "当前歌词颜色",
                config.active_color,
                LyricsColorTarget::Active,
                cx,
            ))
            .child(color_row(
                "下一句颜色",
                config.inactive_color,
                LyricsColorTarget::Inactive,
                cx,
            ))
            .child(color_row(
                "翻译颜色",
                config.translation_color,
                LyricsColorTarget::Translation,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(action_button(
                        "desktop-lyrics-reset-bounds",
                        "重置歌词窗口位置",
                        cx.listener(|this, _, _, cx| this.reset_desktop_lyrics_bounds(cx)),
                    )),
            ),
    )
}

fn global_shortcuts_section(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let enabled = app.config.lyrics_shortcuts.enabled;
    section_card(
        "系统级全局快捷键",
        "默认关闭。开启后即使音栖岛不在前台，也可控制播放、主窗口和桌面歌词",
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(toggle_row(
                "启用全局快捷键",
                if enabled { "已注册到系统" } else { "默认不注册任何系统热键" },
                "global-shortcuts-enabled",
                enabled,
                cx.listener(|this, _, _, cx| this.toggle_global_shortcuts(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(group_label("播放控制"))
            .child(shortcut_row("播放 / 暂停", "Ctrl + Alt + Space"))
            .child(shortcut_row("上一首", "Ctrl + Alt + ←"))
            .child(shortcut_row("下一首", "Ctrl + Alt + →"))
            .child(shortcut_row("快退 10 秒", "Ctrl + Alt + Shift + ←"))
            .child(shortcut_row("快进 10 秒", "Ctrl + Alt + Shift + →"))
            .child(shortcut_row("音量 -5%", "Ctrl + Alt + -"))
            .child(shortcut_row("音量 +5%", "Ctrl + Alt + ="))
            .child(shortcut_row("静音 / 恢复", "Ctrl + Alt + M"))
            .child(shortcut_row("随机播放", "Ctrl + Alt + S"))
            .child(shortcut_row("循环模式", "Ctrl + Alt + R"))
            .child(group_label("窗口控制"))
            .child(shortcut_row("显示主窗口", "Ctrl + Alt + Y"))
            .child(shortcut_row("切换沉浸播放", "Ctrl + Alt + Enter"))
            .child(group_label("歌词控制"))
            .child(shortcut_row("显示 / 隐藏桌面歌词", "Ctrl + Alt + L"))
            .child(shortcut_row("锁定 / 解锁歌词", "Ctrl + Alt + K"))
            .child(shortcut_row("显示 / 隐藏翻译", "Ctrl + Alt + T"))
            .child(shortcut_row("增大歌词字号", "Ctrl + Alt + ↑"))
            .child(shortcut_row("减小歌词字号", "Ctrl + Alt + ↓")),
    )
}

fn section_card(title: &str, subtitle: &str, content: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .p_4()
        .rounded_2xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(subtitle.to_owned()),
                ),
        )
        .child(content)
}

fn alignment_row(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut choices = div().flex().gap_1p5();
    for (alignment, label) in [
        (DesktopLyricsAlignment::Left, "左"),
        (DesktopLyricsAlignment::Center, "中"),
        (DesktopLyricsAlignment::Right, "右"),
    ] {
        let active = app.config.desktop_lyrics.alignment == alignment;
        choices = choices.child(
            div()
                .id(SharedString::from(format!("desktop-lyrics-align-{alignment:?}")))
                .px_2p5()
                .py_1()
                .rounded_full()
                .cursor_pointer()
                .bg(if active { ACCENT_RED.into() } else { theme::bg_hover() })
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if active { TEXT_WHITE } else { TEXT_PRIMARY })
                .transition(press_transition())
                .active(|style| style.scale(0.96))
                .child(label)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.set_desktop_lyrics_alignment(alignment, cx)
                    }),
                ),
        );
    }
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label_block("文字对齐", "桌面歌词布局"))
        .child(choices)
}

fn color_row(
    title: &'static str,
    current: u32,
    target: LyricsColorTarget,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let mut palette = div().flex().items_center().gap_1p5();
    for color in [0xff_3b_5c, 0xff_95_00, 0x34_c7_59, 0x00_7a_ff, 0xaf_52_de, 0xf2_f2_f7] {
        let active = current == color;
        palette = palette.child(
            div()
                .id(SharedString::from(format!("lyrics-color-{target:?}-{color:06x}")))
                .size(px(22.0))
                .rounded_full()
                .cursor_pointer()
                .bg(rgb(color))
                .border_1()
                .border_color(if active { ACCENT_RED.into() } else { BORDER_CARD })
                .transition(press_transition())
                .active(|style| style.scale(0.90))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.set_desktop_lyrics_color(target, color, cx)),
                ),
        );
    }
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label_block(title, format!("#{:06X}", current & 0x00ff_ffff)))
        .child(palette)
}

fn shortcut_row(label: &'static str, shortcut: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_xs()
                .text_color(TEXT_SECONDARY)
                .child(label),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme::bg_pill())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_PRIMARY)
                .child(shortcut),
        )
}

fn group_label(label: &'static str) -> impl IntoElement {
    div()
        .mt_1()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(TEXT_TERTIARY)
        .child(label)
}

fn toggle_row(
    title: &'static str,
    subtitle: impl Into<SharedString>,
    id: &'static str,
    active: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label_block(title, subtitle))
        .child(toggle_switch(id, active, handler))
}

fn label_block(title: &'static str, subtitle: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(TEXT_PRIMARY)
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(subtitle.into()),
        )
}

fn toggle_switch(
    id: &'static str,
    active: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(44.0))
        .h(px(25.0))
        .p(px(3.0))
        .flex_none()
        .rounded_full()
        .cursor_pointer()
        .bg(if active { ACCENT_RED } else { rgb(0xd8_db_e2) })
        .flex()
        .when(active, |style| style.justify_end())
        .when(!active, |style| style.justify_start())
        .child(div().size(px(19.0)).rounded_full().bg(rgb(0xff_ff_ff)).shadow_sm())
        .transition(press_transition())
        .active(|style| style.scale(0.96))
        .on_mouse_down(gpui::MouseButton::Left, handler)
}

fn step_row(
    title: &'static str,
    value: String,
    dec_id: &'static str,
    inc_id: &'static str,
    on_dec: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_inc: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(label_block(title, value))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(step_button(dec_id, "−", on_dec))
                .child(step_button(inc_id, "+", on_inc)),
        )
}

fn step_button(
    id: &'static str,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(25.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::bg_hover())
        .border_1()
        .border_color(BORDER_CARD)
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_PRIMARY)
        .transition(press_transition())
        .active(|style| style.scale(0.92))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, handler)
}

fn action_button(
    id: &'static str,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::bg_hover())
        .border_1()
        .border_color(BORDER_CARD)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_PRIMARY)
        .transition(press_transition())
        .active(|style| style.scale(0.96))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, handler)
}
