use glyphon::{Buffer, CustomGlyphId, FontSystem, Metrics, Shaping};

use crate::draw::DrawContext;
use crate::font;
use crate::icons;
use crate::layout::{update_if_changed, Rect, TextSpec};
use crate::theme::{DropdownTheme, Theme};

#[derive(Debug, Clone)]
pub enum MenuAction {
    NewShell(String),
    OpenSshDialog,
    ConnectSavedSession(String),
    Copy,
    Paste,
}

pub enum MenuPosition {
    BelowAnchor(Rect),
    AtPoint(f32, f32),
}

pub enum MenuEntry {
    Item(MenuItem),
    Separator,
}

impl MenuEntry {
    pub fn item(label: &str, action: MenuAction) -> Self {
        MenuEntry::Item(MenuItem {
            label: label.to_string(),
            action,
            icon: None,
            closeable: false,
        })
    }

    pub fn item_with_icon(label: &str, action: MenuAction, icon: CustomGlyphId) -> Self {
        MenuEntry::Item(MenuItem {
            label: label.to_string(),
            action,
            icon: Some(icon),
            closeable: false,
        })
    }

    pub fn closeable_item_with_icon(label: &str, action: MenuAction, icon: CustomGlyphId) -> Self {
        MenuEntry::Item(MenuItem {
            label: label.to_string(),
            action,
            icon: Some(icon),
            closeable: true,
        })
    }
}

pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
    pub icon: Option<CustomGlyphId>,
    pub closeable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropdownElement {
    None,
    Item(usize),
    CloseButton(usize),
}

pub struct DropdownMenu {
    items: Vec<MenuItem>,
    item_buffers: Vec<Buffer>,
    item_rects: Vec<Rect>,
    close_rects: Vec<Rect>,
    separator_rects: Vec<Rect>,
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
            close_rects: Vec::new(),
            separator_rects: Vec::new(),
            menu_rect: Rect::ZERO,
            hover: DropdownElement::None,
            visible: false,
        }
    }

    pub fn open(
        &mut self,
        entries: Vec<MenuEntry>,
        position: MenuPosition,
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
        let sep_h = theme.separator_height * scale;
        let icon_size = theme.icon_size * scale;
        let icon_gap = theme.icon_gap * scale;
        let close_size = theme.close_size * scale;

        // Calculate total content height
        let mut content_h = 0.0;
        for entry in &entries {
            match entry {
                MenuEntry::Item(_) => content_h += item_h,
                MenuEntry::Separator => content_h += sep_h,
            }
        }

        let menu_h = padding * 2.0 + content_h + border * 2.0;

        let (mut menu_x, menu_y) = match position {
            MenuPosition::BelowAnchor(anchor_rect) => {
                let x = anchor_rect.x + (anchor_rect.width - menu_w) / 2.0;
                let y = anchor_rect.y + anchor_rect.height + gap;
                (x, y)
            }
            MenuPosition::AtPoint(x, y) => (x, y),
        };

        if menu_x + menu_w > surface_width {
            menu_x = surface_width - menu_w;
        }
        if menu_x < 0.0 {
            menu_x = 0.0;
        }

        let final_y = if menu_y + menu_h > surface_height {
            match position {
                MenuPosition::BelowAnchor(anchor_rect) => (anchor_rect.y - gap - menu_h).max(0.0),
                MenuPosition::AtPoint(_, y) => (y - menu_h).max(0.0),
            }
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

        // Lay out entries in order
        self.item_rects.clear();
        self.close_rects.clear();
        self.separator_rects.clear();
        let mut items = Vec::new();
        let mut y = inner_y;

        for entry in entries {
            match entry {
                MenuEntry::Separator => {
                    self.separator_rects.push(Rect {
                        x: inner_x,
                        y,
                        width: inner_w,
                        height: sep_h,
                    });
                    y += sep_h;
                }
                MenuEntry::Item(item) => {
                    self.item_rects.push(Rect {
                        x: inner_x,
                        y,
                        width: inner_w,
                        height: item_h,
                    });

                    if item.closeable {
                        self.close_rects.push(Rect {
                            x: inner_x + inner_w - item_pad_h - close_size,
                            y: y + (item_h - close_size) / 2.0,
                            width: close_size,
                            height: close_size,
                        });
                    } else {
                        self.close_rects.push(Rect::ZERO);
                    }

                    items.push(item);
                    y += item_h;
                }
            }
        }

        // Create/reuse text buffers
        let metrics = Metrics::new(theme.font_size, theme.font_size * font::LINE_HEIGHT);
        while self.item_buffers.len() < items.len() {
            self.item_buffers.push(Buffer::new(font_system, metrics));
        }
        self.item_buffers.truncate(items.len());

        for (i, item) in items.iter().enumerate() {
            let buf = &mut self.item_buffers[i];
            buf.set_metrics(font_system, metrics);

            let mut text_w = inner_w - 2.0 * item_pad_h;
            if item.icon.is_some() {
                text_w -= icon_size + icon_gap;
            }
            if item.closeable {
                text_w -= close_size + icon_gap;
            }

            let buf_width = text_w / scale;
            buf.set_size(font_system, Some(buf_width), Some(metrics.line_height));
            buf.set_text(
                font_system,
                &item.label,
                font::default_attrs(),
                Shaping::Advanced,
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

        // Close buttons first (higher priority, with padding for usability)
        for (i, rect) in self.close_rects.iter().enumerate() {
            if self.items[i].closeable {
                let pad = 4.0;
                if x >= rect.x - pad
                    && x < rect.x + rect.width + pad
                    && y >= rect.y - pad
                    && y < rect.y + rect.height + pad
                {
                    return DropdownElement::CloseButton(i);
                }
            }
        }

        for (i, rect) in self.item_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return DropdownElement::Item(i);
            }
        }

        DropdownElement::None
    }

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
        let icon_size = t.icon_size * scale;
        let icon_gap = t.icon_gap * scale;
        let close_size = t.close_size * scale;

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

        // Separators (use rounded_rect since overlay flat_quads are not rendered)
        for rect in &self.separator_rects {
            ctx.rounded_rect(*rect, colors.dropdown_border.to_linear_f32(), 0.0);
        }

        // Hover highlight (both Item and CloseButton highlight the row)
        match self.hover {
            DropdownElement::Item(idx) | DropdownElement::CloseButton(idx) => {
                if let Some(rect) = self.item_rects.get(idx) {
                    ctx.rounded_rect(
                        *rect,
                        colors.dropdown_item_hover.to_linear_f32(),
                        item_radius,
                    );
                }
            }
            DropdownElement::None => {}
        }

        // Items
        let line_h = t.font_size * font::LINE_HEIGHT * scale;
        for (i, rect) in self.item_rects.iter().enumerate() {
            let item = &self.items[i];
            let is_hovered = matches!(
                self.hover,
                DropdownElement::Item(idx) | DropdownElement::CloseButton(idx) if idx == i
            );

            let mut text_left = rect.x + item_pad_h;

            // Leading icon
            if let Some(icon_id) = item.icon {
                let icon_x = text_left;
                let icon_y = rect.y + (rect.height - icon_size) / 2.0;
                ctx.icon(icon_id, icon_x, icon_y, icon_size);
                text_left += icon_size + icon_gap;
            }

            // Text
            let text_top = rect.y + (rect.height - line_h) / 2.0;
            let color = if is_hovered {
                colors.dropdown_text_active.to_glyphon()
            } else {
                colors.dropdown_text.to_glyphon()
            };

            let mut text_width = rect.x + rect.width - item_pad_h - text_left;
            if item.closeable {
                text_width -= close_size + icon_gap;
            }

            text_specs.push(TextSpec {
                buffer_index: i,
                left: text_left,
                top: text_top,
                bounds: Rect {
                    x: text_left,
                    y: rect.y,
                    width: text_width,
                    height: rect.height,
                },
                color,
            });

            // Close button
            if item.closeable {
                let close_rect = &self.close_rects[i];
                let close_icon = if matches!(self.hover, DropdownElement::CloseButton(idx) if idx == i)
                {
                    icons::ICON_CLOSE_HOVERED
                } else {
                    icons::ICON_CLOSE
                };
                ctx.icon(close_icon, close_rect.x, close_rect.y, close_size);
            }
        }
    }

    pub fn item_buffers(&self) -> &[Buffer] {
        &self.item_buffers
    }
}
