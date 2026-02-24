use std::collections::HashMap;

use crate::font::CellMetrics;
use crate::layout::{LayoutNode, Rect, SplitDirection};
use crate::panel::{Panel, PanelId, PanelViewport};
use crate::panels::terminal::TerminalPanel;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkspaceId(u64);

static NEXT_WORKSPACE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl WorkspaceId {
    pub fn next() -> Self {
        Self(NEXT_WORKSPACE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

pub struct Workspace {
    #[allow(dead_code)]
    pub id: WorkspaceId,
    pub layout: LayoutNode,
    pub panels: HashMap<PanelId, Box<dyn Panel>>,
    pub focused_panel: PanelId,
}

impl Workspace {
    pub fn new(initial_panel: Box<dyn Panel>) -> Self {
        let id = WorkspaceId::next();
        let panel_id = initial_panel.id();
        let mut panels: HashMap<PanelId, Box<dyn Panel>> = HashMap::new();
        panels.insert(panel_id, initial_panel);

        Self {
            id,
            layout: LayoutNode::Leaf { panel_id },
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

    #[allow(dead_code)]
    pub fn add_panel_split(
        &mut self,
        target: PanelId,
        new_panel: Box<dyn Panel>,
        direction: SplitDirection,
    ) {
        let new_id = new_panel.id();
        self.panels.insert(new_id, new_panel);
        self.layout.split_at(target, new_id, direction);
    }

    pub fn remove_panel(&mut self, panel_id: PanelId) {
        self.panels.remove(&panel_id);
        self.layout.remove(panel_id);

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
                let (_, content) =
                    TerminalPanel::compute_island_rects_static(&rect, scale_factor, tab_bar_height);
                let (cols, rows) =
                    TerminalPanel::compute_grid_size_static(&content, cell, scale_factor);

                let viewport = PanelViewport {
                    rect,
                    content_rect: content,
                    cols,
                    rows,
                    scale_factor,
                };
                panel.set_viewport(viewport, cell);
            }
        }
    }

    pub fn hit_test(&self, available: Rect, x: f32, y: f32) -> Option<PanelId> {
        let layouts = self.layout.compute_layout(available);
        LayoutNode::hit_test(&layouts, x, y)
    }
}
