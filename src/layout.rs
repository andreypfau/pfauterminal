use crate::panel::PanelId;

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum SplitDirection {
    Horizontal, // left | right
}

#[derive(Debug)]
pub enum LayoutNode {
    Leaf {
        panel_id: PanelId,
    },
    #[allow(dead_code)]
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    pub fn compute_layout(&self, available: Rect) -> Vec<(PanelId, Rect)> {
        let mut result = Vec::new();
        self.collect_layout(available, &mut result);
        result
    }

    fn collect_layout(&self, rect: Rect, out: &mut Vec<(PanelId, Rect)>) {
        match self {
            LayoutNode::Leaf { panel_id } => {
                out.push((*panel_id, rect));
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match direction {
                SplitDirection::Horizontal => {
                    let first_width = rect.width * ratio;
                    let first_rect = Rect {
                        x: rect.x,
                        y: rect.y,
                        width: first_width,
                        height: rect.height,
                    };
                    let second_rect = Rect {
                        x: rect.x + first_width,
                        y: rect.y,
                        width: rect.width - first_width,
                        height: rect.height,
                    };
                    first.collect_layout(first_rect, out);
                    second.collect_layout(second_rect, out);
                }
            },
        }
    }

    /// Split the leaf containing `target` into two, placing `new_id` in the given direction.
    /// Returns true if the split was performed.
    #[allow(dead_code)]
    pub fn split_at(
        &mut self,
        target: PanelId,
        new_id: PanelId,
        direction: SplitDirection,
    ) -> bool {
        match self {
            LayoutNode::Leaf { panel_id } if *panel_id == target => {
                let old = Box::new(LayoutNode::Leaf { panel_id: target });
                let new = Box::new(LayoutNode::Leaf { panel_id: new_id });
                *self = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first: old,
                    second: new,
                };
                true
            }
            LayoutNode::Leaf { .. } => false,
            LayoutNode::Split { first, second, .. } => {
                first.split_at(target, new_id, direction)
                    || second.split_at(target, new_id, direction)
            }
        }
    }

    /// Remove a panel from the layout. Returns the simplified node if the panel was found,
    /// or None if this entire subtree should be removed.
    pub fn remove(&mut self, target: PanelId) -> bool {
        match self {
            LayoutNode::Leaf { panel_id } => *panel_id == target,
            LayoutNode::Split { first, second, .. } => {
                if first.remove(target) {
                    // Replace self with second
                    let taken = std::mem::replace(
                        second.as_mut(),
                        LayoutNode::Leaf {
                            panel_id: PanelId::ZERO,
                        },
                    );
                    *self = taken;
                    false
                } else if second.remove(target) {
                    // Replace self with first
                    let taken = std::mem::replace(
                        first.as_mut(),
                        LayoutNode::Leaf {
                            panel_id: PanelId::ZERO,
                        },
                    );
                    *self = taken;
                    false
                } else {
                    false
                }
            }
        }
    }

    /// Find which panel contains the given point, given computed layout rects.
    pub fn hit_test(layouts: &[(PanelId, Rect)], x: f32, y: f32) -> Option<PanelId> {
        for (panel_id, rect) in layouts {
            if x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height {
                return Some(*panel_id);
            }
        }
        None
    }
}
