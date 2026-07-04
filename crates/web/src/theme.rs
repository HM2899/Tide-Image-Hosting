use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ThemeSettings {
    pub(crate) mode: String,
    pub(crate) preset: String,
    pub(crate) radius: i64,
    pub(crate) blur: i64,
    pub(crate) mobile_blur: i64,
    pub(crate) card_opacity: f64,
    pub(crate) primary_color: String,
    pub(crate) accent_color: String,
    pub(crate) background_color: String,
    pub(crate) surface_color: String,
    pub(crate) font: String,
    pub(crate) background_image: String,
    pub(crate) simplify_mobile_effects: bool,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            mode: "light".to_string(),
            preset: "blue_white".to_string(),
            radius: 16,
            blur: 18,
            mobile_blur: 10,
            card_opacity: 0.72,
            primary_color: "#1d6fd8".to_string(),
            accent_color: "#58b7ff".to_string(),
            background_color: "#eef7ff".to_string(),
            surface_color: "#ffffff".to_string(),
            font: "系统圆体".to_string(),
            background_image: String::new(),
            simplify_mobile_effects: true,
        }
    }
}

impl ThemeSettings {
    pub(crate) fn from_value(value: &Value) -> Self {
        let default = Self::default();
        Self {
            mode: json_string_or(value, "mode", &default.mode),
            preset: json_string_or(value, "preset", &default.preset),
            radius: value
                .get("radius")
                .and_then(Value::as_i64)
                .unwrap_or(default.radius),
            blur: value
                .get("blur")
                .and_then(Value::as_i64)
                .unwrap_or(default.blur),
            mobile_blur: value
                .get("mobile_blur")
                .and_then(Value::as_i64)
                .unwrap_or(default.mobile_blur),
            card_opacity: json_f64(value, "card_opacity").unwrap_or(default.card_opacity),
            primary_color: json_string_or(value, "primary_color", &default.primary_color),
            accent_color: json_string_or(value, "accent_color", &default.accent_color),
            background_color: json_string_or(value, "background_color", &default.background_color),
            surface_color: json_string_or(value, "surface_color", &default.surface_color),
            font: json_string_or(value, "font", &default.font),
            background_image: json_string_or(value, "background_image", &default.background_image),
            simplify_mobile_effects: json_bool(
                value,
                "simplify_mobile_effects",
                default.simplify_mobile_effects,
            ),
        }
    }
}

pub(crate) fn theme_css(theme: &ThemeSettings) -> String {
    let background_image = if theme.background_image.trim().is_empty() {
        String::new()
    } else {
        format!(
            "background-image: linear-gradient(135deg, rgba(238,247,255,.86), rgba(255,255,255,.72)), url('{}');",
            css_escape(&theme.background_image)
        )
    };
    format!(
        ":root{{--theme-primary:{primary};--theme-accent:{accent};--theme-bg:{bg};--theme-surface:{surface};--theme-radius:{radius}px;--theme-blur:{blur}px;--theme-mobile-blur:{mobile_blur}px;--theme-card-opacity:{opacity};--theme-font:{font};}}body{{{background_image}}}",
        primary = theme.primary_color,
        accent = theme.accent_color,
        bg = theme.background_color,
        surface = theme.surface_color,
        radius = theme.radius.clamp(0, 32),
        blur = theme.blur.clamp(0, 32),
        mobile_blur = theme.mobile_blur.clamp(0, 24),
        opacity = theme.card_opacity.clamp(0.35, 0.95),
        font = theme.font,
        background_image = background_image,
    )
}

pub(crate) fn theme_preset(value: &str) -> ThemeSettings {
    match value {
        "macaron" => ThemeSettings {
            preset: "macaron".to_string(),
            primary_color: "#5d8fdd".to_string(),
            accent_color: "#ff9bb3".to_string(),
            background_color: "#f3f8ff".to_string(),
            surface_color: "#ffffff".to_string(),
            ..ThemeSettings::default()
        },
        "dark_ocean" => ThemeSettings {
            mode: "dark".to_string(),
            preset: "dark_ocean".to_string(),
            primary_color: "#4f9cff".to_string(),
            accent_color: "#70e0ff".to_string(),
            background_color: "#081724".to_string(),
            surface_color: "#102235".to_string(),
            card_opacity: 0.66,
            ..ThemeSettings::default()
        },
        _ => ThemeSettings::default(),
    }
}

fn json_string_or(value: &Value, key: &str, fallback: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn json_f64(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn json_bool(value: &Value, key: &str, fallback: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

fn css_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}
