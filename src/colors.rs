use std::fs;
use std::path::PathBuf;

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use glyphon::Color as GlyphonColor;
use serde::{Deserialize, Serialize};

/// RGBA color as 8-bit hex string "RRGGBBAA".
fn hex_to_rgba(hex: &str) -> (u8, u8, u8, u8) {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    let a = if hex.len() >= 8 {
        u8::from_str_radix(&hex[6..8], 16).unwrap_or(255)
    } else {
        255
    };
    (r, g, b, a)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorScheme {
    pub background: String,
    #[serde(default = "default_chrome")]
    pub chrome: String,
    pub foreground: String,
    pub cursor: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            background: "1E1F22FF".into(),
            chrome: "2B2D30FF".into(),
            foreground: "BCBEC4FF".into(),
            cursor: "BCBEC4FF".into(),
            black: "000000FF".into(),
            red: "CD3131FF".into(),
            green: "0DBC79FF".into(),
            yellow: "E5E510FF".into(),
            blue: "2472C8FF".into(),
            magenta: "BC3FBCFF".into(),
            cyan: "11A8CDFF".into(),
            white: "CCCCCCFF".into(),
            bright_black: "666666FF".into(),
            bright_red: "F14C4CFF".into(),
            bright_green: "23D18BFF".into(),
            bright_yellow: "F5F543FF".into(),
            bright_blue: "3B8EEAFF".into(),
            bright_magenta: "D670D6FF".into(),
            bright_cyan: "29B8DBFF".into(),
            bright_white: "FFFFFFFF".into(),
        }
    }
}

impl ColorScheme {
    /// Load from `~/.pfauterminal/colors.json`, creating with defaults if missing.
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(data) = fs::read_to_string(&path) {
            match serde_json::from_str::<ColorScheme>(&data) {
                Ok(scheme) => return scheme,
                Err(e) => {
                    log::warn!("invalid colors.json, using defaults: {e}");
                }
            }
        }
        let scheme = Self::default();
        scheme.save();
        scheme
    }

    fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = fs::write(&path, json) {
                    log::warn!("failed to write colors.json: {e}");
                }
            }
            Err(e) => log::warn!("failed to serialize colors: {e}"),
        }
    }

    pub fn bg_wgpu(&self) -> [f64; 4] {
        let (r, g, b, a) = hex_to_rgba(&self.background);
        [
            srgb_to_linear(r as f64 / 255.0),
            srgb_to_linear(g as f64 / 255.0),
            srgb_to_linear(b as f64 / 255.0),
            a as f64 / 255.0,
        ]
    }

    /// Background color as linear f32 (for shader uniforms).
    pub fn bg_linear_f32(&self) -> [f32; 4] {
        let bg = self.bg_wgpu();
        [bg[0] as f32, bg[1] as f32, bg[2] as f32, bg[3] as f32]
    }

    /// Chrome (window) background as linear f64 for wgpu clear color.
    pub fn chrome_wgpu(&self) -> [f64; 4] {
        let (r, g, b, a) = hex_to_rgba(&self.chrome);
        [
            srgb_to_linear(r as f64 / 255.0),
            srgb_to_linear(g as f64 / 255.0),
            srgb_to_linear(b as f64 / 255.0),
            a as f64 / 255.0,
        ]
    }

    pub fn fg_glyphon(&self) -> GlyphonColor {
        let (r, g, b, a) = hex_to_rgba(&self.foreground);
        GlyphonColor::rgba(r, g, b, a)
    }

    fn named_to_rgb(&self, c: NamedColor) -> (u8, u8, u8) {
        let hex = match c {
            NamedColor::Black => &self.black,
            NamedColor::Red => &self.red,
            NamedColor::Green => &self.green,
            NamedColor::Yellow => &self.yellow,
            NamedColor::Blue => &self.blue,
            NamedColor::Magenta => &self.magenta,
            NamedColor::Cyan => &self.cyan,
            NamedColor::White => &self.white,
            NamedColor::BrightBlack => &self.bright_black,
            NamedColor::BrightRed => &self.bright_red,
            NamedColor::BrightGreen => &self.bright_green,
            NamedColor::BrightYellow => &self.bright_yellow,
            NamedColor::BrightBlue => &self.bright_blue,
            NamedColor::BrightMagenta => &self.bright_magenta,
            NamedColor::BrightCyan => &self.bright_cyan,
            NamedColor::BrightWhite => &self.bright_white,
            NamedColor::Foreground => &self.foreground,
            NamedColor::Background | NamedColor::Cursor => &self.background,
            _ => &self.foreground,
        };
        let (r, g, b, _) = hex_to_rgba(hex);
        (r, g, b)
    }

    /// Convert an alacritty Color to a glyphon foreground color.
    pub fn to_glyphon_fg(&self, color: Color) -> GlyphonColor {
        let (r, g, b) = match color {
            Color::Named(n) => self.named_to_rgb(n),
            Color::Indexed(idx) => ansi_256(idx),
            Color::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        };
        GlyphonColor::rgb(r, g, b)
    }

    /// Convert an alacritty Color to an RGBA tuple (0.0..1.0) for background quads.
    pub fn to_rgba(&self, color: Color) -> [f32; 4] {
        let (r, g, b) = match color {
            Color::Named(n) => self.named_to_rgb(n),
            Color::Indexed(idx) => ansi_256(idx),
            Color::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        };
        [
            srgb_to_linear(r as f64 / 255.0) as f32,
            srgb_to_linear(g as f64 / 255.0) as f32,
            srgb_to_linear(b as f64 / 255.0) as f32,
            1.0,
        ]
    }

    /// Active tab fill color.
    pub fn tab_active_fill(&self) -> [f32; 4] {
        hex_to_linear_f32("2E436EFF")
    }

    /// Active tab stroke color (1px inside).
    pub fn tab_active_stroke(&self) -> [f32; 4] {
        hex_to_linear_f32("4A6FA5FF")
    }

    /// Hovered inactive tab fill.
    pub fn tab_hover_bg(&self) -> [f32; 4] {
        hex_to_linear_f32("393B40FF")
    }

    /// Hovered inactive tab stroke (1px inside).
    pub fn tab_hover_stroke(&self) -> [f32; 4] {
        hex_to_linear_f32("4E5157FF")
    }

    /// Separator line between tab bar and content.
    pub fn tab_separator(&self) -> [f32; 4] {
        hex_to_linear_f32("393B40FF")
    }

    /// Panel border stroke color (0.5px inside).
    pub fn panel_stroke(&self) -> [f32; 4] {
        hex_to_linear_f32("3A3A3AFF")
    }

    /// Check if a color is the default background.
    pub fn is_default_bg(&self, color: Color) -> bool {
        matches!(
            color,
            Color::Named(NamedColor::Background) | Color::Named(NamedColor::Cursor)
        )
    }
}

/// Convert a hex color string to linear f32 RGBA.
fn hex_to_linear_f32(hex: &str) -> [f32; 4] {
    let (r, g, b, a) = hex_to_rgba(hex);
    [
        srgb_to_linear(r as f64 / 255.0) as f32,
        srgb_to_linear(g as f64 / 255.0) as f32,
        srgb_to_linear(b as f64 / 255.0) as f32,
        a as f32 / 255.0,
    ]
}

/// Convert an sRGB component (0.0..1.0) to linear.
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn default_chrome() -> String {
    "2B2D30FF".into()
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".pfauterminal")
        .join("colors.json")
}

/// Standard 256-color palette (indices 0..=255).
fn ansi_256(idx: u8) -> (u8, u8, u8) {
    match idx {
        0 => (0, 0, 0),
        1 => (205, 49, 49),
        2 => (13, 188, 121),
        3 => (229, 229, 16),
        4 => (36, 114, 200),
        5 => (188, 63, 188),
        6 => (17, 168, 205),
        7 => (204, 204, 204),
        8 => (102, 102, 102),
        9 => (241, 76, 76),
        10 => (35, 209, 139),
        11 => (245, 245, 67),
        12 => (59, 142, 234),
        13 => (214, 112, 214),
        14 => (41, 184, 219),
        15 => (255, 255, 255),
        16..=231 => {
            let idx = idx - 16;
            let r = idx / 36;
            let g = (idx % 36) / 6;
            let b = idx % 6;
            let to_val = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (to_val(r), to_val(g), to_val(b))
        }
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            (v, v, v)
        }
    }
}
