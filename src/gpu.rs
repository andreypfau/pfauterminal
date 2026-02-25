use std::sync::Arc;

use glyphon::{
    Buffer, Cache, FontSystem, Metrics, Resolution, SwashCache, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport,
};
use wgpu::*;

use crate::colors::ColorScheme;
use crate::dropdown::DropdownMenu;
use crate::font::{self, CellMetrics};
use crate::icons::IconManager;
use crate::layout::Rect;
use crate::tab_bar::TabBar;
use crate::terminal_panel::{BgQuad, PanelDrawCommands};

/// Max rounded rects: panel islands + tab bar elements (borders, backgrounds)
const MAX_ROUNDED_RECTS: usize = 64;

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
pub struct RoundedRectUniforms {
    rect_bounds: [f32; 4],
    params: [f32; 4],
    color: [f32; 4],
}

impl RoundedRectUniforms {
    pub fn new(rect: &Rect, radius: f32, shadow_softness: f32, color: [f32; 4]) -> Self {
        Self {
            rect_bounds: [rect.x, rect.y, rect.x + rect.width, rect.y + rect.height],
            params: [radius, shadow_softness, 0.0, 0.0],
            color,
        }
    }
}

pub struct GpuContext {
    pub device: Device,
    pub queue: Queue,
    pub surface: Surface<'static>,
    pub surface_config: SurfaceConfiguration,
    /// The format used for render target views (always sRGB for correct colors).
    render_format: TextureFormat,

    pub font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_text_renderer: TextRenderer,
    viewport: Viewport,
    _cache: Cache,

    pub cell: CellMetrics,
    pub scale_factor: f32,

    // Background quad rendering
    quad_pipeline: RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,

    // Rounded rect (island backgrounds) — dynamic offset uniform buffer
    rounded_rect_pipeline: RenderPipeline,
    rounded_rect_uniform_buffer: wgpu::Buffer,
    rounded_rect_bind_group: BindGroup,
    uniform_aligned_size: usize,

    pub colors: ColorScheme,
}

impl GpuContext {
    pub fn new(window: Arc<winit::window::Window>, colors: ColorScheme) -> Self {
        let size = window.inner_size();
        let scale_factor = window.scale_factor() as f32;

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
        let surface_format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        // Ensure we always render to an sRGB view for correct linear→sRGB conversion.
        // On some Windows configurations the surface format is Bgra8Unorm (not sRGB),
        // which causes shader outputs to skip the linear→sRGB conversion, making
        // everything too dark.
        let render_format = if surface_format.is_srgb() {
            surface_format
        } else {
            match surface_format {
                TextureFormat::Bgra8Unorm => TextureFormat::Bgra8UnormSrgb,
                TextureFormat::Rgba8Unorm => TextureFormat::Rgba8UnormSrgb,
                other => other,
            }
        };

        let view_formats = if render_format != surface_format {
            vec![render_format]
        } else {
            vec![]
        };

        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats,
        };
        surface.configure(&device, &surface_config);

        // glyphon setup — use render_format (sRGB) for atlas and pipelines
        let mut font_system = font::create_font_system();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, render_format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let overlay_text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let cell = font::measure_cell(&mut font_system);

        // Quad pipeline
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
                    format: render_format,
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
            size: 2 * 1024 * 1024,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Rounded rect pipeline with dynamic uniform buffer
        let uniform_alignment = device.limits().min_uniform_buffer_offset_alignment;
        let uniform_size = std::mem::size_of::<RoundedRectUniforms>() as u32;
        let aligned_size = align_up(uniform_size, uniform_alignment);

        let rounded_rect_uniform_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("rounded rect uniforms"),
            size: (aligned_size as usize * MAX_ROUNDED_RECTS) as u64,
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
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                            RoundedRectUniforms,
                        >() as u64),
                    },
                    count: None,
                }],
            });

        let rounded_rect_bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("rounded rect bind group"),
            layout: &rounded_rect_bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: BindingResource::Buffer(BufferBinding {
                    buffer: &rounded_rect_uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<RoundedRectUniforms>() as u64),
                }),
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
                    format: render_format,
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
            render_format,
            font_system,
            swash_cache,
            atlas,
            text_renderer,
            overlay_text_renderer,
            viewport,
            _cache: cache,
            cell,
            scale_factor,
            quad_pipeline,
            quad_vertex_buffer,
            rounded_rect_pipeline,
            rounded_rect_uniform_buffer,
            rounded_rect_bind_group,
            uniform_aligned_size: aligned_size as usize,
            colors,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Render a complete frame with tab bar + multiple panel draw commands.
    /// If `screenshot_path` is Some, saves the rendered frame as a PNG.
    pub fn render_frame(
        &mut self,
        tab_bar: &TabBar,
        dropdown: Option<&DropdownMenu>,
        panel_draws: &[PanelDrawCommands],
        panel_buffers: &[&[Buffer]],
        scale_factor: f32,
        icon_manager: &IconManager,
        screenshot_path: Option<&str>,
    ) -> Result<(), SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame.texture.create_view(&TextureViewDescriptor {
            format: Some(self.render_format),
            ..Default::default()
        });

        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;

        let aligned_size = self.uniform_aligned_size;

        // Get tab bar draw commands — use panel bounds for alignment
        let panel_x = panel_draws.first().map(|d| d.island_rect.x).unwrap_or(0.0);
        let panel_y = panel_draws.first().map(|d| d.island_rect.y).unwrap_or(0.0);
        let panel_w = panel_draws
            .first()
            .map(|d| d.island_rect.width)
            .unwrap_or(w);
        let tab_draw = tab_bar.draw_commands(&self.colors, scale_factor, panel_x, panel_y, panel_w);

        // Upload rounded rect uniforms in two groups:
        // Scene (panels + tab bar) — drawn before text
        // Overlay (dropdown) — drawn after text, on top of everything
        let mut rr_index = 0;

        let upload_rr = |rr_index: &mut usize, uniforms: RoundedRectUniforms| {
            debug_assert!(*rr_index < MAX_ROUNDED_RECTS, "exceeded max rounded rects");
            self.queue.write_buffer(
                &self.rounded_rect_uniform_buffer,
                (*rr_index * aligned_size) as u64,
                bytemuck::cast_slice(&[uniforms]),
            );
            *rr_index += 1;
        };

        // Panel island backgrounds (stroke + fill per panel) — drawn first (behind)
        for draw in panel_draws {
            if draw.island_stroke_width > 0.0 && draw.island_stroke_color[3] > 0.0 {
                upload_rr(
                    &mut rr_index,
                    RoundedRectUniforms::new(
                        &draw.island_rect,
                        draw.island_radius,
                        0.0,
                        draw.island_stroke_color,
                    ),
                );
            }
            let sw = draw.island_stroke_width;
            let inset = draw.island_rect.inset(sw);
            upload_rr(
                &mut rr_index,
                RoundedRectUniforms::new(
                    &inset,
                    (draw.island_radius - sw).max(0.0),
                    0.0,
                    draw.island_color,
                ),
            );
        }

        // Tab bar rounded rects (tab backgrounds, borders)
        for rq in &tab_draw.rounded_quads {
            upload_rr(
                &mut rr_index,
                RoundedRectUniforms::new(&rq.rect, rq.radius, 0.0, rq.color),
            );
        }

        let scene_rr_count = rr_index;

        // Dropdown menu rounded rects (shadow + border + fill + hover) — overlay layer
        let dropdown_draw = dropdown.map(|d| d.draw_commands(&self.colors, scale_factor));
        if let Some(ref dd) = dropdown_draw {
            for rq in &dd.rounded_quads {
                upload_rr(
                    &mut rr_index,
                    RoundedRectUniforms::new(&rq.rect, rq.radius, rq.shadow_softness, rq.color),
                );
            }
        }

        let total_rounded_rects = rr_index;

        // Build all quad vertices (tab bar flat quads + all panel cell backgrounds)
        let mut quad_verts: Vec<QuadVertex> = Vec::new();

        // Tab bar flat quads (separator line)
        for bq in &tab_draw.flat_quads {
            push_quad(&mut quad_verts, bq, w, h);
        }

        // Panel cell background quads
        for draw in panel_draws {
            for bq in &draw.bg_quads {
                push_quad(&mut quad_verts, bq, w, h);
            }
        }

        let quad_vertex_count = quad_verts.len() as u32;
        if !quad_verts.is_empty() {
            self.queue.write_buffer(
                &self.quad_vertex_buffer,
                0,
                bytemuck::cast_slice(&quad_verts),
            );
        }

        // glyphon text
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        // Build scene TextAreas (tab bar + panels) — rendered before overlay
        let mut text_areas: Vec<TextArea> = Vec::new();

        // Carrier buffer for tab bar icons (empty buffer, scale=1.0 so positions are physical px)
        let mut icon_carrier = Buffer::new(&mut self.font_system, Metrics::new(1.0, 1.0));
        icon_carrier.set_size(&mut self.font_system, Some(0.0), Some(0.0));

        text_areas.push(TextArea {
            buffer: &icon_carrier,
            left: 0.0,
            top: 0.0,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            },
            default_color: self.colors.fg_glyphon(),
            custom_glyphs: &tab_draw.custom_glyphs,
        });

        // Tab bar label text areas
        let tab_buffers = tab_bar.tab_buffers();
        for ta in &tab_draw.text_areas {
            if ta.buffer_index < tab_buffers.len() {
                text_areas.push(TextArea {
                    buffer: &tab_buffers[ta.buffer_index],
                    left: ta.left,
                    top: ta.top,
                    scale: scale_factor,
                    bounds: rect_to_text_bounds(&ta.bounds),
                    default_color: if ta.is_active {
                        self.colors.tab_active_text()
                    } else {
                        self.colors.fg_glyphon()
                    },
                    custom_glyphs: &[],
                });
            }
        }

        // Panel text — per-cell rendering
        for (panel_idx, draw) in panel_draws.iter().enumerate() {
            if panel_idx >= panel_buffers.len() {
                break;
            }
            let bufs = panel_buffers[panel_idx];
            for spec in &draw.text_cells {
                if spec.buffer_index >= bufs.len() {
                    continue;
                }
                text_areas.push(TextArea {
                    buffer: &bufs[spec.buffer_index],
                    left: spec.left,
                    top: spec.top,
                    scale: scale_factor,
                    bounds: rect_to_text_bounds(&spec.bounds),
                    default_color: spec.color,
                    custom_glyphs: &[],
                });
            }
        }

        self.text_renderer
            .prepare_with_custom(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
                |req| icon_manager.rasterize(req),
            )
            .expect("prepare scene text");

        // Overlay text (dropdown + SSH dialog) — rendered after overlay rects paint over scene text
        let has_overlay = scene_rr_count < total_rounded_rects;
        let dropdown_buffers = dropdown.map(|d| d.item_buffers());
        let mut overlay_areas: Vec<TextArea> = Vec::new();
        if let (Some(dd), Some(dd_bufs)) = (&dropdown_draw, dropdown_buffers) {
            for ta in &dd.text_areas {
                if ta.buffer_index < dd_bufs.len() {
                    let default_color = if ta.is_hovered {
                        self.colors.dropdown_text_active()
                    } else {
                        self.colors.dropdown_text()
                    };
                    overlay_areas.push(TextArea {
                        buffer: &dd_bufs[ta.buffer_index],
                        left: ta.left,
                        top: ta.top,
                        scale: scale_factor,
                        bounds: rect_to_text_bounds(&ta.bounds),
                        default_color,
                        custom_glyphs: &[],
                    });
                }
            }
        }

        self.overlay_text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                overlay_areas,
                &mut self.swash_cache,
            )
            .expect("prepare overlay text");

        // Encode render pass
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

            // === Scene layer ===

            // 1. Scene rounded rects (panel islands + tab bar elements)
            if scene_rr_count > 0 {
                pass.set_pipeline(&self.rounded_rect_pipeline);
                for i in 0..scene_rr_count {
                    let offset = (i * aligned_size) as u32;
                    pass.set_bind_group(0, &self.rounded_rect_bind_group, &[offset]);
                    pass.draw(0..6, 0..1);
                }
            }

            // 2. Flat quads (tab bar separator + cell backgrounds + cursors)
            if quad_vertex_count > 0 {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
                pass.draw(0..quad_vertex_count, 0..1);
            }

            // 3. Scene text + icons (tab labels + panel text)
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render scene text");

            // === Overlay layer (dropdown) ===

            // 4. Overlay rounded rects (shadow + border + fill + hover highlight)
            if has_overlay {
                pass.set_pipeline(&self.rounded_rect_pipeline);
                for i in scene_rr_count..total_rounded_rects {
                    let offset = (i * aligned_size) as u32;
                    pass.set_bind_group(0, &self.rounded_rect_bind_group, &[offset]);
                    pass.draw(0..6, 0..1);
                }
            }

            // 5. Overlay text (dropdown menu items)
            self.overlay_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render overlay text");
        }

        // Optional: copy rendered texture to staging buffer for screenshot
        let staging = if screenshot_path.is_some() {
            let width = self.surface_config.width;
            let height = self.surface_config.height;
            let bytes_per_pixel = 4u32;
            let padded_row = align_up(width * bytes_per_pixel, 256);

            let staging_buf = self.device.create_buffer(&BufferDescriptor {
                label: Some("screenshot staging"),
                size: (padded_row * height) as u64,
                usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            encoder.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                ImageCopyBuffer {
                    buffer: &staging_buf,
                    layout: ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_row),
                        rows_per_image: Some(height),
                    },
                },
                Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            Some((staging_buf, padded_row, width, height))
        } else {
            None
        };

        self.queue.submit(std::iter::once(encoder.finish()));

        // Read back screenshot if requested
        if let (Some(path), Some((staging_buf, padded_row, width, height))) =
            (screenshot_path, &staging)
        {
            let buffer_slice = staging_buf.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer_slice.map_async(MapMode::Read, move |result| {
                tx.send(result).unwrap();
            });
            self.device.poll(Maintain::Wait);
            rx.recv().unwrap().expect("map staging buffer");

            let data = buffer_slice.get_mapped_range();
            save_screenshot(path, &data, *width, *height, *padded_row);
            drop(data);
            staging_buf.unmap();
        }

        frame.present();

        self.atlas.trim();

        Ok(())
    }
}

fn save_screenshot(path: &str, data: &[u8], width: u32, height: u32, padded_row: u32) {
    let file = std::fs::File::create(path).expect("create screenshot file");
    let writer = std::io::BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);

    let mut writer = encoder.write_header().expect("write PNG header");

    // Convert from BGRA (wgpu surface format) to RGBA, stripping row padding
    let unpadded_row = width * 4;
    let mut rgba_data = Vec::with_capacity((unpadded_row * height) as usize);
    for row in 0..height {
        let row_start = (row * padded_row) as usize;
        let row_end = row_start + unpadded_row as usize;
        let row_data = &data[row_start..row_end];
        for pixel in row_data.chunks_exact(4) {
            // BGRA -> RGBA
            rgba_data.push(pixel[2]); // R
            rgba_data.push(pixel[1]); // G
            rgba_data.push(pixel[0]); // B
            rgba_data.push(pixel[3]); // A
        }
    }

    writer.write_image_data(&rgba_data).expect("write PNG data");
    log::info!("screenshot saved to {path}");
}

pub fn rect_to_text_bounds(r: &Rect) -> TextBounds {
    TextBounds {
        left: r.x as i32,
        top: r.y as i32,
        right: (r.x + r.width) as i32,
        bottom: (r.y + r.height) as i32,
    }
}

fn push_quad(verts: &mut Vec<QuadVertex>, bq: &BgQuad, surface_w: f32, surface_h: f32) {
    let x0 = bq.x;
    let y0 = bq.y;
    let x1 = x0 + bq.w;
    let y1 = y0 + bq.h;

    let nx0 = (x0 / surface_w) * 2.0 - 1.0;
    let ny0 = 1.0 - (y0 / surface_h) * 2.0;
    let nx1 = (x1 / surface_w) * 2.0 - 1.0;
    let ny1 = 1.0 - (y1 / surface_h) * 2.0;

    let c = bq.color;
    verts.push(QuadVertex {
        position: [nx0, ny0],
        color: c,
    });
    verts.push(QuadVertex {
        position: [nx1, ny0],
        color: c,
    });
    verts.push(QuadVertex {
        position: [nx0, ny1],
        color: c,
    });
    verts.push(QuadVertex {
        position: [nx0, ny1],
        color: c,
    });
    verts.push(QuadVertex {
        position: [nx1, ny0],
        color: c,
    });
    verts.push(QuadVertex {
        position: [nx1, ny1],
        color: c,
    });
}

pub fn align_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) & !(alignment - 1)
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

pub const ROUNDED_RECT_SHADER: &str = r#"
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
    let shadow_softness = u.params.y;

    let d = rounded_rect_sdf(pixel - center, half_size, radius);

    var alpha: f32;
    if shadow_softness > 0.0 {
        // Shadow mode: soft Gaussian-like falloff outside the rect
        alpha = 1.0 - smoothstep(0.0, shadow_softness, d);
    } else {
        // Normal mode: sharp anti-aliased edge
        alpha = 1.0 - smoothstep(-0.5, 0.5, d);
    }

    if alpha < 0.001 {
        discard;
    }

    return vec4(u.color.rgb, u.color.a * alpha);
}
"#;
