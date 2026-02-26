use glyphon::{Buffer, FontSystem, Metrics, Shaping};

use crate::draw::DrawContext;
use crate::font;
use crate::layout::{update_if_changed, Rect, TextSpec};
use crate::theme::{DropdownTheme, Theme};

#[derive(Debug, Clone)]
pub enum MenuAction {
    NewShell(String),
    OpenSshDialog,
}

pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropdownElement {
    None,
    Item(usize),
}

pub struct DropdownMenu {
    items: Vec<MenuItem>,
    item_buffers: Vec<Buffer>,
    item_rects: Vec<Rect>,
    menu_rect: Rect,
    hover: DropdownElement,
    visible: bool,
}

impl DropdownMenu {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            item_buffers: Vec::new(),
            item_rects: Vec::new(),
            menu_rect: Rect::ZERO,
            hover: DropdownElement::None,
            visible: false,
        }
    }

    pub fn open(
        &mut self,
        items: Vec<MenuItem>,
        anchor_rect: Rect,
        width: Option<f32>,
        scale: f32,
        surface_width: f32,
        surface_height: f32,
        font_system: &mut FontSystem,
        theme: &DropdownTheme,
    ) {
        let menu_w = width.unwrap_or(theme.width) * scale;
        let padding = theme.padding * scale;
        let item_h = theme.item_height * scale;
        let item_pad_h = theme.item_padding_h * scale;
        let border = theme.border_width * scale;
        let gap = theme.anchor_gap * scale;

        let content_h = item_h * items.len() as f32;
        let menu_h = padding * 2.0 + content_h + border * 2.0;

        let mut menu_x = anchor_rect.x + (anchor_rect.width - menu_w) / 2.0;
        let menu_y = anchor_rect.y + anchor_rect.height + gap;

        if menu_x + menu_w > surface_width {
            menu_x = surface_width - menu_w;
        }
        if menu_x < 0.0 {
            menu_x = 0.0;
        }

        let final_y = if menu_y + menu_h > surface_height {
            (anchor_rect.y - gap - menu_h).max(0.0)
        } else {
            menu_y
        };

        self.menu_rect = Rect {
            x: menu_x,
            y: final_y,
            width: menu_w,
            height: menu_h,
        };

        let inner_x = menu_x + border + padding;
        let inner_y = final_y + border + padding;
        let inner_w = menu_w - 2.0 * (border + padding);

        self.item_rects.clear();
        for i in 0..items.len() {
            self.item_rects.push(Rect {
                x: inner_x,
                y: inner_y + i as f32 * item_h,
                width: inner_w,
                height: item_h,
            });
        }

        let metrics = Metrics::new(theme.font_size, theme.font_size * font::LINE_HEIGHT);
        while self.item_buffers.len() < items.len() {
            self.item_buffers.push(Buffer::new(font_system, metrics));
        }
        self.item_buffers.truncate(items.len());

        for (i, item) in items.iter().enumerate() {
            let buf = &mut self.item_buffers[i];
            buf.set_metrics(font_system, metrics);
            let buf_width = (inner_w - 2.0 * item_pad_h) / scale;
            buf.set_size(font_system, Some(buf_width), Some(metrics.line_height));
            buf.set_text(
                font_system,
                &item.label,
                font::default_attrs(),
                Shaping::Basic,
            );
            buf.shape_until_scroll(font_system, false);
        }

        self.items = items;
        self.hover = DropdownElement::None;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.hover = DropdownElement::None;
    }

    pub fn is_open(&self) -> bool {
        self.visible
    }

    pub fn set_hover(&mut self, hover: DropdownElement) -> bool {
        update_if_changed(&mut self.hover, hover)
    }

    pub fn hit_test(&self, x: f32, y: f32) -> DropdownElement {
        if !self.visible {
            return DropdownElement::None;
        }
        for (i, rect) in self.item_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return DropdownElement::Item(i);
            }
        }
        DropdownElement::None
    }

    /// Returns true if the click is outside the menu bounds (should close).
    pub fn is_outside(&self, x: f32, y: f32) -> bool {
        self.visible && !self.menu_rect.contains(x, y)
    }

    pub fn action_for(&self, idx: usize) -> Option<&MenuAction> {
        self.items.get(idx).map(|item| &item.action)
    }

    pub fn draw(
        &self,
        ctx: &mut DrawContext,
        text_specs: &mut Vec<TextSpec>,
        theme: &Theme,
        scale: f32,
    ) {
        if !self.visible {
            return;
        }

        let t = &theme.dropdown;
        let colors = &theme.colors;

        let radius = t.corner_radius * scale;
        let border = t.border_width * scale;
        let item_radius = t.item_radius * scale;
        let item_pad_h = t.item_padding_h * scale;
        let shadow_spread = t.shadow_spread * scale;
        let shadow_offset_y = t.shadow_offset_y * scale;

        // Drop shadow
        ctx.shadow(
            Rect {
                x: self.menu_rect.x,
                y: self.menu_rect.y + shadow_offset_y,
                width: self.menu_rect.width,
                height: self.menu_rect.height,
            },
            colors.dropdown_shadow.to_linear_f32(),
            radius,
            shadow_spread,
        );

        // Border + fill
        ctx.stroked_rect(
            &self.menu_rect,
            colors.dropdown_border.to_linear_f32(),
            colors.dropdown_bg.to_linear_f32(),
            radius,
            border,
        );

        // Hover highlight
        if let DropdownElement::Item(idx) = self.hover
            && let Some(rect) = self.item_rects.get(idx)
        {
            ctx.rounded_rect(*rect, colors.dropdown_item_hover.to_linear_f32(), item_radius);
        }

        // Text areas
        let line_h = t.font_size * font::LINE_HEIGHT * scale;
        for (i, rect) in self.item_rects.iter().enumerate() {
            let is_hovered = matches!(self.hover, DropdownElement::Item(idx) if idx == i);
            let text_left = rect.x + item_pad_h;
            let text_top = rect.y + (rect.height - line_h) / 2.0;
            let color = if is_hovered {
                colors.dropdown_text_active.to_glyphon()
            } else {
                colors.dropdown_text.to_glyphon()
            };

            text_specs.push(TextSpec {
                buffer_index: i,
                left: text_left,
                top: text_top,
                bounds: *rect,
                color,
            });
        }
    }

    pub fn item_buffers(&self) -> &[Buffer] {
        &self.item_buffers
    }
}
