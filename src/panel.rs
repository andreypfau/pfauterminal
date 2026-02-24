use std::sync::atomic::{AtomicU64, Ordering};

use glyphon::{Buffer, Color as GlyphonColor, FontSystem};
use winit::event::{KeyEvent, MouseScrollDelta};

use crate::colors::ColorScheme;
use crate::font::CellMetrics;
use crate::layout::Rect;

static NEXT_PANEL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PanelId(u64);

impl PanelId {
    pub const ZERO: PanelId = PanelId(0);

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
    pub row: usize,
    pub col: usize,
    pub c: char,
    pub fg: GlyphonColor,
    pub bg: [f32; 4],
    pub is_default_bg: bool,
    pub bold: bool,
    pub italic: bool,
}

/// Cursor position info.
pub struct CursorInfo {
    pub row: usize,
    pub col: usize,
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

pub enum PanelAction {
    SetTitle(String),
    Close,
    #[allow(dead_code)]
    Redraw,
}

pub trait Panel {
    fn id(&self) -> PanelId;
    fn title(&self) -> &str;
    fn set_viewport(&mut self, viewport: PanelViewport, cell: &CellMetrics);
    fn handle_key(&mut self, event: &KeyEvent) -> bool;
    fn handle_scroll(&mut self, delta: MouseScrollDelta, cell_height: f64) -> bool;
    fn prepare_render(
        &mut self,
        font_system: &mut FontSystem,
        colors: &ColorScheme,
    ) -> PanelDrawCommands;
    fn buffers(&self) -> &[Buffer];
    fn drain_actions(&mut self) -> Vec<PanelAction>;
    fn set_title_from_event(&mut self, title: String);
    fn mark_closed(&mut self);
    fn write_to_pty(&self, data: Vec<u8>);
}
