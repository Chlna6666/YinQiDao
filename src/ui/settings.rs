use gpui::{Context, IntoElement, SharedString, div, prelude::*, px, rgb};

use crate::{
    audio::{EqPreset, SpatialPreset, classify_smart_audio},
    desktop_lyrics::LyricsColorTarget,
    model::{SpatialMotionMode, TransitionMode},
    preferences::SpatialControl,
    settings::DesktopLyricsAlignment,
};

use super::{
    components::{SliderStyle, interactive_slider, interactive_vertical_slider},
    shell::MusicApp,
    theme::{
        self, ACCENT_RED, BORDER_CARD, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY,
        TEXT_TERTIARY, TEXT_WHITE, press_transition,
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
        .child(header(app))
        .child(audio_device_group(app, cx))
        .child(smart_audio_group(app, cx))
        .child(eq_group(app, cx))
        .child(spatial_group(app, cx))
        .child(audio_laboratory_group(app, cx))
        .child(track_transition_group(app, cx))
        .child(desktop_lyrics_group(app, cx))
        .child(global_shortcuts_group(app, cx))
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
                        .child("专业音频、智能调音、桌面歌词与全局快捷键均持久化到 config.toml"),
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
    let mut devices = div().flex().flex_col().gap_2();
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
                    .bg(if active {
                        theme::accent_red_muted()
                    } else {
                        theme::bg_hover()
                    })
                    .hover(|style| style.opacity(0.86))
                    .transition(press_transition())
                    .active(|style| style.scale(0.99))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
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
                    .child(value_badge(if active { "使用中" } else { "切换" }))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.set_output_device(id.clone(), cx)),
                    ),
            );
        }
    }

    card(
        "音频硬件与输出",
        "输出设备切换后重新建立音频引擎，并恢复当前持久化 DSP 策略",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(step_row(
                "主输出音量",
                "播放器最终输出增益",
                format!("{}%", (app.config.volume * 100.0).round() as u32),
                "settings-volume-dec",
                "settings-volume-inc",
                cx.listener(|this, _, _, cx| this.adjust_volume(-0.05, cx)),
                cx.listener(|this, _, _, cx| this.adjust_volume(0.05, cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(devices),
    )
}

fn smart_audio_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let current_profile = app
        .snapshot
        .current_track
        .as_ref()
        .map(|track| {
            let (profile, confidence) = classify_smart_audio(track);
            format!("{} · 置信度 {:.0}%", profile.label(), confidence * 100.0)
        })
        .unwrap_or_else(|| "播放歌曲后显示当前自动识别结果".to_string());

    let weak = cx.weak_entity();
    let intensity_slider = interactive_slider(
        "smart-audio-intensity",
        app.config.smart_audio.intensity,
        SliderStyle::settings_control(),
        {
            let weak = weak.clone();
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| {
                    app.adjust_smart_audio_intensity(
                        ratio - app.config.smart_audio.intensity,
                        app_cx,
                    );
                });
            }
        },
        |_ratio, _cx| {},
        move |ratio, cx| {
            let _ = weak.update(cx, |app, app_cx| {
                app.adjust_smart_audio_intensity(
                    ratio - app.config.smart_audio.intensity,
                    app_cx,
                );
            });
        },
    )
    .w(px(280.0));

    card(
        "智能音效 · 自动曲风匹配",
        "根据 Genre、标题、专辑和音源标签选择 EQ 与静态空间参数；已标记 8D / 360° / Binaural / Atmos 的音源避免重复空间化",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block(
                        "自动调音",
                        "每次切歌由音频工作线程重新判断，不依赖设置页存活",
                    ))
                    .child(toggle_switch(
                        "smart-audio-enabled",
                        app.config.smart_audio.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_smart_audio(cx)),
                    )),
            )
            .child(
                div()
                    .p_4()
                    .rounded_xl()
                    .bg(theme::accent_red_muted())
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block("当前识别", current_profile))
                    .child(value_badge(if app.config.smart_audio.enabled {
                        "自动应用"
                    } else {
                        "仅预览"
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .child(label_block(
                        "自动调音强度",
                        "低强度保留更多手动基线，高强度更接近曲风推荐参数",
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(intensity_slider)
                            .child(value_badge(format!(
                                "{}%",
                                (app.config.smart_audio.intensity * 100.0).round() as u32
                            ))),
                    ),
            ),
    )
}

fn eq_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let active_preset = EqPreset::ALL
        .into_iter()
        .find(|preset| preset.matches(&app.config.eq));
    let mut presets = div().flex().flex_wrap().items_center().gap_2();
    for preset in EqPreset::ALL {
        presets = presets.child(preset_chip(
            SharedString::from(format!("eq-preset-{preset:?}")),
            eq_preset_name(preset),
            active_preset == Some(preset),
            cx.listener(move |this, _, _, cx| this.set_manual_eq_preset(preset, cx)),
        ));
    }
    if active_preset.is_none() {
        presets = presets.child(readonly_chip("Custom 自定义", true));
    }

    let frequencies = [
        "31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k",
    ];
    let mut bands = div().flex().items_end().justify_between().gap_2().px_2();
    for (index, frequency) in frequencies.into_iter().enumerate() {
        bands = bands.child(eq_band(app, cx, index, frequency));
    }

    card(
        "专业 10 段图形均衡器",
        "Slider 用于快速定位；每段保留 − / + 精调按钮，单次 0.5 dB，避免拖拽难以准确落点",
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(presets)
                    .child(toggle_switch(
                        "eq-enabled",
                        app.config.eq.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_manual_eq(cx)),
                    )),
            )
            .child(step_row(
                "Preamp 前级增益",
                "提升多个频段时可降低前级，给数字峰值保留余量",
                format!("{:+.1} dB", app.config.eq.preamp_db),
                "eq-preamp-dec",
                "eq-preamp-inc",
                cx.listener(|this, _, _, cx| this.adjust_manual_eq_preamp(-0.5, cx)),
                cx.listener(|this, _, _, cx| this.adjust_manual_eq_preamp(0.5, cx)),
            ))
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
    style.filled_color = if db >= 0.0 {
        ACCENT_RED.into()
    } else {
        rgb(0x00_7a_ff).into()
    };
    let weak = cx.weak_entity();

    div()
        .w(px(58.0))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if db.abs() > 0.01 {
                    ACCENT_RED
                } else {
                    TEXT_TERTIARY
                })
                .child(format!("{db:+.1}")),
        )
        .child(interactive_vertical_slider(
            SharedString::from(format!("eq-band-slider-{index}")),
            ratio,
            px(112.0),
            style,
            move |ratio, cx| {
                let _ = weak.update(cx, |app, app_cx| {
                    app.set_manual_eq_band_ratio(index, ratio, app_cx)
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
        .child(
            div()
                .flex()
                .gap_1()
                .child(step_button(
                    SharedString::from(format!("eq-band-{index}-dec")),
                    "−",
                    cx.listener(move |this, _, _, cx| {
                        this.adjust_manual_eq_band(index, -0.5, cx)
                    }),
                ))
                .child(step_button(
                    SharedString::from(format!("eq-band-{index}-inc")),
                    "+",
                    cx.listener(move |this, _, _, cx| {
                        this.adjust_manual_eq_band(index, 0.5, cx)
                    }),
                )),
        )
}

fn spatial_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let active_preset = SpatialPreset::ALL
        .into_iter()
        .find(|preset| preset.matches(&app.config.spatial));
    let mut presets = div().flex().flex_wrap().items_center().gap_2();
    for preset in SpatialPreset::ALL {
        presets = presets.child(preset_chip(
            SharedString::from(format!("spatial-preset-{preset:?}")),
            spatial_preset_name(preset),
            active_preset == Some(preset),
            cx.listener(move |this, _, _, cx| this.set_manual_spatial_preset(preset, cx)),
        ));
    }
    if active_preset.is_none() {
        presets = presets.child(readonly_chip("Custom 自定义", true));
    }

    let mut parameters = div().flex().flex_col().gap_3();
    for (title, subtitle, control, value) in [
        ("声场宽度 Width", "M/S 侧声道扩展", SpatialControl::Width, app.config.spatial.width),
        ("空间深度 Depth", "早期反射与纵深", SpatialControl::Depth, app.config.spatial.depth),
        ("听感距离 Distance", "空气吸收和距离衰减", SpatialControl::Distance, app.config.spatial.distance),
        ("空间混合 Mix", "原始信号与空间信号比例", SpatialControl::Mix, app.config.spatial.mix),
        ("Crossfeed", "耳机左右声道交叉馈送", SpatialControl::Crossfeed, app.config.spatial.crossfeed),
        ("Room Size", "早期反射空间尺度", SpatialControl::Room, app.config.spatial.room_size),
        ("3D Decorrelation", "非相关空间尾部与包围感", SpatialControl::Immersive3d, app.config.spatial.immersive_3d),
    ] {
        parameters = parameters.child(spatial_parameter_row(
            title,
            subtitle,
            control,
            format!("{}%", (value.clamp(0.0, 1.0) * 100.0).round() as u32),
            0.05,
            cx,
        ));
    }

    if app.config.spatial.motion_mode != SpatialMotionMode::Static {
        parameters = parameters
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(spatial_parameter_row(
                "运动速度 Speed",
                "动态虚拟声源的轨道速度",
                SpatialControl::MotionSpeed,
                format!("{:.3} Hz", app.config.spatial.motion_speed_hz),
                0.01,
                cx,
            ))
            .child(spatial_parameter_row(
                "轨道半径 Radius",
                "虚拟声源绕听者的距离尺度",
                SpatialControl::MotionRadius,
                format!("{}%", (app.config.spatial.motion_radius * 100.0).round() as u32),
                0.05,
                cx,
            ))
            .child(spatial_parameter_row(
                "运动强度 Intensity",
                "动态声源在空间处理中的占比",
                SpatialControl::MotionIntensity,
                format!("{}%", (app.config.spatial.motion_intensity * 100.0).round() as u32),
                0.05,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block(
                        "轨道方向",
                        "动态 8D / 360° / 行星模式的旋转方向",
                    ))
                    .child(
                        div()
                            .id("spatial-direction")
                            .px_3()
                            .py_1p5()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(theme::bg_hover())
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(TEXT_PRIMARY)
                            .child(if app.config.spatial.clockwise {
                                "顺时针 CW · 点击反转"
                            } else {
                                "逆时针 CCW · 点击反转"
                            })
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.toggle_spatial_direction(cx)),
                            ),
                    ),
            );
    }

    card(
        "空间音频 · 立体声声场增强",
        "包含 Studio、Wide、Headphone、Cinema、3D、8D、360°、左右摆动、音乐行星与近耳旋绕；手动调整会关闭自动曲风模式",
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(presets)
                    .child(toggle_switch(
                        "spatial-enabled",
                        app.config.spatial.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_manual_spatial(cx)),
                    )),
            )
            .child(parameters),
    )
}

fn spatial_parameter_row(
    title: &str,
    subtitle: &str,
    control: SpatialControl,
    display: String,
    delta: f32,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    step_row(
        title,
        subtitle,
        display,
        SharedString::from(format!("spatial-{}-dec", spatial_control_key(control))),
        SharedString::from(format!("spatial-{}-inc", spatial_control_key(control))),
        cx.listener(move |this, _, _, cx| {
            this.adjust_manual_spatial(control, -delta, cx)
        }),
        cx.listener(move |this, _, _, cx| {
            this.adjust_manual_spatial(control, delta, cx)
        }),
    )
}

fn audio_laboratory_group(_app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let enabled = crate::audio::audio_debug_enabled();
    card(
        "Audio Laboratory · 专业音频分析",
        "实时对比 SOURCE / POST-EQ / POST-SPATIAL。关闭后停止频谱、LUFS、True Peak 与历史采样，不给正常播放热路径增加分析开销",
        toggle_row(
            "显示独立分析窗口",
            if enabled {
                "分析器正在运行"
            } else {
                "仅在需要 A/B/C、频谱、相位和响度诊断时开启"
            },
            "settings-audio-laboratory-toggle",
            enabled,
            cx.listener(|this, _, _, cx| {
                if crate::audio::audio_debug_enabled() {
                    crate::audio_debug_window::shutdown(cx);
                    this.status = "Audio Laboratory 已关闭".into();
                } else if let Err(error) = crate::audio_debug_window::open(cx) {
                    crate::audio::set_audio_debug_enabled(false);
                    this.status = format!("打开 Audio Laboratory 失败：{error:#}");
                    tracing::warn!(error = %error, "Audio Laboratory 窗口打开失败");
                } else {
                    this.status = "Audio Laboratory 已打开".into();
                }
                cx.notify();
            }),
        ),
    )
}

fn track_transition_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut modes = div().flex().flex_wrap().gap_2();
    for (mode, label, description) in [
        (
            TransitionMode::Direct,
            "直接切换",
            "预加载后无额外淡化，自动换曲保持连续",
        ),
        (
            TransitionMode::FadeOutIn,
            "淡出淡入",
            "当前曲淡出，再让下一首平滑淡入",
        ),
        (
            TransitionMode::Crossfade,
            "交叉淡化",
            "两首同时解码，用平滑恒和曲线重叠混合",
        ),
    ] {
        let active = app.config.transition.mode == mode;
        modes = modes.child(
            div()
                .id(SharedString::from(format!("transition-mode-{mode:?}")))
                .min_w(px(150.0))
                .p_3()
                .rounded_xl()
                .cursor_pointer()
                .border_1()
                .border_color(if active {
                    ACCENT_RED.into()
                } else {
                    BORDER_CARD
                })
                .bg(if active {
                    theme::accent_red_muted()
                } else {
                    theme::bg_hover()
                })
                .transition(press_transition())
                .active(|style| style.scale(0.98))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(if active { ACCENT_RED } else { TEXT_PRIMARY })
                        .child(label),
                )
                .child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(description),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.set_track_transition_mode(mode, cx)
                    }),
                ),
        );
    }

    let mut details = div().flex().flex_col().gap_3();
    if app.config.transition.mode != TransitionMode::Direct {
        details = details.child(step_row(
            "过渡时长",
            if app.config.transition.mode == TransitionMode::Crossfade {
                "进入当前曲尾部窗口后提前启动下一首解码和重叠混音"
            } else {
                "总时长平均分配给淡出与淡入"
            },
            format!(
                "{:.2} s",
                app.config.transition.duration_ms as f32 / 1_000.0
            ),
            "transition-duration-dec",
            "transition-duration-inc",
            cx.listener(|this, _, _, cx| this.adjust_track_transition_duration(-250, cx)),
            cx.listener(|this, _, _, cx| this.adjust_track_transition_duration(250, cx)),
        ));
    }

    if app.config.transition.mode == TransitionMode::Crossfade {
        details = details
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block(
                        "Smart Cue 智能入点",
                        "后台分析下一首前段能量包络，只跳过明确静音/无效空白；古典、氛围、人声使用更保守上限",
                    ))
                    .child(toggle_switch(
                        "transition-smart-cue",
                        app.config.transition.smart_cue,
                        cx.listener(|this, _, _, cx| this.toggle_transition_smart_cue(cx)),
                    )),
            )
            .child_if(app.config.transition.smart_cue, || {
                step_row(
                    "最大智能跳过",
                    "安全上限而非固定跳过；检测不到稳定 onset 时从 0 开始",
                    format!(
                        "{:.2} s",
                        app.config.transition.max_smart_cue_ms as f32 / 1_000.0
                    ),
                    "transition-cue-dec",
                    "transition-cue-inc",
                    cx.listener(|this, _, _, cx| this.adjust_transition_max_cue(-250, cx)),
                    cx.listener(|this, _, _, cx| this.adjust_transition_max_cue(250, cx)),
                )
            });
    }

    card(
        "下一首音乐 · 自动过渡",
        "仅作用于自动换曲。Crossfade 在音频工作线程中同时解码两首歌，并利用预加载阶段 Smart Cue 找到下一首更自然的进入位置",
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block("自动过渡", "关闭时保持普通直接切换"))
                    .child(toggle_switch(
                        "transition-enabled",
                        app.config.transition.enabled,
                        cx.listener(|this, _, _, cx| this.toggle_track_transition(cx)),
                    )),
            )
            .child(modes)
            .child(details),
    )
}

fn desktop_lyrics_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let config = &app.config.desktop_lyrics;
    card(
        "桌面歌词",
        "桌面歌词通过底部播放器“词”按钮显示/隐藏；这里仅配置窗口行为和样式",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "锁定歌词窗口",
                "锁定后禁止拖动位置，仍可通过快捷键解锁",
                "desktop-lyrics-lock",
                config.locked,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_lock(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(toggle_row(
                "始终置顶",
                "Windows 使用原生 HWND_TOPMOST，不依赖普通弹窗层级",
                "desktop-lyrics-topmost",
                config.always_on_top,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_topmost(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(toggle_row(
                "显示翻译",
                "歌词存在翻译时显示双语内容",
                "desktop-lyrics-translation",
                config.show_translation,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_translation(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(toggle_row(
                "显示下一句",
                "双行模式同时显示当前句与下一句",
                "desktop-lyrics-two-line",
                config.two_line,
                cx.listener(|this, _, _, cx| this.toggle_desktop_lyrics_two_line(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(desktop_lyrics_alignment_row(app, cx))
            .child(step_row(
                "歌词字号",
                "透明桌面歌词主行字号",
                format!("{:.0} px", config.font_size),
                "desktop-lyrics-font-dec",
                "desktop-lyrics-font-inc",
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_font(-2.0, cx)),
                cx.listener(|this, _, _, cx| this.adjust_desktop_lyrics_font(2.0, cx)),
            ))
            .child(desktop_lyrics_color_row(
                "当前歌词颜色",
                config.active_color,
                LyricsColorTarget::Active,
                cx,
            ))
            .child(desktop_lyrics_color_row(
                "下一句颜色",
                config.inactive_color,
                LyricsColorTarget::Inactive,
                cx,
            ))
            .child(desktop_lyrics_color_row(
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

fn desktop_lyrics_alignment_row(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut choices = div().flex().gap_2();
    for (alignment, label) in [
        (DesktopLyricsAlignment::Left, "左"),
        (DesktopLyricsAlignment::Center, "中"),
        (DesktopLyricsAlignment::Right, "右"),
    ] {
        let active = app.config.desktop_lyrics.alignment == alignment;
        choices = choices.child(
            div()
                .id(SharedString::from(format!("desktop-lyrics-align-{alignment:?}")))
                .px_3()
                .py_1p5()
                .rounded_full()
                .cursor_pointer()
                .bg(if active {
                    ACCENT_RED.into()
                } else {
                    theme::bg_hover()
                })
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
        .gap_4()
        .child(label_block("文字对齐", "桌面歌词水平布局"))
        .child(choices)
}

fn desktop_lyrics_color_row(
    title: &'static str,
    current: u32,
    target: LyricsColorTarget,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let mut palette = div().flex().items_center().gap_2();
    for color in [0xff_3b_5c, 0xff_95_00, 0x34_c7_59, 0x00_7a_ff, 0xaf_52_de, 0xf2_f2_f7] {
        let active = current == color;
        palette = palette.child(
            div()
                .id(SharedString::from(format!("lyrics-color-{target:?}-{color:06x}")))
                .size(px(24.0))
                .rounded_full()
                .cursor_pointer()
                .bg(rgb(color))
                .border_1()
                .border_color(if active {
                    ACCENT_RED.into()
                } else {
                    BORDER_CARD
                })
                .transition(press_transition())
                .active(|style| style.scale(0.90))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.set_desktop_lyrics_color(target, color, cx)
                    }),
                ),
        );
    }

    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(label_block(title, format!("#{:06X}", current & 0x00ff_ffff)))
        .child(palette)
}

fn global_shortcuts_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let enabled = app.config.lyrics_shortcuts.enabled;
    card(
        "系统级全局快捷键",
        "默认关闭。开启后即使音栖岛不在前台，也可控制播放、主窗口和桌面歌词",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "启用全局快捷键",
                if enabled {
                    "已注册到系统"
                } else {
                    "默认不注册任何系统热键"
                },
                "global-shortcuts-enabled",
                enabled,
                cx.listener(|this, _, _, cx| this.toggle_global_shortcuts(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(shortcut_group_label("播放控制"))
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
            .child(shortcut_group_label("窗口控制"))
            .child(shortcut_row("显示主窗口", "Ctrl + Alt + Y"))
            .child(shortcut_row("切换沉浸播放", "Ctrl + Alt + Enter"))
            .child(shortcut_group_label("歌词控制"))
            .child(shortcut_row("显示 / 隐藏桌面歌词", "Ctrl + Alt + L"))
            .child(shortcut_row("锁定 / 解锁歌词", "Ctrl + Alt + K"))
            .child(shortcut_row("显示 / 隐藏翻译", "Ctrl + Alt + T"))
            .child(shortcut_row("增大歌词字号", "Ctrl + Alt + ↑"))
            .child(shortcut_row("减小歌词字号", "Ctrl + Alt + ↓")),
    )
}

fn shortcut_group_label(label: &'static str) -> impl IntoElement {
    div()
        .mt_1()
        .text_xs()
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(TEXT_TERTIARY)
        .child(label)
}

fn shortcut_row(label: &'static str, shortcut: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .child(label),
        )
        .child(
            div()
                .px_3()
                .py_1p5()
                .rounded_full()
                .bg(theme::bg_pill())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_PRIMARY)
                .child(shortcut),
        )
}

fn directories_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_2();
    if app.config.music_dirs.is_empty() {
        list = list.child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("尚未添加音乐目录"),
        );
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
        "目录变化自动增量同步；必要时可重建本地索引",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(action_button(
                        "settings-add-folder",
                        "添加文件夹",
                        cx.listener(|this, _, _, cx| this.choose_folder(cx)),
                    ))
                    .child(action_button(
                        "settings-rescan",
                        "增量同步",
                        cx.listener(|this, _, _, cx| this.rescan_library(cx)),
                    ))
                    .child(action_button(
                        "settings-reset-index",
                        "重建索引",
                        cx.listener(|this, _, _, cx| this.reset_library_index(cx)),
                    )),
            ),
    )
}

fn online_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    card(
        "在线元数据、封面与歌词",
        "本地封面保持最高优先级；在线服务只补全缺失信息，并继续使用统一候选匹配器",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "联网元数据与封面 fallback",
                "网易、QQ、Spotify、咪咕、千千、酷狗 + MusicBrainz/AcoustID",
                "settings-online-meta",
                app.config.online_metadata,
                cx.listener(|this, _, _, cx| this.toggle_online_metadata(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(toggle_row(
                "在线歌词",
                "本地歌词优先，缺失后再尝试在线同步歌词与翻译",
                "settings-online-lyrics",
                app.config.online_lyrics,
                cx.listener(|this, _, _, cx| this.toggle_online_lyrics(cx)),
            ))
            .child(div().h(px(1.0)).bg(BORDER_HAIRLINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(label_block(
                        "AcoustID 音频指纹",
                        if app
                            .config
                            .acoustid_api_key
                            .as_deref()
                            .is_some_and(|key| !key.trim().is_empty())
                        {
                            "已配置 API Key"
                        } else {
                            "未配置，当前主要依赖本地标签与在线候选匹配"
                        },
                    ))
                    .child(action_button(
                        "settings-acoustid",
                        "配置密钥",
                        cx.listener(|this, _, _, cx| this.edit_acoustid_key(cx)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(action_button(
                        "settings-retry-enrichment",
                        "重新识别当前曲目",
                        cx.listener(|this, _, _, cx| this.retry_current_enrichment(cx)),
                    )),
            ),
    )
}

fn appearance_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    card(
        "视觉与动效",
        "控制沉浸舞台动态背景与模糊强度",
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(toggle_row(
                "播放舞台动态弥散模糊",
                "使用封面主色构建低频背景纹理",
                "settings-blur-toggle",
                app.config.dynamic_blur,
                cx.listener(|this, _, _, cx| this.toggle_blur(cx)),
            ))
            .child_if(app.config.dynamic_blur, || {
                step_row(
                    "Blur Radius",
                    "沉浸舞台背景模糊半径",
                    format!("{:.0} px", app.config.blur_radius),
                    "settings-blur-dec",
                    "settings-blur-inc",
                    cx.listener(|this, _, _, cx| this.adjust_blur_radius(-2.0, cx)),
                    cx.listener(|this, _, _, cx| this.adjust_blur_radius(2.0, cx)),
                )
            }),
    )
}

fn log_group(app: &MusicApp, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let debug = app.config.log.level.eq_ignore_ascii_case("debug");
    let subtitle = format!("当前级别：{}", app.config.log.level.to_uppercase());
    card(
        "日志与系统诊断",
        "Debug 模式输出音频 DSP、解码、Smart Cue、网络与 GPUI 详细跟踪信息",
        toggle_row(
            "Debug 详细日志",
            subtitle,
            "settings-debug-log",
            debug,
            cx.listener(|this, _, _, cx| this.toggle_debug_log(cx)),
        ),
    )
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
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(subtitle.to_owned()),
                ),
        )
        .child(content)
}

fn label_block(title: &str, subtitle: impl Into<SharedString>) -> impl IntoElement {
    let subtitle = subtitle.into();
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
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(subtitle),
        )
}

fn step_row(
    title: &str,
    subtitle: &str,
    value: String,
    dec_id: impl Into<gpui::ElementId>,
    inc_id: impl Into<gpui::ElementId>,
    on_dec: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
    on_inc: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .child(label_block(title, subtitle.to_owned()))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(step_button(dec_id, "−", on_dec))
                .child(
                    div()
                        .min_w(px(72.0))
                        .text_center()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(value),
                )
                .child(step_button(inc_id, "+", on_inc)),
        )
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

fn preset_chip(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    active: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(if active {
            ACCENT_RED.into()
        } else {
            theme::bg_hover()
        })
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if active { TEXT_WHITE } else { TEXT_PRIMARY })
        .transition(press_transition())
        .active(|style| style.scale(0.96))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, handler)
}

fn readonly_chip(label: &'static str, active: bool) -> impl IntoElement {
    div()
        .px_3()
        .py_1p5()
        .rounded_full()
        .bg(if active {
            ACCENT_RED.into()
        } else {
            theme::bg_hover()
        })
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(if active { TEXT_WHITE } else { TEXT_PRIMARY })
        .child(label)
}

fn step_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(26.0))
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

fn toggle_switch(
    id: impl Into<gpui::ElementId>,
    active: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .w(px(46.0))
        .h(px(26.0))
        .p(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if active { ACCENT_RED } else { rgb(0xd8_db_e2) })
        .flex()
        .when(active, |style| style.justify_end())
        .when(!active, |style| style.justify_start())
        .child(
            div()
                .size(px(20.0))
                .rounded_full()
                .bg(rgb(0xff_ff_ff))
                .shadow_sm(),
        )
        .transition(press_transition())
        .active(|style| style.scale(0.96))
        .on_mouse_down(gpui::MouseButton::Left, handler)
}

fn toggle_row(
    title: &str,
    subtitle: impl Into<SharedString>,
    id: impl Into<gpui::ElementId>,
    active: bool,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(label_block(title, subtitle))
        .child(toggle_switch(id, active, handler))
}

fn action_button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_4()
        .py_2()
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

fn value_badge(value: impl Into<SharedString>) -> impl IntoElement {
    div()
        .px_2p5()
        .py_1()
        .rounded_full()
        .bg(theme::bg_pill())
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_SECONDARY)
        .child(value.into())
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
