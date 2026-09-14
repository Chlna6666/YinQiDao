use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginThemeTokens {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub surface_elevated: Option<String>,
    #[serde(default)]
    pub text_primary: Option<String>,
    #[serde(default)]
    pub text_secondary: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub success: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub radius_small: Option<u16>,
    #[serde(default)]
    pub radius_medium: Option<u16>,
    #[serde(default)]
    pub radius_large: Option<u16>,
}

impl PluginThemeTokens {
    pub fn is_empty(&self) -> bool {
        self.background.is_none()
            && self.surface.is_none()
            && self.surface_elevated.is_none()
            && self.text_primary.is_none()
            && self.text_secondary.is_none()
            && self.accent.is_none()
            && self.border.is_none()
            && self.success.is_none()
            && self.warning.is_none()
            && self.error.is_none()
            && self.radius_small.is_none()
            && self.radius_medium.is_none()
            && self.radius_large.is_none()
    }
}

pub fn validate_theme_tokens(tokens: &PluginThemeTokens) -> Result<()> {
    if tokens.is_empty() {
        bail!("插件 Theme 至少需要一个 semantic token");
    }
    for (name, value) in [
        ("background", tokens.background.as_deref()),
        ("surface", tokens.surface.as_deref()),
        ("surface_elevated", tokens.surface_elevated.as_deref()),
        ("text_primary", tokens.text_primary.as_deref()),
        ("text_secondary", tokens.text_secondary.as_deref()),
        ("accent", tokens.accent.as_deref()),
        ("border", tokens.border.as_deref()),
        ("success", tokens.success.as_deref()),
        ("warning", tokens.warning.as_deref()),
        ("error", tokens.error.as_deref()),
    ] {
        if let Some(value) = value
            && !valid_hex_color(value)
        {
            bail!("插件 Theme token {name} 仅接受 #RRGGBB/#RRGGBBAA");
        }
    }
    for (name, value) in [
        ("radius_small", tokens.radius_small),
        ("radius_medium", tokens.radius_medium),
        ("radius_large", tokens.radius_large),
    ] {
        if value.is_some_and(|value| value > 64) {
            bail!("插件 Theme token {name} 超过 64px Host 上限");
        }
    }
    Ok(())
}

fn valid_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_colors_are_bounded_hex_values() {
        let mut tokens = PluginThemeTokens {
            accent: Some("#ff00aa".into()),
            ..PluginThemeTokens::default()
        };
        assert!(validate_theme_tokens(&tokens).is_ok());
        tokens.accent = Some("url(https://example.com/x)".into());
        assert!(validate_theme_tokens(&tokens).is_err());
    }
}
