use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

pub const FONT_DATA: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
pub const FONT_FAMILY: &str = "JetBrains Mono";
pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 1.2;

#[derive(Clone, Copy)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
}

pub fn create_font_system() -> FontSystem {
    let mut font_system = FontSystem::new();
    let db = font_system.db_mut();
    db.load_font_data(FONT_DATA.to_vec());
    db.set_monospace_family(FONT_FAMILY);

    // Remove emoji fonts so the fallback chain never picks colored emoji.
    // Terminal text should be rendered as monochrome glyphs only.
    let emoji_ids: Vec<_> = db
        .faces()
        .filter(|f| f.post_script_name.contains("Emoji"))
        .map(|f| f.id)
        .collect();
    for id in emoji_ids {
        db.remove_face(id);
    }

    font_system
}

pub fn metrics() -> Metrics {
    Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT)
}

/// Default text attributes using the monospace family.
///
/// Uses `Family::Monospace` to leverage cosmic-text's monospace fallback,
/// which tries all monospace system fonts (e.g. Menlo on macOS, Consolas on
/// Windows) when a glyph is missing from the primary font.
pub fn default_attrs() -> Attrs<'static> {
    Attrs::new().family(Family::Monospace)
}

/// Set metrics, size, text, and shape a buffer in one call.
///
/// This consolidates the common pattern of:
///   buf.set_metrics → buf.set_size → buf.set_text → buf.shape_until_scroll
pub fn set_buffer_text(
    buf: &mut Buffer,
    font_system: &mut FontSystem,
    text: &str,
    metrics: Metrics,
    attrs: Attrs,
    width: f32,
) {
    buf.set_metrics(font_system, metrics);
    buf.set_size(font_system, Some(width), Some(metrics.line_height));
    buf.set_text(font_system, text, attrs, Shaping::Basic);
    buf.shape_until_scroll(font_system, false);
}

/// Measure cell dimensions by rendering a reference character.
pub fn measure_cell(font_system: &mut FontSystem) -> CellMetrics {
    let mut buffer = Buffer::new(font_system, metrics());
    buffer.set_size(font_system, Some(200.0), Some(100.0));
    buffer.set_text(font_system, "M", default_attrs(), Shaping::Advanced);
    buffer.shape_until_scroll(font_system, false);

    let width = buffer
        .layout_runs()
        .next()
        .and_then(|run| run.glyphs.first())
        .map(|g| g.w)
        .unwrap_or(FONT_SIZE * 0.6);

    CellMetrics {
        width,
        height: FONT_SIZE * LINE_HEIGHT,
    }
}
