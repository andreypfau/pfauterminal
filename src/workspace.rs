use crate::font::CellMetrics;
use crate::layout::Rect;
use crate::terminal_panel::TerminalPanel;

pub struct Workspace {
    pub panel: TerminalPanel,
}

impl Workspace {
    pub fn new(panel: TerminalPanel) -> Self {
        Self { panel }
    }

    pub fn title(&self) -> &str {
        self.panel.title()
    }

    pub fn compute_viewports(
        &mut self,
        available: Rect,
        cell: &CellMetrics,
        scale_factor: f32,
        tab_bar_height: f32,
    ) {
        let viewport =
            TerminalPanel::compute_viewport(&available, cell, scale_factor, tab_bar_height);
        self.panel.set_viewport(viewport, cell);
    }
}
