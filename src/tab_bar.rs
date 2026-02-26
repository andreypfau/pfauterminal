use glyphon::{Buffer, FontSystem, Metrics, Shaping};

use crate::draw::{centered_text, DrawContext};
use crate::font;
use crate::font::LINE_HEIGHT as TAB_LINE_HEIGHT;
use crate::icons;
use crate::layout::{update_if_changed, Rect, TextSpec};
use crate::theme::{TabBarTheme, Theme};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TabBarElement {
    None,
    Tab(usize),
    CloseButton(usize),
    PlusButton,
}

pub struct TabBar {
    tab_buffers: Vec<Buffer>,
    tab_rects: Vec<Rect>,
    close_rects: Vec<Rect>,
    plus_rect: Rect,
    active_tab: usize,
    hover: TabBarElement,
}

impl TabBar {
    pub fn new() -> Self {
        Self {
            tab_buffers: Vec::new(),
            tab_rects: Vec::new(),
            close_rects: Vec::new(),
            plus_rect: Rect::ZERO,
            active_tab: 0,
            hover: TabBarElement::None,
        }
    }

    pub fn height(theme: &TabBarTheme, scale_factor: f32) -> f32 {
        theme.height * scale_factor
    }

    pub fn update(
        &mut self,
        titles: &[String],
        active: usize,
        surface_width: f32,
        y_offset: f32,
        scale_factor: f32,
        font_system: &mut FontSystem,
        theme: &TabBarTheme,
    ) {
        self.active_tab = active;

        let tab_metrics = Metrics::new(theme.font_size, theme.font_size * TAB_LINE_HEIGHT);
        let pad_h = theme.tab_padding_h * scale_factor;
        let pad_v = theme.tab_padding_v * scale_factor;
        let gap = theme.tab_gap * scale_factor;
        let icon_size = theme.icon_size * scale_factor;
        let icon_gap = theme.icon_gap * scale_factor;
        let close_size = theme.close_size * scale_factor;
        let bar_h = theme.height * scale_factor;
        let plus_w = theme.plus_size * scale_factor;

        let tab_h = pad_v * 2.0 + icon_size;
        let margin_top = y_offset + (bar_h - tab_h) / 2.0;

        // Resize buffers
        while self.tab_buffers.len() < titles.len() {
            self.tab_buffers.push(Buffer::new(font_system, tab_metrics));
        }
        self.tab_buffers.truncate(titles.len());

        // Compute tab widths
        let mut tab_widths = Vec::with_capacity(titles.len());
        for (i, title) in titles.iter().enumerate() {
            let buf = &mut self.tab_buffers[i];
            buf.set_metrics(font_system, tab_metrics);
            buf.set_size(font_system, Some(300.0), Some(tab_metrics.line_height));
            buf.set_text(font_system, title, font::default_attrs(), Shaping::Basic);
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

        let total_tabs_width: f32 = tab_widths.iter().sum();
        let total_gaps = if titles.is_empty() {
            0.0
        } else {
            gap * titles.len() as f32
        };
        let total_width = total_tabs_width + total_gaps + plus_w;

        let panel_x = y_offset;
        let start_x = panel_x + (surface_width - total_width) / 2.0;
        let mut x = start_x.max(panel_x);

        self.tab_rects.clear();
        self.close_rects.clear();

        for &tab_width in &tab_widths {
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
        }

        let plus_y = y_offset + (bar_h - plus_w) / 2.0;
        self.plus_rect = Rect {
            x,
            y: plus_y,
            width: plus_w,
            height: plus_w,
        };
    }

    pub fn hit_test(&self, x: f32, y: f32) -> TabBarElement {
        for (i, rect) in self.close_rects.iter().enumerate() {
            let is_active = i == self.active_tab;
            let is_tab_hovered = matches!(self.hover, TabBarElement::Tab(idx) | TabBarElement::CloseButton(idx) if idx == i);
            let pad = 4.0;
            let hit = x >= rect.x - pad
                && x < rect.x + rect.width + pad
                && y >= rect.y - pad
                && y < rect.y + rect.height + pad;
            if (is_active || is_tab_hovered) && hit {
                return TabBarElement::CloseButton(i);
            }
        }
        for (i, rect) in self.tab_rects.iter().enumerate() {
            if rect.contains(x, y) {
                return TabBarElement::Tab(i);
            }
        }
        if self.plus_rect.contains(x, y) {
            return TabBarElement::PlusButton;
        }
        TabBarElement::None
    }

    pub fn set_hover(&mut self, hover: TabBarElement) -> bool {
        update_if_changed(&mut self.hover, hover)
    }

    pub fn draw(
        &self,
        ctx: &mut DrawContext,
        text_specs: &mut Vec<TextSpec>,
        theme: &Theme,
        scale_factor: f32,
        panel_x: f32,
        panel_y: f32,
        panel_width: f32,
    ) {
        let t = &theme.tab_bar;
        let colors = &theme.colors;

        let icon_size = t.icon_size * scale_factor;
        let icon_gap = t.icon_gap * scale_factor;
        let close_size = t.close_size * scale_factor;
        let pad_h = t.tab_padding_h * scale_factor;
        let border = t.border_width * scale_factor;
        let tab_bar_h = t.height * scale_factor;
        let radius = t.tab_radius * scale_factor;
        let plus_icon_size = t.plus_icon_size * scale_factor;
        let plus_radius = t.plus_radius * scale_factor;

        let active_fill = colors.tab_active_fill.to_linear_f32();
        let active_stroke = colors.tab_active_stroke.to_linear_f32();
        let hover_bg = colors.tab_hover_bg.to_linear_f32();
        let hover_stroke = colors.tab_hover_stroke.to_linear_f32();

        for (i, rect) in self.tab_rects.iter().enumerate() {
            let is_active = i == self.active_tab;
            let is_hovered = matches!(self.hover, TabBarElement::Tab(idx) | TabBarElement::CloseButton(idx) if idx == i);

            if is_active {
                ctx.stroked_rect(rect, active_stroke, active_fill, radius, border);
            } else if is_hovered {
                ctx.stroked_rect(rect, hover_stroke, hover_bg, radius, border);
            }

            // Terminal icon
            let icon_x = rect.x + pad_h;
            let icon_y = rect.y + (rect.height - icon_size) / 2.0;
            ctx.icon(icons::ICON_TERMINAL, icon_x, icon_y, icon_size);

            // Tab label
            let text_left = icon_x + icon_size + icon_gap;
            let text_width =
                rect.width - pad_h - icon_size - icon_gap - icon_gap - close_size - pad_h;
            let line_h = t.font_size * TAB_LINE_HEIGHT * scale_factor;
            let color = if is_active {
                colors.tab_active_text.to_glyphon()
            } else {
                colors.foreground.to_glyphon()
            };
            let bounds = Rect {
                x: text_left,
                y: rect.y,
                width: text_width,
                height: rect.height,
            };
            text_specs.push(centered_text(i, text_left, &bounds, line_h, color));

            // Close button (only on active or hovered tabs)
            if is_active || is_hovered {
                let close_rect = &self.close_rects[i];
                let close_icon = if matches!(self.hover, TabBarElement::CloseButton(idx) if idx == i)
                {
                    icons::ICON_CLOSE_HOVERED
                } else {
                    icons::ICON_CLOSE
                };
                ctx.icon(close_icon, close_rect.x, close_rect.y, close_size);
            }
        }

        // "+" button
        if matches!(self.hover, TabBarElement::PlusButton) {
            ctx.rounded_rect(self.plus_rect, hover_bg, plus_radius);
        }

        ctx.icon_centered(icons::ICON_ADD, &self.plus_rect, plus_icon_size);

        // Separator line
        ctx.flat_quad(
            Rect {
                x: panel_x,
                y: panel_y + tab_bar_h - t.separator_height * scale_factor,
                width: panel_width,
                height: t.separator_height * scale_factor,
            },
            colors.tab_separator.to_linear_f32(),
        );
    }

    pub fn plus_rect(&self) -> Rect {
        self.plus_rect
    }

    pub fn tab_buffers(&self) -> &[Buffer] {
        &self.tab_buffers
    }
}
