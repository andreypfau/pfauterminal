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
    font_system.db_mut().load_font_data(FONT_DATA.to_vec());
    font_system
}

pub fn metrics() -> Metrics {
    Metrics::new(FONT_SIZE, FONT_SIZE * LINE_HEIGHT)
}

/// Default text attributes using the configured font family.
pub fn default_attrs() -> Attrs<'static> {
    Attrs::new().family(Family::Name(FONT_FAMILY))
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
