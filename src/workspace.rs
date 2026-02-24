use std::collections::HashMap;

use crate::font::CellMetrics;
use crate::layout::{LayoutNode, Rect};
use crate::panel::PanelId;
use crate::panels::terminal::TerminalPanel;

pub struct Workspace {
    pub layout: LayoutNode,
    pub panels: HashMap<PanelId, TerminalPanel>,
    pub focused_panel: PanelId,
}

impl Workspace {
    pub fn new(initial_panel: TerminalPanel) -> Self {
        let panel_id = initial_panel.id();
        let mut panels = HashMap::new();
        panels.insert(panel_id, initial_panel);

        Self {
            layout: LayoutNode::new(panel_id),
            panels,
            focused_panel: panel_id,
        }
    }

    pub fn title(&self) -> &str {
        self.panels
            .get(&self.focused_panel)
            .map(|p| p.title())
            .unwrap_or("Terminal")
    }

    pub fn remove_panel(&mut self, panel_id: PanelId) {
        self.panels.remove(&panel_id);

        // If the focused panel was removed, focus another one
        if self.focused_panel == panel_id {
            if let Some(id) = self.panels.keys().next() {
                self.focused_panel = *id;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    pub fn compute_viewports(
        &mut self,
        available: Rect,
        cell: &CellMetrics,
        scale_factor: f32,
        tab_bar_height: f32,
    ) {
        let layouts = self.layout.compute_layout(available);
        for (panel_id, rect) in layouts {
            if let Some(panel) = self.panels.get_mut(&panel_id) {
                let viewport =
                    TerminalPanel::compute_viewport(&rect, cell, scale_factor, tab_bar_height);
                panel.set_viewport(viewport, cell);
            }
        }
    }

    pub fn hit_test(&self, available: Rect, x: f32, y: f32) -> Option<PanelId> {
        let layouts = self.layout.compute_layout(available);
        LayoutNode::hit_test(&layouts, x, y)
    }
}
