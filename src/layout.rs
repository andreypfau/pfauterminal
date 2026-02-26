use glyphon::{Color as GlyphonColor, TextBounds};

/// Background quad for flat-color rendering.
pub struct BgQuad {
    pub rect: Rect,
    pub color: [f32; 4],
}

/// Shared text spec used by all UI components for glyphon rendering.
pub struct TextSpec {
    pub buffer_index: usize,
    pub left: f32,
    pub top: f32,
    pub bounds: Rect,
    pub color: GlyphonColor,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Self = Self {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub(crate) fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - 2.0 * amount).max(0.0),
            height: (self.height - 2.0 * amount).max(0.0),
        }
    }

    /// Convert to glyphon `TextBounds`.
    pub fn to_text_bounds(&self) -> TextBounds {
        TextBounds {
            left: self.x as i32,
            top: self.y as i32,
            right: (self.x + self.width) as i32,
            bottom: (self.y + self.height) as i32,
        }
    }
}

/// A filled rounded rectangle for SDF rendering.
/// When `shadow_softness` > 0, renders as a soft shadow instead of a sharp rect.
pub struct RoundedQuad {
    pub rect: Rect,
    pub color: [f32; 4],
    pub radius: f32,
    pub shadow_softness: f32,
}

/// Update a field only when the new value differs; returns `true` if changed.
pub fn update_if_changed<T: PartialEq>(field: &mut T, value: T) -> bool {
    if *field != value {
        *field = value;
        true
    } else {
        false
    }
}
