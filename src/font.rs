use glyphon::{FontSystem, Metrics};

pub const FONT_DATA: &[u8] = include_bytes!("../fonts/JetBrainsMono-Regular.ttf");
pub const FONT_SIZE: f32 = 14.0;
pub const LINE_HEIGHT: f32 = 1.2;

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

/// Measure cell dimensions by rendering a reference character.
pub fn measure_cell(font_system: &mut FontSystem) -> CellMetrics {
    use glyphon::{Attrs, Buffer, Family, Shaping};

    let mut buffer = Buffer::new(font_system, metrics());
    buffer.set_size(font_system, Some(200.0), Some(100.0));
    buffer.set_text(
        font_system,
        "M",
        Attrs::new().family(Family::Name("JetBrains Mono")),
        Shaping::Advanced,
    );
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
