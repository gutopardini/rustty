//! Glass shapes drawn under the bar text in glass mode.

use bytemuck::{Pod, Zeroable};

use crate::settings::GlassTune;

/// The capsule that holds the tabs.
pub const CONTAINER: u32 = 0;
/// The selected tab, inside the capsule.
pub const PILL: u32 = 1;
/// A round button, such as the settings gear or a window control.
pub const BUTTON: u32 = 2;
/// A dialog body: the settings panel or the close confirmation.
pub const SHEET: u32 = 3;

pub const ICON_NONE: f32 = 0.0;
pub const ICON_GEAR: f32 = 1.0;
pub const ICON_CLOSE: f32 = 2.0;
pub const ICON_MINIMIZE: f32 = 3.0;
pub const ICON_MAXIMIZE: f32 = 4.0;
pub const ICON_RESTORE: f32 = 5.0;

const MAX_SHAPES: usize = 16;

/// A rounded rectangle in physical pixels.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct GlassShape {
    pub rect: [f32; 4],
    pub radius: f32,
    pub kind: u32,
    /// 0 to 1, extra light for a hovered or open control.
    pub lit: f32,
    /// Symbol in the middle of a button, one of the `ICON_*` values.
    pub icon: f32,
    /// Color of a tinted button or sheet, straight alpha. Zero alpha keeps the frosted white.
    pub tint: [f32; 4],
}

impl GlassShape {
    pub fn new(x: f32, y: f32, w: f32, h: f32, kind: u32) -> Self {
        Self { rect: [x, y, w.max(0.0), h.max(0.0)], radius: h.min(w).max(0.0) * 0.5, kind, lit: 0.0, icon: ICON_NONE, tint: [0.0; 4] }
    }

    pub fn with_radius(mut self, radius: f32) -> Self {
        self.radius = radius.min(self.rect[2] * 0.5).min(self.rect[3] * 0.5).max(0.0);
        self
    }

    pub fn with_tint(mut self, color: crate::color::Rgb, alpha: f32) -> Self {
        let channel = |value: u8| f32::from(value) / 255.0;
        self.tint = [channel(color.r), channel(color.g), channel(color.b), alpha.clamp(0.0, 1.0)];
        self
    }

    pub fn with_icon(mut self, icon: f32) -> Self {
        self.icon = icon;
        self
    }

    pub fn with_gear(mut self) -> Self {
        self.icon = ICON_GEAR;
        self
    }

    pub fn lit(mut self, lit: bool) -> Self {
        self.lit = if lit { 1.0 } else { 0.0 };
        self
    }
}

/// Matches `Look` in `glass.wgsl`. Distances stay in logical pixels; the shader scales them.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
struct Look {
    size: [f32; 2],
    scale: f32,
    light: f32,
    body_top: f32,
    body_bottom: f32,
    lit_boost: f32,
    shadow: f32,
    shadow_blur: f32,
    shadow_y: f32,
    pill_top: f32,
    pill_bottom: f32,
    pill_rim: f32,
    pill_rim_size: f32,
    pill_soft: f32,
    groove: f32,
    groove_strength: f32,
    sheet_shadow: f32,
    sheet_spread: f32,
    sheet_y: f32,
    frost_top: f32,
    frost_bottom: f32,
    ink: f32,
    ink_alpha: f32,
    ink_lit: f32,
    margin: f32,
    pad0: f32,
    pad1: f32,
}

fn look(width: u32, height: u32, scale: f32, light: bool, tune: &GlassTune) -> Look {
    Look {
        size: [width as f32, height as f32],
        scale: scale.max(0.5),
        light: if light { 1.0 } else { 0.0 },
        body_top: tune.body_top.pick(light),
        body_bottom: tune.body_bottom.pick(light),
        lit_boost: tune.lit.pick(light),
        shadow: tune.shadow.pick(light),
        shadow_blur: tune.shadow_blur,
        shadow_y: tune.shadow_y,
        pill_top: tune.pill_top.pick(light),
        pill_bottom: tune.pill_bottom.pick(light),
        pill_rim: tune.pill_rim.pick(light),
        pill_rim_size: tune.pill_rim_size,
        pill_soft: tune.pill_soft,
        groove: tune.groove,
        groove_strength: tune.groove_strength.pick(light),
        sheet_shadow: tune.sheet_shadow.pick(light),
        sheet_spread: tune.sheet_spread,
        sheet_y: tune.sheet_y,
        frost_top: tune.frost_top.pick(light),
        frost_bottom: tune.frost_bottom.pick(light),
        ink: tune.ink.pick(light),
        ink_alpha: tune.ink_alpha.pick(light),
        ink_lit: tune.ink_lit,
        margin: tune.margin,
        pad0: 0.0,
        pad1: 0.0,
    }
}

pub struct Glass {
    pipeline: wgpu::RenderPipeline,
    look: wgpu::Buffer,
    shapes: wgpu::Buffer,
    bind: wgpu::BindGroup,
    count: u32,
    /// Shapes from here on are drawn over the grid, under a dialog.
    overlay: u32,
    written: Option<(Look, Vec<GlassShape>, usize)>,
}

impl Glass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("glass"),
            source: wgpu::ShaderSource::Wgsl(include_str!("glass.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glass"),
            entries: &[
                entry(0, wgpu::BufferBindingType::Uniform),
                entry(1, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glass"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("glass"),
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
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let look = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass look"),
            size: std::mem::size_of::<Look>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shapes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("glass shapes"),
            size: (MAX_SHAPES * std::mem::size_of::<GlassShape>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glass"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: look.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: shapes.as_entire_binding() },
            ],
        });
        Self { pipeline, look, shapes, bind, count: 0, overlay: 0, written: None }
    }

    /// Writes only when the shapes or the surface changed.
    /// `overlay` is the index of the first shape drawn over the grid.
    pub fn set(
        &mut self,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
        scale: f32,
        light: bool,
        tune: &GlassTune,
        shapes: &[GlassShape],
        overlay: usize,
    ) {
        let shapes = &shapes[..shapes.len().min(MAX_SHAPES)];
        let overlay = overlay.min(shapes.len());
        let look = look(width, height, scale, light, tune);
        if self
            .written
            .as_ref()
            .is_some_and(|(old, old_shapes, old_overlay)| *old == look && old_shapes == shapes && *old_overlay == overlay)
        {
            return;
        }
        queue.write_buffer(&self.look, 0, bytemuck::bytes_of(&look));
        if !shapes.is_empty() {
            queue.write_buffer(&self.shapes, 0, bytemuck::cast_slice(shapes));
        }
        self.count = shapes.len() as u32;
        self.overlay = overlay as u32;
        self.written = Some((look, shapes.to_vec(), overlay));
    }

    /// The shapes under the bars and the grid.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_range(pass, 0..self.overlay);
    }

    /// The shapes over the grid, under the dialog text.
    pub fn draw_overlay(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_range(pass, self.overlay..self.count);
    }

    fn draw_range(&self, pass: &mut wgpu::RenderPass<'_>, range: std::ops::Range<u32>) {
        if range.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        pass.draw(0..6, range);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn glass_shader_is_valid_wgsl() {
        let module = wgpu::naga::front::wgsl::parse_str(include_str!("glass.wgsl")).expect("shader");
        let mut validator = wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::all(),
        );
        validator.validate(&module).expect("valid shader");
    }

    #[test]
    fn shape_is_a_capsule_by_default() {
        let shape = super::GlassShape::new(0.0, 0.0, 120.0, 30.0, super::CONTAINER);
        assert_eq!(shape.radius, 15.0);
        assert_eq!(std::mem::size_of::<super::GlassShape>(), 48);
    }

    #[test]
    fn default_tune_keeps_the_dark_glass_that_was_in_the_shader() {
        let look = super::look(800, 600, 1.0, false, &crate::settings::GlassTune::default());
        assert_eq!(std::mem::size_of::<super::Look>() % 16, 0);
        assert!((look.body_top - 0.035).abs() < 1e-6);
        assert!((look.body_bottom - 0.11).abs() < 1e-6);
        assert!((look.shadow - 0.28).abs() < 1e-6);
        assert!((look.shadow_blur - 7.0).abs() < 1e-6);
        assert!((look.pill_top - 0.42).abs() < 1e-6);
        assert!((look.pill_bottom - 0.26).abs() < 1e-6);
        assert!((look.margin - 10.0).abs() < 1e-6);
        assert_eq!(look.light, 0.0);
        let day = super::look(800, 600, 2.0, true, &crate::settings::GlassTune::default());
        assert!((day.body_top - 0.22).abs() < 1e-6);
        assert!((day.ink - 0.22).abs() < 1e-6);
        assert!((day.scale - 2.0).abs() < 1e-6);
    }
}
