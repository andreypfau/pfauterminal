use std::sync::Arc;

use glyphon::{
    Buffer, Cache, FontSystem, Metrics, Resolution, SwashCache, TextArea, TextAtlas, TextBounds,
    TextRenderer, Viewport,
};
use wgpu::*;

use crate::colors::ColorScheme;
use crate::draw::DrawContext;
use crate::font::{self, CellMetrics};
use crate::icons::IconManager;
use crate::layout::{BgQuad, CursorData, Rect, RoundedQuad, TextSpec};

/// Max rounded rects: panel islands + tab bar elements (borders, backgrounds)
const MAX_ROUNDED_RECTS: usize = 64;

/// Pick an sRGB render format, fixing Bgra8Unorm/Rgba8Unorm on Windows.
fn pick_srgb_format(fmt: TextureFormat) -> TextureFormat {
    if fmt.is_srgb() {
        return fmt;
    }
    match fmt {
        TextureFormat::Bgra8Unorm => TextureFormat::Bgra8UnormSrgb,
        TextureFormat::Rgba8Unorm => TextureFormat::Rgba8UnormSrgb,
        other => other,
    }
}

fn align_up(value: u32, alignment: u32) -> u32 {
    (value + alignment - 1) & !(alignment - 1)
}

/// Core wgpu primitives returned by GPU initialization.
struct GpuInit {
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    render_format: TextureFormat,
}

/// Initialize wgpu device, queue, surface, and configuration for a window.
fn init_gpu(window: Arc<winit::window::Window>, texture_usages: TextureUsages) -> GpuInit {
    let size = window.inner_size();

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
            label: Some("gpu device"),
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

    let render_format = pick_srgb_format(surface_format);

    let view_formats = if render_format != surface_format {
        vec![render_format]
    } else {
        vec![]
    };

    let surface_config = SurfaceConfiguration {
        usage: texture_usages,
        format: surface_format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats,
    };
    surface.configure(&device, &surface_config);

    GpuInit {
        device,
        queue,
        surface,
        surface_config,
        render_format,
    }
}

struct TextResources {
    font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_text_renderer: TextRenderer,
    viewport: Viewport,
}

fn init_text_resources(
    device: &Device,
    queue: &Queue,
    render_format: TextureFormat,
) -> TextResources {
    let font_system = font::create_font_system();
    let swash_cache = SwashCache::new();
    let cache = Cache::new(device);
    let viewport = Viewport::new(device, &cache);
    let mut atlas = TextAtlas::new(device, queue, &cache, render_format);
    let text_renderer = TextRenderer::new(&mut atlas, device, MultisampleState::default(), None);
    let overlay_text_renderer =
        TextRenderer::new(&mut atlas, device, MultisampleState::default(), None);

    TextResources {
        font_system,
        swash_cache,
        atlas,
        text_renderer,
        overlay_text_renderer,
        viewport,
    }
}

/// Begin a render pass that clears to the given color.
fn begin_clear_pass<'a>(
    encoder: &'a mut CommandEncoder,
    view: &'a TextureView,
    label: &'a str,
    clear_color: wgpu::Color,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: Operations {
                load: LoadOp::Clear(clear_color),
                store: StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        ..Default::default()
    })
}

fn begin_frame(
    surface: &Surface,
    render_format: TextureFormat,
) -> Result<(SurfaceTexture, TextureView), SurfaceError> {
    let frame = surface.get_current_texture()?;
    let view = frame.texture.create_view(&TextureViewDescriptor {
        format: Some(render_format),
        ..Default::default()
    });
    Ok((frame, view))
}

fn update_viewport(viewport: &mut Viewport, queue: &Queue, config: &SurfaceConfiguration) {
    viewport.update(
        queue,
        Resolution {
            width: config.width,
            height: config.height,
        },
    );
}

fn finish_frame(
    queue: &Queue,
    atlas: &mut TextAtlas,
    encoder: CommandEncoder,
    frame: SurfaceTexture,
) {
    queue.submit(std::iter::once(encoder.finish()));
    frame.present();
    atlas.trim();
}

// ---------------------------------------------------------------------------
// RoundedRectPipeline
// ---------------------------------------------------------------------------

/// Rounded rectangle SDF rendering pipeline with dynamic uniform buffer.
pub struct RoundedRectPipeline {
    pipeline: RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: BindGroup,
    aligned_size: usize,
}

impl RoundedRectPipeline {
    pub fn new(device: &Device, render_format: TextureFormat, max_rects: usize) -> Self {
        let uniform_alignment = device.limits().min_uniform_buffer_offset_alignment;
        let uniform_size = std::mem::size_of::<RoundedRectUniforms>() as u32;
        let aligned_size = align_up(uniform_size, uniform_alignment);

        let uniform_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("rounded rect uniforms"),
            size: (aligned_size as usize * max_rects) as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("rounded rect bind group layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<RoundedRectUniforms>() as u64,
                    ),
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("rounded rect bind group"),
            layout: &bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: BindingResource::Buffer(BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<RoundedRectUniforms>() as u64),
                }),
            }],
        });

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("rounded rect shader"),
            source: ShaderSource::Wgsl(ROUNDED_RECT_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("rounded rect pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("rounded rect pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
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
            pipeline,
            uniform_buffer,
            bind_group,
            aligned_size: aligned_size as usize,
        }
    }

    fn upload_uniform(&self, queue: &Queue, index: usize, uniforms: RoundedRectUniforms) {
        queue.write_buffer(
            &self.uniform_buffer,
            (index * self.aligned_size) as u64,
            bytemuck::cast_slice(&[uniforms]),
        );
    }

    /// Upload a batch of RoundedQuads starting at `start_index`. Returns the next index.
    pub fn upload_quads(&self, queue: &Queue, quads: &[RoundedQuad], start_index: usize) -> usize {
        let mut idx = start_index;
        for rq in quads {
            let uniforms =
                RoundedRectUniforms::new(&rq.rect, rq.radius, rq.shadow_softness, rq.color);
            self.upload_uniform(queue, idx, uniforms);
            idx += 1;
        }
        idx
    }

    /// Draw all rounded rects in the given range using the current render pass.
    pub fn draw_range(&self, pass: &mut wgpu::RenderPass, start: usize, count: usize) {
        if count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        for i in start..start + count {
            let offset = (i * self.aligned_size) as u32;
            pass.set_bind_group(0, &self.bind_group, &[offset]);
            pass.draw(0..6, 0..1);
        }
    }
}

// ---------------------------------------------------------------------------
// CursorPipeline
// ---------------------------------------------------------------------------

/// Uniform data for the cursor shader.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct CursorUniforms {
    prev_bounds: [f32; 4],   // [x0, y0, x1, y1]
    target_bounds: [f32; 4], // [x0, y0, x1, y1]
    color: [f32; 4],
    params: [f32; 4], // [radius, time_since_move, time_since_input, move_speed]
    clip: [f32; 4],   // [clip_top, clip_bottom, 0, 0]
}

impl CursorUniforms {
    fn from_cursor_data(data: &CursorData) -> Self {
        let pr = &data.prev_rect;
        let tr = &data.target_rect;
        Self {
            prev_bounds: [pr.x, pr.y, pr.x + pr.width, pr.y + pr.height],
            target_bounds: [tr.x, tr.y, tr.x + tr.width, tr.y + tr.height],
            color: data.color,
            params: [
                data.radius,
                data.time_since_move,
                data.time_since_input,
                data.move_speed,
            ],
            clip: [data.clip_top, data.clip_bottom, 0.0, 0.0],
        }
    }
}

struct CursorPipeline {
    pipeline: RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: BindGroup,
}

impl CursorPipeline {
    fn new(device: &Device, render_format: TextureFormat) -> Self {
        let uniform_buffer = device.create_buffer(&BufferDescriptor {
            label: Some("cursor uniforms"),
            size: std::mem::size_of::<CursorUniforms>() as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
            label: Some("cursor bind group layout"),
            entries: &[BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::VERTEX | ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<CursorUniforms>() as u64
                    ),
                },
                count: None,
            }],
        });

        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("cursor bind group"),
            layout: &bind_group_layout,
            entries: &[BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("cursor shader"),
            source: ShaderSource::Wgsl(CURSOR_SHADER.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
            label: Some("cursor pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("cursor pipeline"),
            layout: Some(&pipeline_layout),
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(FragmentState {
                module: &shader,
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
            pipeline,
            uniform_buffer,
            bind_group,
        }
    }

    fn upload(&self, queue: &Queue, uniforms: CursorUniforms) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
    }

    fn draw(&self, pass: &mut wgpu::RenderPass) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

// ---------------------------------------------------------------------------
// Uniform types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// GpuSimple — lightweight GPU context for the SSH dialog window
// ---------------------------------------------------------------------------

/// Lightweight GPU context for simple windows (e.g. SSH dialog) that only need
/// two-layer rendering (base + overlay) with rounded rects and text.
pub struct GpuSimple {
    pub device: Device,
    pub queue: Queue,
    pub surface: Surface<'static>,
    pub surface_config: SurfaceConfiguration,
    render_format: TextureFormat,

    pub font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_text_renderer: TextRenderer,
    viewport: Viewport,
    rounded_rect: RoundedRectPipeline,
    pub colors: ColorScheme,
}

impl GpuSimple {
    pub fn new(
        window: Arc<winit::window::Window>,
        colors: ColorScheme,
        max_rounded_rects: usize,
    ) -> Self {
        let gpu = init_gpu(window, TextureUsages::RENDER_ATTACHMENT);
        let text = init_text_resources(&gpu.device, &gpu.queue, gpu.render_format);
        let rounded_rect =
            RoundedRectPipeline::new(&gpu.device, gpu.render_format, max_rounded_rects);

        Self {
            device: gpu.device,
            queue: gpu.queue,
            surface: gpu.surface,
            surface_config: gpu.surface_config,
            render_format: gpu.render_format,
            font_system: text.font_system,
            swash_cache: text.swash_cache,
            atlas: text.atlas,
            text_renderer: text.text_renderer,
            overlay_text_renderer: text.overlay_text_renderer,
            viewport: text.viewport,
            rounded_rect,
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

    /// Render a frame with two layers: base (rounded rects + text) then overlay (rounded rects + text).
    pub fn render_simple(
        &mut self,
        clear_color: wgpu::Color,
        base_quads: &[RoundedQuad],
        overlay_quads: &[RoundedQuad],
        base_text: Vec<TextArea>,
        overlay_text: Vec<TextArea>,
    ) -> Result<(), SurfaceError> {
        let (frame, view) = begin_frame(&self.surface, self.render_format)?;

        let base_rr_count = self.rounded_rect.upload_quads(&self.queue, base_quads, 0);
        let total_rr_count =
            self.rounded_rect
                .upload_quads(&self.queue, overlay_quads, base_rr_count);

        update_viewport(&mut self.viewport, &self.queue, &self.surface_config);

        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                base_text,
                &mut self.swash_cache,
            )
            .expect("prepare base text");

        self.overlay_text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                overlay_text,
                &mut self.swash_cache,
            )
            .expect("prepare overlay text");

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut pass = begin_clear_pass(&mut encoder, &view, "render pass", clear_color);

            self.rounded_rect.draw_range(&mut pass, 0, base_rr_count);
            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render base text");

            self.rounded_rect
                .draw_range(&mut pass, base_rr_count, total_rr_count - base_rr_count);
            self.overlay_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render overlay text");
        }

        finish_frame(&self.queue, &mut self.atlas, encoder, frame);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GpuContext — full GPU context for the main terminal window
// ---------------------------------------------------------------------------

pub struct GpuContext {
    pub device: Device,
    pub queue: Queue,
    pub surface: Surface<'static>,
    pub surface_config: SurfaceConfiguration,
    render_format: TextureFormat,

    pub font_system: FontSystem,
    swash_cache: SwashCache,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    overlay_text_renderer: TextRenderer,
    viewport: Viewport,
    rounded_rect: RoundedRectPipeline,
    pub colors: ColorScheme,

    pub cell: CellMetrics,
    pub scale_factor: f32,

    // Background quad rendering
    quad_pipeline: RenderPipeline,
    quad_vertex_buffer: wgpu::Buffer,

    // Cursor animation pipeline
    cursor_pipeline: CursorPipeline,

    // Cached carrier buffers for custom glyphs (icons) — avoids per-frame allocation
    icon_carrier: Buffer,
    overlay_icon_carrier: Buffer,
}

impl GpuContext {
    pub fn new(window: Arc<winit::window::Window>, colors: ColorScheme) -> Self {
        let scale_factor = window.scale_factor() as f32;

        let gpu = init_gpu(
            window,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        );
        let mut text = init_text_resources(&gpu.device, &gpu.queue, gpu.render_format);
        let rounded_rect =
            RoundedRectPipeline::new(&gpu.device, gpu.render_format, MAX_ROUNDED_RECTS);

        let cell = font::measure_cell(&mut text.font_system);

        // Quad pipeline
        let quad_shader = gpu.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("quad shader"),
            source: ShaderSource::Wgsl(QUAD_SHADER.into()),
        });

        let quad_pipeline_layout = gpu
            .device
            .create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some("quad pipeline layout"),
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });

        let quad_pipeline = gpu
            .device
            .create_render_pipeline(&RenderPipelineDescriptor {
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
                        format: gpu.render_format,
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

        let quad_vertex_buffer = gpu.device.create_buffer(&BufferDescriptor {
            label: Some("quad vertex buffer"),
            size: 2 * 1024 * 1024,
            usage: BufferUsages::VERTEX | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let cursor_pipeline = CursorPipeline::new(&gpu.device, gpu.render_format);

        // Cached icon carrier buffers (empty, scale=1.0 so positions are physical px)
        let mut icon_carrier = Buffer::new(&mut text.font_system, Metrics::new(1.0, 1.0));
        icon_carrier.set_size(&mut text.font_system, Some(0.0), Some(0.0));
        let mut overlay_icon_carrier = Buffer::new(&mut text.font_system, Metrics::new(1.0, 1.0));
        overlay_icon_carrier.set_size(&mut text.font_system, Some(0.0), Some(0.0));

        Self {
            device: gpu.device,
            queue: gpu.queue,
            surface: gpu.surface,
            surface_config: gpu.surface_config,
            render_format: gpu.render_format,
            font_system: text.font_system,
            swash_cache: text.swash_cache,
            atlas: text.atlas,
            text_renderer: text.text_renderer,
            overlay_text_renderer: text.overlay_text_renderer,
            viewport: text.viewport,
            rounded_rect,
            colors,
            cell,
            scale_factor,
            quad_pipeline,
            quad_vertex_buffer,
            cursor_pipeline,
            icon_carrier,
            overlay_icon_carrier,
        }
    }

    fn capture_screenshot(&self, path: &str, texture: &Texture) {
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        let padded_row = align_up(width * 4, 256);

        let staging_buf = self.device.create_buffer(&BufferDescriptor {
            label: Some("screenshot staging"),
            size: (padded_row * height) as u64,
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("screenshot encoder"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
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
        self.queue.submit(std::iter::once(encoder.finish()));

        let buffer_slice = staging_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
        self.device.poll(Maintain::Wait);
        rx.recv().unwrap().expect("map staging buffer");

        let data = buffer_slice.get_mapped_range();
        save_screenshot(path, &data, width, height, padded_row);
        drop(data);
        staging_buf.unmap();
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Render a complete frame with scene (panels + tab bar) and overlay (dropdown).
    /// If `screenshot_path` is Some, saves the rendered frame as a PNG.
    pub fn render_frame(
        &mut self,
        scene: &DrawContext,
        overlay: &DrawContext,
        scene_text: &[(&[TextSpec], &[Buffer])],
        overlay_text: &[(&[TextSpec], &[Buffer])],
        icon_manager: &IconManager,
        screenshot_path: Option<&str>,
    ) -> Result<(), SurfaceError> {
        let scale_factor = self.scale_factor;
        let (frame, view) = begin_frame(&self.surface, self.render_format)?;

        let w = self.surface_config.width as f32;
        let h = self.surface_config.height as f32;

        // Upload rounded rect uniforms in two groups:
        // Scene -- drawn before text
        // Overlay -- drawn after text, on top of everything
        let scene_rr_count = self
            .rounded_rect
            .upload_quads(&self.queue, &scene.rounded_quads, 0);

        let total_rounded_rects =
            self.rounded_rect
                .upload_quads(&self.queue, &overlay.rounded_quads, scene_rr_count);

        // Build all quad vertices (scene flat quads: tab bar separator + cell backgrounds)
        let mut quad_verts: Vec<QuadVertex> = Vec::new();
        for bq in &scene.flat_quads {
            push_quad(&mut quad_verts, bq, w, h);
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
        update_viewport(&mut self.viewport, &self.queue, &self.surface_config);

        // Build scene TextAreas
        let mut text_areas: Vec<TextArea> = Vec::new();

        // Icon carrier for custom glyphs (uses cached buffer)
        text_areas.push(TextArea {
            buffer: &self.icon_carrier,
            left: 0.0,
            top: 0.0,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            },
            default_color: self.colors.foreground.to_glyphon(),
            custom_glyphs: &scene.custom_glyphs,
        });

        // Scene text areas (tab bar + panels)
        for &(specs, bufs) in scene_text {
            push_text_specs(&mut text_areas, specs, bufs, scale_factor);
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

        // Overlay text (dropdown) with icon support
        let mut overlay_areas: Vec<TextArea> = Vec::new();

        if !overlay.custom_glyphs.is_empty() {
            overlay_areas.push(TextArea {
                buffer: &self.overlay_icon_carrier,
                left: 0.0,
                top: 0.0,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: h as i32,
                },
                default_color: self.colors.foreground.to_glyphon(),
                custom_glyphs: &overlay.custom_glyphs,
            });
        }

        for &(specs, bufs) in overlay_text {
            push_text_specs(&mut overlay_areas, specs, bufs, scale_factor);
        }

        self.overlay_text_renderer
            .prepare_with_custom(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                overlay_areas,
                &mut self.swash_cache,
                |req| icon_manager.rasterize(req),
            )
            .expect("prepare overlay text");

        // Upload cursor uniforms
        if let Some(cursor) = &scene.cursor {
            self.cursor_pipeline
                .upload(&self.queue, CursorUniforms::from_cursor_data(cursor));
        }

        // Encode render pass
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor {
                label: Some("render encoder"),
            });

        {
            let mut pass = begin_clear_pass(
                &mut encoder,
                &view,
                "main pass",
                self.colors.chrome.to_wgpu_color(),
            );

            // === Scene layer ===
            self.rounded_rect.draw_range(&mut pass, 0, scene_rr_count);

            if quad_vertex_count > 0 {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_vertex_buffer(0, self.quad_vertex_buffer.slice(..));
                pass.draw(0..quad_vertex_count, 0..1);
            }

            // Cursor (between flat quads and text, so text renders on top)
            if scene.cursor.is_some() {
                self.cursor_pipeline.draw(&mut pass);
            }

            self.text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render scene text");

            // === Overlay layer ===
            self.rounded_rect.draw_range(
                &mut pass,
                scene_rr_count,
                total_rounded_rects - scene_rr_count,
            );

            self.overlay_text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
                .expect("render overlay text");
        }

        if let Some(path) = screenshot_path {
            self.queue.submit(std::iter::once(encoder.finish()));
            self.capture_screenshot(path, &frame.texture);
            frame.present();
            self.atlas.trim();
        } else {
            finish_frame(&self.queue, &mut self.atlas, encoder, frame);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

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

pub fn push_text_specs<'a>(
    areas: &mut Vec<TextArea<'a>>,
    specs: &[TextSpec],
    buffers: &'a [Buffer],
    scale: f32,
) {
    for spec in specs {
        if spec.buffer_index < buffers.len() {
            areas.push(TextArea {
                buffer: &buffers[spec.buffer_index],
                left: spec.left,
                top: spec.top,
                scale,
                bounds: spec.bounds.to_text_bounds(),
                default_color: spec.color,
                custom_glyphs: &[],
            });
        }
    }
}

fn push_quad(verts: &mut Vec<QuadVertex>, bq: &BgQuad, surface_w: f32, surface_h: f32) {
    let nx0 = (bq.rect.x / surface_w) * 2.0 - 1.0;
    let ny0 = 1.0 - (bq.rect.y / surface_h) * 2.0;
    let nx1 = ((bq.rect.x + bq.rect.width) / surface_w) * 2.0 - 1.0;
    let ny1 = 1.0 - ((bq.rect.y + bq.rect.height) / surface_h) * 2.0;
    let c = bq.color;
    verts.extend_from_slice(&[
        QuadVertex {
            position: [nx0, ny0],
            color: c,
        },
        QuadVertex {
            position: [nx1, ny0],
            color: c,
        },
        QuadVertex {
            position: [nx0, ny1],
            color: c,
        },
        QuadVertex {
            position: [nx0, ny1],
            color: c,
        },
        QuadVertex {
            position: [nx1, ny0],
            color: c,
        },
        QuadVertex {
            position: [nx1, ny1],
            color: c,
        },
    ]);
}

// ---------------------------------------------------------------------------
// Shaders
// ---------------------------------------------------------------------------

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

const CURSOR_SHADER: &str = r#"
struct Uniforms {
    prev_bounds: vec4<f32>,
    target_bounds: vec4<f32>,
    color: vec4<f32>,
    params: vec4<f32>,   // [radius, time_since_move, time_since_input, move_speed]
    clip: vec4<f32>,     // [clip_top, clip_bottom, 0, 0]
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

    // Exponential ease-out: speed is decided by CPU (fast for typing, slow for glide)
    let t_move = u.params.y;
    let t_input = u.params.z;
    let speed = u.params.w;
    let factor = exp(-speed * t_move);
    let bounds = u.target_bounds + (u.prev_bounds - u.target_bounds) * factor;

    // Clipping to content area
    if pixel.y < u.clip.x || pixel.y > u.clip.y {
        discard;
    }

    // SDF rounded rect at interpolated position
    let center = (bounds.xy + bounds.zw) * 0.5;
    let half_size = (bounds.zw - bounds.xy) * 0.5;
    let radius = min(u.params.x, min(half_size.x, half_size.y));
    let d = rounded_rect_sdf(pixel - center, half_size, radius);
    let shape_alpha = 1.0 - smoothstep(-0.5, 0.5, d);

    // Smooth cosine blink
    let blink_pause = 0.5;
    let blink_period = 1.0;
    var blink_alpha = 1.0;
    if t_input > blink_pause {
        let phase = (t_input - blink_pause) / blink_period * 6.283185;
        blink_alpha = cos(phase) * 0.5 + 0.5;
    }

    let final_alpha = u.color.a * shape_alpha * blink_alpha;
    if final_alpha < 0.001 {
        discard;
    }
    return vec4(u.color.rgb, final_alpha);
}
"#;
