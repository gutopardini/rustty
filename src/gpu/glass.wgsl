// Glass shapes for glass mode: the tab capsule, the selected tab and the round buttons.
// Each shape is a rounded rectangle drawn in one quad, grown by the shadow margin.
// The output is premultiplied: a soft drop shadow, a frosted body that is lighter at the
// top, a brighter band along the inside of the edge, and a specular rim lit from below
// and from the top left.

struct Look {
    size: vec2<f32>,
    scale: f32,
    light: f32,
}

struct Shape {
    rect: vec4<f32>,
    radius: f32,
    kind: u32,
    lit: f32,
    /// Symbol in the middle of a button: 0 none, 1 gear, 2 close, 3 minimize,
    /// 4 maximize, 5 restore.
    icon: f32,
    /// Color of a tinted button or sheet, straight alpha. Zero alpha keeps the frosted white.
    tint: vec4<f32>,
}

@group(0) @binding(0) var<uniform> look: Look;
@group(0) @binding(1) var<storage, read> shapes: array<Shape>;

const CONTAINER: u32 = 0u;
const PILL: u32 = 1u;
const BUTTON: u32 = 2u;
const SHEET: u32 = 3u;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) inst: u32,
}

fn margin() -> f32 {
    return 10.0 * look.scale;
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) inst: u32) -> VsOut {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let shape = shapes[inst];
    let m = margin();
    let origin = shape.rect.xy - vec2<f32>(m);
    let extent = shape.rect.zw + vec2<f32>(2.0 * m);
    let p = origin + corners[vi] * extent;
    let ndc = vec2<f32>(p.x / max(look.size.x, 1.0) * 2.0 - 1.0, 1.0 - p.y / max(look.size.y, 1.0) * 2.0);
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.inst = inst;
    return out;
}

// Signed distance to the rounded rectangle, negative inside.
fn sd_round(p: vec2<f32>, center: vec2<f32>, half: vec2<f32>, r: f32) -> f32 {
    let q = abs(p - center) - (half - vec2<f32>(r));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// Outward direction of the nearest point of the outline.
fn outline_normal(p: vec2<f32>, center: vec2<f32>, half: vec2<f32>, r: f32) -> vec2<f32> {
    let rel = p - center;
    let q = abs(rel) - (half - vec2<f32>(r));
    let side = select(vec2<f32>(-1.0), vec2<f32>(1.0), rel >= vec2<f32>(0.0));
    if (q.x > 0.0 && q.y > 0.0) {
        return normalize(q) * side;
    }
    if (q.x > q.y) {
        return vec2<f32>(side.x, 0.0);
    }
    return vec2<f32>(0.0, side.y);
}

fn over(top: vec4<f32>, under: vec4<f32>) -> vec4<f32> {
    return top + under * (1.0 - top.a);
}

// Outline of a gear, as a signed distance: teeth around a ring with a hole in the middle.
fn gear_distance(p: vec2<f32>, center: vec2<f32>, radius: f32) -> f32 {
    let v = p - center;
    let r = length(v);
    let angle = atan2(v.y, v.x);
    let tooth = clamp((cos(8.0 * angle) + 0.1) * 2.4, 0.0, 1.0);
    let outer = r - radius * mix(0.74, 1.0, tooth);
    let hole = radius * 0.34 - r;
    return max(outer, hole);
}

fn segment_distance(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h);
}

fn box_distance(p: vec2<f32>, center: vec2<f32>, half: vec2<f32>) -> f32 {
    let q = abs(p - center) - half;
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
}

// The window controls as strokes, all with one weight, sized on the button.
fn control_distance(icon: u32, p: vec2<f32>, c: vec2<f32>, side: f32) -> f32 {
    let w = 0.085 * side;
    let a = 0.19 * side;
    switch icon {
        case 2u: {
            let x = 0.85 * a;
            let one = segment_distance(p, c - vec2<f32>(x, x), c + vec2<f32>(x, x));
            let two = segment_distance(p, c + vec2<f32>(-x, x), c + vec2<f32>(x, -x));
            return min(one, two) - 0.5 * w;
        }
        case 3u: {
            return segment_distance(p, c - vec2<f32>(a, 0.0), c + vec2<f32>(a, 0.0)) - 0.5 * w;
        }
        case 4u: {
            return abs(box_distance(p, c, vec2<f32>(0.85 * a))) - 0.5 * w;
        }
        default: {
            let o = 0.09 * side;
            let h = vec2<f32>(0.62 * a);
            let front = box_distance(p, c + vec2<f32>(-o, o), h);
            let back = abs(box_distance(p, c + vec2<f32>(o, -o), h)) - 0.5 * w;
            return min(abs(front) - 0.5 * w, max(back, -front));
        }
    }
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let shape = shapes[in.inst];
    let s = look.scale;
    let p = in.pos.xy;
    let half = shape.rect.zw * 0.5;
    let center = shape.rect.xy + half;
    let r = min(shape.radius, min(half.x, half.y));
    let d = sd_round(p, center, half, r);
    let cover = clamp(0.5 - d, 0.0, 1.0);
    let n = outline_normal(p, center, half, r);
    let light = look.light > 0.5;
    // Measured before any branch: derivatives need every pixel of the quad.
    let g = gear_distance(p, center, min(half.x, half.y) * 0.50);
    let gear_cover = clamp(0.5 - g / max(fwidth(g), 0.001), 0.0, 1.0);
    // 0 at the top edge of the shape, 1 at the bottom.
    let t = clamp((p.y - shape.rect.y) / max(shape.rect.w, 1.0), 0.0, 1.0);
    let depth = -d;

    if (shape.kind == PILL) {
        // The selected tab is sunk into the glass: darker than the capsule, shaded at the
        // top where the rim casts on it, and a faint light along its lower edge.
        var tint = select(vec3<f32>(0.0), vec3<f32>(1.0), light);
        var body_a = select(mix(0.42, 0.26, t), mix(0.62, 0.78, t), light);
        let soft = clamp(0.5 - d / (1.2 * s), 0.0, 1.0);
        var color = vec4<f32>(tint * body_a, body_a) * soft;
        let inner_top = (1.0 - smoothstep(0.0, 6.0 * s, depth)) * max(-n.y, 0.0) * select(0.30, 0.10, light);
        color = over(vec4<f32>(0.0, 0.0, 0.0, inner_top * soft), color);
        return color;
    }

    if (shape.kind == SHEET) {
        // A dialog: a wide soft shadow and a tinted body, a little clearer toward the bottom.
        let spread = 16.0 * s;
        let d_drop = sd_round(p - vec2<f32>(0.0, 6.0 * s), center, half, r);
        let drop = (1.0 - smoothstep(-0.3 * spread, spread, d_drop)) * select(0.40, 0.18, light);
        var sheet = vec4<f32>(0.0, 0.0, 0.0, drop * (1.0 - cover));
        let body = vec4<f32>(shape.tint.rgb * shape.tint.a, shape.tint.a) * cover;
        sheet = over(body, sheet);
        let frost = mix(select(0.06, 0.10, light), select(0.015, 0.03, light), t) * cover;
        return over(vec4<f32>(frost), sheet);
    }

    // Drop shadow under the capsule and the buttons.
    let blur = 7.0 * s;
    let d_shadow = sd_round(p - vec2<f32>(0.0, 3.0 * s), center, half, r);
    let soft_shadow = (1.0 - smoothstep(-0.4 * blur, blur, d_shadow)) * select(0.28, 0.12, light);
    var color = vec4<f32>(0.0, 0.0, 0.0, soft_shadow * (1.0 - cover));

    // Body: frosted, darker at the top and clearer toward the bottom, as glass lit from below.
    var body = select(mix(0.035, 0.11, t), mix(0.22, 0.40, t), light);
    body = body + shape.lit * select(0.06, 0.10, light);
    color = over(vec4<f32>(body * cover), color);

    // Inner shadow under the top rim, so the capsule reads as a groove.
    let inner_top = (1.0 - smoothstep(0.0, 9.0 * s, depth)) * pow(max(-n.y, 0.0), 1.5) * select(0.26, 0.08, light);
    color = over(vec4<f32>(0.0, 0.0, 0.0, inner_top * cover), color);

    // A tinted button, such as the one that closes the tabs, takes its color under the frost.
    if (shape.tint.a > 0.0) {
        let tint = vec4<f32>(shape.tint.rgb * shape.tint.a, shape.tint.a) * cover;
        color = over(color, tint);
    }

    let icon = u32(shape.icon + 0.5);
    if (icon != 0u) {
        var mark = gear_cover;
        if (icon >= 2u) {
            let side = 2.0 * min(half.x, half.y);
            mark = clamp(0.5 - control_distance(icon, p, center, side), 0.0, 1.0);
        }
        let ink = select(0.86, 0.22, light) + shape.lit * 0.14;
        let a = mark * select(0.9, 0.85, light);
        color = over(vec4<f32>(vec3<f32>(ink) * a, a), color);
    }
    return color;
}
