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

    pub fn contains_padded(&self, x: f32, y: f32, pad: f32) -> bool {
        x >= self.x - pad
            && x < self.x + self.width + pad
            && y >= self.y - pad
            && y < self.y + self.height + pad
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - 2.0 * amount).max(0.0),
            height: (self.height - 2.0 * amount).max(0.0),
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

/// Push a stroked (border + fill) rounded rect pair.
pub fn push_stroked_rounded_rect(
    quads: &mut Vec<RoundedQuad>,
    rect: &Rect,
    stroke_color: [f32; 4],
    fill_color: [f32; 4],
    radius: f32,
    border: f32,
) {
    quads.push(RoundedQuad {
        rect: *rect,
        color: stroke_color,
        radius,
        shadow_softness: 0.0,
    });
    quads.push(RoundedQuad {
        rect: rect.inset(border),
        color: fill_color,
        radius: (radius - border).max(0.0),
        shadow_softness: 0.0,
    });
}
