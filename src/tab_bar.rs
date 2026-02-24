use glyphon::{Attrs, Buffer, CustomGlyph, Family, FontSystem, Metrics, Shaping};

use crate::colors::ColorScheme;
use crate::icons;
use crate::layout::Rect;
use crate::panel::BgQuad;

const TAB_BAR_HEIGHT: f32 = 36.0;
const TAB_PADDING_H: f32 = 12.0;
const TAB_PADDING_V: f32 = 5.0;
const TAB_GAP: f32 = 4.0;
const ICON_SIZE: f32 = 13.0;
const ICON_GAP: f32 = 6.0;
const CLOSE_SIZE: f32 = 11.0;
const BORDER_WIDTH: f32 = 1.0;
const SEPARATOR_HEIGHT: f32 = 1.0;
const TAB_RADIUS: f32 = 6.0;
const PLUS_SIZE: f32 = 22.0;
const PLUS_ICON_SIZE: f32 = 12.0;
const PLUS_RADIUS: f32 = 5.0;
const TAB_FONT_SIZE: f32 = 11.0;
const TAB_LINE_HEIGHT: f32 = 1.2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabBarHover {
    None,
    Tab(usize),
    CloseButton(usize),
    PlusButton,
}

#[derive(Debug)]
pub enum TabBarHit {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    None,
}

/// A filled rounded rectangle for SDF rendering.
/// When `shadow_softness` > 0, renders as a soft shadow instead of a sharp rect.
pub struct RoundedQuad {
    pub rect: Rect,
    pub color: [f32; 4],
    pub radius: f32,
    pub shadow_softness: f32,
}

/// Structured draw commands for the tab bar.
pub struct TabBarDrawCommands {
    /// Flat quads (separator line only)
    pub flat_quads: Vec<BgQuad>,
    /// Rounded rect fills (tab backgrounds, borders — rendered via SDF pipeline)
    pub rounded_quads: Vec<RoundedQuad>,
    pub custom_glyphs: Vec<CustomGlyph>,
    pub text_areas: Vec<TabBarTextArea>,
}

pub struct TabBarTextArea {
    pub buffer_index: usize,
    pub left: f32,
    pub top: f32,
    pub bounds: Rect,
    pub is_active: bool,
}

pub struct TabBar {
    tab_buffers: Vec<Buffer>,
    tab_rects: Vec<Rect>,
    close_rects: Vec<Rect>,
    plus_rect: Rect,
    active_tab: usize,
    hover: TabBarHover,
}

impl TabBar {
    pub fn new() -> Self {
        Self {
            tab_buffers: Vec::new(),
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            plus_rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            active_tab: 0,
            hover: TabBarHover::None,
        }
    }

    pub fn height(scale_factor: f32) -> f32 {
        TAB_BAR_HEIGHT * scale_factor
    }

    pub fn set_active(&mut self, idx: usize) {
        self.active_tab = idx;
    }

    pub fn update(
        &mut self,
        titles: &[String],
        active: usize,
        surface_width: f32,
        y_offset: f32,
        scale_factor: f32,
        font_system: &mut FontSystem,
    ) {
        self.active_tab = active;

        let tab_metrics = Metrics::new(TAB_FONT_SIZE, TAB_FONT_SIZE * TAB_LINE_HEIGHT);
        let pad_h = TAB_PADDING_H * scale_factor;
        let pad_v = TAB_PADDING_V * scale_factor;
        let gap = TAB_GAP * scale_factor;
        let icon_size = ICON_SIZE * scale_factor;
        let icon_gap = ICON_GAP * scale_factor;
        let close_size = CLOSE_SIZE * scale_factor;
        let bar_h = TAB_BAR_HEIGHT * scale_factor;
        let plus_w = PLUS_SIZE * scale_factor;

        // Tab height = padding_v + max(icon, close, text_height) + padding_v
        // Content height dominated by icon (13px > close 11px)
        let tab_h = pad_v * 2.0 + icon_size;
        let margin_top = y_offset + (bar_h - tab_h) / 2.0;

        // Resize buffers
        while self.tab_buffers.len() < titles.len() {
            self.tab_buffers.push(Buffer::new(font_system, tab_metrics));
        }
        self.tab_buffers.truncate(titles.len());

        // First pass: compute tab widths to determine total width for centering
        let mut tab_widths = Vec::with_capacity(titles.len());
        for (i, title) in titles.iter().enumerate() {
            let buf = &mut self.tab_buffers[i];
            buf.set_metrics(font_system, tab_metrics);
            buf.set_size(font_system, Some(300.0), Some(tab_metrics.line_height));
            buf.set_text(
                font_system,
                title,
                Attrs::new().family(Family::Name("JetBrains Mono")),
                Shaping::Basic,
            );
            buf.shape_until_scroll(font_system, false);

            let text_width = buf
                .layout_runs()
                .next()
                .map(|run| run.glyphs.iter().map(|g| g.w).sum::<f32>())
                .unwrap_or(50.0)
                * scale_factor;

            let tab_width =
                pad_h + icon_size + icon_gap + text_width + icon_gap + close_size + pad_h;
            tab_widths.push(tab_width);
        }

        // Total width of all tabs + gaps + plus button
        let total_tabs_width: f32 = tab_widths.iter().sum();
        let total_gaps = if titles.is_empty() {
            0.0
        } else {
            gap * titles.len() as f32 // gaps between tabs + gap before plus
        };
        let total_width = total_tabs_width + total_gaps + plus_w;

        // Center horizontally within the panel area
        // y_offset doubles as x_offset since panel_area_padding is uniform
        let panel_x = y_offset;
        let start_x = panel_x + (surface_width - total_width) / 2.0;
        let mut x = start_x.max(panel_x);

        // Layout tabs
        self.tab_rects.clear();
        self.close_rects.clear();

        for (i, &tab_width) in tab_widths.iter().enumerate() {
            self.tab_rects.push(Rect {
                x,
                y: margin_top,
                width: tab_width,
                height: tab_h,
            });

            let close_x = x + tab_width - pad_h - close_size;
            let close_y = margin_top + (tab_h - close_size) / 2.0;
            self.close_rects.push(Rect {
                x: close_x,
                y: close_y,
                width: close_size,
                height: close_size,
            });

            x += tab_width + gap;
            let _ = i;
        }

        // Plus button (centered vertically in bar)
        let plus_y = y_offset + (bar_h - plus_w) / 2.0;
        self.plus_rect = Rect {
            x,
            y: plus_y,
            width: plus_w,
            height: plus_w,
        };
    }

    pub fn compute_hover(&self, x: f32, y: f32) -> TabBarHover {
        // Close buttons checked first with expanded hit area for easier targeting
        for (i, rect) in self.close_rects.iter().enumerate() {
            let is_active = i == self.active_tab;
            let is_tab_hovered = matches!(self.hover, TabBarHover::Tab(idx) | TabBarHover::CloseButton(idx) if idx == i);
            if (is_active || is_tab_hovered) && rect.contains_padded(x, y, 4.0) {
                return TabBarHover::CloseButton(i);
            }
        }

        for (i, rect) in self.tab_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return TabBarHover::Tab(i);
            }
        }

        if self.plus_rect.contains(x, y) {
            return TabBarHover::PlusButton;
        }

        TabBarHover::None
    }

    pub fn set_hover(&mut self, hover: TabBarHover) -> bool {
        if self.hover != hover {
            self.hover = hover;
            true
        } else {
            false
        }
    }

    pub fn hit_test(&self, x: f32, y: f32) -> TabBarHit {
        for (i, rect) in self.close_rects.iter().enumerate() {
            let is_active = i == self.active_tab;
            let is_tab_hovered = matches!(self.hover, TabBarHover::Tab(idx) | TabBarHover::CloseButton(idx) if idx == i);
            if (is_active || is_tab_hovered) && rect.contains_padded(x, y, 4.0) {
                return TabBarHit::CloseTab(i);
            }
        }

        for (i, rect) in self.tab_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return TabBarHit::Tab(i);
            }
        }

        if self.plus_rect.contains(x, y) {
            return TabBarHit::NewTab;
        }

        TabBarHit::None
    }

    pub fn draw_commands(
        &self,
        colors: &ColorScheme,
        scale_factor: f32,
        panel_x: f32,
        panel_y: f32,
        panel_width: f32,
    ) -> TabBarDrawCommands {
        let mut flat_quads = Vec::new();
        let mut rounded_quads = Vec::new();
        let mut custom_glyphs = Vec::new();
        let mut text_areas = Vec::new();

        let icon_size = ICON_SIZE * scale_factor;
        let icon_gap = ICON_GAP * scale_factor;
        let close_size = CLOSE_SIZE * scale_factor;
        let pad_h = TAB_PADDING_H * scale_factor;
        let border = BORDER_WIDTH * scale_factor;
        let tab_bar_h = TAB_BAR_HEIGHT * scale_factor;
        let radius = TAB_RADIUS * scale_factor;
        let plus_icon_size = PLUS_ICON_SIZE * scale_factor;
        let plus_radius = PLUS_RADIUS * scale_factor;

        let active_fill = colors.tab_active_fill();
        let active_stroke = colors.tab_active_stroke();
        let hover_bg = colors.tab_hover_bg();
        let hover_stroke = colors.tab_hover_stroke();

        for (i, rect) in self.tab_rects.iter().enumerate() {
            let is_active = i == self.active_tab;
            let is_hovered = matches!(self.hover, TabBarHover::Tab(idx) | TabBarHover::CloseButton(idx) if idx == i);

            if is_active {
                push_stroked_rounded_rect(
                    &mut rounded_quads,
                    rect,
                    active_stroke,
                    active_fill,
                    radius,
                    border,
                );
            } else if is_hovered {
                push_stroked_rounded_rect(
                    &mut rounded_quads,
                    rect,
                    hover_stroke,
                    hover_bg,
                    radius,
                    border,
                );
            }

            // Terminal icon
            let icon_x = rect.x + pad_h;
            let icon_y = rect.y + (rect.height - icon_size) / 2.0;
            custom_glyphs.push(icon_glyph(icons::ICON_TERMINAL, icon_x, icon_y, icon_size));

            // Tab label text area — vertically centered within the tab rect
            let text_left = icon_x + icon_size + icon_gap;
            let text_width =
                rect.width - pad_h - icon_size - icon_gap - icon_gap - close_size - pad_h;
            let line_h = TAB_FONT_SIZE * TAB_LINE_HEIGHT * scale_factor;
            let text_top = rect.y + (rect.height - line_h) / 2.0;
            text_areas.push(TabBarTextArea {
                buffer_index: i,
                left: text_left,
                top: text_top,
                bounds: Rect {
                    x: text_left,
                    y: rect.y,
                    width: text_width,
                    height: rect.height,
                },
                is_active,
            });

            // Close button icon (only on active or hovered tabs)
            if is_active || is_hovered {
                let close_rect = &self.close_rects[i];
                let close_icon = if matches!(self.hover, TabBarHover::CloseButton(idx) if idx == i)
                {
                    icons::ICON_CLOSE_HOVERED
                } else {
                    icons::ICON_CLOSE
                };
                custom_glyphs.push(icon_glyph(
                    close_icon,
                    close_rect.x,
                    close_rect.y,
                    close_size,
                ));
            }
        }

        // "+" button
        let is_plus_hovered = matches!(self.hover, TabBarHover::PlusButton);
        if is_plus_hovered {
            // Hover: just a filled rounded rect, no stroke
            rounded_quads.push(RoundedQuad {
                rect: self.plus_rect,
                color: hover_bg,
                radius: plus_radius,
                shadow_softness: 0.0,
            });
        }

        // "+" icon centered in plus button
        let plus_ix = self.plus_rect.x + (self.plus_rect.width - plus_icon_size) / 2.0;
        let plus_iy = self.plus_rect.y + (self.plus_rect.height - plus_icon_size) / 2.0;
        custom_glyphs.push(icon_glyph(
            icons::ICON_ADD,
            plus_ix,
            plus_iy,
            plus_icon_size,
        ));

        // Separator line at bottom of tab bar area (inside panel)
        flat_quads.push(BgQuad {
            x: panel_x,
            y: panel_y + tab_bar_h - SEPARATOR_HEIGHT * scale_factor,
            w: panel_width,
            h: SEPARATOR_HEIGHT * scale_factor,
            color: colors.tab_separator(),
        });

        TabBarDrawCommands {
            flat_quads,
            rounded_quads,
            custom_glyphs,
            text_areas,
        }
    }

    pub fn plus_rect(&self) -> Rect {
        self.plus_rect
    }

    pub fn tab_buffers(&self) -> &[Buffer] {
        &self.tab_buffers
    }
}

fn push_stroked_rounded_rect(
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

fn icon_glyph(id: glyphon::CustomGlyphId, left: f32, top: f32, size: f32) -> CustomGlyph {
    CustomGlyph {
        id,
        left,
        top,
        width: size,
        height: size,
        color: None,
        snap_to_physical_pixel: true,
        metadata: 0,
    }
}
