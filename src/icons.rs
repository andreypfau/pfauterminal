use glyphon::{CustomGlyphId, RasterizeCustomGlyphRequest, RasterizedCustomGlyph};

pub const ICON_TERMINAL: CustomGlyphId = 1;
pub const ICON_ADD: CustomGlyphId = 2;
pub const ICON_CLOSE: CustomGlyphId = 3;
pub const ICON_CLOSE_HOVERED: CustomGlyphId = 4;

const SVG_TERMINAL: &str = include_str!("../icons/terminal_dark.svg");
const SVG_ADD: &str = include_str!("../icons/add_dark.svg");
const SVG_CLOSE: &str = include_str!("../icons/closeSmall_dark.svg");
const SVG_CLOSE_HOVERED: &str = include_str!("../icons/closeSmallHovered_dark.svg");

pub struct IconManager {
    tree_terminal: resvg::usvg::Tree,
    tree_add: resvg::usvg::Tree,
    tree_close: resvg::usvg::Tree,
    tree_close_hovered: resvg::usvg::Tree,
}

impl IconManager {
    pub fn new() -> Self {
        let opts = resvg::usvg::Options::default();
        Self {
            tree_terminal: resvg::usvg::Tree::from_str(SVG_TERMINAL, &opts)
                .expect("parse terminal svg"),
            tree_add: resvg::usvg::Tree::from_str(SVG_ADD, &opts).expect("parse add svg"),
            tree_close: resvg::usvg::Tree::from_str(SVG_CLOSE, &opts).expect("parse close svg"),
            tree_close_hovered: resvg::usvg::Tree::from_str(SVG_CLOSE_HOVERED, &opts)
                .expect("parse close hovered svg"),
        }
    }

    pub fn rasterize(&self, req: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
        let tree = match req.id {
            ICON_TERMINAL => &self.tree_terminal,
            ICON_ADD => &self.tree_add,
            ICON_CLOSE => &self.tree_close,
            ICON_CLOSE_HOVERED => &self.tree_close_hovered,
            _ => return None,
        };

        let w = req.width as u32;
        let h = req.height as u32;
        if w == 0 || h == 0 {
            return None;
        }

        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;

        let svg_size = tree.size();
        let sx = w as f32 / svg_size.width();
        let sy = h as f32 / svg_size.height();

        resvg::render(
            tree,
            resvg::tiny_skia::Transform::from_scale(sx, sy),
            &mut pixmap.as_mut(),
        );

        Some(RasterizedCustomGlyph {
            data: pixmap.data().to_vec(),
            content_type: glyphon::ContentType::Color,
        })
    }
}
