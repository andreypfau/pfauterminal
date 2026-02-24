use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

use crate::colors::ColorScheme;
use crate::layout::Rect;
use crate::tab_bar::RoundedQuad;

// Design constants (logical pixels, matching Pencil spec)
const MENU_WIDTH: f32 = 200.0;
const MENU_CORNER_RADIUS: f32 = 8.0;
const MENU_PADDING: f32 = 6.0;
const MENU_ITEM_HEIGHT: f32 = 32.0;
const MENU_ITEM_PADDING_H: f32 = 12.0;
const MENU_ITEM_RADIUS: f32 = 6.0;
const MENU_BORDER_WIDTH: f32 = 1.0;
const MENU_FONT_SIZE: f32 = 13.0;
const MENU_ANCHOR_GAP: f32 = 4.0;
const MENU_SHADOW_SPREAD: f32 = 20.0;
const MENU_SHADOW_OFFSET_Y: f32 = 4.0;

#[derive(Debug, Clone)]
pub enum MenuAction {
    NewShell(String),
    #[allow(dead_code)]
    Custom(u32),
}

pub struct MenuItem {
    pub label: String,
    pub action: MenuAction,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropdownHover {
    None,
    Item(usize),
}

#[derive(Debug)]
pub enum DropdownHit {
    Item(usize),
    Outside,
    None,
}

pub struct DropdownTextArea {
    pub buffer_index: usize,
    pub left: f32,
    pub top: f32,
    pub bounds: Rect,
    pub is_hovered: bool,
}

pub struct DropdownDrawCommands {
    pub rounded_quads: Vec<RoundedQuad>,
    pub text_areas: Vec<DropdownTextArea>,
}

pub struct DropdownMenu {
    items: Vec<MenuItem>,
    item_buffers: Vec<Buffer>,
    item_rects: Vec<Rect>,
    menu_rect: Rect,
    hover: DropdownHover,
    visible: bool,
}

impl DropdownMenu {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            item_buffers: Vec::new(),
            item_rects: Vec::new(),
            menu_rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            hover: DropdownHover::None,
            visible: false,
        }
    }

    pub fn open(
        &mut self,
        items: Vec<MenuItem>,
        anchor_rect: Rect,
        scale: f32,
        surface_width: f32,
        surface_height: f32,
        font_system: &mut FontSystem,
    ) {
        let menu_w = MENU_WIDTH * scale;
        let padding = MENU_PADDING * scale;
        let item_h = MENU_ITEM_HEIGHT * scale;
        let item_pad_h = MENU_ITEM_PADDING_H * scale;
        let border = MENU_BORDER_WIDTH * scale;
        let gap = MENU_ANCHOR_GAP * scale;

        let content_h = item_h * items.len() as f32;
        let menu_h = padding * 2.0 + content_h + border * 2.0;

        // Position: centered below anchor, clamped to surface bounds
        let mut menu_x = anchor_rect.x + (anchor_rect.width - menu_w) / 2.0;
        let menu_y = anchor_rect.y + anchor_rect.height + gap;

        // Clamp horizontally
        if menu_x + menu_w > surface_width {
            menu_x = surface_width - menu_w;
        }
        if menu_x < 0.0 {
            menu_x = 0.0;
        }

        // Clamp vertically (flip above anchor if needed)
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

        // Compute item rects (inside border + padding)
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

        // Shape text buffers
        let metrics = Metrics::new(MENU_FONT_SIZE, MENU_FONT_SIZE * 1.2);
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
                Attrs::new().family(Family::Name("JetBrains Mono")),
                Shaping::Basic,
            );
            buf.shape_until_scroll(font_system, false);
        }

        self.items = items;
        self.hover = DropdownHover::None;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.hover = DropdownHover::None;
    }

    pub fn is_open(&self) -> bool {
        self.visible
    }

    pub fn compute_hover(&self, x: f32, y: f32) -> DropdownHover {
        if !self.visible {
            return DropdownHover::None;
        }
        for (i, rect) in self.item_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return DropdownHover::Item(i);
            }
        }
        DropdownHover::None
    }

    pub fn set_hover(&mut self, hover: DropdownHover) -> bool {
        if self.hover != hover {
            self.hover = hover;
            true
        } else {
            false
        }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> DropdownHit {
        if !self.visible {
            return DropdownHit::None;
        }

        for (i, rect) in self.item_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return DropdownHit::Item(i);
            }
        }

        if self.menu_rect.contains(x, y) {
            return DropdownHit::None;
        }

        DropdownHit::Outside
    }

    pub fn action_for(&self, idx: usize) -> Option<&MenuAction> {
        self.items.get(idx).map(|item| &item.action)
    }

    pub fn draw_commands(&self, colors: &ColorScheme, scale: f32) -> DropdownDrawCommands {
        let mut rounded_quads = Vec::new();
        let mut text_areas = Vec::new();

        if !self.visible {
            return DropdownDrawCommands {
                rounded_quads,
                text_areas,
            };
        }

        let radius = MENU_CORNER_RADIUS * scale;
        let border = MENU_BORDER_WIDTH * scale;
        let item_radius = MENU_ITEM_RADIUS * scale;
        let item_pad_h = MENU_ITEM_PADDING_H * scale;
        let shadow_spread = MENU_SHADOW_SPREAD * scale;
        let shadow_offset_y = MENU_SHADOW_OFFSET_Y * scale;

        // 0. Drop shadow (soft SDF shadow behind the menu)
        rounded_quads.push(RoundedQuad {
            rect: Rect {
                x: self.menu_rect.x,
                y: self.menu_rect.y + shadow_offset_y,
                width: self.menu_rect.width,
                height: self.menu_rect.height,
            },
            color: colors.dropdown_shadow(),
            radius,
            shadow_softness: shadow_spread,
        });

        // 1. Border rect (outer)
        rounded_quads.push(RoundedQuad {
            rect: self.menu_rect,
            color: colors.dropdown_border(),
            radius,
            shadow_softness: 0.0,
        });

        // 2. Fill rect (inset by border)
        rounded_quads.push(RoundedQuad {
            rect: self.menu_rect.inset(border),
            color: colors.dropdown_bg(),
            radius: (radius - border).max(0.0),
            shadow_softness: 0.0,
        });

        // 3. Hover highlight (if any)
        if let DropdownHover::Item(idx) = self.hover {
            if let Some(rect) = self.item_rects.get(idx) {
                rounded_quads.push(RoundedQuad {
                    rect: *rect,
                    color: colors.dropdown_item_hover(),
                    radius: item_radius,
                    shadow_softness: 0.0,
                });
            }
        }

        // 4. Text areas
        let line_h = MENU_FONT_SIZE * 1.2 * scale;
        for (i, rect) in self.item_rects.iter().enumerate() {
            let is_hovered = matches!(self.hover, DropdownHover::Item(idx) if idx == i);
            let text_left = rect.x + item_pad_h;
            let text_top = rect.y + (rect.height - line_h) / 2.0;

            text_areas.push(DropdownTextArea {
                buffer_index: i,
                left: text_left,
                top: text_top,
                bounds: *rect,
                is_hovered,
            });
        }

        DropdownDrawCommands {
            rounded_quads,
            text_areas,
        }
    }

    pub fn item_buffers(&self) -> &[Buffer] {
        &self.item_buffers
    }
}
