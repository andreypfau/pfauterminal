use std::cell::Cell;

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
    /// Horizontal scroll offset in physical pixels.
    /// Uses Cell so draw() can adjust it through &self.
    scroll_offset: Cell<f32>,
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
            scroll_offset: Cell::new(0.0),
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
        let rel_x = (x - self.rect.x - self.pad_h * scale).max(0.0) + self.scroll_offset.get();
        let logical_x = rel_x / scale;
        // Find the closest character position from glyph layout
        let display = self.display_text();
        let char_count = display.chars().count();
        let mut best_pos = char_count;
        let mut best_dist = f32::MAX;
        for i in 0..=char_count {
            let gx = self.cursor_x_from_layout(i);
            let dist = (gx - logical_x).abs();
            if dist < best_dist {
                best_dist = dist;
                best_pos = i;
            }
        }
        self.cursor_pos = best_pos;
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

    /// Get the pixel X offset of a character position from the layout glyphs.
    /// Returns the offset in logical pixels (unscaled).
    fn cursor_x_from_layout(&self, char_pos: usize) -> f32 {
        let display = self.display_text();
        let byte_target = char_to_byte(&display, char_pos);
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                if glyph.start >= byte_target {
                    return glyph.x;
                }
            }
            // Cursor is at or past the end of this run
            if let Some(last) = run.glyphs.last() {
                if byte_target >= last.end {
                    return last.x + last.w;
                }
            }
        }
        // Fallback: use char_width approximation
        char_pos as f32 * self.char_width
    }

    /// Ensure the cursor is visible within the field by adjusting scroll_offset (in pixels).
    fn ensure_cursor_visible(&self, scale: f32) {
        let pad = self.pad_h * scale;
        let field_inner_w = self.rect.width - 2.0 * pad;
        if field_inner_w <= 0.0 {
            return;
        }
        let cursor_x = self.cursor_x_from_layout(self.cursor_pos) * scale;
        let mut offset = self.scroll_offset.get();
        // Cursor scrolled left of the visible window
        if cursor_x < offset {
            offset = cursor_x;
        }
        // Cursor scrolled right of the visible window
        if cursor_x > offset + field_inner_w {
            offset = cursor_x - field_inner_w;
        }
        self.scroll_offset.set(offset);
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
        // Use a very large width so glyphon never wraps — horizontal
        // scrolling is handled by scroll_offset + text bounds clipping.
        font::set_buffer_text(
            &mut self.buffer,
            font_system,
            &text,
            self.metrics,
            attrs,
            f32::MAX,
        );
    }

    pub fn draw<'a>(
        &'a self,
        ctx: &mut DrawContext,
        text_areas: &mut Vec<TextArea<'a>>,
        scale: f32,
        colors: &ColorScheme,
    ) {
        self.ensure_cursor_visible(scale);

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
        let char_w = self.char_width * scale;
        let line_h = self.metrics.line_height;
        let text_y = self.rect.y + (self.rect.height - line_h * scale) / 2.0;
        let scroll = self.scroll_offset.get();
        let text_left = self.rect.x + pad - scroll;

        let text_color = if self.is_showing_placeholder() {
            colors.text_placeholder.to_glyphon()
        } else {
            colors.dropdown_text.to_glyphon()
        };

        text_areas.push(TextArea {
            buffer: &self.buffer,
            left: text_left,
            top: text_y,
            scale,
            bounds: self.rect.to_text_bounds(),
            default_color: text_color,
            custom_glyphs: &[],
        });

        if self.focused {
            let cursor_h = line_h * scale;
            let cursor_y = self.rect.y + (self.rect.height - cursor_h) / 2.0;

            // Draw selection highlight (clipped to field bounds)
            if let Some(anchor) = self.selection_anchor {
                let start = anchor.min(self.cursor_pos);
                let end = anchor.max(self.cursor_pos);
                if start != end {
                    let sel_x = self.rect.x + pad
                        + self.cursor_x_from_layout(start) * scale - scroll;
                    let sel_end_x = self.rect.x + pad
                        + self.cursor_x_from_layout(end) * scale - scroll;
                    // Clip selection to field inner area
                    let field_left = self.rect.x + pad;
                    let field_right = self.rect.x + self.rect.width - pad;
                    let clip_x = sel_x.max(field_left);
                    let clip_right = sel_end_x.min(field_right);
                    if clip_right > clip_x {
                        ctx.rounded_rect(
                            Rect {
                                x: clip_x,
                                y: cursor_y,
                                width: clip_right - clip_x,
                                height: cursor_h,
                            },
                            colors.selection.to_linear_f32(),
                            0.0,
                        );
                    }
                }
            }

            // Draw cursor
            let cursor_x = self.rect.x + pad
                + self.cursor_x_from_layout(self.cursor_pos) * scale - scroll;
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
