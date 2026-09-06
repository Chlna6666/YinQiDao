use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use anyhow::Result;
use gpui::{Context, IntoElement, SharedString, Timer, div, hsla, prelude::*, px, rgb};
use lucide_gpui::icons as lucide_icons;

use crate::{
    audio::{EqPreset, PlayerCommand, SpatialPreset, clamp_eq, clamp_spatial},
    model::{SpatialMotionMode, SpatialSettings},
};

use super::{
    components::{SliderStyle, interactive_slider, interactive_vertical_slider},
    shell::MusicApp,
    theme::{
        self, ACCENT_RED, BORDER_CARD, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY,
        TEXT_TERTIARY, TEXT_WHITE, press_transition, themed_icon,
    },
};

static AUDIO_SAVE_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum SpatialControl {
    Width,
    Depth,
    Distance,
    Mix,
    Crossfeed,
    Room,
    Immersive3d,
    MotionSpeed,
    MotionRadius,
    MotionIntensity,
}

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    div()
        .id("settings-v2-scroll")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .p_8()
        .gap_8()
        .child(header(app))
        .child(audio_device_group(app, cx))
        .child(eq_group(app, cx))
        .child(spatial_group(app, cx))
        .child(directories_group(app, cx))
        .child(online_group(app, cx))
        .child(appearance_group(app, cx))
        .child(log_group(app, cx))
        .into_any_element()
}

fn header(app: &MusicApp) -> impl IntoElement {
    div()
        .flex()
        .items_end()
        .justify_between()
        .child(
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
                        .child("专业音频参数实时作用于 DSP，并使用版本化 config.toml 持久保存"),
                ),
        )
        .child(
            div()
                .px_3()
                .py_1()
                .rounded_full()
                .bg(theme::bg_pill())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_SECONDARY)
                .child(format!("Config Schema v{}", app.config.schema_version)),
        )
}

fn audio_device_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let volume = app.config.volume;
    let weak = cx.weak_entity();
    let volume_slider = interactive_slider(
        "settings-master-volume",
        volume,
        SliderStyle::settings_control(),
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| app.set_master_volume_absolute(ratio, app_cx));
            }
        },
        |_ratio, _cx| {},
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| app.set_master_volume_absolute(ratio, app_cx));
            }
        },
    )
    .w(px(300.0));

    let mut devices = div().flex().flex_col().gap_1p5();
    if app.output_devices.is_empty() {
        devices = devices.child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("未检测到可用音频输出设备"),
        );
    } else {
        for device in &app.output_devices {
            let id = device.id.clone();
            let active = app.config.output_device.as_deref() == Some(device.id.as_str());
            devices = devices.child(
                div()
                    .id(SharedString::from(format!("settings-output-{}", device.id)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .py_2p5()
                    .rounded_xl()
                    .cursor_pointer()
                    .bg(if active { theme::accent_red_muted() } else { theme::bg_hover() })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(TEXT_PRIMARY)
                                    .child(device.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child(format!("{} Hz · {} 声道", device.sample_rate, device.channels)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if active { ACCENT_RED } else { TEXT_SECONDARY })
                            .child(if active { "使用中" } else { "切换" }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.set_output_device(id.clone(), cx)),
                    ),
            );
        }
    }

    card(
        "音频硬件与输出",
        "主音量使用感知增益曲线；输出设备切换后会重建 DSP 并恢复全部持久化参数",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label_block("主输出音量", "实时应用到音频处理链末端"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(volume_slider)
                            .child(value_badge(format!("{}%", (volume * 100.0).round() as u32))),
                    ),
            )
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(devices),
    )
}

fn eq_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let active_preset = EqPreset::ALL
        .into_iter()
        .find(|preset| preset.matches(&app.config.eq));
    let mut presets = div().flex().flex_wrap().items_center().gap_2();
    for preset in EqPreset::ALL {
        let active = active_preset == Some(preset);
        presets = presets.child(preset_chip(
            format!("eq-v2-{preset:?}"),
            eq_preset_name(preset),
            active,
            cx.listener(move |this, _, _, cx| {
                this.apply_eq(preset, cx);
                this.schedule_audio_config_save(cx);
            }),
        ));
    }
    presets = presets.child(
        div()
            .id("eq-v2-custom")
            .px_3()
            .py_1p5()
            .rounded_full()
            .bg(if active_preset.is_none() { ACCENT_RED.into() } else { theme::bg_hover() })
            .text_xs()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(if active_preset.is_none() { TEXT_WHITE } else { TEXT_SECONDARY })
            .child("Custom 自定义"),
    );

    let weak = cx.weak_entity();
    let preamp_ratio = ((app.config.eq.preamp_db + 12.0) / 18.0).clamp(0.0, 1.0);
    let preamp_slider = interactive_slider(
        "eq-preamp-slider",
        preamp_ratio,
        SliderStyle::settings_control(),
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| {
                    app.set_eq_preamp_absolute(-12.0 + ratio * 18.0, app_cx)
                });
            }
        },
        |_ratio, _cx| {},
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| {
                    app.set_eq_preamp_absolute(-12.0 + ratio * 18.0, app_cx)
                });
            }
        },
    )
    .w(px(280.0));

    let frequencies = ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
    let mut bands = div().flex().items_end().justify_between().gap_3().px_3();
    for (index, frequency) in frequencies.into_iter().enumerate() {
        bands = bands.child(eq_band(app, cx, index, frequency));
    }

    card(
        "专业 10 段图形均衡器",
        "31 Hz–16 kHz 高精度 IIR 滤波；拖动任意频段即进入 Custom，自定义值与 Preamp 永久保存",
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(presets)
                    .child(toggle_switch(
                        "eq-v2-enabled",
                        app.config.eq.enabled,
                        cx.listener(|this, _, _, cx| {
                            this.toggle_eq(cx);
                            this.schedule_audio_config_save(cx);
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label_block("Preamp 前级增益", "建议提升频段时适当降低前级，保留数字余量"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(preamp_slider)
                            .child(value_badge(format!("{:+.1} dB", app.config.eq.preamp_db))),
                    ),
            )
            .child(
                div()
                    .p_5()
                    .rounded_2xl()
                    .bg(rgb(0xf8_f9_fb))
                    .border_1()
                    .border_color(BORDER_CARD)
                    .child(bands),
            ),
    )
}

fn eq_band(app: &MusicApp, cx: &mut Context<MusicApp>, index: usize, frequency: &str) -> impl IntoElement {
    let db = app.config.eq.bands_db[index];
    let ratio = ((db + 12.0) / 24.0).clamp(0.0, 1.0);
    let mut style = SliderStyle::settings_control();
    style.filled_color = if db >= 0.0 { ACCENT_RED.into() } else { rgb(0x00_7a_ff).into() };
    let weak = cx.weak_entity();
    div()
        .w(px(54.0))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if db.abs() > 0.01 { ACCENT_RED } else { TEXT_TERTIARY })
                .child(format!("{db:+.1}")),
        )
        .child(interactive_vertical_slider(
            SharedString::from(format!("eq-band-slider-{index}")),
            ratio,
            px(112.0),
            style,
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| {
                    app.set_eq_band_absolute(index, -12.0 + ratio * 24.0, app_cx)
                });
            },
        ))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_SECONDARY)
                .child(frequency.to_owned()),
        )
}

fn spatial_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let active_preset = SpatialPreset::ALL
        .into_iter()
        .find(|preset| preset.matches(&app.config.spatial));
    let mut presets = div().flex().flex_wrap().items_center().gap_2();
    for preset in SpatialPreset::ALL {
        let active = active_preset == Some(preset);
        presets = presets.child(preset_chip(
            format!("spatial-v2-{preset:?}"),
            spatial_preset_name(preset),
            active,
            cx.listener(move |this, _, _, cx| this.apply_spatial_preset_v2(preset, cx)),
        ));
    }
    if active_preset.is_none() {
        presets = presets.child(
            div()
                .px_3()
                .py_1p5()
                .rounded_full()
                .bg(ACCENT_RED)
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_WHITE)
                .child("Custom 自定义"),
        );
    }

    let dynamic = app.config.spatial.motion_mode != SpatialMotionMode::Static;
    let mut controls = div().flex().flex_col().gap_4();
    for (label, subtitle, control) in [
        ("声场宽度 Width", "M/S 侧声道扩展范围", SpatialControl::Width),
        ("空间深度 Depth", "早期反射与前后层次", SpatialControl::Depth),
        ("听感距离 Distance", "空气吸收与远近衰减", SpatialControl::Distance),
        ("空间混合 Mix", "原始信号与空间处理信号比例", SpatialControl::Mix),
        ("耳机 Crossfeed", "降低极端左右分离并稳定中心声像", SpatialControl::Crossfeed),
        ("Room Size", "早期反射空间尺度", SpatialControl::Room),
        ("3D Decorrelation", "增强非相关空间尾部与包围感", SpatialControl::Immersive3d),
    ] {
        controls = controls.child(spatial_slider_row(app, cx, label, subtitle, control));
    }

    if dynamic {
        controls = controls
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label_block(
                        "动态轨道引擎",
                        motion_mode_description(app.config.spatial.motion_mode),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(value_badge(if app.config.spatial.clockwise {
                                "顺时针 CW".to_string()
                            } else {
                                "逆时针 CCW".to_string()
                            }))
                            .child(
                                div()
                                    .id("spatial-direction-toggle")
                                    .px_3()
                                    .py_1p5()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(theme::bg_hover())
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(TEXT_PRIMARY)
                                    .child("反转方向")
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _, _, cx| this.toggle_spatial_direction_v2(cx)),
                                    ),
                            ),
                    ),
            )
            .child(spatial_slider_row(
                app,
                cx,
                "运动速度 Speed",
                "完整轨道的旋转速度",
                SpatialControl::MotionSpeed,
            ))
            .child(spatial_slider_row(
                app,
                cx,
                "轨道半径 Radius",
                "虚拟声源绕头部运行的距离尺度",
                SpatialControl::MotionRadius,
            ))
            .child(spatial_slider_row(
                app,
                cx,
                "运动强度 Intensity",
                "动态声源对最终空间信号的占比",
                SpatialControl::MotionIntensity,
            ));
    }

    card(
        "空间音频 · 立体声增强 · 动态环绕",
        "静态声场 + ILD/ITD 动态定位 + 前后频谱线索 + 早期反射；支持 8D、360°、行星与近耳轨道",
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(presets)
                    .child(toggle_switch(
                        "spatial-v2-enabled",
                        app.config.spatial.enabled,
                        cx.listener(|this, _, _, cx| {
                            this.toggle_spatial(cx);
                            this.schedule_audio_config_save(cx);
                        }),
                    )),
            )
            .child(
                div()
                    .p_5()
                    .rounded_2xl()
                    .bg(rgb(0xf8_f9_fb))
                    .border_1()
                    .border_color(BORDER_CARD)
                    .child(controls),
            ),
    )
}

fn spatial_slider_row(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    label: &str,
    subtitle: &str,
    control: SpatialControl,
) -> impl IntoElement {
    let (ratio, display) = spatial_control_value(&app.config.spatial, control);
    let weak = cx.weak_entity();
    let slider = interactive_slider(
        SharedString::from(format!("spatial-control-{}", spatial_control_key(control))),
        ratio,
        SliderStyle::settings_control(),
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| app.set_spatial_control_v2(control, ratio, app_cx));
            }
        },
        |_ratio, _cx| {},
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| app.set_spatial_control_v2(control, ratio, app_cx));
            }
        },
    )
    .w(px(300.0));

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(label_block(label, subtitle))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(slider)
                .child(value_badge(display)),
        )
}

fn directories_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_2();
    if app.config.music_dirs.is_empty() {
        list = list.child(div().text_xs().text_color(TEXT_TERTIARY).child("尚未添加音乐目录"));
    } else {
        for dir in &app.config.music_dirs {
            list = list.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(theme::bg_hover())
                    .text_xs()
                    .text_color(TEXT_PRIMARY)
                    .child(dir.to_string_lossy().to_string()),
            );
        }
    }
    card(
        "本地音乐目录与监控",
        "目录变化自动增量同步；重建索引用于修复数据库与文件状态不一致",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(action_button(
                        "settings-v2-add-folder",
                        "添加文件夹",
                        cx.listener(|this, _, _, cx| this.choose_folder(cx)),
                    ))
                    .child(action_button(
                        "settings-v2-rescan",
                        "增量同步",
                        cx.listener(|this, _, _, cx| this.rescan_library(cx)),
                    ))
                    .child(action_button(
                        "settings-v2-reset-index",
                        "重建索引",
                        cx.listener(|this, _, _, cx| this.reset_library_index(cx)),
                    )),
            ),
    )
}

fn online_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    card(
        "在线元数据、封面与歌词",
        "本地封面始终优先；在线服务仅在本地缺失时补封面，并使用全局候选 Ranker 匹配元数据与歌词",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "联网元数据与封面 fallback",
                "网易、QQ、Spotify、咪咕、千千、酷狗 + MusicBrainz/AcoustID",
                "settings-v2-online-meta",
                app.config.online_metadata,
                cx.listener(|this, _, _, cx| this.toggle_online_metadata(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(toggle_row(
                "在线同步歌词",
                "优先本地 LRC/内嵌歌词，再补翻译和 LRCLIB",
                "settings-v2-online-lyrics",
                app.config.online_lyrics,
                cx.listener(|this, _, _, cx| this.toggle_online_lyrics(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label_block(
                        "AcoustID 音频指纹 API Key",
                        &app.config.acoustid_api_key.as_ref().map_or_else(
                            || "未配置；当前使用标签与平台候选匹配".to_string(),
                            |key| format!("已配置：{}", mask_key(key)),
                        ),
                    ))
                    .child(action_button(
                        "settings-v2-acoustid",
                        "配置密钥",
                        cx.listener(|this, _, _, cx| this.edit_acoustid_key(cx)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(action_button(
                        "settings-v2-retry-online",
                        "重新识别当前曲目",
                        cx.listener(|this, _, _, cx| this.retry_current_enrichment(cx)),
                    )),
            ),
    )
}

fn appearance_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let ratio = (app.config.blur_radius / 80.0).clamp(0.0, 1.0);
    let weak = cx.weak_entity();
    let slider = interactive_slider(
        "settings-v2-blur-radius",
        ratio,
        SliderStyle::settings_control(),
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| app.set_blur_radius(ratio * 80.0, app_cx));
            }
        },
        |_ratio, _cx| {},
        move |ratio, cx| {
            let _ = weak.update(cx, |app, app_cx| app.set_blur_radius(ratio * 80.0, app_cx));
        },
    )
    .w(px(300.0));

    card(
        "视觉与动效",
        "控制沉浸舞台背景与模糊半径",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "播放舞台动态弥散模糊",
                "使用封面主色生成低频背景纹理",
                "settings-v2-blur-toggle",
                app.config.dynamic_blur,
                cx.listener(|this, _, _, cx| this.toggle_blur(cx)),
            ))
            .child_if(app.config.dynamic_blur, || {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label_block("Blur Radius", "0–80 px"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(slider)
                            .child(value_badge(format!("{:.0} px", app.config.blur_radius))),
                    )
            }),
    )
}

fn log_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let debug = app.config.log.level.eq_ignore_ascii_case("debug");
    card(
        "日志与系统诊断",
        "Debug 模式输出音频 DSP、解码、网络和 GPUI 详细跟踪信息",
        toggle_row(
            "Debug 详细日志",
            &format!("当前级别：{}", app.config.log.level.to_uppercase()),
            "settings-v2-debug-log",
            debug,
            cx.listener(|this, _, _, cx| this.toggle_debug_log(cx)),
        ),
    )
}

impl MusicApp {
    fn set_master_volume_absolute(&mut self, value: f32, cx: &mut Context<Self>) {
        self.config.volume = value.clamp(0.0, 1.0);
        self.send(PlayerCommand::SetVolume(self.config.volume));
        self.save_config();
        self.schedule_audio_config_save(cx);
        cx.notify();
    }

    fn set_eq_band_absolute(&mut self, index: usize, db: f32, cx: &mut Context<Self>) {
        if let Some(band) = self.config.eq.bands_db.get_mut(index) {
            *band = db;
            self.config.eq.enabled = true;
            self.config.eq = clamp_eq(self.config.eq.clone());
            self.send(PlayerCommand::SetEq(self.config.eq.clone()));
            self.save_config();
            self.schedule_audio_config_save(cx);
            cx.notify();
        }
    }

    fn set_eq_preamp_absolute(&mut self, db: f32, cx: &mut Context<Self>) {
        self.config.eq.preamp_db = db;
        self.config.eq.enabled = true;
        self.config.eq = clamp_eq(self.config.eq.clone());
        self.send(PlayerCommand::SetEq(self.config.eq.clone()));
        self.save_config();
        self.schedule_audio_config_save(cx);
        cx.notify();
    }

    fn apply_spatial_preset_v2(&mut self, preset: SpatialPreset, cx: &mut Context<Self>) {
        self.config.spatial = preset.settings();
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.save_config();
        self.schedule_audio_config_save(cx);
        self.status = format!("空间音频已切换为 {}", spatial_preset_name(preset));
        cx.notify();
    }

    fn set_spatial_control_v2(
        &mut self,
        control: SpatialControl,
        ratio: f32,
        cx: &mut Context<Self>,
    ) {
        let ratio = ratio.clamp(0.0, 1.0);
        match control {
            SpatialControl::Width => self.config.spatial.width = ratio,
            SpatialControl::Depth => self.config.spatial.depth = ratio,
            SpatialControl::Distance => self.config.spatial.distance = ratio,
            SpatialControl::Mix => self.config.spatial.mix = ratio,
            SpatialControl::Crossfeed => self.config.spatial.crossfeed = ratio,
            SpatialControl::Room => self.config.spatial.room_size = ratio,
            SpatialControl::Immersive3d => self.config.spatial.immersive_3d = ratio,
            SpatialControl::MotionSpeed => self.config.spatial.motion_speed_hz = 0.01 + ratio * 0.34,
            SpatialControl::MotionRadius => self.config.spatial.motion_radius = ratio,
            SpatialControl::MotionIntensity => self.config.spatial.motion_intensity = ratio,
        }
        self.config.spatial.enabled = true;
        self.config.spatial = clamp_spatial(self.config.spatial.clone());
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.save_config();
        self.schedule_audio_config_save(cx);
        cx.notify();
    }

    fn toggle_spatial_direction_v2(&mut self, cx: &mut Context<Self>) {
        self.config.spatial.clockwise = !self.config.spatial.clockwise;
        self.config.spatial.enabled = true;
        self.send(PlayerCommand::SetSpatial(self.config.spatial.clone()));
        self.save_config();
        self.schedule_audio_config_save(cx);
        cx.notify();
    }

    fn schedule_audio_config_save(&mut self, cx: &mut Context<Self>) {
        let generation = AUDIO_SAVE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
        cx.spawn(async move |this, cx| -> Result<()> {
            Timer::after(Duration::from_millis(350)).await;
            if AUDIO_SAVE_GENERATION.load(Ordering::Acquire) != generation {
                return Ok(());
            }
            let (store, config) = this.update(cx, |app, _cx| {
                (app.config_store.clone(), app.config.clone())
            })?;
            let _ = crate::runtime::spawn_blocking(move || {
                let _ = store.save(&config);
            });
            Ok(())
        })
        .detach();
    }
}

fn spatial_control_value(settings: &SpatialSettings, control: SpatialControl) -> (f32, String) {
    match control {
        SpatialControl::Width => percent(settings.width),
        SpatialControl::Depth => percent(settings.depth),
        SpatialControl::Distance => percent(settings.distance),
        SpatialControl::Mix => percent(settings.mix),
        SpatialControl::Crossfeed => percent(settings.crossfeed),
        SpatialControl::Room => percent(settings.room_size),
        SpatialControl::Immersive3d => percent(settings.immersive_3d),
        SpatialControl::MotionSpeed => {
            let ratio = ((settings.motion_speed_hz - 0.01) / 0.34).clamp(0.0, 1.0);
            let period = 1.0 / settings.motion_speed_hz.max(0.01);
            (ratio, format!("{period:.1}s / 圈"))
        }
        SpatialControl::MotionRadius => percent(settings.motion_radius),
        SpatialControl::MotionIntensity => percent(settings.motion_intensity),
    }
}

fn percent(value: f32) -> (f32, String) {
    (value.clamp(0.0, 1.0), format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32))
}

fn spatial_control_key(control: SpatialControl) -> &'static str {
    match control {
        SpatialControl::Width => "width",
        SpatialControl::Depth => "depth",
        SpatialControl::Distance => "distance",
        SpatialControl::Mix => "mix",
        SpatialControl::Crossfeed => "crossfeed",
        SpatialControl::Room => "room",
        SpatialControl::Immersive3d => "3d",
        SpatialControl::MotionSpeed => "motion-speed",
        SpatialControl::MotionRadius => "motion-radius",
        SpatialControl::MotionIntensity => "motion-intensity",
    }
}

fn eq_preset_name(preset: EqPreset) -> &'static str {
    match preset {
        EqPreset::Flat => "Flat 原声",
        EqPreset::Pop => "Pop 流行",
        EqPreset::Rock => "Rock 摇滚",
        EqPreset::Vocal => "Vocal 人声",
        EqPreset::Classical => "Classical 古典",
    }
}

fn spatial_preset_name(preset: SpatialPreset) -> &'static str {
    match preset {
        SpatialPreset::Studio => "Studio 监听",
        SpatialPreset::Wide => "Wide 宽阔",
        SpatialPreset::Headphones => "Headphone 耳机",
        SpatialPreset::Cinema => "Cinema 影院",
        SpatialPreset::Immersive3d => "3D 沉浸",
        SpatialPreset::Orbit8d => "8D 环绕",
        SpatialPreset::Orbit360 => "360° 环绕",
        SpatialPreset::Pendulum => "左右摆动",
        SpatialPreset::Planetary => "音乐行星",
        SpatialPreset::NearEar => "近耳旋绕",
    }
}

fn motion_mode_description(mode: SpatialMotionMode) -> &'static str {
    match mode {
        SpatialMotionMode::Static => "固定声场",
        SpatialMotionMode::Orbit8d => "8D 八字轨道：声音在左右与前后之间连续穿行",
        SpatialMotionMode::Orbit360 => "360° 完整绕头轨道：结合耳间时差与前后频谱线索",
        SpatialMotionMode::Pendulum => "左右摆动：保持较稳定的前方距离，突出横向移动",
        SpatialMotionMode::FrontBack => "前后环绕：主要沿前后轴运动，减少左右晃动",
        SpatialMotionMode::Planetary => "音乐行星：轨道半径随旋转周期变化，形成近远层次",
        SpatialMotionMode::NearEar => "近耳旋绕：强化近侧耳的电平差与短时差定位",
    }
}

fn card(title: &str, subtitle: &str, content: impl IntoElement) -> impl IntoElement {
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
                .gap_1()
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(title.to_owned()),
                )
                .child(div().text_xs().text_color(TEXT_SECONDARY).child(subtitle.to_owned())),
        )
        .child(content)
}

fn label_block(title: &str, subtitle: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(TEXT_PRIMARY)
                .child(title.to_owned()),
        )
        .child(div().text_xs().text_color(TEXT_TERTIARY).child(subtitle.to_owned()))
}

fn value_badge(value: String) -> impl IntoElement {
    div()
        .min_w(px(72.0))
        .px_2p5()
        .py_1()
        .rounded_lg()
        .bg(theme::bg_pill())
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_center()
        .text_color(TEXT_PRIMARY)
        .child(value)
}

fn toggle_row(
    title: &str,
    subtitle: &str,
    id: &'static str,
    enabled: bool,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(label_block(title, subtitle))
        .child(toggle_switch(id, enabled, listener))
}

fn toggle_switch(
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
    switch = if enabled { switch.justify_end() } else { switch.justify_start() };
    switch
        .child(div().size(px(21.0)).rounded_full().bg(rgb(0xff_ff_ff)))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn preset_chip(
    id: String,
    label: &'static str,
    active: bool,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .px_3()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(if active { ACCENT_RED.into() } else { theme::bg_hover() })
        .text_xs()
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .text_color(if active { TEXT_WHITE } else { TEXT_PRIMARY })
        .hover(|s| s.opacity(0.86))
        .transition(press_transition())
        .active(|s| s.scale(0.96))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn action_button(
    id: &'static str,
    label: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(theme::bg_hover())
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_PRIMARY)
        .hover(|s| s.bg(theme::bg_active()))
        .transition(press_transition())
        .active(|s| s.scale(0.96))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        "••••••••".to_string()
    } else {
        format!("{}••••{}", &key[..4], &key[key.len() - 4..])
    }
}
