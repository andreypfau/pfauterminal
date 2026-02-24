use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use glyphon::{Attrs, Buffer, Family, FontSystem, Shaping};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};

use crate::colors::ColorScheme;
use crate::font::{self, CellMetrics};
use crate::layout::Rect;
use crate::panel::{
    BgQuad, CellInfo, CursorInfo, Panel, PanelAction, PanelDrawCommands, PanelId, PanelViewport,
    TextLineSpec,
};
use crate::terminal::{EventProxy, TermSize, Terminal};

/// Padding between panel edge and cell grid (logical pixels).
const ISLAND_PADDING: f32 = 16.0;
/// Corner radius of the island (logical pixels).
const ISLAND_RADIUS: f32 = 10.0;
/// Island stroke width (logical pixels).
const ISLAND_STROKE_WIDTH: f32 = 0.5;

pub struct TerminalPanel {
    id: PanelId,
    terminal: Terminal,
    viewport: Option<PanelViewport>,
    line_buffers: Vec<Buffer>,
    line_hashes: Vec<u64>,
    title: String,
    pending_actions: Vec<PanelAction>,
    // Reusable scratch buffers
    scratch_text: String,
    scratch_spans: Vec<(Range<usize>, Attrs<'static>)>,
}

impl TerminalPanel {
    pub fn new(
        id: PanelId,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        event_proxy: EventProxy,
        shell: Option<String>,
    ) -> Self {
        let size = TermSize::new(cols, rows);
        let terminal = Terminal::new(size, cell_width, cell_height, event_proxy, shell);
        Self {
            id,
            terminal,
            viewport: None,
            line_buffers: Vec::new(),
            line_hashes: Vec::new(),
            title: String::from("Terminal"),
            pending_actions: Vec::new(),
            scratch_text: String::with_capacity(256),
            scratch_spans: Vec::with_capacity(256),
        }
    }

    fn extract_cells(&self, colors: &ColorScheme) -> (Vec<Vec<CellInfo>>, Option<CursorInfo>) {
        let vp = match &self.viewport {
            Some(vp) => vp,
            None => return (Vec::new(), None),
        };

        let term = self.terminal.term.lock();
        let content = term.renderable_content();

        let rows = vp.rows;
        let cols = vp.cols;
        let default_fg = colors.fg_glyphon();

        let mut lines: Vec<Vec<CellInfo>> = (0..rows)
            .map(|row| {
                (0..cols)
                    .map(|col| CellInfo {
                        row,
                        col,
                        c: ' ',
                        fg: default_fg,
                        bg: [0.0; 4],
                        is_default_bg: true,
                        bold: false,
                        italic: false,
                    })
                    .collect()
            })
            .collect();

        for indexed in content.display_iter {
            let row = indexed.point.line.0 as usize;
            let col = indexed.point.column.0;
            if row < rows && col < cols {
                let cell = &*indexed;
                let flags = cell.flags;
                let bold = flags.contains(alacritty_terminal::term::cell::Flags::BOLD);
                let italic = flags.contains(alacritty_terminal::term::cell::Flags::ITALIC);

                let (fg_color, bg_color) =
                    if flags.contains(alacritty_terminal::term::cell::Flags::INVERSE) {
                        (cell.bg, cell.fg)
                    } else {
                        (cell.fg, cell.bg)
                    };

                lines[row][col] = CellInfo {
                    row,
                    col,
                    c: cell.c,
                    fg: colors.to_glyphon_fg(fg_color),
                    bg: colors.to_rgba(bg_color),
                    is_default_bg: colors.is_default_bg(bg_color),
                    bold,
                    italic,
                };
            }
        }

        let cursor = {
            let cp = content.cursor.point;
            let row = cp.line.0 as usize;
            let col = cp.column.0;
            if row < rows && col < cols {
                Some(CursorInfo { row, col })
            } else {
                None
            }
        };

        (lines, cursor)
    }

    /// Compute island and content rects from the panel's allocated rect.
    /// `top_inset` reserves space at the top of the island for the tab bar (physical px).
    /// The island covers the full rect; the content area is below the top_inset with padding.
    pub fn compute_island_rects_static(
        rect: &Rect,
        scale_factor: f32,
        top_inset: f32,
    ) -> (Rect, Rect) {
        let p = ISLAND_PADDING * scale_factor;

        let island = *rect;

        let content = Rect {
            x: island.x + p,
            y: island.y + top_inset + p,
            width: island.width - 2.0 * p,
            height: island.height - top_inset - 2.0 * p,
        };

        (island, content)
    }

    /// Compute cols/rows that fit in the content rect.
    pub fn compute_grid_size_static(
        content_rect: &Rect,
        cell: &CellMetrics,
        scale_factor: f32,
    ) -> (usize, usize) {
        let pcw = cell.width * scale_factor;
        let pch = cell.height * scale_factor;
        let cols = (content_rect.width / pcw).floor().max(1.0) as usize;
        let rows = (content_rect.height / pch).floor().max(1.0) as usize;
        (cols, rows)
    }
}

impl Panel for TerminalPanel {
    fn id(&self) -> PanelId {
        self.id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn set_viewport(&mut self, viewport: PanelViewport, cell: &CellMetrics) {
        let old_cols = self.viewport.as_ref().map(|v| v.cols);
        let old_rows = self.viewport.as_ref().map(|v| v.rows);

        let cell_w = (cell.width * viewport.scale_factor) as u16;
        let cell_h = (cell.height * viewport.scale_factor) as u16;

        if old_cols != Some(viewport.cols) || old_rows != Some(viewport.rows) {
            let size = TermSize::new(viewport.cols, viewport.rows);
            self.terminal.resize(size, cell_w, cell_h);
            self.line_hashes.clear();
        }

        self.viewport = Some(viewport);
    }

    fn handle_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }

        let bytes: Option<Vec<u8>> = match event.logical_key.as_ref() {
            Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
            Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
            Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
            Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
            Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
            Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
            Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
            Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
            Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
            Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
            Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
            Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
            Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
            Key::Named(NamedKey::Insert) => Some(b"\x1b[2~".to_vec()),
            Key::Character(c) => {
                if c.len() == 1 {
                    let ch = c.chars().next().unwrap();
                    if ch.is_ascii_lowercase() && event.text.is_none() {
                        Some(vec![ch as u8 - b'a' + 1])
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(b) = bytes {
            self.terminal.write(b);
            true
        } else if let Some(t) = &event.text {
            let s: String = t.to_string();
            if !s.is_empty() {
                self.terminal.write(Cow::Owned(s.into_bytes()));
                true
            } else {
                false
            }
        } else {
            false
        }
    }

    fn handle_scroll(&mut self, delta: MouseScrollDelta, cell_height: f64) -> bool {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as i32,
            MouseScrollDelta::PixelDelta(pos) => (pos.y / cell_height) as i32,
        };
        if lines != 0 {
            self.terminal.scroll(lines);
            true
        } else {
            false
        }
    }

    fn prepare_render(
        &mut self,
        font_system: &mut FontSystem,
        colors: &ColorScheme,
    ) -> PanelDrawCommands {
        let vp = match &self.viewport {
            Some(vp) => vp,
            None => {
                return PanelDrawCommands {
                    island_rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 0.0,
                        height: 0.0,
                    },
                    island_color: colors.bg_linear_f32(),
                    island_radius: 0.0,
                    island_stroke_color: [0.0; 4],
                    island_stroke_width: 0.0,
                    bg_quads: Vec::new(),
                    text_lines: Vec::new(),
                };
            }
        };

        let scale = vp.scale_factor;
        let pcw = vp.content_rect.width / vp.cols as f32;
        let pch = vp.content_rect.height / vp.rows as f32;

        // Island rect is the full viewport rect (panel area handles spacing)
        let island_rect = vp.rect;

        let (lines, cursor) = self.extract_cells(colors);

        // Build background quads
        let mut bg_quads = Vec::new();
        let content_x = vp.content_rect.x;
        let content_y = vp.content_rect.y;

        for line in &lines {
            for ci in line {
                if !ci.is_default_bg {
                    bg_quads.push(BgQuad {
                        x: content_x + ci.col as f32 * pcw,
                        y: content_y + ci.row as f32 * pch,
                        w: pcw,
                        h: pch,
                        color: ci.bg,
                    });
                }
            }
        }

        // Cursor quad
        if let Some(cur) = &cursor {
            bg_quads.push(BgQuad {
                x: content_x + cur.col as f32 * pcw,
                y: content_y + cur.row as f32 * pch,
                w: pcw,
                h: pch,
                color: [0.8, 0.8, 0.8, 0.7],
            });
        }

        // Update line buffers
        let metrics = font::metrics();
        while self.line_buffers.len() < lines.len() {
            self.line_buffers.push(Buffer::new(font_system, metrics));
        }
        self.line_buffers.truncate(lines.len());
        while self.line_hashes.len() < lines.len() {
            self.line_hashes.push(0);
        }
        self.line_hashes.truncate(lines.len());

        let cell_metrics = font::measure_cell(font_system);
        let line_width = vp.cols as f32 * cell_metrics.width + cell_metrics.width;

        for (row_idx, line) in lines.iter().enumerate() {
            let hash = hash_line(line);
            if self.line_hashes[row_idx] == hash {
                continue;
            }
            self.line_hashes[row_idx] = hash;

            let buf = &mut self.line_buffers[row_idx];
            buf.set_size(font_system, Some(line_width), Some(cell_metrics.height));

            self.scratch_text.clear();
            self.scratch_spans.clear();

            for ci in line {
                let start = self.scratch_text.len();
                self.scratch_text.push(if ci.c == ' ' || ci.c == '\0' {
                    ' '
                } else {
                    ci.c
                });
                let end = self.scratch_text.len();

                let mut attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
                attrs = attrs.color(ci.fg);
                if ci.bold {
                    attrs = attrs.weight(glyphon::Weight::BOLD);
                }
                if ci.italic {
                    attrs = attrs.style(glyphon::Style::Italic);
                }
                self.scratch_spans.push((start..end, attrs));
            }

            let rich: Vec<(&str, Attrs)> = self
                .scratch_spans
                .iter()
                .map(|(range, attrs)| (&self.scratch_text[range.clone()], *attrs))
                .collect();

            buf.set_rich_text(
                font_system,
                rich,
                Attrs::new().family(Family::Name("JetBrains Mono")),
                Shaping::Basic,
            );
            buf.shape_until_scroll(font_system, false);
        }

        // Build text line specs
        let text_lines: Vec<TextLineSpec> = (0..self.line_buffers.len())
            .map(|row_idx| TextLineSpec {
                left: content_x,
                top: content_y + row_idx as f32 * pch,
                bounds: island_rect,
            })
            .collect();

        PanelDrawCommands {
            island_rect,
            island_color: colors.bg_linear_f32(),
            island_radius: ISLAND_RADIUS * scale,
            island_stroke_color: colors.panel_stroke(),
            island_stroke_width: ISLAND_STROKE_WIDTH * scale,
            bg_quads,
            text_lines,
        }
    }

    fn line_buffers(&self) -> &[Buffer] {
        &self.line_buffers
    }

    fn drain_actions(&mut self) -> Vec<PanelAction> {
        std::mem::take(&mut self.pending_actions)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl TerminalPanel {
    pub fn set_title_from_event(&mut self, title: String) {
        self.title = title.clone();
        self.pending_actions.push(PanelAction::SetTitle(title));
    }

    pub fn mark_closed(&mut self) {
        self.pending_actions.push(PanelAction::Close);
    }
}

fn hash_line(line: &[CellInfo]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for ci in line {
        ci.c.hash(&mut hasher);
        ci.fg.0.hash(&mut hasher);
        ci.bg.iter().for_each(|b| b.to_bits().hash(&mut hasher));
        ci.bold.hash(&mut hasher);
        ci.italic.hash(&mut hasher);
    }
    hasher.finish()
}
