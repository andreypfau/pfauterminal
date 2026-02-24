use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use glyphon::{Attrs, Buffer, Color as GlyphonColor, Family, FontSystem, Shaping};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};

use crate::colors::ColorScheme;
use crate::font::{self, CellMetrics};
use crate::layout::Rect;
use crate::terminal::{EventProxy, TermSize, Terminal};

// --- Panel types ---

static NEXT_PANEL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelId(u64);

impl PanelId {
    pub fn next() -> Self {
        Self(NEXT_PANEL_ID.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct PanelViewport {
    pub rect: Rect,
    pub content_rect: Rect,
    pub cols: usize,
    pub rows: usize,
    pub scale_factor: f32,
}

/// Per-cell rendering info extracted from a panel.
pub struct CellInfo {
    pub c: char,
    pub fg: GlyphonColor,
    pub bg: [f32; 4],
    pub is_default_bg: bool,
    pub bold: bool,
    pub italic: bool,
}

/// Draw commands returned by a panel for the GPU to render.
pub struct PanelDrawCommands {
    /// Island background rect (physical px).
    pub island_rect: Rect,
    /// Island background color (linear sRGB).
    pub island_color: [f32; 4],
    /// Island corner radius (physical px).
    pub island_radius: f32,
    /// Island stroke color (linear sRGB). If alpha > 0, renders an inside stroke.
    pub island_stroke_color: [f32; 4],
    /// Island stroke width (physical px).
    pub island_stroke_width: f32,
    /// Cell background quads: (physical_rect, linear_color).
    pub bg_quads: Vec<BgQuad>,
    /// Per-cell text specs for building TextAreas.
    pub text_cells: Vec<TextCellSpec>,
}

pub struct BgQuad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [f32; 4],
}

/// Describes a single character cell for glyphon rendering.
pub struct TextCellSpec {
    pub left: f32,
    pub top: f32,
    pub color: GlyphonColor,
    pub buffer_index: usize,
    pub bounds: Rect,
}

// --- Terminal panel ---

/// Padding between panel edge and cell grid (logical pixels).
const ISLAND_PADDING: f32 = 16.0;
/// Corner radius of the island (logical pixels).
const ISLAND_RADIUS: f32 = 10.0;
/// Island stroke width (logical pixels).
const ISLAND_STROKE_WIDTH: f32 = 0.5;

/// Key for the per-character buffer pool: (char, bold, italic).
/// Color is NOT part of the key — it's applied via TextArea::default_color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CharKey(char, bool, bool);

pub struct TerminalPanel {
    id: PanelId,
    terminal: Terminal,
    viewport: Option<PanelViewport>,
    /// Pool of single-character Buffers, indexed by CharKey.
    char_buffers: Vec<Buffer>,
    char_key_map: HashMap<CharKey, usize>,
    title: String,
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
            char_buffers: Vec::new(),
            char_key_map: HashMap::new(),
            title: String::from("Terminal"),
        }
    }

    fn extract_cells(&self, colors: &ColorScheme) -> (Vec<Vec<CellInfo>>, Option<(usize, usize)>) {
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
            .map(|_| {
                (0..cols)
                    .map(|_| CellInfo {
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
                use alacritty_terminal::term::cell::Flags;

                let cell = &*indexed;
                let flags = cell.flags;

                // Spacers and hidden cells should render as empty (background only).
                let is_invisible = flags.intersects(
                    Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER | Flags::HIDDEN,
                );

                let bold = flags.contains(Flags::BOLD);
                let italic = flags.contains(Flags::ITALIC);
                let dim = flags.contains(Flags::DIM);

                let (fg_color, bg_color) = if flags.contains(Flags::INVERSE) {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };

                let c = if is_invisible { ' ' } else { cell.c };

                let fg = if dim {
                    let base = colors.to_glyphon_fg(fg_color);
                    GlyphonColor::rgba(base.r() / 2, base.g() / 2, base.b() / 2, base.a())
                } else {
                    colors.to_glyphon_fg(fg_color)
                };

                lines[row][col] = CellInfo {
                    c,
                    fg,
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
                Some((row, col))
            } else {
                None
            }
        };

        (lines, cursor)
    }

    /// Compute a full PanelViewport for the given rect, cell metrics, scale, and tab bar inset.
    pub fn compute_viewport(
        rect: &Rect,
        cell: &CellMetrics,
        scale_factor: f32,
        tab_bar_height: f32,
    ) -> PanelViewport {
        let p = ISLAND_PADDING * scale_factor;
        let content = Rect {
            x: rect.x + p,
            y: rect.y + tab_bar_height + p,
            width: rect.width - 2.0 * p,
            height: rect.height - tab_bar_height - 2.0 * p,
        };
        let pcw = cell.width * scale_factor;
        let pch = cell.height * scale_factor;
        let cols = (content.width / pcw).floor().max(1.0) as usize;
        let rows = (content.height / pch).floor().max(1.0) as usize;
        PanelViewport {
            rect: *rect,
            content_rect: content,
            cols,
            rows,
            scale_factor,
        }
    }

    pub fn id(&self) -> PanelId {
        self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: String) {
        self.title = title;
    }

    pub fn set_viewport(&mut self, viewport: PanelViewport, cell: &CellMetrics) {
        let old_cols = self.viewport.as_ref().map(|v| v.cols);
        let old_rows = self.viewport.as_ref().map(|v| v.rows);

        let cell_w = (cell.width * viewport.scale_factor) as u16;
        let cell_h = (cell.height * viewport.scale_factor) as u16;

        if old_cols != Some(viewport.cols) || old_rows != Some(viewport.rows) {
            let size = TermSize::new(viewport.cols, viewport.rows);
            self.terminal.resize(size, cell_w, cell_h);
            // Clear buffer pool on resize since cell metrics change
            self.char_buffers.clear();
            self.char_key_map.clear();
        }

        self.viewport = Some(viewport);
    }

    pub fn handle_key(&mut self, event: &KeyEvent) -> bool {
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

    pub fn handle_scroll(&mut self, delta: MouseScrollDelta, cell_height: f64) -> bool {
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

    pub fn prepare_render(
        &mut self,
        font_system: &mut FontSystem,
        colors: &ColorScheme,
    ) -> PanelDrawCommands {
        let vp = match &self.viewport {
            Some(vp) => vp,
            None => {
                return PanelDrawCommands {
                    island_rect: Rect::ZERO,
                    island_color: colors.bg_linear_f32(),
                    island_radius: 0.0,
                    island_stroke_color: [0.0; 4],
                    island_stroke_width: 0.0,
                    bg_quads: Vec::new(),
                    text_cells: Vec::new(),
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

        for (row, line) in lines.iter().enumerate() {
            for (col, ci) in line.iter().enumerate() {
                if !ci.is_default_bg {
                    bg_quads.push(BgQuad {
                        x: content_x + col as f32 * pcw,
                        y: content_y + row as f32 * pch,
                        w: pcw,
                        h: pch,
                        color: ci.bg,
                    });
                }
            }
        }

        // Cursor quad
        if let Some((cur_row, cur_col)) = cursor {
            bg_quads.push(BgQuad {
                x: content_x + cur_col as f32 * pcw,
                y: content_y + cur_row as f32 * pch,
                w: pcw,
                h: pch,
                color: [0.8, 0.8, 0.8, 0.7],
            });
        }

        // Per-cell text rendering: each visible character gets its own Buffer
        // positioned at its exact grid position. This bypasses cosmic_text's
        // paragraph layout engine, guaranteeing correct terminal grid positioning.
        //
        // Block-drawing characters (U+2580–U+259F) are rendered as colored quads
        // instead of text glyphs to guarantee seamless tiling with no gaps.
        let metrics = font::metrics();
        let cell_metrics = font::measure_cell(font_system);
        let mut text_cells = Vec::new();

        for (row, line) in lines.iter().enumerate() {
            for (col, ci) in line.iter().enumerate() {
                if ci.c == ' ' || ci.c == '\0' {
                    continue;
                }

                // Block-drawing characters → colored quads
                let cx = content_x + col as f32 * pcw;
                let cy = content_y + row as f32 * pch;
                if let Some(rects) = block_char_rects(ci.c, cx, cy, pcw, pch) {
                    let color = glyphon_to_linear(ci.fg);
                    for (rx, ry, rw, rh) in rects {
                        bg_quads.push(BgQuad {
                            x: rx,
                            y: ry,
                            w: rw,
                            h: rh,
                            color,
                        });
                    }
                    continue;
                }

                let key = CharKey(ci.c, ci.bold, ci.italic);
                let buf_idx = if let Some(&idx) = self.char_key_map.get(&key) {
                    idx
                } else {
                    // Create a new single-character buffer
                    let mut buf = Buffer::new(font_system, metrics);
                    // Buffer is slightly wider than one cell to avoid clipping
                    buf.set_size(
                        font_system,
                        Some(cell_metrics.width * 2.0),
                        Some(cell_metrics.height),
                    );
                    let mut attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
                    if ci.bold {
                        attrs = attrs.weight(glyphon::Weight::BOLD);
                    }
                    if ci.italic {
                        attrs = attrs.style(glyphon::Style::Italic);
                    }
                    let text = ci.c.to_string();
                    buf.set_text(font_system, &text, attrs, Shaping::Advanced);
                    buf.shape_until_scroll(font_system, false);

                    let idx = self.char_buffers.len();
                    self.char_buffers.push(buf);
                    self.char_key_map.insert(key, idx);
                    idx
                };

                text_cells.push(TextCellSpec {
                    left: cx,
                    top: cy,
                    color: ci.fg,
                    buffer_index: buf_idx,
                    bounds: island_rect,
                });
            }
        }

        PanelDrawCommands {
            island_rect,
            island_color: colors.bg_linear_f32(),
            island_radius: ISLAND_RADIUS * scale,
            island_stroke_color: colors.panel_stroke(),
            island_stroke_width: ISLAND_STROKE_WIDTH * scale,
            bg_quads,
            text_cells,
        }
    }

    pub fn buffers(&self) -> &[Buffer] {
        &self.char_buffers
    }

    pub fn write_to_pty(&self, data: Vec<u8>) {
        self.terminal.write(Cow::Owned(data));
    }
}

/// Convert a GlyphonColor (sRGB u8) to linear f32 RGBA for GPU quads.
fn glyphon_to_linear(c: GlyphonColor) -> [f32; 4] {
    use crate::colors::srgb_to_linear;
    [
        srgb_to_linear(c.r() as f32 / 255.0),
        srgb_to_linear(c.g() as f32 / 255.0),
        srgb_to_linear(c.b() as f32 / 255.0),
        c.a() as f32 / 255.0,
    ]
}

/// Return sub-rectangles for block-drawing characters (U+2580–U+259F).
/// Each rect is (x, y, w, h) in physical pixels. Returns None for non-block chars.
fn block_char_rects(
    c: char,
    cx: f32,
    cy: f32,
    cw: f32,
    ch: f32,
) -> Option<Vec<(f32, f32, f32, f32)>> {
    let hw = cw / 2.0;
    let hh = ch / 2.0;

    match c {
        // Vertical fractional blocks (lower N/8)
        '\u{2581}' => Some(vec![(cx, cy + ch * 7.0 / 8.0, cw, ch / 8.0)]),
        '\u{2582}' => Some(vec![(cx, cy + ch * 6.0 / 8.0, cw, ch * 2.0 / 8.0)]),
        '\u{2583}' => Some(vec![(cx, cy + ch * 5.0 / 8.0, cw, ch * 3.0 / 8.0)]),
        '\u{2584}' => Some(vec![(cx, cy + hh, cw, hh)]), // ▄ lower half
        '\u{2585}' => Some(vec![(cx, cy + ch * 3.0 / 8.0, cw, ch * 5.0 / 8.0)]),
        '\u{2586}' => Some(vec![(cx, cy + ch * 2.0 / 8.0, cw, ch * 6.0 / 8.0)]),
        '\u{2587}' => Some(vec![(cx, cy + ch / 8.0, cw, ch * 7.0 / 8.0)]),
        '\u{2588}' => Some(vec![(cx, cy, cw, ch)]), // █ full block
        // Horizontal fractional blocks (left N/8)
        '\u{2589}' => Some(vec![(cx, cy, cw * 7.0 / 8.0, ch)]),
        '\u{258A}' => Some(vec![(cx, cy, cw * 6.0 / 8.0, ch)]),
        '\u{258B}' => Some(vec![(cx, cy, cw * 5.0 / 8.0, ch)]),
        '\u{258C}' => Some(vec![(cx, cy, hw, ch)]), // ▌ left half
        '\u{258D}' => Some(vec![(cx, cy, cw * 3.0 / 8.0, ch)]),
        '\u{258E}' => Some(vec![(cx, cy, cw * 2.0 / 8.0, ch)]),
        '\u{258F}' => Some(vec![(cx, cy, cw / 8.0, ch)]),
        // Other halves
        '\u{2580}' => Some(vec![(cx, cy, cw, hh)]), // ▀ upper half
        '\u{2590}' => Some(vec![(cx + hw, cy, hw, ch)]), // ▐ right half
        '\u{2594}' => Some(vec![(cx, cy, cw, ch / 8.0)]), // ▔ upper 1/8
        '\u{2595}' => Some(vec![(cx + cw * 7.0 / 8.0, cy, cw / 8.0, ch)]), // ▕ right 1/8
        // Quadrant elements
        '\u{2596}' => Some(vec![(cx, cy + hh, hw, hh)]), // ▖ lower-left
        '\u{2597}' => Some(vec![(cx + hw, cy + hh, hw, hh)]), // ▗ lower-right
        '\u{2598}' => Some(vec![(cx, cy, hw, hh)]),      // ▘ upper-left
        '\u{2599}' => Some(vec![
            // ▙
            (cx, cy, hw, hh),
            (cx, cy + hh, cw, hh),
        ]),
        '\u{259A}' => Some(vec![
            // ▚
            (cx, cy, hw, hh),
            (cx + hw, cy + hh, hw, hh),
        ]),
        '\u{259B}' => Some(vec![
            // ▛
            (cx, cy, cw, hh),
            (cx, cy + hh, hw, hh),
        ]),
        '\u{259C}' => Some(vec![
            // ▜
            (cx, cy, cw, hh),
            (cx + hw, cy + hh, hw, hh),
        ]),
        '\u{259D}' => Some(vec![(cx + hw, cy, hw, hh)]), // ▝ upper-right
        '\u{259E}' => Some(vec![
            // ▞
            (cx + hw, cy, hw, hh),
            (cx, cy + hh, hw, hh),
        ]),
        '\u{259F}' => Some(vec![
            // ▟
            (cx + hw, cy, hw, hh),
            (cx, cy + hh, cw, hh),
        ]),
        _ => None,
    }
}
