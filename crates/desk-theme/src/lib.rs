//! Theme resolver and token provider for Desk-Remote.
//!
//! Exclusively reads `design/tokens/themes.json` embedded at build time.

use desk_error::{DeskError, DeskResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const THEMES_JSON: &str = include_str!("../../../design/tokens/themes.json");

/// Selected theme mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }
}

/// Strongly typed 27 tokens defined by the Desk-Remote design system contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThemeTokens {
    pub background: String,
    pub surface: String,
    pub surface_alt: String,
    pub surface_hover: String,
    pub surface_pressed: String,
    pub border: String,
    pub focus_ring: String,
    pub text: String,
    pub text_muted: String,
    pub primary: String,
    pub primary_hover: String,
    pub primary_pressed: String,
    pub on_primary: String,
    pub accent: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
    pub info: String,
    pub offline: String,
    pub success_bg: String,
    pub warning_bg: String,
    pub danger_bg: String,
    pub info_bg: String,
    pub offline_bg: String,
    pub disabled_bg: String,
    pub disabled_text: String,
    pub disabled_border: String,
}

impl ThemeTokens {
    fn from_json_map(map: &Value) -> Option<Self> {
        let get = |k: &str| map.get(k)?.as_str().map(|s| s.to_string());

        Some(Self {
            background: get("background")?,
            surface: get("surface")?,
            surface_alt: get("surface-alt")?,
            surface_hover: get("surface-hover")?,
            surface_pressed: get("surface-pressed")?,
            border: get("border")?,
            focus_ring: get("focus-ring")?,
            text: get("text")?,
            text_muted: get("text-muted")?,
            primary: get("primary")?,
            primary_hover: get("primary-hover")?,
            primary_pressed: get("primary-pressed")?,
            on_primary: get("on-primary")?,
            accent: get("accent")?,
            success: get("success")?,
            warning: get("warning")?,
            danger: get("danger")?,
            info: get("info")?,
            offline: get("offline")?,
            success_bg: get("success-bg")?,
            warning_bg: get("warning-bg")?,
            danger_bg: get("danger-bg")?,
            info_bg: get("info-bg")?,
            offline_bg: get("offline-bg")?,
            disabled_bg: get("disabled-bg")?,
            disabled_text: get("disabled-text")?,
            disabled_border: get("disabled-border")?,
        })
    }
}

/// Fully resolved theme ready for application UI rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTheme {
    pub palette: String,
    pub mode: ThemeMode,
    pub is_dark: bool,
    pub tokens: ThemeTokens,
}

/// Returns the list of approved palettes.
pub fn available_palettes() -> Vec<&'static str> {
    vec!["ink", "petrol"]
}

/// Resolves a localized label for a palette.
pub fn palette_label(palette: &str, locale: &str) -> String {
    let root: Value = match serde_json::from_str(THEMES_JSON) {
        Ok(v) => v,
        Err(_) => return palette.to_string(),
    };

    let p = root
        .get("palettes")
        .and_then(|palettes| palettes.get(palette));
    if let Some(lbl) = p.and_then(|pal| pal.get("label")) {
        let key = if locale.starts_with("tr") { "tr" } else { "en" };
        if let Some(val) = lbl.get(key).and_then(|v| v.as_str()) {
            return val.to_string();
        }
    }

    palette.to_string()
}

/// Resolves theme tokens given palette, mode preference, and current OS dark state.
pub fn try_resolve(
    palette: &str,
    mode: ThemeMode,
    system_is_dark: bool,
) -> DeskResult<ResolvedTheme> {
    let effective_palette = match palette {
        "petrol" => "petrol",
        _ => "ink", // default fallback
    };

    let is_dark = match mode {
        ThemeMode::System => system_is_dark,
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
    };

    let root: Value = serde_json::from_str(THEMES_JSON)
        .map_err(|e| DeskError::Theme(format!("Embedded themes.json is invalid JSON: {}", e)))?;

    let variant_key = if is_dark { "dark" } else { "light" };

    let token_map = root
        .get("palettes")
        .and_then(|p| p.get(effective_palette))
        .and_then(|pal| pal.get(variant_key))
        .ok_or_else(|| {
            DeskError::Theme(format!(
                "Missing variant '{}' for palette '{}'",
                variant_key, effective_palette
            ))
        })?;

    let tokens = ThemeTokens::from_json_map(token_map).ok_or_else(|| {
        DeskError::Theme("Missing required theme tokens in themes.json".to_string())
    })?;

    Ok(ResolvedTheme {
        palette: effective_palette.to_string(),
        mode,
        is_dark,
        tokens,
    })
}

/// Resolves theme tokens with safe fallback to Ink Light on internal error.
pub fn resolve(palette: &str, mode: ThemeMode, system_is_dark: bool) -> ResolvedTheme {
    match try_resolve(palette, mode, system_is_dark) {
        Ok(theme) => theme,
        Err(_) => match try_resolve("ink", ThemeMode::Light, false) {
            Ok(fallback) => fallback,
            Err(_) => ResolvedTheme {
                palette: "ink".to_string(),
                mode,
                is_dark: false,
                tokens: ThemeTokens {
                    background: String::new(),
                    surface: String::new(),
                    surface_alt: String::new(),
                    surface_hover: String::new(),
                    surface_pressed: String::new(),
                    border: String::new(),
                    focus_ring: String::new(),
                    text: String::new(),
                    text_muted: String::new(),
                    primary: String::new(),
                    primary_hover: String::new(),
                    primary_pressed: String::new(),
                    on_primary: String::new(),
                    accent: String::new(),
                    success: String::new(),
                    warning: String::new(),
                    danger: String::new(),
                    info: String::new(),
                    offline: String::new(),
                    success_bg: String::new(),
                    warning_bg: String::new(),
                    danger_bg: String::new(),
                    info_bg: String::new(),
                    offline_bg: String::new(),
                    disabled_bg: String::new(),
                    disabled_text: String::new(),
                    disabled_border: String::new(),
                },
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_palettes_and_modes_resolve() {
        for pal in &["ink", "petrol"] {
            // Light resolution
            let theme_light = resolve(pal, ThemeMode::Light, false);
            assert_eq!(theme_light.palette, *pal);
            assert!(!theme_light.is_dark);
            assert!(theme_light.tokens.background.starts_with('#'));
            assert_eq!(theme_light.tokens.background.len(), 7);

            // Dark resolution
            let theme_dark = resolve(pal, ThemeMode::Dark, false);
            assert_eq!(theme_dark.palette, *pal);
            assert!(theme_dark.is_dark);
            assert!(theme_dark.tokens.background.starts_with('#'));
            assert_eq!(theme_dark.tokens.background.len(), 7);

            // System resolution
            let theme_sys_light = resolve(pal, ThemeMode::System, false);
            assert!(!theme_sys_light.is_dark);

            let theme_sys_dark = resolve(pal, ThemeMode::System, true);
            assert!(theme_sys_dark.is_dark);
        }
    }

    #[test]
    fn test_unknown_palette_fallback_to_ink() {
        let theme = resolve("unknown_custom_pal", ThemeMode::Light, false);
        assert_eq!(theme.palette, "ink");
    }

    #[test]
    fn test_labels() {
        assert_eq!(palette_label("ink", "en"), "Ink");
        assert_eq!(palette_label("petrol", "en"), "Petrol");
    }
}
