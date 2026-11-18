// Window corners, drawn after everything else. The mask scales the frame by the
// coverage of a rounded rectangle, so the corners become transparent. The edge
// draws the 1px inner outline of an Adwaita window.

struct Frame {
    size: vec2<f32>,
    radius: f32,
    edge: f32,
}

@group(0) @binding(0) var<uniform> f: Frame;

// Instances 0..4 are the corners, a radius-sized square each.
// Instances 4..8 are the 1px strips along the edges between them.
fn quad(inst: u32) -> vec4<f32> {
    let w = f.size.x;
    let h = f.size.y;
    let r = f.radius;
    switch inst {
        case 0u: { return vec4<f32>(0.0, 0.0, r, r); }
        case 1u: { return vec4<f32>(w - r, 0.0, r, r); }
        case 2u: { return vec4<f32>(0.0, h - r, r, r); }
        case 3u: { return vec4<f32>(w - r, h - r, r, r); }
        case 4u: { return vec4<f32>(r, 0.0, w - 2.0 * r, 1.0); }
        case 5u: { return vec4<f32>(r, h - 1.0, w - 2.0 * r, 1.0); }
        case 6u: { return vec4<f32>(0.0, r, 1.0, h - 2.0 * r); }
        default: { return vec4<f32>(w - 1.0, r, 1.0, h - 2.0 * r); }
    }
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) inst: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let rect = quad(inst);
    let p = rect.xy + corners[vi] * max(rect.zw, vec2<f32>(0.0));
    let ndc = vec2<f32>(p.x / max(f.size.x, 1.0) * 2.0 - 1.0, 1.0 - p.y / max(f.size.y, 1.0) * 2.0);
    return vec4<f32>(ndc, 0.0, 1.0);
}

// Signed distance to the window's rounded outline. Negative inside.
fn outline_distance(p: vec2<f32>) -> f32 {
    let half = f.size * 0.5;
    let q = abs(p - half) - (half - vec2<f32>(f.radius));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - f.radius;
}

@fragment
fn fs_mask(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let cover = clamp(0.5 - outline_distance(pos.xy), 0.0, 1.0);
    return vec4<f32>(0.0, 0.0, 0.0, cover);
}

@fragment
fn fs_edge(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let d = outline_distance(pos.xy);
    let band = clamp(0.5 - d, 0.0, 1.0) - clamp(-0.5 - d, 0.0, 1.0);
    let a = f.edge * band;
    return vec4<f32>(a, a, a, a);
}
