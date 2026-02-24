use crate::panel::PanelId;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    pub fn contains_padded(&self, x: f32, y: f32, pad: f32) -> bool {
        x >= self.x - pad
            && x < self.x + self.width + pad
            && y >= self.y - pad
            && y < self.y + self.height + pad
    }

    pub fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - 2.0 * amount).max(0.0),
            height: (self.height - 2.0 * amount).max(0.0),
        }
    }
}

#[derive(Debug)]
pub struct LayoutNode {
    pub panel_id: PanelId,
}

impl LayoutNode {
    pub fn new(panel_id: PanelId) -> Self {
        Self { panel_id }
    }

    pub fn compute_layout(&self, available: Rect) -> Vec<(PanelId, Rect)> {
        vec![(self.panel_id, available)]
    }

    /// Find which panel contains the given point, given computed layout rects.
    pub fn hit_test(layouts: &[(PanelId, Rect)], x: f32, y: f32) -> Option<PanelId> {
        layouts
            .iter()
            .find(|(_, rect)| rect.contains(x, y))
            .map(|(id, _)| *id)
    }
}
