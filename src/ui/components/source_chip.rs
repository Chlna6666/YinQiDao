//! 多来源来源标识（Source chip）。
//!
//! 当用户同时启用多个音乐插件（网易云 + QQ音乐 + …）时，列表/卡片需要标明内容来自哪个
//! 来源。设计规范：`text_xs` 级小圆片、`bg_pill` 底、`TEXT_TERTIARY` 文字；仅在多来源
//! 混合视图下显示，单一来源时不加噪。

use gpui::{AnyElement, div, prelude::*, px};

use crate::ui::theme;

/// 来源标识小圆片。`label` 为 provider 显示名（来自 Host 目录）。
pub fn source_chip(label: &str) -> AnyElement {
    div()
        .flex_none()
        .px_1p5()
        .py(px(1.0))
        .rounded_full()
        .bg(theme::bg_pill())
        .border_1()
        .border_color(theme::BORDER_HAIRLINE)
        .text_xs()
        .text_size(px(theme::FONT_CAPTION_MIN))
        .text_color(theme::TEXT_TERTIARY)
        .truncate()
        .max_w(px(72.0))
        .child(label.to_string())
        .into_any_element()
}

/// 多来源判定 + 显示名提取：仅当启用来源 ≥2 时才返回标识文案。
pub fn provider_label_for(
    labels: &std::collections::BTreeMap<String, String>,
    provider_id: &str,
) -> Option<String> {
    if labels.len() > 1 {
        labels.get(provider_id).cloned()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_label_only_in_multi_source_mode() {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("netease".into(), "网易云音乐".into());
        assert_eq!(provider_label_for(&labels, "netease"), None);
        labels.insert("qqmusic".into(), "QQ音乐".into());
        assert_eq!(
            provider_label_for(&labels, "qqmusic"),
            Some("QQ音乐".into())
        );
        assert_eq!(provider_label_for(&labels, "unknown"), None);
    }

    #[test]
    fn source_chip_renders_label_text() {
        // GPUI 元素树在纯构建期不便于断言文本；这里只保证函数不 panic 并产生元素。
        let _chip = source_chip("QQ音乐");
        let _chip = source_chip("");
    }
}
