use glyphon::{Attrs, Buffer, Color as GlyphonColor, FontSystem, Metrics, TextArea};

use crate::colors::{ColorScheme, HexColor};
use crate::draw::DrawContext;
use crate::font;
use crate::layout::{update_if_changed, Rect};

// ---------------------------------------------------------------------------
// Label
// ---------------------------------------------------------------------------

pub struct Label {
    buffer: Buffer,
    pos: (f32, f32),
    bounds: Rect,
    color: GlyphonColor,
}

impl Label {
    pub fn new(text: &str, attrs: Attrs, metrics: Metrics, font_system: &mut FontSystem) -> Self {
        let mut buffer = Buffer::new(font_system, metrics);
        font::set_buffer_text(&mut buffer, font_system, text, metrics, attrs, 600.0);
        Self {
            buffer,
            pos: (0.0, 0.0),
            bounds: Rect::ZERO,
            color: GlyphonColor::rgba(255, 255, 255, 255),
        }
    }

    pub fn set_position(&mut self, x: f32, y: f32, bounds: Rect) {
        self.pos = (x, y);
        self.bounds = bounds;
    }

    pub fn set_color(&mut self, color: GlyphonColor) {
        self.color = color;
    }

    pub fn draw<'a>(&'a self, text_areas: &mut Vec<TextArea<'a>>, scale: f32) {
        text_areas.push(TextArea {
            buffer: &self.buffer,
            left: self.pos.0,
            top: self.pos.1,
            scale,
            bounds: self.bounds.to_text_bounds(),
            default_color: self.color,
            custom_glyphs: &[],
        });
    }
}

// ---------------------------------------------------------------------------
// TextField
// ---------------------------------------------------------------------------

pub struct TextField {
    buffer: Buffer,
    rect: Rect,
    value: String,
    placeholder: String,
    cursor_pos: usize,
    /// When set, marks the anchor of a selection range (the other end is cursor_pos).
    selection_anchor: Option<usize>,
    focused: bool,
    password: bool,
    char_width: f32,
    metrics: Metrics,
    radius: f32,
    pad_h: f32,
}

impl TextField {
    pub fn new(
        placeholder: &str,
        password: bool,
        metrics: Metrics,
        char_width: f32,
        radius: f32,
        pad_h: f32,
        font_system: &mut FontSystem,
    ) -> Self {
        let buffer = Buffer::new(font_system, metrics);
        Self {
            buffer,
            rect: Rect::ZERO,
            value: String::new(),
            placeholder: placeholder.to_string(),
            cursor_pos: 0,
            selection_anchor: None,
            focused: false,
            password,
            char_width,
            metrics,
            radius,
            pad_h,
        }
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, text: &str, font_system: &mut FontSystem) {
        self.value = text.to_string();
        self.cursor_pos = text.chars().count();
        self.selection_anchor = None;
        self.refresh_buffer(font_system);
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn set_char_width(&mut self, char_width: f32) {
        self.char_width = char_width;
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn click(&mut self, x: f32, scale: f32) {
        let rel_x = (x - self.rect.x - self.pad_h * scale).max(0.0);
        let char_w = self.char_width * scale;
        let pos = (rel_x / char_w).round() as usize;
        self.cursor_pos = pos.min(self.value.chars().count());
        self.selection_anchor = None;
        self.focused = true;
    }

    pub fn insert_text(&mut self, text: &str, font_system: &mut FontSystem) {
        self.delete_selection_inner();
        for c in text.chars() {
            if c.is_control() {
                continue;
            }
            let byte_pos = char_to_byte(&self.value, self.cursor_pos);
            self.value.insert(byte_pos, c);
            self.cursor_pos += 1;
        }
        self.refresh_buffer(font_system);
    }

    pub fn delete_back(&mut self, font_system: &mut FontSystem) {
        if self.has_selection() {
            self.delete_selection_inner();
            self.refresh_buffer(font_system);
            return;
        }
        if self.cursor_pos > 0 {
            let byte_start = char_to_byte(&self.value, self.cursor_pos - 1);
            let byte_end = char_to_byte(&self.value, self.cursor_pos);
            self.value.drain(byte_start..byte_end);
            self.cursor_pos -= 1;
            self.refresh_buffer(font_system);
        }
    }

    pub fn delete_forward(&mut self, font_system: &mut FontSystem) {
        if self.has_selection() {
            self.delete_selection_inner();
            self.refresh_buffer(font_system);
            return;
        }
        let char_count = self.value.chars().count();
        if self.cursor_pos < char_count {
            let byte_start = char_to_byte(&self.value, self.cursor_pos);
            let byte_end = char_to_byte(&self.value, self.cursor_pos + 1);
            self.value.drain(byte_start..byte_end);
            self.refresh_buffer(font_system);
        }
    }

    pub fn move_left(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else if self.has_selection() {
            let (start, _) = self.selection_range();
            self.cursor_pos = start;
            self.selection_anchor = None;
            return;
        }
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
        }
        if shift && self.selection_anchor == Some(self.cursor_pos) {
            self.selection_anchor = None;
        }
    }

    pub fn move_right(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else if self.has_selection() {
            let (_, end) = self.selection_range();
            self.cursor_pos = end;
            self.selection_anchor = None;
            return;
        }
        let char_count = self.value.chars().count();
        if self.cursor_pos < char_count {
            self.cursor_pos += 1;
        }
        if shift && self.selection_anchor == Some(self.cursor_pos) {
            self.selection_anchor = None;
        }
    }

    pub fn move_home(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else {
            self.selection_anchor = None;
        }
        self.cursor_pos = 0;
        if shift && self.selection_anchor == Some(0) {
            self.selection_anchor = None;
        }
    }

    pub fn move_end(&mut self, shift: bool) {
        if shift {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor_pos);
            }
        } else {
            self.selection_anchor = None;
        }
        let end = self.value.chars().count();
        self.cursor_pos = end;
        if shift && self.selection_anchor == Some(end) {
            self.selection_anchor = None;
        }
    }

    pub fn select_all(&mut self) {
        let len = self.value.chars().count();
        if len > 0 {
            self.selection_anchor = Some(0);
            self.cursor_pos = len;
        }
    }

    pub fn has_selection(&self) -> bool {
        self.selection_anchor.is_some()
    }

    /// Returns (start, end) character indices of the selection, ordered.
    fn selection_range(&self) -> (usize, usize) {
        let anchor = self.selection_anchor.unwrap_or(self.cursor_pos);
        let start = anchor.min(self.cursor_pos);
        let end = anchor.max(self.cursor_pos);
        (start, end)
    }

    /// Returns the selected text, or `None` if nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let anchor = self.selection_anchor?;
        let start = anchor.min(self.cursor_pos);
        let end = anchor.max(self.cursor_pos);
        if start == end {
            return None;
        }
        let byte_start = char_to_byte(&self.value, start);
        let byte_end = char_to_byte(&self.value, end);
        Some(self.value[byte_start..byte_end].to_string())
    }

    /// Deletes the selected text, leaving cursor at the start of the former selection.
    fn delete_selection_inner(&mut self) {
        if let Some(anchor) = self.selection_anchor.take() {
            let start = anchor.min(self.cursor_pos);
            let end = anchor.max(self.cursor_pos);
            if start != end {
                let byte_start = char_to_byte(&self.value, start);
                let byte_end = char_to_byte(&self.value, end);
                self.value.drain(byte_start..byte_end);
                self.cursor_pos = start;
            }
        }
    }

    fn display_text(&self) -> String {
        if self.password {
            "*".repeat(self.value.chars().count())
        } else if self.value.is_empty() {
            self.placeholder.clone()
        } else {
            self.value.clone()
        }
    }

    fn is_showing_placeholder(&self) -> bool {
        !self.password && self.value.is_empty()
    }

    fn refresh_buffer(&mut self, font_system: &mut FontSystem) {
        let text = self.display_text();
        let attrs = font::default_attrs();
        font::set_buffer_text(
            &mut self.buffer,
            font_system,
            &text,
            self.metrics,
            attrs,
            600.0,
        );
    }

    pub fn draw<'a>(
        &'a self,
        ctx: &mut DrawContext,
        text_areas: &mut Vec<TextArea<'a>>,
        scale: f32,
        colors: &ColorScheme,
    ) {
        let border_w = if self.focused {
            2.0 * scale
        } else {
            1.0 * scale
        };
        let border_col = if self.focused {
            colors.field_focused
        } else {
            colors.field_border
        };

        ctx.stroked_rect(
            &self.rect,
            border_col.to_linear_f32(),
            colors.background.to_linear_f32(),
            self.radius * scale,
            border_w,
        );

        let pad = self.pad_h * scale;
        let line_h = self.metrics.line_height;
        let text_y = self.rect.y + (self.rect.height - line_h * scale) / 2.0;

        let text_color = if self.is_showing_placeholder() {
            colors.text_placeholder.to_glyphon()
        } else {
            colors.dropdown_text.to_glyphon()
        };

        text_areas.push(TextArea {
            buffer: &self.buffer,
            left: self.rect.x + pad,
            top: text_y,
            scale,
            bounds: self.rect.to_text_bounds(),
            default_color: text_color,
            custom_glyphs: &[],
        });

        if self.focused {
            let cursor_h = line_h * scale;
            let cursor_y = self.rect.y + (self.rect.height - cursor_h) / 2.0;

            // Draw selection highlight
            if let Some(anchor) = self.selection_anchor {
                let start = anchor.min(self.cursor_pos);
                let end = anchor.max(self.cursor_pos);
                if start != end {
                    let sel_x =
                        self.rect.x + self.pad_h * scale + start as f32 * self.char_width * scale;
                    let sel_w = (end - start) as f32 * self.char_width * scale;
                    ctx.rounded_rect(
                        Rect {
                            x: sel_x,
                            y: cursor_y,
                            width: sel_w,
                            height: cursor_h,
                        },
                        colors.selection.to_linear_f32(),
                        0.0,
                    );
                }
            }

            // Draw cursor
            let cursor_x =
                self.rect.x + self.pad_h * scale + self.cursor_pos as f32 * self.char_width * scale;
            ctx.rounded_rect(
                Rect {
                    x: cursor_x,
                    y: cursor_y,
                    width: 1.5 * scale,
                    height: cursor_h,
                },
                colors.cursor.to_linear_f32(),
                0.0,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Button
// ---------------------------------------------------------------------------

pub enum ButtonKind {
    Filled {
        bg: HexColor,
        bg_hover: HexColor,
    },
    Stroked {
        fill: HexColor,
        fill_hover: HexColor,
        stroke: HexColor,
    },
}

pub struct Button {
    buffer: Buffer,
    rect: Rect,
    hovered: bool,
    kind: ButtonKind,
    text_color: GlyphonColor,
    radius: f32,
    padding_h: f32,
    line_height: f32,
}

impl Button {
    pub fn new(
        label: &str,
        kind: ButtonKind,
        text_color: GlyphonColor,
        radius: f32,
        padding_h: f32,
        attrs: Attrs,
        metrics: Metrics,
        font_system: &mut FontSystem,
    ) -> Self {
        let mut buffer = Buffer::new(font_system, metrics);
        font::set_buffer_text(&mut buffer, font_system, label, metrics, attrs, 600.0);
        Self {
            buffer,
            rect: Rect::ZERO,
            hovered: false,
            kind,
            text_color,
            radius,
            padding_h,
            line_height: metrics.line_height,
        }
    }

    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.rect.contains(x, y)
    }

    pub fn set_hovered(&mut self, h: bool) -> bool {
        update_if_changed(&mut self.hovered, h)
    }

    pub fn draw<'a>(
        &'a self,
        ctx: &mut DrawContext,
        text_areas: &mut Vec<TextArea<'a>>,
        scale: f32,
    ) {
        let r = self.radius * scale;
        match &self.kind {
            ButtonKind::Filled { bg, bg_hover } => {
                let color = if self.hovered { bg_hover } else { bg };
                ctx.rounded_rect(self.rect, color.to_linear_f32(), r);
            }
            ButtonKind::Stroked {
                fill,
                fill_hover,
                stroke,
            } => {
                let fill_color = if self.hovered { fill_hover } else { fill };
                ctx.stroked_rect(
                    &self.rect,
                    stroke.to_linear_f32(),
                    fill_color.to_linear_f32(),
                    r,
                    1.0 * scale,
                );
            }
        }

        let text_y = self.rect.y + (self.rect.height - self.line_height * scale) / 2.0;
        text_areas.push(TextArea {
            buffer: &self.buffer,
            left: self.rect.x + self.padding_h * scale,
            top: text_y,
            scale,
            bounds: self.rect.to_text_bounds(),
            default_color: self.text_color,
            custom_glyphs: &[],
        });
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_pos, _)| byte_pos)
        .unwrap_or(s.len())
}
