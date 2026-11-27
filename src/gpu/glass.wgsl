// Glass shapes for glass mode: the tab capsule, the selected tab and the round buttons.
// Each shape is a rounded rectangle drawn in one quad, grown by the shadow margin.
// The output is premultiplied: a soft drop shadow, a frosted body that is lighter at the
// top, a brighter band along the inside of the edge, and a specular rim lit from below
// and from the top left.

// Amounts come from ui.conf. Distances are logical pixels. A tone is already chosen
// for this theme: dark or light.
struct Look {
    size: vec2<f32>,
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
const DIVIDER: u32 = 4u;
const LIMITS_ICON: u32 = 5u;
const LIMITS_CARD: u32 = 6u;
const BAR: u32 = 7u;
const DOT: u32 = 8u;
const TAB_STRIP: u32 = 9u;
const TAB_HOVER: u32 = 10u;
const TOOLTIP: u32 = 11u;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) @interpolate(flat) inst: u32,
}

fn margin() -> f32 {
    return look.margin * look.scale;
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

fn ndot(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return a.x * b.x - a.y * b.y;
}

// Exact Euclidean signed distance to a symmetric diamond with half-axes `b`.
fn sd_rhombus(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let q = abs(p);
    let h = clamp(ndot(b - 2.0 * q, b) / max(dot(b, b), 0.0001), -1.0, 1.0);
    let d = length(q - 0.5 * b * vec2<f32>(1.0 - h, 1.0 + h));
    return d * sign(q.x * b.y + q.y * b.x - b.x * b.y);
}

// Three stacked isometric diamonds colored by quota health (6 = ok, 7 = warn, 8 = crit).
fn layers_icon(p: vec2<f32>, center: vec2<f32>, side: f32, icon: u32, light: bool) -> vec4<f32> {
    let rx = 0.35 * side;
    let ry = 0.165 * side;
    let rr = max(0.025 * side, 0.5);
    let step = 0.155 * side;
    let gap = max(0.035 * side, 0.9);
    let c0 = center + vec2<f32>(0.0, -0.165 * side);
    let c1 = c0 + vec2<f32>(0.0, step);
    let c2 = c0 + vec2<f32>(0.0, 2.0 * step);

    let d0 = sd_rhombus(p - c0, vec2<f32>(rx, ry)) - rr;
    let d1_raw = sd_rhombus(p - c1, vec2<f32>(rx, ry)) - rr;
    let d2_raw = sd_rhombus(p - c2, vec2<f32>(rx, ry)) - rr;
    let d1 = max(d1_raw, -(d0 - gap));
    let d2 = max(d2_raw, -(d1_raw - gap));

    let a0 = clamp(0.5 - d0, 0.0, 1.0);
    let a1 = clamp(0.5 - d1, 0.0, 1.0);
    let a2 = clamp(0.5 - d2, 0.0, 1.0);

    let gx = clamp((p.x - (center.x - rx)) / max(2.0 * rx, 1.0), 0.0, 1.0);
    var col0: vec3<f32>;
    var col1: vec3<f32>;
    var col2: vec3<f32>;
    if (icon == 8u) {
        col0 = mix(vec3<f32>(1.00, 0.40, 0.44), vec3<f32>(0.96, 0.24, 0.46), gx);
        col1 = mix(vec3<f32>(0.99, 0.58, 0.28), vec3<f32>(0.94, 0.36, 0.38), gx);
        col2 = mix(vec3<f32>(0.82, 0.36, 0.76), vec3<f32>(0.64, 0.40, 0.92), gx);
    } else if (icon == 7u) {
        col0 = mix(vec3<f32>(1.00, 0.82, 0.30), vec3<f32>(0.98, 0.60, 0.24), gx);
        col1 = mix(vec3<f32>(0.94, 0.66, 0.48), vec3<f32>(0.86, 0.48, 0.36), gx);
        col2 = mix(vec3<f32>(0.60, 0.54, 0.96), vec3<f32>(0.76, 0.62, 0.96), gx);
    } else {
        col0 = mix(vec3<f32>(0.08, 0.96, 0.84), vec3<f32>(0.10, 0.80, 1.00), gx);
        col1 = mix(vec3<f32>(0.18, 0.74, 0.98), vec3<f32>(0.38, 0.58, 0.98), gx);
        col2 = mix(vec3<f32>(0.56, 0.54, 0.98), vec3<f32>(0.76, 0.68, 0.98), gx);
    }
    if (light) {
        col0 = col0 * 0.84;
        col1 = col1 * 0.84;
        col2 = col2 * 0.84;
    }

    var out = vec4<f32>(col2 * a2, a2);
    out = over(vec4<f32>(col1 * a1, a1), out);
    out = over(vec4<f32>(col0 * a0, a0), out);
    return out;
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
    let g_flat = gear_distance(p, center, min(half.x, half.y) * 0.68);
    let gear_flat_cover = clamp(0.5 - g_flat / max(fwidth(g_flat), 0.001), 0.0, 1.0);
    // 0 at the top edge of the shape, 1 at the bottom.
    let t = clamp((p.y - shape.rect.y) / max(shape.rect.w, 1.0), 0.0, 1.0);
    let depth = -d;

    if (shape.kind == TAB_STRIP) {
        let y_top = shape.rect.y;
        let y_end = shape.rect.y + shape.rect.w;
        if (p.y < y_top - 0.5 || p.y > y_end + 0.5 || p.x < shape.rect.x || p.x > shape.rect.x + shape.rect.z) {
            return vec4<f32>(0.0);
        }
        let bottom_pad = round(4.0 * s);
        let y_bot = max(y_end - bottom_pad, y_top + 2.0);
        let x0 = shape.icon;
        let x1 = shape.lit;
        var d_active = p.y - y_top;
        if (x1 > x0 + 2.0) {
            let rad = min(shape.radius, min((x1 - x0) * 0.35, (y_bot - y_top) * 0.45));
            let cx = (x0 + x1) * 0.5;
            let hx = (x1 - x0) * 0.5;
            let bx = abs(p.x - cx) - (hx - rad);
            let by = p.y - (y_bot - rad);
            let body_d = length(max(vec2<f32>(bx, by), vec2<f32>(0.0))) + min(max(bx, by), 0.0) - rad;
            let dl = vec2<f32>(p.x - (x0 - rad), (y_top + rad) - p.y);
            let d_left_bar = length(max(dl, vec2<f32>(0.0))) + min(max(dl.x, dl.y), 0.0) - rad;
            let dr = vec2<f32>((x1 + rad) - p.x, (y_top + rad) - p.y);
            let d_right_bar = length(max(dr, vec2<f32>(0.0))) + min(max(dr.x, dr.y), 0.0) - rad;
            let d_flares = max(-min(d_left_bar, d_right_bar), p.y - (y_top + rad));
            d_active = min(body_d, d_flares);
        }
        let top_clip = clamp(p.y - y_top + 0.5, 0.0, 1.0);
        let bar_cover = clamp(d_active + 0.5, 0.0, 1.0) * top_clip;
        let bar_a = shape.tint.a * bar_cover;
        var out = vec4<f32>(shape.tint.rgb * bar_a, bar_a);
        let rim_w = 1.0 * s;
        let rim = (1.0 - smoothstep(0.0, rim_w, abs(d_active))) * top_clip;
        let rim_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
        let rim_a = rim * select(0.11, 0.13, light);
        out = over(vec4<f32>(rim_tint * rim_a, rim_a), out);
        return out;
    }

    if (shape.kind == TAB_HOVER) {
        var out = vec4<f32>(0.0);
        if (shape.lit > 0.005) {
            let hover_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
            let ha = shape.lit * cover;
            out = vec4<f32>(hover_tint * ha, ha);
        }
        let icon = u32(shape.icon + 0.5);
        if (icon == 9u) {
            let side = 2.0 * min(half.x, half.y);
            let arm = 0.23 * side;
            let stroke = max(0.078 * side, 1.25 * s);
            let h = segment_distance(p, center - vec2<f32>(arm, 0.0), center + vec2<f32>(arm, 0.0));
            let v = segment_distance(p, center - vec2<f32>(0.0, arm), center + vec2<f32>(0.0, arm));
            let d_plus = min(h, v) - 0.5 * stroke;
            let mark = clamp(0.5 - d_plus, 0.0, 1.0);
            let ink_col = select(vec3<f32>(look.ink), shape.tint.rgb, shape.tint.a > 0.0);
            let base_a = select(look.ink_alpha, shape.tint.a, shape.tint.a > 0.0);
            let a = mark * base_a;
            return over(vec4<f32>(ink_col * a, a), out);
        }
        return out;
    }

    if (shape.kind == TOOLTIP) {
        let spread = max(6.0 * s, 0.001);
        let d_drop = sd_round(p - vec2<f32>(0.0, 3.0 * s), center, half, r);
        let drop = (1.0 - smoothstep(-0.3 * spread, spread, d_drop)) * select(0.42, 0.22, light);
        var card = vec4<f32>(0.0, 0.0, 0.0, drop * (1.0 - cover));
        let body = vec4<f32>(shape.tint.rgb * shape.tint.a, shape.tint.a) * cover;
        card = over(body, card);
        let border_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
        let rim = (1.0 - smoothstep(0.0, 1.05 * s, abs(d))) * select(0.15, 0.16, light);
        card = over(vec4<f32>(border_tint * rim, rim), card);
        return card;
    }

    if (shape.kind == DIVIDER) {
        let tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
        let a = select(0.14, 0.16, light) * cover;
        return vec4<f32>(tint * a, a);
    }

    if (shape.kind == LIMITS_ICON) {
        var out = vec4<f32>(0.0);
        if (shape.lit > 0.01) {
            let hover_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
            let ha = shape.lit * select(0.11, 0.09, light) * cover;
            out = vec4<f32>(hover_tint * ha, ha);
        }
        let icon = u32(shape.icon + 0.5);
        if (icon == 1u) {
            let ink_col = select(vec3<f32>(look.ink), shape.tint.rgb, shape.tint.a > 0.0);
            let base_a = select(look.ink_alpha, shape.tint.a, shape.tint.a > 0.0);
            let a = gear_flat_cover * clamp(base_a + shape.lit * 0.12, 0.0, 1.0);
            return over(vec4<f32>(ink_col * a, a), out);
        }
        let side = 2.0 * min(half.x, half.y);
        let mark = layers_icon(p, center, side * 0.92, icon, light);
        return over(mark, out);
    }

    if (shape.kind == LIMITS_CARD) {
        let glass_on = shape.lit > 0.5;
        let spread = max(look.sheet_spread * s, 0.001);
        let d_drop = sd_round(p - vec2<f32>(0.0, look.sheet_y * s), center, half, r);
        let drop_strength = select(look.sheet_shadow * 0.85, look.sheet_shadow * 1.15, glass_on);
        let drop = (1.0 - smoothstep(-0.3 * spread, spread, d_drop)) * drop_strength;
        var card = vec4<f32>(0.0, 0.0, 0.0, drop * (1.0 - cover));

        let body_a = select(1.0, shape.tint.a, glass_on);
        let body = vec4<f32>(shape.tint.rgb * body_a, body_a) * cover;
        card = over(body, card);

        let u = clamp((p.x - shape.rect.x) / max(shape.rect.z, 1.0), 0.0, 1.0);
        if (glass_on) {
            let frost = mix(look.frost_top * 1.15, look.frost_bottom, t) * cover;
            card = over(vec4<f32>(frost), card);

            // Subtle ambient corner sheen echoing the cyan-violet-copper rim.
            let left_glow = exp(-length(vec2<f32>(u * 1.4, t * 1.2)) * 2.2) * select(0.09, 0.04, light);
            let right_glow = exp(-length(vec2<f32>((1.0 - u) * 1.5, (1.0 - t) * 1.3)) * 2.3) * select(0.08, 0.035, light);
            let sheen = vec3<f32>(0.10, 0.82, 0.96) * left_glow + vec3<f32>(0.76, 0.44, 0.86) * right_glow;
            card = over(vec4<f32>(sheen * cover, (left_glow + right_glow) * 0.35 * cover), card);

            // Iridescent luminous rim: cyan -> violet/magenta -> warm copper-rose.
            let c_left = vec3<f32>(0.10, 0.88, 1.00);
            let c_mid = vec3<f32>(0.72, 0.44, 0.98);
            let c_right = vec3<f32>(0.94, 0.62, 0.56);
            let spectrum = select(
                mix(c_left, c_mid, u * 2.0),
                mix(c_mid, c_right, (u - 0.5) * 2.0),
                u > 0.5,
            );
            let edge_fade = mix(1.0, 0.32, smoothstep(0.28, 1.0, t));
            let rim_w = 1.35 * s;
            let rim = (1.0 - smoothstep(0.0, rim_w, abs(d))) * edge_fade * select(0.78, 0.52, light);
            let inner_glow = (1.0 - smoothstep(0.0, 7.5 * s, max(depth, 0.0))) * cover * edge_fade * select(0.16, 0.07, light);
            let edge_a = clamp(rim + inner_glow, 0.0, 1.0);
            card = over(vec4<f32>(spectrum * edge_a, edge_a), card);
        } else {
            // Opaque mode: crisp subtle themed border without glass bloom.
            let border_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
            let rim = (1.0 - smoothstep(0.0, 1.15 * s, abs(d))) * select(0.16, 0.18, light);
            card = over(vec4<f32>(border_tint * rim, rim), card);
        }
        return card;
    }

    if (shape.kind == BAR) {
        let glass_on = shape.icon > 0.5;
        let fill = clamp(shape.lit, 0.0, 1.0);
        let track_tint = select(vec3<f32>(1.0), vec3<f32>(0.0), light);
        let track_a = select(0.09, 0.11, light) * cover;
        var out = vec4<f32>(track_tint * track_a, track_a);

        if (fill > 0.005) {
            let min_w = min(shape.rect.w, shape.rect.z);
            let fill_w = clamp(shape.rect.z * fill, min_w, shape.rect.z);
            let f_half = vec2<f32>(fill_w * 0.5, half.y);
            let f_center = vec2<f32>(shape.rect.x + f_half.x, center.y);
            let f_r = min(r, f_half.x);
            let d_fill = sd_round(p, f_center, f_half, f_r);
            let f_cover = clamp(0.5 - d_fill, 0.0, 1.0) * cover;

            let fx = clamp((p.x - shape.rect.x) / max(fill_w, 1.0), 0.0, 1.0);
            let left_col = shape.tint.rgb * 0.80 + vec3<f32>(0.04, 0.02, 0.12);
            let right_col = min(shape.tint.rgb * 1.16 + vec3<f32>(0.06), vec3<f32>(1.0));
            var bar_col = mix(left_col, right_col, fx);
            if (glass_on) {
                let top_sheen = (1.0 - smoothstep(0.0, 0.55, t)) * 0.20;
                bar_col = min(bar_col + vec3<f32>(top_sheen), vec3<f32>(1.0));
                let glow = (1.0 - smoothstep(0.0, 5.0 * s, max(d_fill, 0.0))) * select(0.24, 0.10, light) * (1.0 - f_cover);
                out = over(vec4<f32>(shape.tint.rgb * glow, glow), out);
            }
            out = over(vec4<f32>(bar_col * f_cover, f_cover), out);
        }
        return out;
    }

    if (shape.kind == DOT) {
        let glass_on = shape.icon > 0.5;
        var out = vec4<f32>(0.0);
        if (glass_on) {
            let glow = (1.0 - smoothstep(0.0, 4.0 * s, max(d, 0.0))) * select(0.30, 0.12, light) * (1.0 - cover);
            out = vec4<f32>(shape.tint.rgb * glow, glow);
        }
        return over(vec4<f32>(shape.tint.rgb * cover, cover), out);
    }

    if (shape.kind == PILL) {
        // The selected tab is sunk into the glass: darker than the capsule, shaded at the
        // top where the rim casts on it, and a faint light along its lower edge.
        var tint = select(vec3<f32>(0.0), vec3<f32>(1.0), light);
        var body_a = mix(look.pill_top, look.pill_bottom, t);
        let soft = clamp(0.5 - d / max(look.pill_soft * s, 0.001), 0.0, 1.0);
        var color = vec4<f32>(tint * body_a, body_a) * soft;
        let inner_top = (1.0 - smoothstep(0.0, max(look.pill_rim_size * s, 0.001), depth)) * max(-n.y, 0.0) * look.pill_rim;
        color = over(vec4<f32>(0.0, 0.0, 0.0, inner_top * soft), color);
        return color;
    }

    if (shape.kind == SHEET) {
        // A dialog: a wide soft shadow and a tinted body, a little clearer toward the bottom.
        let spread = max(look.sheet_spread * s, 0.001);
        let d_drop = sd_round(p - vec2<f32>(0.0, look.sheet_y * s), center, half, r);
        let drop = (1.0 - smoothstep(-0.3 * spread, spread, d_drop)) * look.sheet_shadow;
        var sheet = vec4<f32>(0.0, 0.0, 0.0, drop * (1.0 - cover));
        let body = vec4<f32>(shape.tint.rgb * shape.tint.a, shape.tint.a) * cover;
        sheet = over(body, sheet);
        let frost = mix(look.frost_top, look.frost_bottom, t) * cover;
        return over(vec4<f32>(frost), sheet);
    }

    // Drop shadow under the capsule and the buttons.
    let blur = max(look.shadow_blur * s, 0.001);
    let d_shadow = sd_round(p - vec2<f32>(0.0, look.shadow_y * s), center, half, r);
    let soft_shadow = (1.0 - smoothstep(-0.4 * blur, blur, d_shadow)) * look.shadow;
    var color = vec4<f32>(0.0, 0.0, 0.0, soft_shadow * (1.0 - cover));

    // Body: frosted, darker at the top and clearer toward the bottom, as glass lit from below.
    var body = mix(look.body_top, look.body_bottom, t);
    body = body + shape.lit * look.lit_boost;
    color = over(vec4<f32>(body * cover), color);

    // Inner shadow under the top rim, so the capsule reads as a groove.
    let inner_top = (1.0 - smoothstep(0.0, max(look.groove * s, 0.001), depth)) * pow(max(-n.y, 0.0), 1.5) * look.groove_strength;
    color = over(vec4<f32>(0.0, 0.0, 0.0, inner_top * cover), color);

    // A tinted button, such as the one that closes the tabs, takes its color under the frost.
    if (shape.tint.a > 0.0) {
        let tint = vec4<f32>(shape.tint.rgb * shape.tint.a, shape.tint.a) * cover;
        color = over(color, tint);
    }

    let icon = u32(shape.icon + 0.5);
    if (icon >= 6u) {
        let side = 2.0 * min(half.x, half.y);
        let mark = layers_icon(p, center, side * 0.80, icon, light);
        color = over(mark, color);
    } else if (icon != 0u) {
        var mark = gear_cover;
        if (icon >= 2u) {
            let side = 2.0 * min(half.x, half.y);
            mark = clamp(0.5 - control_distance(icon, p, center, side), 0.0, 1.0);
        }
        let ink = look.ink + shape.lit * look.ink_lit;
        let a = mark * look.ink_alpha;
        color = over(vec4<f32>(vec3<f32>(ink) * a, a), color);
    }
    return color;
}
