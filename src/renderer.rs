use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use glyphon::{
    Attrs, Buffer, Cache, Color as GlyphonColor, Family, FontSystem, Resolution, Shaping,
    SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::*;

use crate::colors::ColorScheme;
use crate::font::{self, CellMetrics};

/// Margin between window edge and island edge (logical pixels).
const ISLAND_MARGIN: f32 = 8.0;
/// Padding between island edge and cell grid (logical pixels).
const ISLAND_PADDING: f32 = 4.0;
/// Corner radius of the island (logical pixels).
const ISLAND_RADIUS: f32 = 10.0;

/// Background quad vertex.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    color: [f32; 4],
}

/// Uniform data for the rounded rectangle shader.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct RoundedRectUniforms {
    rect_bounds: [f32; 4], // min_x, min_y, max_x, max_y (physical pixels)
    params: [f32; 4],      // radius, 0, 0, 0
    color: [f32; 4],       // r, g, b, a (linear sRGB)
}

/// Holds all wgpu + glyphon state for rendering.
pub struct Renderer {
    pub device: Device,
    pub queue: Queue,
    pub surface: Surface<'static>,
    pub surface_config: SurfaceConfiguration,

    // glyphon text rendering
    pub font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    viewport: Viewport,
    _cache: Cache,

    // Cell grid (logical pixels)
    pub cell: CellMetrics,
    pub scale_factor: f32,
    pub cols: usize,
    pub rows: usize,

    // Line buffers (one Buffer per terminal line) + content hashes for dirty-checking
    line_buffers: Vec<Buffer>,
    line_hashes: Vec<u64>,

    // Background quad rendering
    quad_pipeline: RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,
    quad_vertex_count: u32,

    // Rounded rect (island background)
    rounded_rect_pipeline: RenderPipeline,
    rounded_rect_uniform_buffer: wgpu::Buffer,
    rounded_rect_bind_group: BindGroup,

    // Reusable scratch buffers to avoid per-frame allocation
    scratch_text: String,
    scratch_spans: Vec<(std::ops::Range<usize>, Attrs<'static>)>,

    // Color scheme
    pub colors: ColorScheme,
}

impl Renderer {
    pub fn new(window: Arc<winit::window::Window>, colors: ColorScheme) -> Self {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;

        // wgpu setup
        let instance = Instance::new(InstanceDescriptor {
            backends: Backends::PRIMARY,
            ..Default::default()
        });

        let surface = instance.create_surface(window).expect("create surface");

        let adapter = pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
            compatible_surface: Some(&surface),
            power_preference: PowerPreference::LowPower,
            ..Default::default()
        }))
        .expect("no adapter");

        let (device, queue) = pollster::block_on(adapter.request_device(
            &DeviceDescriptor {
                label: Some("terminal device"),
                ..Default::default()
            },
            None,
        ))
        .expect("request device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &surface_config);

        // glyphon setup
        let mut font_system = font::create_font_system();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        // Measure cell in logical pixels
        let cell = font::measure_cell(&mut font_system);
        let phys_cell_w = cell.width * scale_factor;
        let phys_cell_h = cell.height * scale_factor;
        let total_inset = (ISLAND_MARGIN + ISLAND_PADDING) * scale_factor;
        let cols = ((size.width as f32 - 2.0 * total_inset) / phys_cell_w)
            .floor()
            .max(1.0) as usize;
        let rows = ((size.height as f32 - 2.0 * total_inset) / phys_cell_h)
            .floor()
            .max(1.0) as usize;

        // Background quad pipeline
        let quad_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("quad shader"),
            source: ShaderSource::Wgsl(QUAD_SHADER.into()),
        });

        let quad_pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("quad pipeline layout"),
            bind_group_layouts: &[],
            push_constant_ranges: &[],
        });

        let quad_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("quad pipeline"),
            layout: Some(&quad_pipeline_layout),
            vertex: VertexState {
                module: &quad_shader,
                entry_point: Some("vs_main"),
                buffers: &[VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadVertex>() as BufferAddress,
                    step_mode: VertexStepMode::Vertex,
                    attributes: &vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &quad_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let quad_vertex_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("quad vertex buffer"),
            size: 1024 * 1024,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Rounded rect pipeline (island background)
        let rounded_rect_uniform_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("rounded rect uniforms"),
            size: std::mem::size_of::<RoundedRectUniforms>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let rounded_rect_bind_group_layout =
            device.create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: Some("rounded rect bind group layout"),
                entries: &[BindGroupLayoutEntry {
                    binding: 0,
                    visibility: ShaderStages::FRAGMENT,
                    ty: BindingType::Buffer {
                        ty: BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        let rounded_rect_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("rounded rect bind group"),
            layout: &rounded_rect_bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: rounded_rect_uniform_buffer.as_entire_binding(),
            }],
        });

        let rounded_rect_shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("rounded rect shader"),
            source: ShaderSource::Wgsl(ROUNDED_RECT_SHADER.into()),
        });

        let rounded_rect_pipeline_layout =
            device.create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some("rounded rect pipeline layout"),
                bind_group_layouts: &[&rounded_rect_bind_group_layout],
                push_constant_ranges: &[],
            });

        let rounded_rect_pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("rounded rect pipeline"),
            layout: Some(&rounded_rect_pipeline_layout),
            vertex: VertexState {
                module: &rounded_rect_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &rounded_rect_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(ColorTargetState {
                    format,
                    blend: Some(BlendState::ALPHA_BLENDING),
                    write_mask: ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: PrimitiveState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            viewport,
            _cache: cache,
            cell,
            scale_factor,
            cols,
            rows,
            line_buffers: Vec::new(),
            line_hashes: Vec::new(),
            quad_pipeline,
            quad_vertex_buffer,
            quad_vertex_count: 0,
            rounded_rect_pipeline,
            rounded_rect_uniform_buffer,
            rounded_rect_bind_group,
            scratch_text: String::with_capacity(256),
            scratch_spans: Vec::with_capacity(256),
            colors,
        }
    }

    /// Physical cell width (logical * scale_factor).
    fn phys_cell_w(&self) -> f32 {
        self.cell.width * self.scale_factor
    }

    /// Physical cell height (logical * scale_factor).
    fn phys_cell_h(&self) -> f32 {
        self.cell.height * self.scale_factor
    }

    /// Reconfigure surface and recalculate grid on resize.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        let total_inset = (ISLAND_MARGIN + ISLAND_PADDING) * self.scale_factor;
        self.cols = ((width as f32 - 2.0 * total_inset) / self.phys_cell_w())
            .floor()
            .max(1.0) as usize;
        self.rows = ((height as f32 - 2.0 * total_inset) / self.phys_cell_h())
            .floor()
            .max(1.0) as usize;
        // Invalidate line cache on resize
        self.line_hashes.clear();
    }

    /// Update scale factor (e.g. when moving between monitors).
    pub fn set_scale_factor(&mut self, scale_factor: f32) {
        self.scale_factor = scale_factor;
        let w = self.surface_config.width;
        let h = self.surface_config.height;
        let total_inset = (ISLAND_MARGIN + ISLAND_PADDING) * self.scale_factor;
        self.cols = ((w as f32 - 2.0 * total_inset) / self.phys_cell_w())
            .floor()
            .max(1.0) as usize;
        self.rows = ((h as f32 - 2.0 * total_inset) / self.phys_cell_h())
            .floor()
            .max(1.0) as usize;
        self.line_hashes.clear();
    }

    /// Physical pixel offset from window edge to first cell.
    fn content_offset(&self) -> (f32, f32) {
        let total = (ISLAND_MARGIN + ISLAND_PADDING) * self.scale_factor;
        (total, total)
    }

    /// Island rectangle bounds in physical pixels (min_x, min_y, max_x, max_y).
    fn island_rect(&self) -> (f32, f32, f32, f32) {
        let m = ISLAND_MARGIN * self.scale_factor;
        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;
        (m, m, w - m, h - m)
    }

    /// Prepare text content from terminal cells and render a frame.
    pub fn render(
        &mut self,
        lines: &[Vec<CellInfo>],
        cursor: Option<CursorInfo>,
    ) -> Result<(), SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor::default());

        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;
        let pcw = self.phys_cell_w();
        let pch = self.phys_cell_h();
        let (offset_x, offset_y) = self.content_offset();

        // -- Update island background uniforms --
        let (ix0, iy0, ix1, iy1) = self.island_rect();
        let island_uniforms = RoundedRectUniforms {
            rect_bounds: [ix0, iy0, ix1, iy1],
            params: [ISLAND_RADIUS * self.scale_factor, 0.0, 0.0, 0.0],
            color: self.colors.bg_linear_f32(),
        };
        self.queue.write_buffer(
            &self.rounded_rect_uniform_buffer,
            0,
            bytemuck::cast_slice(&[island_uniforms]),
        );

        // -- Background quads (offset by island margin+padding) --
        let mut quad_verts: Vec<QuadVertex> = Vec::new();

        for line in lines {
            for ci in line {
                if !ci.is_default_bg {
                    let x0 = offset_x + ci.col as f32 * pcw;
                    let y0 = offset_y + ci.row as f32 * pch;
                    let x1 = x0 + pcw;
                    let y1 = y0 + pch;

                    let nx0 = (x0 / w) * 2.0 - 1.0;
                    let ny0 = 1.0 - (y0 / h) * 2.0;
                    let nx1 = (x1 / w) * 2.0 - 1.0;
                    let ny1 = 1.0 - (y1 / h) * 2.0;

                    let c = ci.bg;
                    quad_verts.push(QuadVertex {
                        position: [nx0, ny0],
                        color: c,
                    });
                    quad_verts.push(QuadVertex {
                        position: [nx1, ny0],
                        color: c,
                    });
                    quad_verts.push(QuadVertex {
                        position: [nx0, ny1],
                        color: c,
                    });
                    quad_verts.push(QuadVertex {
                        position: [nx0, ny1],
                        color: c,
                    });
                    quad_verts.push(QuadVertex {
                        position: [nx1, ny0],
                        color: c,
                    });
                    quad_verts.push(QuadVertex {
                        position: [nx1, ny1],
                        color: c,
                    });
                }
            }
        }

        // Cursor quad (offset)
        if let Some(cur) = &cursor {
            let x0 = offset_x + cur.col as f32 * pcw;
            let y0 = offset_y + cur.row as f32 * pch;
            let x1 = x0 + pcw;
            let y1 = y0 + pch;
            let nx0 = (x0 / w) * 2.0 - 1.0;
            let ny0 = 1.0 - (y0 / h) * 2.0;
            let nx1 = (x1 / w) * 2.0 - 1.0;
            let ny1 = 1.0 - (y1 / h) * 2.0;
            let c = [0.8, 0.8, 0.8, 0.7];
            quad_verts.push(QuadVertex {
                position: [nx0, ny0],
                color: c,
            });
            quad_verts.push(QuadVertex {
                position: [nx1, ny0],
                color: c,
            });
            quad_verts.push(QuadVertex {
                position: [nx0, ny1],
                color: c,
            });
            quad_verts.push(QuadVertex {
                position: [nx0, ny1],
                color: c,
            });
            quad_verts.push(QuadVertex {
                position: [nx1, ny0],
                color: c,
            });
            quad_verts.push(QuadVertex {
                position: [nx1, ny1],
                color: c,
            });
        }

        self.quad_vertex_count = quad_verts.len() as u32;
        if !quad_verts.is_empty() {
            self.queue.write_buffer(
                &self.quad_vertex_buffer,
                0,
                bytemuck::cast_slice(&quad_verts),
            );
        }

        // -- Text via glyphon --
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        // Ensure we have enough line buffers + hashes (grown independently
        // since resize() clears hashes but not buffers)
        let metrics = font::metrics();
        while self.line_buffers.len() < lines.len() {
            self.line_buffers
                .push(Buffer::new(&mut self.font_system, metrics));
        }
        self.line_buffers.truncate(lines.len());
        while self.line_hashes.len() < lines.len() {
            self.line_hashes.push(0);
        }
        self.line_hashes.truncate(lines.len());

        // Only reshape lines whose content changed (hash-based dirty check)
        let line_width = self.cols as f32 * self.cell.width + self.cell.width;
        for (row_idx, line) in lines.iter().enumerate() {
            let hash = hash_line(line);
            if self.line_hashes[row_idx] == hash {
                continue;
            }
            self.line_hashes[row_idx] = hash;

            let buf = &mut self.line_buffers[row_idx];
            buf.set_size(
                &mut self.font_system,
                Some(line_width),
                Some(self.cell.height),
            );

            self.scratch_text.clear();
            self.scratch_spans.clear();

            for ci in line {
                let start = self.scratch_text.len();
                self.scratch_text.push(if ci.c == ' ' || ci.c == '\0' {
                    ' '
                } else {
                    ci.c
                });
                let end = self.scratch_text.len();

                let mut attrs = Attrs::new().family(Family::Name("JetBrains Mono"));
                attrs = attrs.color(ci.fg);
                if ci.bold {
                    attrs = attrs.weight(glyphon::Weight::BOLD);
                }
                if ci.italic {
                    attrs = attrs.style(glyphon::Style::Italic);
                }
                self.scratch_spans.push((start..end, attrs));
            }

            let rich: Vec<(&str, Attrs)> = self
                .scratch_spans
                .iter()
                .map(|(range, attrs)| (&self.scratch_text[range.clone()], *attrs))
                .collect();

            buf.set_rich_text(
                &mut self.font_system,
                rich,
                Attrs::new().family(Family::Name("JetBrains Mono")),
                Shaping::Basic,
            );
            buf.shape_until_scroll(&mut self.font_system, false);
        }

        // Build TextAreas — positions in physical pixels (offset by island margin+padding)
        let scale = self.scale_factor;
        let text_areas: Vec<TextArea> = self
            .line_buffers
            .iter()
            .enumerate()
            .map(|(row_idx, buf)| TextArea {
                buffer: buf,
                left: offset_x,
                top: offset_y + row_idx as f32 * pch,
                scale,
                bounds: TextBounds {
                    left: ix0 as i32,
                    top: iy0 as i32,
                    right: ix1 as i32,
                    bottom: iy1 as i32,
                },
                default_color: self.colors.fg_glyphon(),
                custom_glyphs: &[],
            })
            .collect();

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("prepare text");

        // -- Encode render pass --
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("main pass"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear({
                            let chrome = self.colors.chrome_wgpu();
                            wgpu::Color {
                                r: chrome[0],
                                g: chrome[1],
                                b: chrome[2],
                                a: chrome[3],
                            }
                        }),
                        store: StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            // 1. Draw island background (rounded rect)
            pass.set_pipeline(&self.rounded_rect_pipeline);
            pass.set_bind_group(0, &self.rounded_rect_bind_group, &[]);
            pass.draw(0..6, 0..1);

            // 2. Draw cell background quads + cursor
            if self.quad_vertex_count > 0 {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
                pass.draw(0..self.quad_vertex_count, 0..1);
            }

            // 3. Draw text
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render text");
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();

        self.atlas.trim();

        Ok(())
    }
}

/// Hash a line's visible content + colors for dirty checking.
fn hash_line(line: &[CellInfo]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for ci in line {
        ci.c.hash(&mut hasher);
        ci.fg.0.hash(&mut hasher);
        ci.bg.iter().for_each(|b| b.to_bits().hash(&mut hasher));
        ci.bold.hash(&mut hasher);
        ci.italic.hash(&mut hasher);
    }
    hasher.finish()
}

/// Info about a single cell, extracted from alacritty_terminal.
pub struct CellInfo {
    pub row: usize,
    pub col: usize,
    pub c: char,
    pub fg: GlyphonColor,
    pub bg: [f32; 4],
    pub is_default_bg: bool,
    pub bold: bool,
    pub italic: bool,
}

/// Cursor position info.
pub struct CursorInfo {
    pub row: usize,
    pub col: usize,
}

const QUAD_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

const ROUNDED_RECT_SHADER: &str = r#"
struct Uniforms {
    rect_bounds: vec4<f32>,
    params: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, 1.0),
        vec2(1.0, 1.0),
        vec2(-1.0, -1.0),
        vec2(-1.0, -1.0),
        vec2(1.0, 1.0),
        vec2(1.0, -1.0),
    );
    var out: VertexOutput;
    out.position = vec4(positions[vi], 0.0, 1.0);
    return out;
}

fn rounded_rect_sdf(p: vec2<f32>, half_size: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - half_size + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - r;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = in.position.xy;
    let rect_min = u.rect_bounds.xy;
    let rect_max = u.rect_bounds.zw;
    let center = (rect_min + rect_max) * 0.5;
    let half_size = (rect_max - rect_min) * 0.5;
    let radius = u.params.x;

    let d = rounded_rect_sdf(pixel - center, half_size, radius);
    let alpha = 1.0 - smoothstep(-0.5, 0.5, d);

    if alpha < 0.001 {
        discard;
    }

    return vec4(u.color.rgb, u.color.a * alpha);
}
"#;
