use std::collections::VecDeque;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

use crate::cell::Uniforms;
use crate::graphics::{Image, MAX_PLACEMENTS, Placement};

const VERTICES_PER_IMAGE: usize = 6;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
}

struct Sprite {
    image: Arc<Image>,
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    width: u32,
    height: u32,
}

pub(super) struct Images {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    vertex_buffer: wgpu::Buffer,
    sprites: Vec<Sprite>,
    /// Textures whose last submit is old enough to overwrite.
    free: Vec<Sprite>,
    /// The previous two submits may still sample these.
    retired: VecDeque<Vec<Sprite>>,
}

impl Images {
    pub(super) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terminal images"),
            source: wgpu::ShaderSource::Wgsl(include_str!("images.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terminal images"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terminal images"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terminal images"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                })],
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
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terminal images"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terminal image vertices"),
            size: (MAX_PLACEMENTS * VERTICES_PER_IMAGE * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            sampler,
            vertex_buffer,
            sprites: Vec::new(),
            free: Vec::new(),
            retired: VecDeque::new(),
        }
    }

    pub(super) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid: &Uniforms,
        placements: &[Placement],
    ) {
        while self.retired.len() > 2 {
            self.free.extend(self.retired.pop_front().unwrap());
        }
        if placements.is_empty() {
            if !self.sprites.is_empty() {
                self.retired.push_back(std::mem::take(&mut self.sprites));
            }
            return;
        }
        let mut previous = std::mem::take(&mut self.sprites);
        let mut vertices = Vec::with_capacity(placements.len() * VERTICES_PER_IMAGE);
        for placement in placements {
            let Some(quad) = image_vertices(grid, placement) else {
                continue;
            };
            vertices.extend_from_slice(&quad);
            if let Some(index) = previous
                .iter()
                .position(|sprite| Arc::ptr_eq(&sprite.image, &placement.image))
            {
                self.sprites.push(previous.swap_remove(index));
                continue;
            }
            let image = &placement.image;
            let sprite = if let Some(index) = self
                .free
                .iter()
                .position(|sprite| sprite.width == image.width && sprite.height == image.height)
            {
                let mut sprite = self.free.swap_remove(index);
                write_pixels(queue, &sprite.texture, image);
                sprite.image = Arc::clone(image);
                sprite
            } else {
                self.texture(device, queue, image)
            };
            self.sprites.push(sprite);
        }
        if !previous.is_empty() {
            self.retired.push_back(previous);
        }
        if !vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        }
    }

    fn texture(&self, device: &wgpu::Device, queue: &wgpu::Queue, image: &Arc<Image>) -> Sprite {
        let size = wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("terminal image"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_pixels(queue, &texture, image);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terminal image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Sprite {
            image: Arc::clone(image),
            texture,
            bind,
            width: image.width,
            height: image.height,
        }
    }

    pub(super) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.sprites.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        for (index, sprite) in self.sprites.iter().enumerate() {
            let start = (index * VERTICES_PER_IMAGE) as u32;
            pass.set_bind_group(0, &sprite.bind, &[]);
            pass.draw(start..start + VERTICES_PER_IMAGE as u32, 0..1);
        }
    }
}

fn write_pixels(queue: &wgpu::Queue, texture: &wgpu::Texture, image: &Image) {
    queue.write_texture(
        texture.as_image_copy(),
        &image.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(image.width * 4),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: image.width,
            height: image.height,
            depth_or_array_layers: 1,
        },
    );
}

fn image_vertices(grid: &Uniforms, placement: &Placement) -> Option<[Vertex; VERTICES_PER_IMAGE]> {
    let box_w = f32::from(placement.cols) * grid.cell_w;
    let box_h = f32::from(placement.rows) * grid.cell_h;
    let source_w = placement.image.width as f32;
    let source_h = placement.image.height as f32;
    let scale = (box_w / source_w).min(box_h / source_h);
    let width = source_w * scale;
    let height = source_h * scale;
    let x = grid.pad_x + f32::from(placement.col) * grid.cell_w + (box_w - width) * 0.5;
    let y = grid.pad_y + placement.row as f32 * grid.cell_h + (box_h - height) * 0.5;
    let start_row = placement.visible_rows.start.max(0) as f32;
    let end_row = placement.visible_rows.end.min(grid.rows as i32) as f32;
    let left = x.max(grid.pad_x);
    let top = y.max(grid.pad_y + start_row * grid.cell_h);
    let right = (x + width)
        .min(grid.pad_x + grid.cols as f32 * grid.cell_w)
        .min(grid.surf_w);
    let bottom = (y + height)
        .min(grid.pad_y + end_row * grid.cell_h)
        .min(grid.surf_h);
    if left >= right || top >= bottom {
        return None;
    }
    let vertex = |px, py| Vertex {
        position: [px / grid.surf_w * 2.0 - 1.0, 1.0 - py / grid.surf_h * 2.0],
        uv: [(px - x) / width, (py - y) / height],
    };
    Some([
        vertex(left, top),
        vertex(right, top),
        vertex(left, bottom),
        vertex(left, bottom),
        vertex(right, top),
        vertex(right, bottom),
    ])
}
