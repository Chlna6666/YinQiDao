use gpui::{Rgba, rgb, rgba};

use crate::{plugin::extensions::PluginThemeSnapshot, ui::theme};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PluginPageTheme {
    pub background: Rgba,
    pub surface: Rgba,
    pub surface_elevated: Rgba,
    pub text_primary: Rgba,
    pub text_secondary: Rgba,
    pub text_tertiary: Rgba,
    pub accent: Rgba,
    pub border: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub error: Rgba,
    pub radius_small: f32,
    pub radius_medium: f32,
    pub radius_large: f32,
}

impl PluginPageTheme {
    pub fn host() -> Self {
        Self {
            background: theme::BG_CANVAS,
            surface: theme::BG_CARD,
            surface_elevated: theme::BG_CARD,
            text_primary: theme::TEXT_PRIMARY,
            text_secondary: theme::TEXT_SECONDARY,
            text_tertiary: theme::TEXT_TERTIARY,
            accent: theme::ACCENT_RED,
            border: theme::BORDER_CARD,
            success: rgb(0x34_c7_59),
            warning: rgb(0xff_9f_0a),
            error: theme::ACCENT_RED,
            radius_small: 8.0,
            radius_medium: 12.0,
            radius_large: 16.0,
        }
    }

    /// Convert an already Host-validated static plugin Theme snapshot into paint-ready values.
    ///
    /// Parsing belongs to the controller/control path. The resulting value is plain Copy data, so
    /// GPUI render/paint never touches plugin files, parses strings or executes guest code.
    pub fn try_from_snapshot(snapshot: &PluginThemeSnapshot) -> Result<Self, String> {
        let host = Self::host();
        let background = parse_optional("background", snapshot.background.as_deref(), host.background)?;
        let surface = parse_optional("surface", snapshot.surface.as_deref(), host.surface)?;
        let surface_elevated = parse_optional(
            "surface_elevated",
            snapshot.surface_elevated.as_deref(),
            if snapshot.surface.is_some() {
                surface
            } else {
                host.surface_elevated
            },
        )?;
        let text_primary = parse_optional(
            "text_primary",
            snapshot.text_primary.as_deref(),
            host.text_primary,
        )?;
        let text_secondary = parse_optional(
            "text_secondary",
            snapshot.text_secondary.as_deref(),
            host.text_secondary,
        )?;
        let text_tertiary = if snapshot.text_secondary.is_some() {
            text_secondary.opacity(0.82)
        } else {
            host.text_tertiary
        };
        let accent = parse_optional("accent", snapshot.accent.as_deref(), host.accent)?;
        let border = parse_optional("border", snapshot.border.as_deref(), host.border)?;
        let success = parse_optional("success", snapshot.success.as_deref(), host.success)?;
        let warning = parse_optional("warning", snapshot.warning.as_deref(), host.warning)?;
        let error = parse_optional("error", snapshot.error.as_deref(), host.error)?;

        Ok(Self {
            background,
            surface,
            surface_elevated,
            text_primary,
            text_secondary,
            text_tertiary,
            accent,
            border,
            success,
            warning,
            error,
            radius_small: bounded_radius(snapshot.radius_small, host.radius_small),
            radius_medium: bounded_radius(snapshot.radius_medium, host.radius_medium),
            radius_large: bounded_radius(snapshot.radius_large, host.radius_large),
        })
    }

    pub fn accent_muted(self) -> Rgba {
        self.accent.alpha(0.12)
    }

    pub fn accent_foreground(self) -> Rgba {
        let effective = self.surface.blend(self.accent);
        if relative_luminance(effective) > 0.48 {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_WHITE
        }
    }
}

fn bounded_radius(value: Option<u16>, fallback: f32) -> f32 {
    value.map_or(fallback, |value| f32::from(value.min(64)))
}

fn parse_optional(name: &str, value: Option<&str>, fallback: Rgba) -> Result<Rgba, String> {
    match value {
        Some(value) => parse_hex_color(value).map_err(|error| format!("Theme token {name}: {error}")),
        None => Ok(fallback),
    }
}

fn parse_hex_color(value: &str) -> Result<Rgba, String> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| "颜色必须以 # 开头".to_owned())?;
    let parsed = u32::from_str_radix(hex, 16).map_err(|_| "颜色包含非法十六进制字符".to_owned())?;
    match hex.len() {
        6 => Ok(rgb(parsed)),
        8 => Ok(rgba(parsed)),
        _ => Err("颜色仅接受 #RRGGBB/#RRGGBBAA".to_owned()),
    }
}

fn relative_luminance(color: Rgba) -> f32 {
    fn channel(value: f32) -> f32 {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_converted_to_paint_ready_values() {
        let snapshot = PluginThemeSnapshot {
            accent: Some("#33669980".into()),
            text_secondary: Some("#778899".into()),
            radius_large: Some(64),
            ..PluginThemeSnapshot::default()
        };
        let palette = PluginPageTheme::try_from_snapshot(&snapshot).expect("valid theme");
        assert!((palette.accent.r - 0x33 as f32 / 255.0).abs() < f32::EPSILON);
        assert!((palette.accent.a - 0x80 as f32 / 255.0).abs() < f32::EPSILON);
        assert_eq!(palette.radius_large, 64.0);
    }

    #[test]
    fn invalid_color_fails_closed() {
        let snapshot = PluginThemeSnapshot {
            accent: Some("red".into()),
            ..PluginThemeSnapshot::default()
        };
        assert!(PluginPageTheme::try_from_snapshot(&snapshot).is_err());
    }
}
