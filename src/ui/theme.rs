#![allow(dead_code)]

use std::time::{Duration, Instant};

use gpui::{Easing, Hsla, Rgba, Styled, Svg, Transition, TransitionProperty, hsla, px, rgb, svg};

// ============================================================================
// 色彩 · 浅色页面（Apple Music Light & Clean Premium Palette）
// 语义：ACCENT 交互强调 / BG 分层表面 / BORDER 发丝边界 / TEXT 文字层级
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

/// 强调色描边（选中卡片边框等）
pub fn accent_red_border() -> Hsla {
    hsla(348.0, 0.95, 0.56, 0.25)
}

/// 强调色柔和高亮底（选中播放列表药丸），比 muted 更实的粉底
pub fn accent_red_soft() -> Hsla {
    hsla(348.0, 0.90, 0.95, 1.0)
}

/// 强调色柔和描边
pub fn accent_red_soft_border() -> Hsla {
    hsla(348.0, 0.90, 0.60, 0.30)
}

/// 语义状态前景/底色（成功/警告/错误），供状态药丸与提示组件使用
pub fn success_fg() -> Hsla {
    hsla(140.0, 0.45, 0.36, 1.0)
}

pub fn success_tint(alpha: f32) -> Hsla {
    hsla(140.0, 0.45, 0.45, alpha)
}

pub fn warning_fg() -> Hsla {
    hsla(38.0, 0.75, 0.36, 1.0)
}

pub fn warning_tint(alpha: f32) -> Hsla {
    hsla(40.0, 0.85, 0.50, alpha)
}

pub fn error_tint(alpha: f32) -> Hsla {
    hsla(8.0, 0.72, 0.52, alpha)
}

pub fn info_fg() -> Hsla {
    hsla(210.0, 0.75, 0.45, 1.0)
}

pub fn info_tint(alpha: f32) -> Hsla {
    hsla(210.0, 0.65, 0.50, alpha)
}

/// 语义状态色（成功 / 警告 / 信息；对齐 Apple 系统色）
pub const SUCCESS_GREEN: Rgba = rgb(0x34_c7_59);
pub const WARNING_AMBER: Rgba = rgb(0xff_9f_0a);
pub const INFO_BLUE: Rgba = rgb(0x00_77_ff);

/// macOS 红绿灯（窗口与舞台标题栏共用一套）
pub const TRAFFIC_LIGHT_RED: Rgba = rgb(0xff_5f_56);
pub const TRAFFIC_LIGHT_AMBER: Rgba = rgb(0xff_bd_2e);
pub const TRAFFIC_LIGHT_GREEN: Rgba = rgb(0x27_c9_3f);

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

/// 全透明占位（"无背景"语义）
pub fn transparent() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.0)
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
// 色彩 · 深色舞台世界（沉浸 Stage / 桌面歌词 / 调试窗）
// 近黑表面收敛为三档；白 alpha 走 dark:: 语义函数，效果性 glow 保留显式 alpha
// ============================================================================

/// 沉浸舞台基准底色（统一 0x0e0f16，收敛 0x0c0f14/0x101318 等漂移值）
pub const STAGE_BG: Rgba = rgb(0x0e0f16);
/// 更深一层的下沉表面（调试窗背景、舞台内嵌面板）
pub const STAGE_BG_SUNKEN: Rgba = rgb(0x070a0e);
/// 浮于舞台之上的面板（调试窗卡片）
pub const STAGE_BG_ELEVATED: Rgba = rgb(0x171c23);

pub mod dark {
    use gpui::{Hsla, hsla};

    /// 深色表面主文字（标题、强数值）
    pub fn text_primary() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.96)
    }

    /// 深色表面次级文字（副标题、说明）
    pub fn text_secondary() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.72)
    }

    /// 深色表面三级文字（弱提示、时间戳）
    pub fn text_tertiary() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.62)
    }

    /// 深色表面四级文字/描边（最弱层级）
    pub fn text_quaternary() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.45)
    }

    /// 深色表面悬停填充（0.08/0.10 两档收敛）
    pub fn hover_fill() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.09)
    }

    /// 深色表面悬停填充·强（音量/模式按钮等高亮反馈）
    pub fn hover_fill_strong() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.16)
    }

    /// 效果性白色（glow、图标填充、渐变端点等艺术参数），显式 alpha 保留
    pub fn white(alpha: f32) -> Hsla {
        hsla(0.0, 0.0, 1.0, alpha)
    }

    /// 黑色遮罩/阴影（scrim），显式 alpha
    pub fn scrim(alpha: f32) -> Hsla {
        hsla(0.0, 0.0, 0.0, alpha)
    }
    /// 深色表面描边（调试窗卡片边框等）
    pub fn border() -> Hsla {
        hsla(0.0, 0.0, 1.0, 0.10)
    }
}

// ============================================================================
// 字体排印（对齐 Apple Text Styles：caption→title3）
// 12/14/16/18/20/24 为正文阶梯；文字统一走 gpui text_xs/sm/base/lg/xl/2xl
// ============================================================================

/// 图标尺寸 token（15 种历史尺寸就近归一）
pub const ICON_SM: f32 = 13.0;
pub const ICON_MD: f32 = 16.0;
pub const ICON_LG: f32 = 20.0;
pub const ICON_XL: f32 = 24.0;

/// 正文阶梯最小可读字号（Apple caption2）；替代历史 px(9.5) 徽标微字号
pub const FONT_CAPTION_MIN: f32 = 11.0;

// ============================================================================
// 间距 · 4pt 网格
// 优先使用 gpui 命名 scale（gap_1..8、px_*/py_* 及 *_0p5/_1p5/_2p5 半步），
// 命名 scale 覆盖不了的精确值再使用以下常量。
// ============================================================================

/// 发丝级间距（文本堆叠微间距）
pub const SPACE_HAIRLINE: f32 = 1.0;

// ============================================================================
// 圆角 · 语义约定：行/ Chips=SM(6)、小面板=MD(8)、卡片=LG(12)、海报/大卡=XL(14)
// ============================================================================

pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 8.0;
pub const RADIUS_LG: f32 = 12.0;
pub const RADIUS_XL: f32 = 14.0;

// ============================================================================
// 动效 · 时长与过渡 token
// hover_transition = hover 态变化；press_transition = 按压/位移反馈
// ============================================================================

pub const DURATION_FAST: Duration = Duration::from_millis(120);
pub const DURATION_STANDARD: Duration = Duration::from_millis(150);
pub const DURATION_EMPHASIZED: Duration = Duration::from_millis(220);
pub const DURATION_LAYOUT: Duration = Duration::from_millis(300);

/// 按钮点击压感过渡
pub fn press_transition() -> Transition {
    Transition::new(DURATION_FAST)
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity, TransitionProperty::Transform])
}

/// 悬停色彩平滑过渡
pub fn hover_transition() -> Transition {
    Transition::new(DURATION_STANDARD)
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity, TransitionProperty::Transform])
}

/// 按压缩放 token（收敛历史 0.90–0.995 九档）
pub const PRESS_STRONG: f32 = 0.92; // 小型图标控制键（红绿灯、迷你传输键）
pub const PRESS_STANDARD: f32 = 0.96; // 列表行、卡片、chips（默认）
pub const PRESS_SUBTLE: f32 = 0.98; // 大面积可点区域（模态、侧栏、插件 UI）
/// 卡片悬停放大
pub const HOVER_SCALE: f32 = 1.02;

// ============================================================================
// 动效 · 可打断采样运动（Apple HIG interruptible motion）
// set_target 从当前采样值平滑重定向到新目标（时长按剩余距离缩放），
// advance 每帧推进，渲染层用 value() 采样 —— 全仓唯一的采样运动实现。
// ============================================================================

/// 标准 ease-in-out cubic（对称缓入缓出）
pub fn ease_in_out_cubic(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress * progress * progress
    } else {
        1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
    }
}

/// 标准 ease-out cubic（快出缓停）
pub fn ease_out_cubic(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug)]
pub struct SampledMotion {
    base_duration: Duration,
    ease: fn(f32) -> f32,
    value: f32,
    from: f32,
    to: f32,
    started_at: Option<Instant>,
    duration: Duration,
}

impl SampledMotion {
    /// `visible` 为初始状态（false=0.0，true=1.0），运动范围归一化到 0..=1。
    pub fn new(base_duration: Duration, ease: fn(f32) -> f32, visible: bool) -> Self {
        let value = if visible { 1.0 } else { 0.0 };
        Self {
            base_duration,
            ease,
            value,
            from: value,
            to: value,
            started_at: None,
            duration: base_duration,
        }
    }

    fn sample(&self, now: Instant) -> f32 {
        let Some(started_at) = self.started_at else {
            return self.value;
        };
        if self.duration.is_zero() {
            return self.to;
        }
        let linear =
            now.saturating_duration_since(started_at).as_secs_f32() / self.duration.as_secs_f32();
        let eased = (self.ease)(linear);
        self.from + (self.to - self.from) * eased
    }

    /// 重定向到 0/1 目标；返回 true 表示开启了新的运动段。
    pub fn set_target(&mut self, visible: bool) -> bool {
        let target = if visible { 1.0 } else { 0.0 };
        if (self.to - target).abs() <= 0.001 {
            return false;
        }

        let now = Instant::now();
        let current = self.sample(now).clamp(0.0, 1.0);
        self.value = current;
        self.from = current;
        self.to = target;

        let distance = (target - current).abs();
        if distance <= 0.001 {
            self.value = target;
            self.from = target;
            self.started_at = None;
            return true;
        }

        self.duration =
            Duration::from_secs_f32((self.base_duration.as_secs_f32() * distance).max(0.001));
        self.started_at = Some(now);
        true
    }

    /// 推进到 `now`；返回 true 表示仍在运动中（需要继续逐帧采样）。
    pub fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };
        self.value = self.sample(now).clamp(0.0, 1.0);
        if now.saturating_duration_since(started_at) >= self.duration {
            self.value = self.to;
            self.from = self.to;
            self.started_at = None;
            false
        } else {
            true
        }
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.value.clamp(0.0, 1.0)
    }

    #[inline]
    pub fn target_value(&self) -> f32 {
        self.to.clamp(0.0, 1.0)
    }

    /// 当前运动段的结束时刻（用于逐帧 invalidation 调度）。
    pub fn deadline(&self) -> Option<Instant> {
        self.started_at.map(|started_at| started_at + self.duration)
    }
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
