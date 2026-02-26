use std::fmt;
use std::fs;
use std::path::PathBuf;

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use glyphon::Color as GlyphonColor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Pre-parsed RGBA color. Stored as bytes, parsed once at load time.
#[derive(Debug, Clone, Copy)]
pub struct HexColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl HexColor {
    pub const fn from_u32(v: u32) -> Self {
        Self {
            r: (v >> 24) as u8,
            g: (v >> 16) as u8,
            b: (v >> 8) as u8,
            a: v as u8,
        }
    }

    fn from_hex(hex: &str) -> Self {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        let bytes = hex.as_bytes();
        let parse = |start: usize| -> u8 {
            bytes
                .get(start..start + 2)
                .and_then(|s| std::str::from_utf8(s).ok())
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .unwrap_or(0)
        };
        Self {
            r: parse(0),
            g: parse(2),
            b: parse(4),
            a: if bytes.len() >= 8 { parse(6) } else { 255 },
        }
    }

    /// Convert to linear f32 RGBA for GPU pipelines.
    pub fn to_linear_f32(self) -> [f32; 4] {
        rgba_u8_to_linear(self.r, self.g, self.b, self.a)
    }

    /// Convert to `wgpu::Color` (linear f64) for render pass clear colors.
    pub fn to_wgpu_color(self) -> wgpu::Color {
        let lin = rgba_u8_to_linear(self.r, self.g, self.b, self.a);
        wgpu::Color {
            r: lin[0] as f64,
            g: lin[1] as f64,
            b: lin[2] as f64,
            a: lin[3] as f64,
        }
    }

    /// Convert to glyphon Color for text rendering.
    pub fn to_glyphon(self) -> GlyphonColor {
        GlyphonColor::rgba(self.r, self.g, self.b, self.a)
    }
}

impl Serialize for HexColor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!(
            "{:02X}{:02X}{:02X}{:02X}",
            self.r, self.g, self.b, self.a
        ))
    }
}

impl<'de> Deserialize<'de> for HexColor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(HexColor::from_hex(&s))
    }
}

impl fmt::Display for HexColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}",
            self.r, self.g, self.b, self.a
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorScheme {
    // Terminal colors
    pub background: HexColor,
    pub chrome: HexColor,
    pub foreground: HexColor,
    pub cursor: HexColor,
    pub black: HexColor,
    pub red: HexColor,
    pub green: HexColor,
    pub yellow: HexColor,
    pub blue: HexColor,
    pub magenta: HexColor,
    pub cyan: HexColor,
    pub white: HexColor,
    pub bright_black: HexColor,
    pub bright_red: HexColor,
    pub bright_green: HexColor,
    pub bright_yellow: HexColor,
    pub bright_blue: HexColor,
    pub bright_magenta: HexColor,
    pub bright_cyan: HexColor,
    pub bright_white: HexColor,

    // Tab bar colors
    pub tab_active_fill: HexColor,
    pub tab_active_stroke: HexColor,
    pub tab_active_text: HexColor,
    pub tab_hover_bg: HexColor,
    pub tab_hover_stroke: HexColor,
    pub tab_separator: HexColor,

    // Selection
    pub selection: HexColor,

    // Panel colors
    pub panel_stroke: HexColor,

    // Dropdown colors
    pub dropdown_bg: HexColor,
    pub dropdown_border: HexColor,
    pub dropdown_shadow: HexColor,
    pub dropdown_item_hover: HexColor,
    pub dropdown_text: HexColor,
    pub dropdown_text_active: HexColor,

    // Dialog/form colors
    pub field_border: HexColor,
    pub field_focused: HexColor,
    pub ok_bg: HexColor,
    pub ok_hover_bg: HexColor,
    pub text_dim: HexColor,
    pub text_placeholder: HexColor,
}

impl Default for ColorScheme {
    fn default() -> Self {
        Self {
            background: HexColor::from_u32(0x1E1F22FF),
            chrome: HexColor::from_u32(0x2B2D30FF),
            foreground: HexColor::from_u32(0xBCBEC4FF),
            cursor: HexColor::from_u32(0xBCBEC4FF),
            black: HexColor::from_u32(0x000000FF),
            red: HexColor::from_u32(0xCD3131FF),
            green: HexColor::from_u32(0x0DBC79FF),
            yellow: HexColor::from_u32(0xE5E510FF),
            blue: HexColor::from_u32(0x2472C8FF),
            magenta: HexColor::from_u32(0xBC3FBCFF),
            cyan: HexColor::from_u32(0x11A8CDFF),
            white: HexColor::from_u32(0xCCCCCCFF),
            bright_black: HexColor::from_u32(0x666666FF),
            bright_red: HexColor::from_u32(0xF14C4CFF),
            bright_green: HexColor::from_u32(0x23D18BFF),
            bright_yellow: HexColor::from_u32(0xF5F543FF),
            bright_blue: HexColor::from_u32(0x3B8EEAFF),
            bright_magenta: HexColor::from_u32(0xD670D6FF),
            bright_cyan: HexColor::from_u32(0x29B8DBFF),
            bright_white: HexColor::from_u32(0xFFFFFFFF),
            tab_active_fill: HexColor::from_u32(0x233558FF),
            tab_active_stroke: HexColor::from_u32(0x2E4D89FF),
            tab_active_text: HexColor::from_u32(0xD1D3D9FF),
            tab_hover_bg: HexColor::from_u32(0x393B40FF),
            tab_hover_stroke: HexColor::from_u32(0x4E5157FF),
            tab_separator: HexColor::from_u32(0x393B40FF),
            selection: HexColor::from_u32(0x264F78FF),
            panel_stroke: HexColor::from_u32(0x3A3A3AFF),
            dropdown_bg: HexColor::from_u32(0x2B2D30FF),
            dropdown_border: HexColor::from_u32(0x43454AFF),
            dropdown_shadow: HexColor::from_u32(0x00000073),
            dropdown_item_hover: HexColor::from_u32(0x2E436EFF),
            dropdown_text: HexColor::from_u32(0xCDD0D6FF),
            dropdown_text_active: HexColor::from_u32(0xFFFFFFFF),
            field_border: HexColor::from_u32(0x5E6066FF),
            field_focused: HexColor::from_u32(0x2F7CF6FF),
            ok_bg: HexColor::from_u32(0x2F7CF6FF),
            ok_hover_bg: HexColor::from_u32(0x3D8BFAFF),
            text_dim: HexColor::from_u32(0x6F737AFF),
            text_placeholder: HexColor::from_u32(0x9A9DA3FF),
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
        Self::default()
    }

    fn named_to_rgb(&self, c: NamedColor) -> (u8, u8, u8) {
        let hc = match c {
            NamedColor::Black => self.black,
            NamedColor::Red => self.red,
            NamedColor::Green => self.green,
            NamedColor::Yellow => self.yellow,
            NamedColor::Blue => self.blue,
            NamedColor::Magenta => self.magenta,
            NamedColor::Cyan => self.cyan,
            NamedColor::White => self.white,
            NamedColor::BrightBlack => self.bright_black,
            NamedColor::BrightRed => self.bright_red,
            NamedColor::BrightGreen => self.bright_green,
            NamedColor::BrightYellow => self.bright_yellow,
            NamedColor::BrightBlue => self.bright_blue,
            NamedColor::BrightMagenta => self.bright_magenta,
            NamedColor::BrightCyan => self.bright_cyan,
            NamedColor::BrightWhite => self.bright_white,
            NamedColor::Foreground => self.foreground,
            NamedColor::Background | NamedColor::Cursor => self.background,
            _ => self.foreground,
        };
        (hc.r, hc.g, hc.b)
    }

    fn color_to_rgb(&self, color: Color) -> (u8, u8, u8) {
        match color {
            Color::Named(n) => self.named_to_rgb(n),
            Color::Indexed(idx) => ansi_256(idx),
            Color::Spec(rgb) => (rgb.r, rgb.g, rgb.b),
        }
    }

    /// Convert an alacritty Color to a glyphon foreground color.
    pub fn to_glyphon_fg(&self, color: Color) -> GlyphonColor {
        let (r, g, b) = self.color_to_rgb(color);
        GlyphonColor::rgb(r, g, b)
    }

    /// Convert an alacritty Color to an RGBA tuple (0.0..1.0) for background quads.
    pub fn to_rgba(&self, color: Color) -> [f32; 4] {
        let (r, g, b) = self.color_to_rgb(color);
        rgba_u8_to_linear(r, g, b, 255)
    }

    /// Check if a color is the default background.
    pub fn is_default_bg(&self, color: Color) -> bool {
        matches!(
            color,
            Color::Named(NamedColor::Background) | Color::Named(NamedColor::Cursor)
        )
    }
}

/// Convert an sRGB component (0.0..1.0) to linear.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert sRGB u8 RGBA to linear f32 RGBA for GPU pipelines.
pub fn rgba_u8_to_linear(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        srgb_to_linear(r as f32 / 255.0),
        srgb_to_linear(g as f32 / 255.0),
        srgb_to_linear(b as f32 / 255.0),
        a as f32 / 255.0,
    ]
}

fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pfauterminal")
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
