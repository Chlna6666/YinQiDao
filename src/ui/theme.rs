#![allow(dead_code)]

use std::time::Duration;

use gpui::{
    Easing, Hsla, IntoElement, Rgba, Styled, Svg, Transition, TransitionProperty, div, hsla,
    prelude::*, px, rgb, svg,
};

// ============================================================================
// Apple Music Color Tokens (Light & Clean Premium Palette)
// ============================================================================

/// Apple Music 标志性纯正红
pub const ACCENT_RED: Rgba = rgb(0xfa_23_3b);
pub const ACCENT_RED_HOVER: Rgba = rgb(0xe0_1f_34);

pub fn accent_red_muted() -> Hsla {
    hsla(348.0, 0.95, 0.56, 0.10)
}

pub fn accent_red_active() -> Hsla {
    hsla(348.0, 0.95, 0.56, 0.18)
}

/// 页面基准底色 (暖调浅灰，苹果桌面级纯净表面)
pub const BG_CANVAS: Rgba = rgb(0xf5_f5_f7);
pub const BG_CANVAS_ALT: Rgba = rgb(0xf0_f0_f3);

/// 侧边栏微半透明磨砂底色
pub const BG_SIDEBAR: Rgba = rgb(0xf4_f5_f8);

/// 高层级卡片与面板白底 (Elevated Card)
pub const BG_CARD: Rgba = rgb(0xff_ff_ff);

pub fn bg_card_transparent() -> Hsla {
    hsla(0.0, 0.0, 1.0, 0.78)
}

/// 交互状态背景
pub fn bg_hover() -> Hsla {
    hsla(220.0, 0.12, 0.92, 0.70)
}

pub fn bg_active() -> Hsla {
    hsla(220.0, 0.16, 0.88, 0.85)
}

pub fn bg_pill() -> Hsla {
    hsla(220.0, 0.10, 0.94, 1.0)
}

/// 发丝级边框与微弱分割线 (Hairline divider)
pub const BORDER_HAIRLINE: Rgba = rgb(0xe6_e7_eb);
pub const BORDER_CARD: Rgba = rgb(0xec_ee_f2);

/// 文字色彩层级 (Apple Typography Hierarchy)
pub const TEXT_PRIMARY: Rgba = rgb(0x1d_1d_1f); // 高雅深灰黑 (非粗暴纯黑)
pub const TEXT_SECONDARY: Rgba = rgb(0x86_86_8b); // 经典次级辅助中性灰
pub const TEXT_TERTIARY: Rgba = rgb(0xa1_a1_a6); // 轻量提示灰
pub const TEXT_WHITE: Rgba = rgb(0xff_ff_ff);

// ============================================================================
// Micro-interactions & Transitions
// ============================================================================

/// 按钮点击压感过渡 (Spring/Cubic ease)
pub fn press_transition() -> Transition {
    Transition::new(Duration::from_millis(120))
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity, TransitionProperty::Transform])
}

/// 悬停色彩平滑过渡
pub fn hover_transition() -> Transition {
    Transition::new(Duration::from_millis(150))
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity, TransitionProperty::Transform])
}

// ============================================================================
// Shared Visual Helpers
// ============================================================================

/// 统一的高清图标渲染
pub fn themed_icon(path: &'static str, size: f32, color: Hsla) -> Svg {
    svg()
        .path(path)
        .size(px(size))
        .text_color(color)
        .flex_none()
}

/// 格式化毫秒为标准时间字符串 03:42
pub fn format_time(ms: u64) -> String {
    let total_secs = ms / 1000;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins:02}:{secs:02}")
}

/// 格式化剩余时间 -01:24
pub fn format_remaining_time(position_ms: u64, duration_ms: u64) -> String {
    if duration_ms <= position_ms {
        return "-00:00".to_string();
    }
    let remaining_ms = duration_ms - position_ms;
    format!("-{}", format_time(remaining_ms))
}

/// Apple 风格正在播放动态声波跳动指示器 (3根音频柱)
pub fn waveform_animation(is_playing: bool) -> impl IntoElement {
    let base_color = hsla(348.0, 0.95, 0.56, 1.0);
    if !is_playing {
        return div()
            .flex()
            .items_end()
            .gap(px(2.0))
            .h(px(14.0))
            .w(px(12.0))
            .child(div().w(px(2.5)).h(px(4.0)).rounded_full().bg(base_color))
            .child(div().w(px(2.5)).h(px(6.0)).rounded_full().bg(base_color))
            .child(div().w(px(2.5)).h(px(3.0)).rounded_full().bg(base_color))
            .into_any_element();
    }

    div()
        .flex()
        .items_end()
        .gap(px(2.0))
        .h(px(14.0))
        .w(px(12.0))
        .child(div().w(px(2.5)).h(px(8.0)).rounded_full().bg(base_color))
        .child(div().w(px(2.5)).h(px(14.0)).rounded_full().bg(base_color))
        .child(div().w(px(2.5)).h(px(10.0)).rounded_full().bg(base_color))
        .into_any_element()
}

/// 根据 ID 衍生雅致封面渐变色，避免纯黑或单调颜色
pub fn elegant_gradient_for(id: i64) -> (Rgba, Rgba) {
    let palettes = [
        (rgb(0x3a_38_97), rgb(0xa3_6f_a9)), // 暮光紫
        (rgb(0x23_25_26), rgb(0x41_43_45)), // 深空曜石
        (rgb(0xd3_10_27), rgb(0xea_38_4d)), // 胭脂红
        (rgb(0x13_4e_5e), rgb(0x71_b2_80)), // 碧海林间
        (rgb(0x2c_3e_50), rgb(0x34_98_db)), // 蔚蓝沉静
        (rgb(0x61_43_85), rgb(0x51_63_95)), // 极光紫蓝
        (rgb(0xb2_45_92), rgb(0xf1_5f_79)), // 晚霞暖瑰
        (rgb(0x1f_1c_2c), rgb(0x92_8d_ab)), // 夜幕轻云
    ];
    palettes[(id.unsigned_abs() as usize) % palettes.len()]
}
