//! Grade instanciada na GPU. Cada célula é um quad; o atlas de glifos fica em VRAM.

use std::sync::Arc;

use anyhow::Context;
use winit::dpi::PhysicalSize;
use winit::window::Window;

use crate::cell::{GlyphRect, GpuCell, Uniforms};
use crate::color::DEFAULT_BG;
use crate::font::{Atlas, Face};
use crate::term::Snapshot;

const MAX_GLYPHS: usize = 8192;

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    bind: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    cell_buf: wgpu::Buffer,
    cell_capacity: usize,
    glyph_buf: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    gpu_cells: Vec<GpuCell>,
    cols: u16,
    rows: u16,
    cell_px_w: f32,
    cell_px_h: f32,
    seen_generation: u64,
}

impl Renderer {
    pub fn new(window: Arc<Window>, vsync: bool) -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(window.clone())
            .context("criando surface Vulkan")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .context("nenhum adapter Vulkan")?;
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
        .context("abrindo device Vulkan")?;

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
        let present_mode = if vsync {
            wgpu::PresentMode::Fifo
        } else {
            wgpu::PresentMode::AutoNoVsync
        };
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

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
                    blend: Some(wgpu::BlendState::REPLACE),
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

        let bind = make_bind(
            &device,
            &bind_layout,
            &uniform_buf,
            &cell_buf,
            &glyph_buf,
            &atlas_view,
            &sampler,
        );
        let renderer = Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_layout,
            bind,
            uniform_buf,
            cell_buf,
            cell_capacity,
            glyph_buf,
            atlas_tex,
            atlas_view,
            sampler,
            gpu_cells: Vec::new(),
            cols: 0,
            rows: 0,
            cell_px_w: 1.0,
            cell_px_h: 1.0,
            seen_generation: 0,
        };
        renderer.clear_atlas();
        Ok(renderer)
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn upload(&mut self, atlas: &mut Atlas, face: &Face, snap: &Snapshot) {
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
        self.write_uniforms(snap);
    }

    pub fn render(&mut self) -> anyhow::Result<()> {
        let count = u32::from(self.cols) * u32::from(self.rows);
        if count == 0 {
            return Ok(());
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                anyhow::bail!("swapchain rejeitou o frame");
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rustty"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(DEFAULT_BG.r) / 255.0,
                            g: f64::from(DEFAULT_BG.g) / 255.0,
                            b: f64::from(DEFAULT_BG.b) / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.draw(0..6, 0..count);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
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

    fn write_uniforms(&self, snap: &Snapshot) {
        let (cursor_col, cursor_row, cursor_on) = match snap.cursor {
            Some((col, row)) => (u32::from(col), u32::from(row), 1),
            None => (0, 0, 0),
        };
        let cell_w = self.cell_px_w.max(1.0);
        let cell_h = self.cell_px_h.max(1.0);
        let surf_w = self.config.width as f32;
        let surf_h = self.config.height as f32;
        let grid_w = f32::from(snap.cols) * cell_w;
        let grid_h = f32::from(snap.rows) * cell_h;
        let uniforms = Uniforms {
            cols: u32::from(snap.cols),
            rows: u32::from(snap.rows),
            cursor_col,
            cursor_row,
            cursor_on,
            atlas_w: Atlas::SIZE,
            atlas_h: Atlas::SIZE,
            _reserved: 0,
            pad_x: ((surf_w - grid_w) * 0.5).max(0.0),
            pad_y: ((surf_h - grid_h) * 0.5).max(0.0),
            cell_w,
            cell_h,
            surf_w,
            surf_h,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        self.queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&uniforms));
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
            &self.sampler,
        );
    }
}

fn resolve(atlas: &mut Atlas, face: &Face, cell: &crate::cell::Cell) -> GpuCell {
    let slot = if cell.flags & crate::cell::SPACER != 0 {
        0
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
    sampler: &wgpu::Sampler,
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
        ],
    })
}
