use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

use super::{settings_impl, shell::MusicApp};

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let settings_page = settings_impl::render(app, cx);
    let enabled = crate::audio::audio_debug_enabled();

    let mut toggle = div()
        .id("settings-audio-laboratory-toggle")
        .w(px(46.0))
        .h(px(26.0))
        .p(px(3.0))
        .rounded_full()
        .flex()
        .items_center()
        .cursor_pointer()
        .bg(if enabled {
            rgb(0xff_3b_30)
        } else {
            rgb(0xd7_da_df)
        });
    toggle = if enabled {
        toggle.justify_end()
    } else {
        toggle.justify_start()
    };
    toggle = toggle
        .child(
            div()
                .size(px(20.0))
                .rounded_full()
                .bg(rgb(0xff_ff_ff)),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if crate::audio::audio_debug_enabled() {
                    crate::audio_debug_window::shutdown(cx);
                    this.status = "Audio Laboratory 已关闭".into();
                } else if let Err(error) = crate::audio_debug_window::open(cx) {
                    // `open` enables the analyzer before creating the native window. Roll that
                    // state back when window creation fails so playback does not keep paying the
                    // analyzer cost without a visible laboratory.
                    crate::audio::set_audio_debug_enabled(false);
                    this.status = format!("打开 Audio Laboratory 失败：{error:#}");
                    tracing::warn!(error = %error, "Audio Laboratory 窗口打开失败");
                } else {
                    this.status = "Audio Laboratory 已打开".into();
                }
                cx.notify();
            }),
        );

    div()
        .size_full()
        .flex()
        .flex_col()
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .child(settings_page),
        )
        .child(div().h(px(1.0)).bg(rgb(0xe3_e5_e8)))
        .child(
            div()
                .px_8()
                .py_3()
                .bg(rgb(0xfb_fc_fd))
                .flex()
                .items_center()
                .justify_between()
                .gap_6()
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(rgb(0x18_1a_1f))
                                .child("Audio Laboratory · 专业音频分析"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x72_77_80))
                                .child(
                                    "显示独立分析窗口，对比 SOURCE / POST-EQ / POST-SPATIAL；关闭时停止实时分析，不影响正常播放热路径",
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(if enabled {
                                    rgb(0x34_7a_50)
                                } else {
                                    rgb(0x8b_90_98)
                                })
                                .child(if enabled { "已显示" } else { "已关闭" }),
                        )
                        .child(toggle),
                ),
        )
        .into_any_element()
}
