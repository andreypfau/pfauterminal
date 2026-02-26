use glyphon::{CustomGlyphId, RasterizeCustomGlyphRequest, RasterizedCustomGlyph};

pub const ICON_TERMINAL: CustomGlyphId = 1;
pub const ICON_ADD: CustomGlyphId = 2;
pub const ICON_CLOSE: CustomGlyphId = 3;
pub const ICON_CLOSE_HOVERED: CustomGlyphId = 4;

const ICONS: [(CustomGlyphId, &str); 4] = [
    (ICON_TERMINAL, include_str!("../icons/terminal_dark.svg")),
    (ICON_ADD, include_str!("../icons/add_dark.svg")),
    (ICON_CLOSE, include_str!("../icons/closeSmall_dark.svg")),
    (
        ICON_CLOSE_HOVERED,
        include_str!("../icons/closeSmallHovered_dark.svg"),
    ),
];

pub struct IconManager {
    trees: Vec<(CustomGlyphId, resvg::usvg::Tree)>,
}

impl IconManager {
    pub fn new() -> Self {
        let opts = resvg::usvg::Options::default();
        Self {
            trees: ICONS
                .iter()
                .map(|&(id, svg)| {
                    (
                        id,
                        resvg::usvg::Tree::from_str(svg, &opts).expect("parse svg"),
                    )
                })
                .collect(),
        }
    }

    pub fn rasterize(&self, req: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
        let tree = &self.trees.iter().find(|(id, _)| *id == req.id)?.1;

        let w = req.width as u32;
        let h = req.height as u32;
        if w == 0 || h == 0 {
            return None;
        }

        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
        let svg_size = tree.size();
        resvg::render(
            tree,
            resvg::tiny_skia::Transform::from_scale(
                w as f32 / svg_size.width(),
                h as f32 / svg_size.height(),
            ),
            &mut pixmap.as_mut(),
        );

        Some(RasterizedCustomGlyph {
            data: pixmap.data().to_vec(),
            content_type: glyphon::ContentType::Color,
        })
    }
}
