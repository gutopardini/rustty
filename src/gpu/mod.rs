//! Instanced grid on the GPU. Each cell is a quad, and the glyph atlas stays in VRAM.

use std::{ops::Range, sync::Arc};

use anyhow::Context;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::cell::{GlyphRect, GpuCell, Uniforms};
use crate::color::{Rgb, DEFAULT_BG};
use crate::confirm::{self, Dialog};
use crate::font::{Atlas, Face};
use crate::term::Snapshot;

mod frame;
mod glass;
mod images;

use frame::Frame;
pub use glass::{
    GlassShape, BAR as GLASS_BAR, BUTTON as GLASS_BUTTON, CONTAINER as GLASS_CONTAINER,
    DIVIDER as GLASS_DIVIDER, DOT as GLASS_DOT, ICON_CLOSE, ICON_GEAR, ICON_LIMITS_CRIT,
    ICON_LIMITS_OK, ICON_LIMITS_WARN, ICON_MAXIMIZE, ICON_MINIMIZE, ICON_PLUS, ICON_RESTORE,
    LIMITS_CARD as GLASS_LIMITS_CARD, LIMITS_ICON as GLASS_LIMITS_ICON, PILL as GLASS_PILL,
    SHEET as GLASS_SHEET, TAB_HOVER as GLASS_TAB_HOVER, TAB_STRIP as GLASS_TAB_STRIP,
    TOOLTIP as GLASS_TOOLTIP,
};
use glass::Glass;
use images::Images;

const MAX_GLYPHS: usize = 8192;
const BAR_CELLS: usize = 1024;
const AGENT_PIXELS: &[u8; 64 * 64 * crate::agent::ICON_COUNT as usize * 4] =
    include_bytes!("../../assets/agents/atlas.rgba");

struct DialogLayer {
    uniforms: wgpu::Buffer,
    cells: wgpu::Buffer,
    bind: wgpu::BindGroup,
    count: u32,
}

pub struct Renderer {
    surface: Option<wgpu::Surface<'static>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    images: Option<Images>,
    bind_layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    cell_buf: wgpu::Buffer,
    cell_capacity: usize,
    glyph_buf: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    _agent_tex: wgpu::Texture,
    agent_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    icon_sampler: wgpu::Sampler,
    gpu_cells: Vec<GpuCell>,
    cols: u16,
    rows: u16,
    cell_px_w: f32,
    cell_px_h: f32,
    scrollbar_visible: bool,
    seen_generation: u64,
    bar_uniform: wgpu::Buffer,
    bar_cells_buf: wgpu::Buffer,
    bar_bind: wgpu::BindGroup,
    bar_gpu: Vec<GpuCell>,
    bar_count: u32,
    title_uniform: wgpu::Buffer,
    title_cells_buf: wgpu::Buffer,
    title_bind: wgpu::BindGroup,
    title_gpu: Vec<GpuCell>,
    title_count: u32,
    dialog: Option<DialogLayer>,
    left_w: f32,
    top_h: f32,
    /// Space between the title bar and the first row.
    grid_pad_top: f32,
    chrome_h: f32,
    /// Rounded corners. Absent when the surface cannot be transparent.
    frame: Option<Frame>,
    clear: Rgb,
    /// Opacity of the window tint in glass mode. `None` keeps the window opaque.
    glass: Option<f32>,
    glass_layer: Option<Glass>,
    selection: (u32, u32),
    presented: Option<Presented>,
}

#[derive(Clone, Copy)]
struct Presented {
    cols: u16,
    rows: u16,
    cursor: Option<(u16, u16)>,
    composer: Option<[u16; 4]>,
    scroll_top: f32,
    scroll_h: f32,
}

impl Renderer {
    pub fn new(window: Arc<Window>, vsync: bool) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .context("creating the Vulkan surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .context("no Vulkan adapter")?;
        let info = adapter.get_info();
        tracing::info!(name = %info.name, backend = ?info.backend, device = info.device, "GPU");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustty"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .context("opening the Vulkan device")?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|format| {
                matches!(
                    format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .unwrap_or(caps.formats[0]);
        // Mailbox still follows the display, but a newer frame replaces one that has not
        // been shown. present() then returns without parking the key handler on vblank.
        let present_mode = present_mode(vsync, &caps.present_modes);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            // Premultiplied alpha lets the corners be transparent.
            alpha_mode: if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                caps.alpha_modes[0]
            },
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        tracing::info!(alpha = ?config.alpha_mode, "surface");
        Self::build(Some(surface), device, queue, config)
    }

    /// Headless Vulkan renderer targeting an offscreen `Rgba8Unorm` texture (for automated docs screenshots).
    pub fn new_offscreen(width: u32, height: u32) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            apply_limit_buckets: false,
        }))
        .context("no Vulkan adapter")?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rustty-offscreen"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .context("opening the Vulkan device")?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8Unorm,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
            view_formats: vec![],
        };
        Self::build(None, device, queue, config)
    }

    fn build(
        surface: Option<wgpu::Surface<'static>>,
        device: wgpu::Device,
        queue: wgpu::Queue,
        config: wgpu::SurfaceConfiguration,
    ) -> anyhow::Result<Self> {
        let format = config.format;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rustty-cells"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rustty-cells"),
            entries: &[
                buffer_entry(0, wgpu::BufferBindingType::Uniform, wgpu::ShaderStages::VERTEX_FRAGMENT),
                buffer_entry(
                    1,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    wgpu::ShaderStages::VERTEX,
                ),
                buffer_entry(
                    2,
                    wgpu::BufferBindingType::Storage { read_only: true },
                    wgpu::ShaderStages::VERTEX,
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rustty-cells"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rustty-cells"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // An opaque cell replaces what is under it, as before. A see-through
                    // cell in glass mode leaves the glass and the window tint visible.
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let glyph_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glyphs"),
            size: (MAX_GLYPHS * std::mem::size_of::<GlyphRect>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let cell_capacity = 80 * 24;
        let cell_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cells"),
            size: (cell_capacity * std::mem::size_of::<GpuCell>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: Atlas::SIZE,
                height: Atlas::SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let agent_size = wgpu::Extent3d {
            width: 64 * crate::agent::ICON_COUNT,
            height: 64,
            depth_or_array_layers: 1,
        };
        let agent_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("agent-icons"),
            size: agent_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &agent_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            AGENT_PIXELS,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64 * crate::agent::ICON_COUNT * 4),
                rows_per_image: Some(64),
            },
            agent_size,
        );
        let agent_view = agent_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let icon_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("agent-icons"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let bind = make_bind(
            &device,
            &bind_layout,
            &uniform_buf,
            &cell_buf,
            &glyph_buf,
            &atlas_view,
            &agent_view,
            &sampler,
            &icon_sampler,
        );
        let bar_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tab-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bar_cells_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tab-cells"),
            size: (BAR_CELLS * std::mem::size_of::<GpuCell>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bar_bind = make_bind(
            &device,
            &bind_layout,
            &bar_uniform,
            &bar_cells_buf,
            &glyph_buf,
            &atlas_view,
            &agent_view,
            &sampler,
            &icon_sampler,
        );
        let title_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("title-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let title_cells_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("title-cells"),
            size: (BAR_CELLS * std::mem::size_of::<GpuCell>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let title_bind = make_bind(
            &device,
            &bind_layout,
            &title_uniform,
            &title_cells_buf,
            &glyph_buf,
            &atlas_view,
            &agent_view,
            &sampler,
            &icon_sampler,
        );
        let frame = (config.alpha_mode == wgpu::CompositeAlphaMode::PreMultiplied)
            .then(|| Frame::new(&device, config.format));
        let renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            images: None,
            bind_layout,
            bind,
            uniform_buf,
            cell_buf,
            cell_capacity,
            glyph_buf,
            atlas_tex,
            atlas_view,
            _agent_tex: agent_tex,
            agent_view,
            sampler,
            icon_sampler,
            gpu_cells: Vec::new(),
            cols: 0,
            rows: 0,
            cell_px_w: 1.0,
            cell_px_h: 1.0,
            scrollbar_visible: false,
            seen_generation: 0,
            bar_uniform,
            bar_cells_buf,
            bar_bind,
            bar_gpu: vec![GpuCell { slot: 0, fg: 0, bg: 0, flags: 0 }; BAR_CELLS],
            bar_count: 0,
            title_uniform,
            title_cells_buf,
            title_bind,
            title_gpu: vec![GpuCell { slot: 0, fg: 0, bg: 0, flags: 0 }; BAR_CELLS],
            title_count: 0,
            dialog: None,
            left_w: 0.0,
            top_h: 0.0,
            grid_pad_top: 0.0,
            chrome_h: 0.0,
            frame,
            clear: DEFAULT_BG,
            glass: None,
            glass_layer: None,
            selection: (0, 0),
            presented: None,
        };
        renderer.clear_atlas();
        Ok(renderer)
    }

    /// `top` and `bottom` are the bars. `pad_top` is the space between the top bar and the first row.
    pub fn set_insets(&mut self, left: f32, top: f32, pad_top: f32, bottom: f32) {
        self.left_w = left.max(0.0);
        self.top_h = top.max(0.0);
        self.grid_pad_top = pad_top.max(0.0);
        self.chrome_h = bottom.max(0.0);
    }

    /// Corner radius in pixels. Zero for a maximized window.
    pub fn set_corner_radius(&mut self, radius: f32) {
        if let Some(frame) = &mut self.frame {
            frame.set_radius(radius);
        }
    }

    pub fn set_clear(&mut self, color: Rgb) {
        self.clear = color;
    }

    /// Glass needs a premultiplied surface, the same one that cuts the corners round.
    pub fn supports_glass(&self) -> bool {
        self.frame.is_some()
    }

    /// `Some(opacity)` turns the default background into a tint of that opacity.
    /// `None` restores the opaque window.
    pub fn set_glass(&mut self, opacity: Option<f32>) {
        let opacity = opacity.filter(|_| self.supports_glass()).map(|value| value.clamp(0.0, 1.0));
        self.glass = opacity;
        if let Some(frame) = &mut self.frame {
            frame.set_glass(opacity.is_some());
        }
    }

    /// Vector shapes in physical pixels: glass capsules/buttons/sheets, plus the AI Limits
    /// icon and floating card (which also draw cleanly in opaque mode). Shapes before `overlay`
    /// are drawn under the bars and the grid; the rest go over the grid, under overlay text.
    pub fn set_glass_shapes(
        &mut self,
        shapes: &[GlassShape],
        overlay: usize,
        scale: f32,
        light: bool,
        tune: &crate::settings::GlassTune,
    ) {
        if shapes.is_empty() && self.glass_layer.is_none() {
            return;
        }
        let layer = self.glass_layer.get_or_insert_with(|| Glass::new(&self.device, self.config.format));
        layer.set(&self.queue, self.config.width, self.config.height, scale, light, tune, shapes, overlay);
    }

    pub fn upload_dialog(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        dialog: Option<&Dialog>,
    ) {
        let Some(dialog) = dialog else {
            self.dialog = None;
            return;
        };
        self.upload_overlay_cells(
            atlas,
            face,
            dialog.origin_col,
            dialog.origin_row,
            dialog.cols,
            dialog.rows,
            &dialog.cells,
            self.glass.is_some(),
        );
    }

    pub fn upload_popover(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        popover: Option<&crate::limits::Popover>,
    ) {
        let Some(popover) = popover else {
            self.dialog = None;
            return;
        };
        self.upload_overlay_cells(
            atlas,
            face,
            popover.origin_col,
            popover.origin_row,
            popover.cols,
            popover.rows,
            &popover.cells,
            true,
        );
    }

    pub fn upload_tooltip(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        tooltip: Option<&crate::tabs::TabTooltip>,
        pad_x: f32,
        pad_y: f32,
    ) {
        let Some(tooltip) = tooltip else {
            self.dialog = None;
            return;
        };
        self.upload_overlay_cells_at(
            atlas,
            face,
            pad_x,
            pad_y,
            tooltip.cols,
            tooltip.rows,
            &tooltip.cells,
            true,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_overlay_cells(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        origin_col: u16,
        origin_row: u16,
        cols: u16,
        rows: u16,
        source: &[crate::cell::Cell],
        transparent_bg: bool,
    ) {
        let cell_w = atlas.cell_w() as f32;
        let cell_h = atlas.cell_h() as f32;
        let pad_x = self.left_w + f32::from(origin_col) * cell_w;
        let pad_y = self.top_h + self.grid_pad_top + f32::from(origin_row) * cell_h;
        self.upload_overlay_cells_at(atlas, face, pad_x, pad_y, cols, rows, source, transparent_bg);
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_overlay_cells_at(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        pad_x: f32,
        pad_y: f32,
        cols: u16,
        rows: u16,
        source: &[crate::cell::Cell],
        transparent_bg: bool,
    ) {
        let max_cells = confirm::MAX_CELLS.max(crate::limits::MAX_CELLS);
        let cells: Vec<_> = source
            .iter()
            .take(max_cells)
            .map(|cell| resolve(atlas, face, cell))
            .collect();
        self.flush_atlas(atlas);

        let layer = self.dialog.get_or_insert_with(|| {
            let uniforms = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay-dialog-uniforms"),
                size: std::mem::size_of::<Uniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let cells = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay-dialog-cells"),
                size: (max_cells * std::mem::size_of::<GpuCell>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = make_bind(
                &self.device,
                &self.bind_layout,
                &uniforms,
                &cells,
                &self.glyph_buf,
                &self.atlas_view,
                &self.agent_view,
                &self.sampler,
                &self.icon_sampler,
            );
            DialogLayer {
                uniforms,
                cells,
                bind,
                count: 0,
            }
        });

        let surf_w = self.config.width as f32;
        let cell_w = atlas.cell_w() as f32;
        let cell_h = atlas.cell_h() as f32;
        let uniforms = Uniforms {
            cols: u32::from(cols),
            rows: u32::from(rows),
            atlas_w: Atlas::SIZE,
            atlas_h: Atlas::SIZE,
            pad_x,
            pad_y,
            cell_w,
            cell_h,
            surf_w,
            surf_h: self.config.height as f32,
            glass: u32::from(transparent_bg),
            glass_key: self.clear.pack(),
            ..bytemuck::Zeroable::zeroed()
        };
        self.queue.write_buffer(&layer.uniforms, 0, bytemuck::bytes_of(&uniforms));
        self.queue.write_buffer(&layer.cells, 0, bytemuck::cast_slice(&cells));
        layer.count = cells.len() as u32;
    }

    pub fn stamp(&mut self, atlas: &mut Atlas, face: &Face, col: u16, row: u16, width: u16, cells: &[crate::cell::Cell]) {
        if self.cols == 0 || width == 0 || self.gpu_cells.is_empty() {
            return;
        }
        let mut dirty = Vec::new();
        for (index, cell) in cells.iter().enumerate() {
            let x = usize::from(col) + index % usize::from(width);
            let y = usize::from(row) + index / usize::from(width);
            if x >= usize::from(self.cols) || y >= usize::from(self.rows) {
                continue;
            }
            let slot = y * usize::from(self.cols) + x;
            if slot >= self.gpu_cells.len() {
                continue;
            }
            self.gpu_cells[slot] = resolve(atlas, face, cell);
            if dirty.last() != Some(&y) {
                dirty.push(y);
            }
        }
        self.flush_atlas(atlas);
        for y in dirty {
            let start = y * usize::from(self.cols);
            let end = start + usize::from(self.cols);
            let bytes = bytemuck::cast_slice(&self.gpu_cells[start..end]);
            self.queue.write_buffer(
                &self.cell_buf,
                (start * std::mem::size_of::<GpuCell>()) as u64,
                bytes,
            );
        }
    }

    pub fn upload_bar(&mut self, atlas: &mut Atlas, face: &Face, cells: &[crate::cell::Cell], strip: Rgb, edge: Rgb, bottom_pad: f32) {
        self.cell_px_w = atlas.cell_w() as f32;
        self.cell_px_h = atlas.cell_h() as f32;
        let count = cells.len().min(self.bar_gpu.len());
        for (index, cell) in cells.iter().take(count).enumerate() {
            self.bar_gpu[index] = resolve(atlas, face, cell);
        }
        if count > 0 {
            self.queue.write_buffer(
                &self.bar_cells_buf,
                0,
                bytemuck::cast_slice(&self.bar_gpu[..count]),
            );
        }
        self.flush_atlas(atlas);
        self.bar_count = count as u32;
        let uniform = self.bar_uniform.clone();
        let pad_y = (self.config.height as f32 - self.chrome_h).max(0.0);
        let strip_h = (self.chrome_h - bottom_pad.max(0.0)).max(1.0);
        self.write_strip_uniforms(&uniform, self.bar_count.max(1), pad_y, strip_h, strip, edge, true);
    }

    pub fn upload_title(&mut self, atlas: &mut Atlas, face: &Face, cells: &[crate::cell::Cell], strip: Rgb) {
        self.cell_px_w = atlas.cell_w() as f32;
        self.cell_px_h = atlas.cell_h() as f32;
        let count = cells.len().min(self.title_gpu.len());
        for (index, cell) in cells.iter().take(count).enumerate() {
            self.title_gpu[index] = resolve(atlas, face, cell);
        }
        if count > 0 {
            self.queue.write_buffer(
                &self.title_cells_buf,
                0,
                bytemuck::cast_slice(&self.title_gpu[..count]),
            );
        }
        self.flush_atlas(atlas);
        self.title_count = count as u32;
        let uniform = self.title_uniform.clone();
        let height = self.top_h;
        self.write_strip_uniforms(&uniform, self.title_count.max(1), 0.0, height, strip, strip, self.glass.is_some());
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
    }

    /// `lo` is inclusive and `hi` is exclusive. `(0, 0)` clears the highlight.
    pub fn set_selection(&mut self, span: (u32, u32)) {
        self.selection = span;
    }

    /// Rewrites the uniforms from the last uploaded grid. The parser is not involved.
    pub fn refresh_cached(&mut self, show_composer: bool, accent: Rgb, scrollbar: Range<u32>) -> bool {
        let Some(frame) = self.presented else {
            return false;
        };
        self.write_uniforms(frame, show_composer, accent, scrollbar);
        true
    }

    pub fn upload(
        &mut self,
        atlas: &mut Atlas,
        face: &Face,
        snap: &Snapshot,
        show_composer: bool,
        accent: Rgb,
        scrollbar: Range<u32>,
    ) {
        if snap.cols == 0 || snap.rows == 0 {
            return;
        }
        self.ensure_grid(snap.cols, snap.rows);
        self.cell_px_w = atlas.cell_w() as f32;
        self.cell_px_h = atlas.cell_h() as f32;
        if atlas.generation() != self.seen_generation {
            self.clear_atlas();
            self.seen_generation = atlas.generation();
        }
        if snap.full {
            for (index, cell) in snap.cells.iter().enumerate() {
                self.gpu_cells[index] = resolve(atlas, face, cell);
            }
            self.flush_atlas(atlas);
            if atlas.generation() != self.seen_generation {
                self.clear_atlas();
                self.seen_generation = atlas.generation();
                for (index, cell) in snap.cells.iter().enumerate() {
                    self.gpu_cells[index] = resolve(atlas, face, cell);
                }
                self.flush_atlas(atlas);
            }
            self.queue.write_buffer(
                &self.cell_buf,
                0,
                bytemuck::cast_slice(&self.gpu_cells),
            );
        } else {
            for (row, cells) in &snap.rows_dirty {
                let start = usize::from(*row) * usize::from(self.cols);
                for (col, cell) in cells.iter().enumerate() {
                    self.gpu_cells[start + col] = resolve(atlas, face, cell);
                }
                let bytes = bytemuck::cast_slice(&self.gpu_cells[start..start + cells.len()]);
                self.queue.write_buffer(
                    &self.cell_buf,
                    (start * std::mem::size_of::<GpuCell>()) as u64,
                    bytes,
                );
            }
            self.flush_atlas(atlas);
        }
        self.scrollbar_visible = snap.scroll_h > 0.0 && !scrollbar.is_empty();
        let frame = Presented {
            cols: snap.cols,
            rows: snap.rows,
            cursor: snap.cursor,
            composer: snap.composer,
            scroll_top: snap.scroll_top,
            scroll_h: snap.scroll_h,
        };
        self.presented = Some(frame);
        let grid = self.write_uniforms(frame, show_composer, accent, scrollbar);
        if !snap.images.is_empty() {
            self.images.get_or_insert_with(|| Images::new(&self.device, self.config.format));
        }
        if let Some(images) = &mut self.images {
            images.upload(&self.device, &self.queue, &grid, &snap.images);
        }
    }

    pub fn render(&mut self, show_terminal_overlays: bool) -> anyhow::Result<()> {
        let count = u32::from(self.cols) * u32::from(self.rows);
        if count == 0 {
            return Ok(());
        }
        let Some(surface) = &self.surface else {
            anyhow::bail!("offscreen renderer has no surface");
        };
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("swapchain rejected the frame");
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let cmd = self.encode_pass(&view, count, show_terminal_overlays);
        self.queue.submit([cmd]);
        self.queue.present(frame);
        Ok(())
    }

    /// Renders the current frame offscreen and reads back premultiplied RGBA8 pixels.
    pub fn render_to_rgba(&mut self, show_terminal_overlays: bool) -> anyhow::Result<Vec<u8>> {
        let count = u32::from(self.cols) * u32::from(self.rows);
        let width = self.config.width;
        let height = self.config.height;
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-target"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rustty-offscreen"),
        });
        self.record_pass(&mut encoder, &view, count, show_terminal_overlays);

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(align) * align;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("offscreen-readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(5)),
        });
        rx.recv().context("waiting for readback")??;

        let mapped = slice.get_mapped_range()?;
        let mut out = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            out.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        staging.unmap();
        Ok(out)
    }

    fn encode_pass(
        &mut self,
        view: &wgpu::TextureView,
        count: u32,
        show_terminal_overlays: bool,
    ) -> wgpu::CommandBuffer {
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rustty"),
        });
        self.record_pass(&mut encoder, view, count, show_terminal_overlays);
        encoder.finish()
    }

    fn record_pass(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        count: u32,
        show_terminal_overlays: bool,
    ) {
        if let Some(corners) = &mut self.frame {
            corners.prepare(&self.queue, self.config.width, self.config.height);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cells"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear_color(self.clear, self.glass)),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(glass) = self.glass_layer.as_ref() {
            glass.draw(&mut pass);
        }
        pass.set_pipeline(&self.pipeline);
        if self.title_count > 0 {
            pass.set_bind_group(0, &self.title_bind, &[]);
            pass.draw(0..6, 0..self.title_count + 1);
        }
        if self.bar_count > 0 {
            pass.set_bind_group(0, &self.bar_bind, &[]);
            pass.draw(0..6, 0..self.bar_count + 1);
        }
        if count > 0 {
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..6, 0..count);
        }
        if show_terminal_overlays {
            if let Some(images) = &self.images {
                images.draw(&mut pass);
            }
            if self.scrollbar_visible {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.draw(0..6, count..count + 1);
            }
        }
        if let Some(glass) = self.glass_layer.as_ref() {
            glass.draw_overlay(&mut pass);
            pass.set_pipeline(&self.pipeline);
        }
        if let Some(dialog) = &self.dialog {
            pass.set_bind_group(0, &dialog.bind, &[]);
            pass.draw(0..6, 0..dialog.count);
        }
        if let Some(frame) = &self.frame {
            frame.draw(&mut pass);
        }
    }

    fn ensure_grid(&mut self, cols: u16, rows: u16) {
        let needed = usize::from(cols) * usize::from(rows);
        if needed <= self.cell_capacity && cols == self.cols && rows == self.rows {
            return;
        }
        if needed > self.cell_capacity {
            self.cell_capacity = needed.next_power_of_two();
            self.cell_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("cells"),
                size: (self.cell_capacity * std::mem::size_of::<GpuCell>()) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.rebuild_bind();
        }
        self.gpu_cells.resize(needed, GpuCell { slot: 0, fg: 0, bg: 0, flags: 0 });
        self.cols = cols;
        self.rows = rows;
    }

    fn write_uniforms(
        &self,
        frame: Presented,
        show_composer: bool,
        accent: Rgb,
        scrollbar: Range<u32>,
    ) -> Uniforms {
        let (cursor_col, cursor_row, cursor_on) = match frame.cursor {
            Some((col, row)) => (u32::from(col), u32::from(row), 1),
            None => (0, 0, 0),
        };
        let cell_w = self.cell_px_w.max(1.0);
        let cell_h = self.cell_px_h.max(1.0);
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        let uniforms = Uniforms {
            cols: u32::from(frame.cols),
            rows: u32::from(frame.rows),
            cursor_col,
            cursor_row,
            cursor_on,
            atlas_w: Atlas::SIZE,
            atlas_h: Atlas::SIZE,
            _reserved: 0,
            pad_x: self.left_w,
            pad_y: self.top_h + self.grid_pad_top,
            cell_w,
            cell_h,
            surf_w,
            surf_h,
            scroll_top: frame.scroll_top,
            scroll_h: frame.scroll_h,
            composer: frame.composer
                .filter(|_| show_composer)
                .map_or([0; 4], |bounds| bounds.map(u32::from)),
            composer_accent: accent.pack(),
            fill_bg: self.clear.pack(),
            scroll_left: scrollbar.start as f32,
            scroll_right: scrollbar.end as f32,
            sel_lo: self.selection.0,
            sel_hi: self.selection.1,
            scroll_y0: self.top_h,
            scroll_y1: (surf_h - self.chrome_h).max(self.top_h),
            glass: u32::from(self.glass.is_some()),
            glass_key: self.clear.pack(),
            _glass_pad: [0; 2],
        };
        self.queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
        uniforms
    }

    #[allow(clippy::too_many_arguments)]
    fn write_strip_uniforms(
        &self,
        buf: &wgpu::Buffer,
        cols: u32,
        pad_y: f32,
        strip_h: f32,
        strip: Rgb,
        edge: Rgb,
        transparent_bg: bool,
    ) {
        let cell_w = self.cell_px_w.max(1.0);
        let glyph_h = self.cell_px_h.max(1.0);
        let cell_h = strip_h.max(glyph_h);
        let uniforms = Uniforms {
            cols,
            rows: 1,
            cursor_col: 0,
            cursor_row: 0,
            cursor_on: 0,
            atlas_w: Atlas::SIZE,
            atlas_h: Atlas::SIZE,
            _reserved: glyph_h.round() as u32,
            pad_x: 0.0,
            pad_y,
            cell_w,
            cell_h,
            surf_w: self.config.width as f32,
            surf_h: self.config.height as f32,
            scroll_top: 0.0,
            scroll_h: 0.0,
            composer: [0; 4],
            composer_accent: strip.pack(),
            fill_bg: edge.pack(),
            scroll_left: 0.0,
            scroll_right: 0.0,
            sel_lo: 0,
            sel_hi: 0,
            scroll_y0: 0.0,
            scroll_y1: 0.0,
            glass: u32::from(transparent_bg),
            glass_key: strip.pack(),
            _glass_pad: [0; 2],
        };
        self.queue.write_buffer(buf, 0, bytemuck::bytes_of(&uniforms));
    }

    fn flush_atlas(&mut self, atlas: &mut Atlas) {
        for upload in atlas.take_uploads() {
            if upload.w == 0 || upload.h == 0 {
                continue;
            }
            let (data, bytes_per_row) = pad_rows(&upload.pixels, upload.w, upload.h);
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.x,
                        y: upload.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(upload.h),
                },
                wgpu::Extent3d {
                    width: upload.w,
                    height: upload.h,
                    depth_or_array_layers: 1,
                },
            );
        }
        if let Some(rects) = atlas.rects_if_dirty() {
            self.queue
                .write_buffer(&self.glyph_buf, 0, bytemuck::cast_slice(rects));
        }
    }

    fn clear_atlas(&self) {
        let bytes_per_row = Atlas::SIZE;
        let data = vec![0u8; (Atlas::SIZE as usize) * (Atlas::SIZE as usize)];
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(Atlas::SIZE),
            },
            wgpu::Extent3d {
                width: Atlas::SIZE,
                height: Atlas::SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    fn rebuild_bind(&mut self) {
        self.bind = make_bind(
            &self.device,
            &self.bind_layout,
            &self.uniform_buf,
            &self.cell_buf,
            &self.glyph_buf,
            &self.atlas_view,
            &self.agent_view,
            &self.sampler,
            &self.icon_sampler,
        );
    }
}

fn resolve(atlas: &mut Atlas, face: &Face, cell: &crate::cell::Cell) -> GpuCell {
    let slot = if cell.flags & crate::cell::SPACER != 0 {
        0
    } else if cell.flags & crate::cell::AGENT_ICON != 0 {
        cell.cp.saturating_sub(crate::agent::ICON_BASE).min(crate::agent::ICON_COUNT - 1)
    } else {
        atlas.prepare(face, cell.cp, cell.flags)
    };
    GpuCell {
        slot,
        fg: cell.fg,
        bg: cell.bg,
        flags: cell.flags,
    }
}

fn pad_rows(pixels: &[u8], width: u32, height: u32) -> (Vec<u8>, u32) {
    let bytes_per_row = (width as usize).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize) as u32;
    let mut data = vec![0u8; bytes_per_row as usize * height as usize];
    for y in 0..height as usize {
        let src = y * width as usize;
        let dst = y * bytes_per_row as usize;
        data[dst..dst + width as usize].copy_from_slice(&pixels[src..src + width as usize]);
    }
    (data, bytes_per_row)
}

fn buffer_entry(
    binding: u32,
    ty: wgpu::BufferBindingType,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn make_bind(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform_buf: &wgpu::Buffer,
    cell_buf: &wgpu::Buffer,
    glyph_buf: &wgpu::Buffer,
    atlas_view: &wgpu::TextureView,
    agent_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    icon_sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rustty-cells"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: cell_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: glyph_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(atlas_view),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(agent_view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(icon_sampler),
            },
        ],
    })
}

/// The window tint. Premultiplied, so a glass window stores the color scaled by its opacity.
fn clear_color(color: Rgb, glass: Option<f32>) -> wgpu::Color {
    let alpha = f64::from(glass.unwrap_or(1.0));
    wgpu::Color {
        r: f64::from(color.r) / 255.0 * alpha,
        g: f64::from(color.g) / 255.0 * alpha,
        b: f64::from(color.b) / 255.0 * alpha,
        a: alpha,
    }
}

fn present_mode(vsync: bool, modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    if !vsync {
        if modes.contains(&wgpu::PresentMode::AutoNoVsync) {
            return wgpu::PresentMode::AutoNoVsync;
        }
        if modes.contains(&wgpu::PresentMode::Immediate) {
            return wgpu::PresentMode::Immediate;
        }
        return wgpu::PresentMode::Fifo;
    }
    if modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

#[cfg(test)]
mod tests {
    use super::{clear_color, present_mode};
    use crate::color::Rgb;

    #[test]
    fn opaque_window_clears_as_before_and_glass_premultiplies() {
        let color = Rgb::new(0x13, 0x16, 0x19);
        let opaque = clear_color(color, None);
        assert_eq!(opaque.a, 1.0);
        assert!((opaque.r - 0x13 as f64 / 255.0).abs() < 1e-9);
        let glass = clear_color(color, Some(0.5));
        assert_eq!(glass.a, 0.5);
        assert!((glass.g - 0x16 as f64 / 255.0 * 0.5).abs() < 1e-9);
    }

    #[test]
    fn mailbox_keeps_vsync_without_queueing_frames() {
        use wgpu::PresentMode::{AutoNoVsync, Fifo, Mailbox};
        assert_eq!(present_mode(true, &[Fifo, Mailbox]), Mailbox);
        assert_eq!(present_mode(true, &[Fifo]), Fifo);
        assert_eq!(present_mode(false, &[Fifo, Mailbox, AutoNoVsync]), AutoNoVsync);
    }

    #[test]
    fn tab_shader_is_valid_wgsl() {
        let module = wgpu::naga::front::wgsl::parse_str(include_str!("shader.wgsl")).expect("shader");
        let mut validator = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("valid shader");
    }
}
