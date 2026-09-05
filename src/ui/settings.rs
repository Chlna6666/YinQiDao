use gpui::{Context, IntoElement, SharedString, div, hsla, prelude::*, px, rgb};
use lucide_gpui::icons as lucide_icons;

use crate::audio::EqPreset;

use super::{
    shell::MusicApp,
    theme::{
        self, ACCENT_RED, BORDER_CARD, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY,
        TEXT_TERTIARY, TEXT_WHITE, press_transition, themed_icon,
    },
};

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    div()
        .id("settings-scroll")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .p_8()
        .gap_8()
        .child(header())
        .child(audio_device_group(app, cx))
        .child(eq_group(app, cx))
        .child(spatial_group(app, cx))
        .child(directories_group(app, cx))
        .child(online_enrichment_group(app, cx))
        .child(appearance_group(app, cx))
        .child(log_group(app, cx))
        .into_any_element()
}

fn header() -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_2xl()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(TEXT_PRIMARY)
                .child("偏好设置"),
        )
        .child(
            div()
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .child("所有音频引擎与硬件参数均实时作用于后续采样，并自动持久化保存"),
        )
}

// ============================================================================
// 分组 1: 音频输出与音量
// ============================================================================

fn audio_device_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut devices_list = div().flex().flex_col().gap_1p5();
    if app.output_devices.is_empty() {
        devices_list = devices_list.child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("未检测到可用的音频输出硬件"),
        );
    } else {
        for device in &app.output_devices {
            let name = device.name.clone();
            let device_id = device.id.clone();
            let active = app.config.output_device.as_deref() == Some(device.id.as_str());

            devices_list = devices_list.child(
                div()
                    .id(SharedString::from(format!("output-dev-{device_id}")))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2p5()
                    .rounded_xl()
                    .cursor_pointer()
                    .bg(if active {
                        theme::accent_red_muted()
                    } else {
                        theme::bg_hover()
                    })
                    .hover(|s| s.opacity(0.85))
                    .transition(press_transition())
                    .active(|s| s.scale(0.99))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(themed_icon(
                                lucide_icons::icon_speaker(),
                                16.0,
                                if active {
                                    ACCENT_RED.into()
                                } else {
                                    hsla(220.0, 0.08, 0.50, 1.0)
                                },
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(if active {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::NORMAL
                                            })
                                            .text_color(TEXT_PRIMARY)
                                            .child(name),
                                    )
                                    .child(div().text_xs().text_color(TEXT_TERTIARY).child(
                                        format!(
                                            "{} Hz · {} 声道",
                                            device.sample_rate, device.channels
                                        ),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child_if(active, || {
                                themed_icon(lucide_icons::icon_check(), 14.0, ACCENT_RED.into())
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(if active { ACCENT_RED } else { TEXT_SECONDARY })
                                    .child(if active { "使用中" } else { "切换" }),
                            ),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.set_output_device(device_id.clone(), cx);
                        }),
                    ),
            );
        }
    }

    apple_settings_card(
        "音频硬件与输出",
        "选择默认音频回放端点与监听设备",
        div()
            .flex()
            .flex_col()
            .gap_4()
            // 音量推子行
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(TEXT_PRIMARY)
                            .child("主输出音量"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(apple_round_step_btn(
                                "vol-down",
                                lucide_icons::icon_volume_1(),
                                cx.listener(|this, _, _, cx| this.adjust_volume(-0.05, cx)),
                            ))
                            .child(
                                div()
                                    .w(px(60.0))
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_center()
                                    .text_color(TEXT_PRIMARY)
                                    .child(format!(
                                        "{}%",
                                        (app.config.volume * 100.0).round() as u32
                                    )),
                            )
                            .child(apple_round_step_btn(
                                "vol-up",
                                lucide_icons::icon_volume_2(),
                                cx.listener(|this, _, _, cx| this.adjust_volume(0.05, cx)),
                            )),
                    ),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(devices_list),
    )
}

// ============================================================================
// 分组 2: 10 段专业图形均衡器
// ============================================================================

fn eq_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut presets = div().flex().items_center().gap_2();
    for (index, preset) in EqPreset::ALL.into_iter().enumerate() {
        let is_current = preset.settings().bands_db == app.config.eq.bands_db;
        presets = presets.child(
            div()
                .id(SharedString::from(format!("eq-preset-{index}")))
                .px_3()
                .py_1p5()
                .rounded_full()
                .cursor_pointer()
                .bg(if is_current {
                    ACCENT_RED.into()
                } else {
                    theme::bg_hover()
                })
                .text_xs()
                .font_weight(if is_current {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if is_current { TEXT_WHITE } else { TEXT_PRIMARY })
                .hover(|s| s.opacity(0.85))
                .transition(press_transition())
                .active(|s| s.scale(0.96))
                .child(preset_name(preset))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.apply_eq(preset, cx)),
                ),
        );
    }

    let frequencies = [
        "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
    ];
    let mut eq_bands = div().flex().items_end().justify_between().px_2().gap_2();
    for (index, frequency) in frequencies.into_iter().enumerate() {
        eq_bands = eq_bands.child(eq_band_slider(
            frequency,
            app.config.eq.bands_db[index],
            index,
            cx,
        ));
    }

    apple_settings_card(
        "专业 10 段图形均衡器",
        "高精度 IIR 滤波器阵列，为不同流派量身微调频响",
        div()
            .flex()
            .flex_col()
            .gap_5()
            // 顶部开关与预设
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(presets)
                    .child(apple_toggle_switch(
                        "eq-switch",
                        app.config.eq.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_eq(cx)),
                    )),
            )
            // 频段推子组
            .child(
                div()
                    .p_4()
                    .rounded_2xl()
                    .bg(rgb(0xf8_f9_fb))
                    .border_1()
                    .border_color(BORDER_CARD)
                    .child(eq_bands),
            ),
    )
}

fn eq_band_slider(
    freq: &str,
    val_db: f32,
    index: usize,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let magnitude = (val_db.abs() / 12.0).clamp(0.0, 1.0);

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(if val_db != 0.0 {
                    ACCENT_RED
                } else {
                    TEXT_TERTIARY
                })
                .child(format!("{val_db:+.1}")),
        )
        // 垂直推杆柱
        .child(
            div()
                .h(px(64.0))
                .w(px(8.0))
                .flex()
                .flex_col()
                .justify_end()
                .rounded_full()
                .bg(rgb(0xe4_e6_ec))
                .child(
                    div()
                        .h(px(6.0 + magnitude * 54.0))
                        .w_full()
                        .rounded_full()
                        .bg(if val_db >= 0.0 {
                            ACCENT_RED
                        } else {
                            rgb(0x00_7a_ff)
                        }),
                ),
        )
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_SECONDARY)
                .child(freq.to_owned()),
        )
        // 微调按钮
        .child(
            div()
                .flex()
                .gap_1()
                .child(apple_micro_btn(
                    &format!("eq-dec-{index}"),
                    lucide_icons::icon_minus(),
                    cx.listener(move |this, _, _, cx| this.adjust_eq(index, -1.0, cx)),
                ))
                .child(apple_micro_btn(
                    &format!("eq-inc-{index}"),
                    lucide_icons::icon_plus(),
                    cx.listener(move |this, _, _, cx| this.adjust_eq(index, 1.0, cx)),
                )),
        )
}

// ============================================================================
// 分组 3: 立体声空间音频
// ============================================================================

fn spatial_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    apple_settings_card(
        "空间音频与立体声声场增强",
        "模拟宽阔声场深度与临场混音，沉浸感倍增",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(TEXT_PRIMARY)
                            .child("启用实时空间音频引擎"),
                    )
                    .child(apple_toggle_switch(
                        "spatial-switch",
                        app.config.spatial.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_spatial(cx)),
                    )),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(spatial_slider_row(
                "声场宽度 (Width)",
                app.config.spatial.width,
                0,
                cx,
            ))
            .child(spatial_slider_row(
                "声场深度 (Depth)",
                app.config.spatial.depth,
                1,
                cx,
            ))
            .child(spatial_slider_row(
                "听感距离 (Distance)",
                app.config.spatial.distance,
                2,
                cx,
            ))
            .child(spatial_slider_row(
                "空间混合比 (Mix)",
                app.config.spatial.mix,
                3,
                cx,
            )),
    )
}

fn spatial_slider_row(
    label: &str,
    val: f32,
    index: u8,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_1()
        .child(
            div()
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .child(label.to_owned()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(apple_round_step_btn(
                    &format!("spatial-dec-{index}"),
                    lucide_icons::icon_minus(),
                    cx.listener(move |this, _, _, cx| this.adjust_spatial(index, -0.1, cx)),
                ))
                .child(
                    div()
                        .w(px(50.0))
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_center()
                        .text_color(TEXT_PRIMARY)
                        .child(format!("{}%", (val * 100.0).round() as u32)),
                )
                .child(apple_round_step_btn(
                    &format!("spatial-inc-{index}"),
                    lucide_icons::icon_plus(),
                    cx.listener(move |this, _, _, cx| this.adjust_spatial(index, 0.1, cx)),
                )),
        )
}

// ============================================================================
// 分组 4: 音乐目录管理
// ============================================================================

fn directories_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut dirs_list = div().flex().flex_col().gap_2();
    if app.config.music_dirs.is_empty() {
        dirs_list = dirs_list.child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("尚未添加任何受监控的音乐目录"),
        );
    } else {
        for dir in &app.config.music_dirs {
            dirs_list = dirs_list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(rgb(0xf8_f9_fb))
                    .border_1()
                    .border_color(BORDER_CARD)
                    .child(themed_icon(
                        lucide_icons::icon_folder(),
                        16.0,
                        ACCENT_RED.into(),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(TEXT_PRIMARY)
                            .truncate()
                            .child(dir.to_string_lossy().to_string()),
                    ),
            );
        }
    }

    apple_settings_card(
        "本地音乐目录与监控",
        "添加音乐文件夹，音栖岛会自动监听并同步歌曲库变动",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dirs_list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("settings-add-folder-btn")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(theme::accent_red_muted())
                            .text_color(ACCENT_RED)
                            .hover(|s| s.bg(theme::accent_red_active()))
                            .transition(press_transition())
                            .active(|s| s.scale(0.96))
                            .child(themed_icon(
                                lucide_icons::icon_folder_plus(),
                                15.0,
                                ACCENT_RED.into(),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("添加文件夹"),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.choose_folder(cx)),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-rescan-btn")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(rgb(0xff_ff_ff))
                            .text_color(TEXT_PRIMARY)
                            .border_1()
                            .border_color(BORDER_CARD)
                            .hover(|s| s.bg(theme::bg_hover()))
                            .transition(press_transition())
                            .active(|s| s.scale(0.96))
                            .child(themed_icon(
                                lucide_icons::icon_refresh_cw(),
                                14.0,
                                hsla(220.0, 0.08, 0.50, 1.0),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("快速增量同步"),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.rescan_library(cx)),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-reset-index-btn")
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_4()
                            .py_2()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(rgb(0xff_ff_ff))
                            .text_color(rgb(0xd9_38_3a))
                            .border_1()
                            .border_color(BORDER_CARD)
                            .hover(|s| s.bg(hsla(0.0, 0.70, 0.95, 1.0)))
                            .transition(press_transition())
                            .active(|s| s.scale(0.96))
                            .child(themed_icon(
                                lucide_icons::icon_trash_2(),
                                14.0,
                                rgb(0xd9_38_3a).into(),
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("重置并重建索引"),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.reset_library_index(cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(BORDER_HAIRLINE)
                    .child(
                        div()
                            .text_xs()
                            .text_color(TEXT_TERTIARY)
                            .child(format!("当前已收录 {} 首曲目", app.tracks.len())),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(if app.scan_in_progress {
                                ACCENT_RED
                            } else {
                                TEXT_TERTIARY
                            })
                            .child(if app.scan_in_progress {
                                "正在增量同步中..."
                            } else {
                                "索引状态：已就绪 (增量模式)"
                            }),
                    ),
            ),
    )
}

// ============================================================================
// 分组 5: 在线元数据与智能歌词同步
// ============================================================================

fn online_enrichment_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    apple_settings_card(
        "在线元数据与歌词服务",
        "通过 MusicBrainz 与 AcoustID 识别封面与歌词",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("联网匹配曲目元数据与封面"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child("按网易云、QQ、Spotify、咪咕、千千、酷狗优先级补充元数据、年份与封面"),
                            ),
                    )
                    .child(apple_toggle_switch(
                        "online-meta-switch",
                        app.config.online_metadata,
                        cx.listener(|this, _, _, cx| this.toggle_online_metadata(cx)),
                    )),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("智能实时滚动歌词匹配"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child("优先读取本地 .lrc 与内嵌标签，再获取中英文同步歌词，最后回退 LRCLIB"),
                            ),
                    )
                    .child(apple_toggle_switch(
                        "online-lyrics-switch",
                        app.config.online_lyrics,
                        cx.listener(|this, _, _, cx| this.toggle_online_lyrics(cx)),
                    )),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            // AcoustID Key 配置卡片
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("AcoustID 音频指纹 API Key"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child(if let Some(key) = &app.config.acoustid_api_key {
                                        if app.acoustid_key_active {
                                            format!("当前输入：{key} (按 Enter 保存，Esc 取消)")
                                        } else {
                                            format!("已配置：{}", mask_key(key))
                                        }
                                    } else if app.acoustid_key_active {
                                        "请输入你的 API Key 后按 Enter 保存".to_string()
                                    } else {
                                        "未配置（优先基于标签检索，配置 Key 可开启纯音频指纹识别）".to_string()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-edit-acoustid-btn")
                            .px_3()
                            .py_1p5()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(if app.acoustid_key_active {
                                ACCENT_RED.into()
                            } else {
                                theme::bg_hover()
                            })
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if app.acoustid_key_active {
                                TEXT_WHITE
                            } else {
                                TEXT_PRIMARY
                            })
                            .hover(|s| s.opacity(0.85))
                            .transition(press_transition())
                            .child(if app.acoustid_key_active {
                                "正在编辑..."
                            } else {
                                "配置密钥"
                            })
                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| this.edit_acoustid_key(cx))),
                    ),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            // 一键重新识别当前歌曲
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("重新联网识别当前曲目"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child("强制清除当前曲目的识别缓存，重新从云端获取元数据与 LRC 歌词"),
                            ),
                    )
                    .child(
                        div()
                            .id("settings-retry-enrichment-btn")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .px_3()
                            .py_1p5()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(theme::accent_red_muted())
                            .text_color(ACCENT_RED)
                            .text_xs()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .hover(|s| s.bg(theme::accent_red_active()))
                            .transition(press_transition())
                            .child(themed_icon(
                                lucide_icons::icon_sparkles(),
                                13.0,
                                ACCENT_RED.into(),
                            ))
                            .child("重新识别")
                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                                this.retry_current_enrichment(cx);
                            })),
                    ),
            ),
    )
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "••••••••".to_string()
    } else {
        format!("{}••••{}", &key[..4], &key[key.len() - 4..])
    }
}

// ============================================================================
// 分组 6: 外观与动效
// ============================================================================

fn appearance_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let current_px = app.config.blur_radius;
    let presets: [f32; 6] = [8.0, 16.0, 24.0, 32.0, 48.0, 64.0];

    apple_settings_card(
        "视觉与动效",
        "定制沉浸舞台的高斯模糊与动画平滑度",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("播放舞台流体动态弥散模糊"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child("根据专辑主色调在 GPU 渲染亚克力高斯模糊呼吸背景"),
                            ),
                    )
                    .child(apple_toggle_switch(
                        "blur-switch",
                        app.config.dynamic_blur,
                        cx.listener(|this, _, _, cx| this.toggle_blur(cx)),
                    )),
            )
            .child_if(app.config.dynamic_blur, || {
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .pt_3()
                    .border_t_1()
                    .border_color(BORDER_CARD)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(TEXT_PRIMARY)
                                            .child("自定义高斯模糊半径 (Blur Radius)"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(TEXT_TERTIARY)
                                            .child("调整舞台弥散模糊的像素大小（像素值越大越柔和朦胧，推荐 16px~32px）"),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("blur-minus-btn")
                                            .px_2p5()
                                            .py_1()
                                            .rounded_lg()
                                            .bg(theme::bg_pill())
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::bg_hover()))
                                            .transition(press_transition())
                                            .child(themed_icon(
                                                lucide_icons::icon_minus(),
                                                13.0,
                                                TEXT_PRIMARY.into(),
                                            ))
                                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                                                this.adjust_blur_radius(-2.0, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .min_w(px(56.0))
                                            .text_center()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(ACCENT_RED)
                                            .child(format!("{:.0} px", current_px)),
                                    )
                                    .child(
                                        div()
                                            .id("blur-plus-btn")
                                            .px_2p5()
                                            .py_1()
                                            .rounded_lg()
                                            .bg(theme::bg_pill())
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::bg_hover()))
                                            .transition(press_transition())
                                            .child(themed_icon(
                                                lucide_icons::icon_plus(),
                                                13.0,
                                                TEXT_PRIMARY.into(),
                                            ))
                                            .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                                                this.adjust_blur_radius(2.0, cx);
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .children(presets.into_iter().map(|px_val| {
                                let is_active = (current_px - px_val).abs() < 1.0;
                                let label = if (px_val - 16.0).abs() < 0.1 {
                                    "16 px (推荐)".to_string()
                                } else {
                                    format!("{:.0} px", px_val)
                                };
                                div()
                                    .id(SharedString::from(format!("blur-preset-{px_val}")))
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .text_xs()
                                    .font_weight(if is_active {
                                        gpui::FontWeight::SEMIBOLD
                                    } else {
                                        gpui::FontWeight::MEDIUM
                                    })
                                    .bg(if is_active {
                                        ACCENT_RED.into()
                                    } else {
                                        theme::bg_pill()
                                    })
                                    .text_color(if is_active {
                                        TEXT_WHITE
                                    } else {
                                        TEXT_SECONDARY
                                    })
                                    .hover(|s| {
                                        if is_active {
                                            s
                                        } else {
                                            s.bg(theme::bg_hover()).text_color(TEXT_PRIMARY)
                                        }
                                    })
                                    .transition(press_transition())
                                    .child(label)
                                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| {
                                        this.set_blur_radius(px_val, cx);
                                    }))
                            })),
                    )
            }),
    )
}

// ============================================================================
// 通用 Apple macOS 风格组件
// ============================================================================

fn apple_settings_card(title: &str, subtitle: &str, content: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .rounded_2xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .child(subtitle.to_owned()),
                ),
        )
        .child(content)
}

fn apple_toggle_switch(
    id: &'static str,
    enabled: bool,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let mut switch = div()
        .id(SharedString::from(id))
        .w(px(46.0))
        .h(px(26.0))
        .p(px(2.5))
        .rounded_full()
        .cursor_pointer()
        .bg(if enabled { ACCENT_RED } else { rgb(0xe5_e7_eb) })
        .transition(press_transition())
        .active(|s| s.scale(0.95))
        .flex()
        .items_center();

    if enabled {
        switch = switch.justify_end();
    } else {
        switch = switch.justify_start();
    }

    switch
        .child(div().size(px(21.0)).rounded_full().bg(rgb(0xff_ff_ff)))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn apple_round_step_btn(
    id: &str,
    icon: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id.to_string()))
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::bg_hover())
        .hover(|s| s.bg(theme::bg_active()))
        .transition(press_transition())
        .active(|s| s.scale(0.90))
        .child(themed_icon(icon, 13.0, hsla(220.0, 0.08, 0.45, 1.0)))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn apple_micro_btn(
    id: &str,
    icon: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id.to_string()))
        .size(px(22.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|s| s.scale(0.90))
        .child(themed_icon(icon, 11.0, hsla(220.0, 0.08, 0.45, 1.0)))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn preset_name(preset: EqPreset) -> &'static str {
    match preset {
        EqPreset::Flat => "Flat 原声",
        EqPreset::Pop => "Pop 流行",
        EqPreset::Rock => "Rock 摇滚",
        EqPreset::Vocal => "Vocal 人声",
        EqPreset::Classical => "Classical 古典",
    }
}

fn log_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let is_debug = app.config.log.level.eq_ignore_ascii_case("debug");

    apple_settings_card(
        "日志与系统诊断",
        "配置日志记录级别与每日归档。日志自动保存至 logs/ 目录",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child("Debug 详细调试日志"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_SECONDARY)
                                    .child("输出底层音频解码、网络交互与 GPUI 事件的详细跟踪信息"),
                            ),
                    )
                    .child(apple_toggle_switch(
                        "debug-log-toggle",
                        is_debug,
                        cx.listener(|this, _, _, cx| this.toggle_debug_log(cx)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt_2()
                    .border_t_1()
                    .border_color(BORDER_HAIRLINE)
                    .child(div().text_xs().text_color(TEXT_TERTIARY).child(format!(
                        "当前级别：{} (已启用滚动文件归档)",
                        app.config.log.level.to_uppercase()
                    )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(TEXT_TERTIARY)
                            .child("归档保留：最近 7 天"),
                    ),
            ),
    )
}
