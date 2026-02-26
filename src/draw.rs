use glyphon::{CustomGlyph, CustomGlyphId};

use crate::layout::{BgQuad, Rect, RoundedQuad, TextSpec};

pub struct DrawContext {
    pub rounded_quads: Vec<RoundedQuad>,
    pub flat_quads: Vec<BgQuad>,
    pub custom_glyphs: Vec<CustomGlyph>,
}

impl DrawContext {
    pub fn new() -> Self {
        Self {
            rounded_quads: Vec::new(),
            flat_quads: Vec::new(),
            custom_glyphs: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.rounded_quads.clear();
        self.flat_quads.clear();
        self.custom_glyphs.clear();
    }

    pub fn rounded_rect(&mut self, rect: Rect, color: [f32; 4], radius: f32) {
        self.rounded_quads.push(RoundedQuad {
            rect,
            color,
            radius,
            shadow_softness: 0.0,
        });
    }

    pub fn shadow(&mut self, rect: Rect, color: [f32; 4], radius: f32, softness: f32) {
        self.rounded_quads.push(RoundedQuad {
            rect,
            color,
            radius,
            shadow_softness: softness,
        });
    }

    pub fn stroked_rect(
        &mut self,
        rect: &Rect,
        stroke: [f32; 4],
        fill: [f32; 4],
        radius: f32,
        border: f32,
    ) {
        self.rounded_quads.push(RoundedQuad {
            rect: *rect,
            color: stroke,
            radius,
            shadow_softness: 0.0,
        });
        self.rounded_quads.push(RoundedQuad {
            rect: rect.inset(border),
            color: fill,
            radius: (radius - border).max(0.0),
            shadow_softness: 0.0,
        });
    }

    pub fn flat_quad(&mut self, rect: Rect, color: [f32; 4]) {
        self.flat_quads.push(BgQuad { rect, color });
    }

    pub fn icon(&mut self, id: CustomGlyphId, left: f32, top: f32, size: f32) {
        self.custom_glyphs.push(CustomGlyph {
            id,
            left,
            top,
            width: size,
            height: size,
            color: None,
            snap_to_physical_pixel: true,
            metadata: 0,
        });
    }

    pub fn icon_centered(&mut self, id: CustomGlyphId, rect: &Rect, size: f32) {
        let left = rect.x + (rect.width - size) / 2.0;
        let top = rect.y + (rect.height - size) / 2.0;
        self.icon(id, left, top, size);
    }
}

pub fn centered_text(
    buffer_index: usize,
    left: f32,
    rect: &Rect,
    line_height_px: f32,
    color: glyphon::Color,
) -> TextSpec {
    TextSpec {
        buffer_index,
        left,
        top: rect.y + (rect.height - line_height_px) / 2.0,
        bounds: *rect,
        color,
    }
}
