//! Theming core (PLAN-GUI §11.1).
//!
//! Semantic token model: a `Theme` is a bundle of color, markdown, metric,
//! and font tokens — not a `egui::Visuals` clone. Built-ins ship compiled
//! in; the `to_visuals()` path projects the tokens onto the chrome, while
//! markdown tokens feed the renderer (G6-2 will wire those).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

impl Appearance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        }
    }
}

impl std::fmt::Display for Appearance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Appearance {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dark" => Ok(Appearance::Dark),
            "light" => Ok(Appearance::Light),
            _ => Err(format!("appearance: expected `dark` or `light`, got `{s}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    #[serde(default = "default_appearance")]
    pub appearance: Appearance,
    #[serde(default)]
    pub colors: ColorTokens,
    #[serde(default)]
    pub markdown: MarkdownTokens,
    #[serde(default)]
    pub metrics: Metrics,
    #[serde(default)]
    pub fonts: Fonts,
}

fn default_appearance() -> Appearance {
    Appearance::Dark
}

// Every token field is `#[serde(default)]` so a custom TOML can omit any
// token and it falls back to the type's default (PLAN-GUI §11.2 "missing
// tokens fall back"; per-field fallback is the G6-1 form of that rule).

fn default_density() -> f32 {
    1.0
}
fn default_corner() -> f32 {
    6.0
}
fn default_row_height() -> f32 {
    22.0
}
fn default_max_width() -> f32 {
    760.0
}
fn default_ui_family() -> String {
    "Inter".to_string()
}
fn default_mono_family() -> String {
    "JetBrains Mono".to_string()
}
fn default_ui_size() -> f32 {
    13.0
}
fn default_mono_size() -> f32 {
    12.5
}

/// Chrome color tokens (PLAN-GUI §11.1 `colors`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ColorTokens {
    #[serde(default)]
    pub bg: Color,
    #[serde(default)]
    pub bg_raised: Color,
    #[serde(default)]
    pub bg_sunken: Color,
    #[serde(default)]
    pub bg_hover: Color,
    #[serde(default)]
    pub bg_active: Color,
    #[serde(default)]
    pub text: Color,
    #[serde(default)]
    pub text_muted: Color,
    #[serde(default)]
    pub text_faint: Color,
    #[serde(default)]
    pub border: Color,
    #[serde(default)]
    pub border_strong: Color,
    #[serde(default)]
    pub accent: Color,
    #[serde(default)]
    pub accent_text: Color,
    #[serde(default)]
    pub selection: Color,
    #[serde(default)]
    pub focus_ring: Color,
    #[serde(default)]
    pub rail_bg: Color,
    #[serde(default)]
    pub list_bg: Color,
    #[serde(default)]
    pub editor_bg: Color,
    #[serde(default)]
    pub panel_bg: Color,
    #[serde(default)]
    pub status_ok: Color,
    #[serde(default)]
    pub status_warn: Color,
    #[serde(default)]
    pub status_error: Color,
    #[serde(default)]
    pub gated: Color,
    #[serde(default)]
    pub ungated: Color,
    #[serde(default)]
    pub dirty: Color,
    #[serde(default)]
    pub conflict: Color,
}

/// Markdown content tokens (PLAN-GUI §11.1 `markdown`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct MarkdownTokens {
    #[serde(default)]
    pub h1: Color,
    #[serde(default)]
    pub h2: Color,
    #[serde(default)]
    pub h3: Color,
    #[serde(default)]
    pub h4: Color,
    #[serde(default)]
    pub h5: Color,
    #[serde(default)]
    pub h6: Color,
    #[serde(default)]
    pub body: Color,
    #[serde(default)]
    pub bold: Color,
    #[serde(default)]
    pub italic: Color,
    #[serde(default)]
    pub code: Color,
    #[serde(default)]
    pub code_bg: Color,
    #[serde(default)]
    pub quote: Color,
    #[serde(default)]
    pub quote_bar: Color,
    #[serde(default)]
    pub link: Color,
    #[serde(default)]
    pub link_hover: Color,
    #[serde(default)]
    pub list_marker: Color,
    #[serde(default)]
    pub table_header: Color,
    #[serde(default)]
    pub table_grid: Color,
    #[serde(default)]
    pub rule: Color,
    #[serde(default)]
    pub syntax_error: Color,
}

/// Layout metric tokens (PLAN-GUI §11.1 `metrics`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(default = "default_density")]
    pub density: f32,
    #[serde(default = "default_corner")]
    pub corner_radius: f32,
    #[serde(default = "default_row_height")]
    pub row_height: f32,
    #[serde(default = "default_max_width")]
    pub content_max_width: f32,
}

/// Font tokens (PLAN-GUI §11.1 `fonts`). `ui_file` / `mono_file` are
/// optional and *never read before frame 1* (PLAN-GUI §11.1 bullet: "Font
/// files are never read before frame 1").
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Fonts {
    #[serde(default = "default_ui_family")]
    pub ui_family: String,
    #[serde(default = "default_mono_family")]
    pub mono_family: String,
    #[serde(default = "default_ui_size")]
    pub ui_size: f32,
    #[serde(default = "default_mono_size")]
    pub mono_size: f32,
    #[serde(default)]
    pub ui_file: Option<PathBuf>,
    #[serde(default)]
    pub mono_file: Option<PathBuf>,
}

/// A theme color. Serialized as a `#RRGGBB` hex string; stored as 0xAARRGGBB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(u32);

impl Default for Color {
    fn default() -> Self {
        Color(0xFF000000)
    }
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Color(0xFF000000 | (r as u32) << 16 | (g as u32) << 8 | (b as u32))
    }
    pub const fn r(self) -> u8 {
        ((self.0 >> 16) & 0xFF) as u8
    }
    pub const fn g(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }
    pub const fn b(self) -> u8 {
        (self.0 & 0xFF) as u8
    }
    #[allow(dead_code)] // reserved for the G6-2 renderer / palette export
    pub const fn to_u32(self) -> u32 {
        self.0
    }
    pub fn rel_luminance(self) -> f32 {
        srgb_lin(self.r()) * 0.2126 + srgb_lin(self.g()) * 0.7152 + srgb_lin(self.b()) * 0.0722
    }
    pub fn contrast_with(self, other: Color) -> f32 {
        let l1 = self.rel_luminance();
        let l2 = other.rel_luminance();
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }
}

fn srgb_lin(v: u8) -> f32 {
    let v = (v as f32) / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

impl Color {
    pub fn as_color32(self) -> egui::Color32 {
        egui::Color32::from_rgb(self.r(), self.g(), self.b())
    }

    /// Parse a `#RGB` (shorthand), `#RRGGBB`, or `0xRRGGBB` hex string.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s
            .strip_prefix('#')
            .or_else(|| s.strip_prefix("0x"))
            .unwrap_or(s);
        let mut nibbles: Vec<u8> = Vec::new();
        for c in s.chars() {
            nibbles.push(c.to_digit(16)? as u8);
        }
        match nibbles.len() {
            3 => Some(Color::new(
                nibbles[0].wrapping_mul(17),
                nibbles[1].wrapping_mul(17),
                nibbles[2].wrapping_mul(17),
            )),
            6 => Some(Color::new(
                nibbles[0].wrapping_mul(16) + nibbles[1],
                nibbles[2].wrapping_mul(16) + nibbles[3],
                nibbles[4].wrapping_mul(16) + nibbles[5],
            )),
            _ => None,
        }
    }

    /// `#RRGGBB`.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r(), self.g(), self.b())
    }
}

impl std::fmt::Display for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_hex().as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::from_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color: {s}")))
    }
}

impl serde::Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

/// Marknote Dark — the default theme (PLAN-GUI §11.1).
pub fn marknote_dark() -> Theme {
    use Appearance::Dark;
    let c = ColorTokens {
        bg: Color::new(0x11, 0x12, 0x14),
        bg_raised: Color::new(0x1a, 0x1b, 0x1e),
        bg_sunken: Color::new(0x0c, 0x0d, 0x0f),
        bg_hover: Color::new(0x23, 0x25, 0x29),
        bg_active: Color::new(0x2c, 0x2f, 0x34),
        text: Color::new(0xe6, 0xe6, 0xe6),
        text_muted: Color::new(0x9b, 0x9f, 0xa6),
        text_faint: Color::new(0x60, 0x64, 0x6b),
        border: Color::new(0x2c, 0x2f, 0x34),
        border_strong: Color::new(0x42, 0x46, 0x4d),
        accent: Color::new(0x7c, 0xa2, 0xf0),
        accent_text: Color::new(0x0f, 0x0f, 0x11),
        selection: Color::new(0x2b, 0x3d, 0x63),
        focus_ring: Color::new(0x7c, 0xa2, 0xf0),
        rail_bg: Color::new(0x0e, 0x0f, 0x11),
        list_bg: Color::new(0x14, 0x15, 0x18),
        editor_bg: Color::new(0x11, 0x12, 0x14),
        panel_bg: Color::new(0x1a, 0x1b, 0x1e),
        status_ok: Color::new(0x5c, 0xd1, 0x6a),
        status_warn: Color::new(0xe0, 0xb8, 0x4f),
        status_error: Color::new(0xe0, 0x6c, 0x6c),
        gated: Color::new(0x8f, 0x7e, 0xd8),
        ungated: Color::new(0x6b, 0x71, 0x7a),
        dirty: Color::new(0xe0, 0xb8, 0x4f),
        conflict: Color::new(0xe0, 0x6c, 0x6c),
    };
    let m = MarkdownTokens {
        h1: Color::new(0xf2, 0xf2, 0xf2),
        h2: Color::new(0xe6, 0xe6, 0xe6),
        h3: Color::new(0xd9, 0xd9, 0xd9),
        h4: Color::new(0xcc, 0xcc, 0xcc),
        h5: Color::new(0xc0, 0xc0, 0xc0),
        h6: Color::new(0xb5, 0xb5, 0xb5),
        body: Color::new(0xd6, 0xd6, 0xd6),
        bold: Color::new(0xf2, 0xf2, 0xf2),
        italic: Color::new(0xcb, 0xcb, 0xcb),
        code: Color::new(0x9c, 0xd8, 0xb0),
        code_bg: Color::new(0x1c, 0x20, 0x24),
        quote: Color::new(0xb0, 0xb4, 0xbb),
        quote_bar: Color::new(0x7c, 0xa2, 0xf0),
        link: Color::new(0x7c, 0xa2, 0xf0),
        link_hover: Color::new(0xa2, 0xc0, 0xf8),
        list_marker: Color::new(0x9b, 0x9f, 0xa6),
        table_header: Color::new(0x23, 0x26, 0x2b),
        table_grid: Color::new(0x2c, 0x2f, 0x34),
        rule: Color::new(0x2c, 0x2f, 0x34),
        syntax_error: Color::new(0xe0, 0x6c, 0x6c),
    };
    Theme {
        name: "marknote-dark".to_string(),
        appearance: Dark,
        colors: c,
        markdown: m,
        metrics: Metrics {
            density: 1.0,
            corner_radius: 6.0,
            row_height: 22.0,
            content_max_width: 760.0,
        },
        fonts: Fonts::default(),
    }
}

/// Marknote Light.
pub fn marknote_light() -> Theme {
    use Appearance::Light;
    let c = ColorTokens {
        bg: Color::new(0xfb, 0xfb, 0xfa),
        bg_raised: Color::new(0xff, 0xff, 0xff),
        bg_sunken: Color::new(0xf0, 0xf0, 0xef),
        bg_hover: Color::new(0xec, 0xec, 0xea),
        bg_active: Color::new(0xe2, 0xe2, 0xdf),
        text: Color::new(0x1c, 0x1e, 0x21),
        text_muted: Color::new(0x5b, 0x5f, 0x66),
        text_faint: Color::new(0x9a, 0x9e, 0xa4),
        border: Color::new(0xe0, 0xe0, 0xdd),
        border_strong: Color::new(0xc4, 0xc6, 0xc9),
        accent: Color::new(0x2f, 0x5d, 0xd8),
        accent_text: Color::new(0xff, 0xff, 0xff),
        selection: Color::new(0xd8, 0xe4, 0xfa),
        focus_ring: Color::new(0x2f, 0x5d, 0xd8),
        rail_bg: Color::new(0xf4, 0xf4, 0xf2),
        list_bg: Color::new(0xff, 0xff, 0xff),
        editor_bg: Color::new(0xff, 0xff, 0xff),
        panel_bg: Color::new(0xff, 0xff, 0xff),
        status_ok: Color::new(0x23, 0x6b, 0x2c),
        status_warn: Color::new(0x7a, 0x5a, 0x12),
        status_error: Color::new(0xc4, 0x3b, 0x3b),
        gated: Color::new(0x6b, 0x57, 0xc0),
        ungated: Color::new(0x8a, 0x8e, 0x94),
        dirty: Color::new(0x7a, 0x5a, 0x12),
        conflict: Color::new(0xc4, 0x3b, 0x3b),
    };
    let m = MarkdownTokens {
        h1: Color::new(0x12, 0x14, 0x17),
        h2: Color::new(0x1c, 0x1e, 0x21),
        h3: Color::new(0x24, 0x27, 0x2b),
        h4: Color::new(0x2c, 0x2f, 0x34),
        h5: Color::new(0x34, 0x38, 0x3d),
        h6: Color::new(0x3c, 0x40, 0x45),
        body: Color::new(0x2b, 0x2e, 0x32),
        bold: Color::new(0x12, 0x14, 0x17),
        italic: Color::new(0x34, 0x38, 0x3d),
        code: Color::new(0x1f, 0x7a, 0x3a),
        code_bg: Color::new(0xee, 0xf2, 0xef),
        quote: Color::new(0x5b, 0x5f, 0x66),
        quote_bar: Color::new(0x2f, 0x5d, 0xd8),
        link: Color::new(0x2f, 0x5d, 0xd8),
        link_hover: Color::new(0x1f, 0x46, 0xa8),
        list_marker: Color::new(0x5b, 0x5f, 0x66),
        table_header: Color::new(0xef, 0xef, 0xec),
        table_grid: Color::new(0xe0, 0xe0, 0xdd),
        rule: Color::new(0xe0, 0xe0, 0xdd),
        syntax_error: Color::new(0xc4, 0x3b, 0x3b),
    };
    Theme {
        name: "marknote-light".to_string(),
        appearance: Light,
        colors: c,
        markdown: m,
        metrics: Metrics {
            density: 1.0,
            corner_radius: 6.0,
            row_height: 22.0,
            content_max_width: 760.0,
        },
        fonts: Fonts::default(),
    }
}

/// High Contrast — colorblind-safe, black-on-white with high-saturation
/// accents (PLAN-GUI §11.1 "High Contrast (colorblind-safe)").
pub fn high_contrast() -> Theme {
    use Appearance::Light;
    let c = ColorTokens {
        bg: Color::new(0x00, 0x00, 0x00),
        bg_raised: Color::new(0x0a, 0x0a, 0x0a),
        bg_sunken: Color::new(0x00, 0x00, 0x00),
        bg_hover: Color::new(0x1a, 0x1a, 0x1a),
        bg_active: Color::new(0x2a, 0x2a, 0x2a),
        text: Color::new(0xff, 0xff, 0xff),
        text_muted: Color::new(0xd0, 0xd0, 0xd0),
        text_faint: Color::new(0xa0, 0xa0, 0xa0),
        border: Color::new(0x60, 0x60, 0x60),
        border_strong: Color::new(0xff, 0xff, 0xff),
        accent: Color::new(0x00, 0xff, 0xff),
        accent_text: Color::new(0x00, 0x00, 0x00),
        selection: Color::new(0x00, 0x5f, 0x5f),
        focus_ring: Color::new(0x00, 0xff, 0xff),
        rail_bg: Color::new(0x00, 0x00, 0x00),
        list_bg: Color::new(0x00, 0x00, 0x00),
        editor_bg: Color::new(0x00, 0x00, 0x00),
        panel_bg: Color::new(0x0a, 0x0a, 0x0a),
        status_ok: Color::new(0x00, 0xff, 0x00),
        status_warn: Color::new(0xff, 0xff, 0x00),
        status_error: Color::new(0xff, 0x4d, 0x4d),
        gated: Color::new(0x00, 0xff, 0xff),
        ungated: Color::new(0xff, 0x80, 0x00),
        dirty: Color::new(0xff, 0xff, 0x00),
        conflict: Color::new(0xff, 0x4d, 0x4d),
    };
    let m = MarkdownTokens {
        h1: Color::new(0xff, 0xff, 0xff),
        h2: Color::new(0xff, 0xff, 0xff),
        h3: Color::new(0xee, 0xee, 0xee),
        h4: Color::new(0xee, 0xee, 0xee),
        h5: Color::new(0xdd, 0xdd, 0xdd),
        h6: Color::new(0xdd, 0xdd, 0xdd),
        body: Color::new(0xff, 0xff, 0xff),
        bold: Color::new(0xff, 0xff, 0xff),
        italic: Color::new(0xee, 0xee, 0xee),
        code: Color::new(0x00, 0xff, 0x00),
        code_bg: Color::new(0x00, 0x33, 0x00),
        quote: Color::new(0xff, 0xff, 0x00),
        quote_bar: Color::new(0x00, 0xff, 0xff),
        link: Color::new(0x00, 0xff, 0xff),
        link_hover: Color::new(0x80, 0xff, 0xff),
        list_marker: Color::new(0xd0, 0xd0, 0xd0),
        table_header: Color::new(0x1a, 0x1a, 0x1a),
        table_grid: Color::new(0x60, 0x60, 0x60),
        rule: Color::new(0x60, 0x60, 0x60),
        syntax_error: Color::new(0xff, 0x4d, 0x4d),
    };
    Theme {
        name: "high-contrast".to_string(),
        appearance: Light,
        colors: c,
        markdown: m,
        metrics: Metrics {
            density: 1.0,
            corner_radius: 2.0,
            row_height: 24.0,
            content_max_width: 760.0,
        },
        fonts: Fonts::default(),
    }
}

pub fn builtin(name: &str) -> Option<Theme> {
    let t = [marknote_dark(), marknote_light(), high_contrast()];
    t.into_iter().find(|t| t.name == name)
}

/// Every built-in theme, in display order (for Settings → Appearance).
pub fn all_builtins() -> Vec<Theme> {
    vec![marknote_dark(), marknote_light(), high_contrast()]
}

/// Load + validate a custom theme from a TOML file (PLAN-GUI §11.2:
/// `deny_unknown_fields`, bounded ≤ 64 KB). A bad theme must never prevent
/// launch — callers fall back to a built-in on error.
pub fn from_toml_path(p: &Path) -> Result<Theme, ThemeError> {
    let bytes = std::fs::read(p).map_err(ThemeError::Io)?;
    if bytes.len() > MAX_THEME_BYTES {
        return Err(ThemeError::TooLarge);
    }
    let s = std::str::from_utf8(&bytes).map_err(|e| ThemeError::Other(e.to_string()))?;
    let mut theme: Theme = toml::from_str(s).map_err(ThemeError::Parse)?;
    // The file's stem is the canonical name unless the theme declares its
    // own (the §11.2 "one theme per file" rule makes the stem authoritative,
    // but a `name =` override is allowed for clarity).
    if theme.name.is_empty() {
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            theme.name = stem.to_string();
        }
    }
    Ok(theme)
}

/// The theme-TOML size ceiling (PLAN-GUI §11.2 "the theme TOML is bounded").
pub const MAX_THEME_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub enum ThemeError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    TooLarge,
    Other(String),
}

impl std::fmt::Display for ThemeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeError::Io(e) => write!(f, "io: {e}"),
            ThemeError::Parse(e) => write!(f, "parse: {e}"),
            ThemeError::TooLarge => write!(f, "theme TOML exceeds 64 KB"),
            ThemeError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for ThemeError {}

/// The `<config>/kb-gui/themes` folder (PLAN-GUI §11.2).
pub fn themes_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("kb-gui").join("themes")
}

/// Project the theme tokens onto an `egui::Visuals` (chrome only; markdown
/// tokens feed the renderer separately, G6-2). Start from egui's built-in
/// dark/light base, then apply the semantic overrides.
pub fn to_visuals(theme: &Theme) -> egui::Visuals {
    let base = match theme.appearance {
        Appearance::Dark => egui::Visuals::dark(),
        Appearance::Light => egui::Visuals::light(),
    };
    let mut v = base;
    let c = &theme.colors;

    // Panel / window chrome.
    v.panel_fill = c.rail_bg.as_color32();
    v.window_fill = c.editor_bg.as_color32();
    v.window_stroke = egui::Stroke::new(1.0, c.border.as_color32());
    v.window_corner_radius = egui::CornerRadius::from(theme.metrics.corner_radius.max(0.0));
    v.menu_corner_radius = egui::CornerRadius::from(theme.metrics.corner_radius.max(0.0));

    // Text colors.
    v.override_text_color = Some(c.text.as_color32());
    v.weak_text_color = Some(c.text_muted.as_color32());
    v.hyperlink_color = theme.markdown.link.as_color32();

    // Surfaces.
    v.faint_bg_color = c.bg_sunken.as_color32();
    v.extreme_bg_color = c.bg_sunken.as_color32();
    v.code_bg_color = theme.markdown.code_bg.as_color32();
    v.text_edit_bg_color = Some(c.editor_bg.as_color32());
    v.warn_fg_color = c.status_warn.as_color32();
    v.error_fg_color = c.status_error.as_color32();

    // Widget states (inactive → bg_raised; hovered → bg_hover;
    // active → bg_active; strong borders on hover/active for contrast).
    v.widgets.inactive.bg_fill = c.bg_raised.as_color32();
    v.widgets.inactive.weak_bg_fill = c.bg_raised.as_color32();
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, c.border.as_color32());
    v.widgets.hovered.bg_fill = c.bg_hover.as_color32();
    v.widgets.hovered.weak_bg_fill = c.bg_hover.as_color32();
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, c.border_strong.as_color32());
    v.widgets.active.bg_fill = c.bg_active.as_color32();
    v.widgets.active.weak_bg_fill = c.bg_active.as_color32();
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, c.border_strong.as_color32());
    v.widgets.open.bg_fill = c.bg_raised.as_color32();

    // Corner radius on widgets from the theme metric.
    v.widgets.inactive.corner_radius =
        egui::CornerRadius::from(theme.metrics.corner_radius.max(0.0));
    v.widgets.hovered.corner_radius =
        egui::CornerRadius::from(theme.metrics.corner_radius.max(0.0));
    v.widgets.active.corner_radius = egui::CornerRadius::from(theme.metrics.corner_radius.max(0.0));

    // Selection + focus ring (accent-driven).
    v.selection.bg_fill = c.selection.as_color32();
    v.selection.stroke = egui::Stroke::new(1.0, c.focus_ring.as_color32());

    v
}

/// Apply a theme to an egui context (the single call site the app/main use).
pub fn apply(ctx: &egui::Context, theme: &Theme) {
    ctx.set_visuals(to_visuals(theme));
}

/// WCAG 2.x contrast audit (PLAN-GUI §11.1 "Contrast is checked in tests
/// against WCAG AA"). Returns every text-on-surface pair below its required
/// ratio (4.5 for body/UI text, 3.0 for large text / accents). Empty means
/// all pairs pass.
pub fn contrast_audit(theme: &Theme) -> Vec<ContrastRow> {
    let c = &theme.colors;
    let pairs: [(&'static str, Color, Color, f32); 7] = [
        ("text/bg", c.text, c.bg, 4.5),
        ("text_muted/bg", c.text_muted, c.bg, 4.5),
        ("accent_text/accent", c.accent, c.accent_text, 4.5),
        ("status_ok/bg", c.status_ok, c.bg, 4.5),
        ("status_warn/bg", c.status_warn, c.bg, 4.5),
        ("status_error/bg", c.status_error, c.bg, 4.5),
        (
            "code/code_bg",
            theme.markdown.code,
            theme.markdown.code_bg,
            4.5,
        ),
    ];
    pairs
        .into_iter()
        .map(|(pair, fg, bg, required)| {
            let ratio = fg.contrast_with(bg);
            ContrastRow {
                pair,
                ratio,
                required,
                ok: ratio >= required,
            }
        })
        .filter(|r| !r.ok)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContrastRow {
    pub pair: &'static str,
    pub ratio: f32,
    pub required: f32,
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_from_and_to_hex_roundtrip() {
        let c = Color::new(0x1a, 0x2b, 0x3c);
        assert_eq!(c.to_hex(), "#1a2b3c");
        assert_eq!(Color::from_hex("#1a2b3c"), Some(c));
        assert_eq!(Color::from_hex("0x1a2b3c"), Some(c));
        assert_eq!(Color::from_hex("1a2b3c"), Some(c));
    }

    #[test]
    fn color_from_hex_shorthand() {
        // CSS #RGB shorthand doubles each nibble: #f84 → #ff8844.
        let c = Color::new(0xff, 0x88, 0x44);
        assert_eq!(Color::from_hex("#f84"), Some(c));
        assert_eq!(Color::from_hex("#fff"), Some(Color::new(0xff, 0xff, 0xff)));
    }

    #[test]
    fn color_from_hex_rejects_bad() {
        assert!(Color::from_hex("#xyz").is_none());
        assert!(Color::from_hex("#12345").is_none()); // 5 nibbles
        assert!(Color::from_hex("").is_none());
    }

    #[test]
    fn color_contrast_black_on_white_is_max() {
        let black = Color::new(0, 0, 0);
        let white = Color::new(255, 255, 255);
        let r = black.contrast_with(white);
        // WCAG spec: black on white is ~21:1. Allow float slack.
        assert!((r - 21.0).abs() < 0.5, "got {r}");
    }

    #[test]
    fn color_similarity_contrast_is_one() {
        let a = Color::new(200, 200, 200);
        let r = a.contrast_with(a);
        assert!((r - 1.0).abs() < 1e-3, "got {r}");
    }

    #[test]
    fn builtin_names_resolve() {
        let names: Vec<String> = all_builtins().into_iter().map(|t| t.name).collect();
        for expected in ["marknote-dark", "marknote-light", "high-contrast"] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing {expected} in {names:?}"
            );
        }
        assert!(builtin("does-not-exist").is_none());
    }

    #[test]
    fn builtin_names_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for t in all_builtins() {
            assert!(seen.insert(t.name.clone()), "dup {name:?}", name = t.name);
        }
    }

    #[test]
    fn builtins_pass_contrast_audit() {
        // Every built-in theme must pass the §11.1 WCAG AA checks
        // (otherwise the audit would surface a regression the very moment
        // we ship it — this is the §11.1 acceptance criterion "Contrast is
        // checked in tests").
        for t in all_builtins() {
            let failures = contrast_audit(&t);
            assert!(failures.is_empty(), "{}: {:?}", t.name, failures);
        }
    }

    #[test]
    fn from_toml_roundtrip() {
        // Round-trip a full theme through TOML to lock the schema shape
        // (a `deny_unknown_fields` regression would otherwise be silent).
        let t = marknote_dark();
        let s = toml::to_string(&t).expect("serialize");
        let back: Theme = toml::from_str(&s).expect("deserialize");
        assert_eq!(back.name, t.name);
        assert_eq!(back.colors.bg, t.colors.bg);
        assert_eq!(back.appearance, t.appearance);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        // A TOML that only sets `name` should still produce a valid
        // `Theme` (every other token falls back to `Default`).
        let s = r#"name = "minimal""#;
        let t: Theme = toml::from_str(s).expect("partial parse");
        assert_eq!(t.name, "minimal");
        // `Default` is black; the fallback for every color is black.
        assert_eq!(t.colors.bg, Color(0xFF000000));
    }

    #[test]
    fn custom_theme_toml_loads() {
        let dir = std::env::temp_dir().join("kb-gui-test-theme");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join("custom.toml");
        std::fs::write(
            &p,
            "name = \"custom\"\nappearance = \"dark\"\ncolors.bg = \"#121212\"",
        )
        .expect("write");
        let t = from_toml_path(&p).expect("load");
        assert_eq!(t.name, "custom");
        assert_eq!(t.colors.bg, Color::new(0x12, 0x12, 0x12));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn custom_theme_too_large_fails() {
        let dir = std::env::temp_dir().join("kb-gui-test-theme-big");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join("big.toml");
        let data = vec![b'x'; MAX_THEME_BYTES + 1];
        std::fs::write(&p, data).expect("write");
        assert!(matches!(from_toml_path(&p), Err(ThemeError::TooLarge)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn high_contrast_is_light_appearance() {
        let t = high_contrast();
        assert_eq!(t.appearance, Appearance::Light);
    }

    #[test]
    fn appearance_from_str() {
        assert_eq!("dark".parse::<Appearance>().unwrap(), Appearance::Dark);
        assert_eq!("light".parse::<Appearance>().unwrap(), Appearance::Light);
        assert!("nope".parse::<Appearance>().is_err());
    }

    #[test]
    fn to_visuals_is_deterministic() {
        for t in all_builtins() {
            let v1 = to_visuals(&t);
            let v2 = to_visuals(&t);
            assert_eq!(v1.window_fill, v2.window_fill);
            assert_eq!(v1.panel_fill, v2.panel_fill);
        }
    }
}
