//! Rounded window corners with the Adwaita inner outline.
//!
//! The surface is premultiplied. The mask multiplies what is already drawn by the
//! coverage of the rounded outline, so only the corners are touched.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct FrameUniforms {
    size: [f32; 2],
    radius: f32,
    edge: f32,
}

pub struct Frame {
    mask: wgpu::RenderPipeline,
    edge: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
    written: Option<FrameUniforms>,
    radius: f32,
    edge_alpha: f32,
}

/// Opacity of the 1px outline inside the window. Zero: the window has no border, only
/// its rounded corners.
const EDGE_ALPHA: f32 = 0.0;
const GLASS_EDGE_ALPHA: f32 = 0.0;

impl Frame {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("window frame"),
            source: wgpu::ShaderSource::Wgsl(include_str!("frame.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("window frame"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("window frame"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
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
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // The frame keeps only the covered part of each pixel, color and alpha alike.
        let scale = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::SrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let mask = pipeline("fs_mask", wgpu::BlendState { color: scale, alpha: scale });
        // The outline lightens the color. An opaque window stays opaque, and a glass
        // window gains the outline's own alpha, so the color stays premultiplied.
        let edge = pipeline(
            "fs_edge",
            wgpu::BlendState { color: wgpu::BlendComponent::OVER, alpha: wgpu::BlendComponent::OVER },
        );
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("window frame"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("window frame"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }],
        });
        Self { mask, edge, uniforms, bind, written: None, radius: 0.0, edge_alpha: EDGE_ALPHA }
    }

    /// Corner radius in pixels. Zero draws square corners and no outline, as for a maximized window.
    pub fn set_radius(&mut self, radius: f32) {
        self.radius = radius.max(0.0);
    }

    pub fn set_glass(&mut self, glass: bool) {
        self.edge_alpha = if glass { GLASS_EDGE_ALPHA } else { EDGE_ALPHA };
    }

    pub fn prepare(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        let size = [width as f32, height as f32];
        let radius = self.radius.min(size[0] * 0.5).min(size[1] * 0.5).floor();
        let uniforms = FrameUniforms { size, radius, edge: self.edge_alpha };
        if self.written != Some(uniforms) {
            queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&uniforms));
            self.written = Some(uniforms);
        }
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.written.is_none_or(|uniforms| uniforms.radius < 1.0) {
            return;
        }
        pass.set_bind_group(0, &self.bind, &[]);
        if self.edge_alpha > 0.0 {
            pass.set_pipeline(&self.edge);
            pass.draw(0..6, 0..8);
        }
        pass.set_pipeline(&self.mask);
        pass.draw(0..6, 0..4);
    }
}
