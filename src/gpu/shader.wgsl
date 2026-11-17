struct Uniforms {
    cols: u32,
    rows: u32,
    cursor_col: u32,
    cursor_row: u32,
    cursor_on: u32,
    atlas_w: u32,
    atlas_h: u32,
    _reserved: u32,
    pad_x: f32,
    pad_y: f32,
    cell_w: f32,
    cell_h: f32,
    surf_w: f32,
    surf_h: f32,
    scroll_top: f32,
    scroll_h: f32,
    composer: vec4<u32>,
    composer_accent: u32,
}

struct Cell {
    slot: u32,
    fg: u32,
    bg: u32,
    flags: u32,
}

struct Glyph {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> cells: array<Cell>;
@group(0) @binding(2) var<storage, read> glyphs: array<Glyph>;
@group(0) @binding(3) var atlas: texture_2d<f32>;
@group(0) @binding(4) var atlas_samp: sampler;
@group(0) @binding(5) var agent_icons: texture_2d<f32>;

const CURSOR_CELL: u32 = 0x80000000u;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg: vec4<f32>,
    @location(2) bg: vec4<f32>,
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) flags: u32,
    @location(5) @interpolate(flat) slot: u32,
}

fn unpack_color(color: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(color & 0xffu) / 255.0,
        f32((color >> 8u) & 0xffu) / 255.0,
        f32((color >> 16u) & 0xffu) / 255.0,
        1.0,
    );
}

@vertex
fn vs(@builtin(vertex_index) vi: u32, @builtin(instance_index) inst: u32) -> VsOut {
    var out: VsOut;
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let corner = corners[vi];
    let cell = cells[inst];
    let spacer = (cell.flags & 16u) != 0u;
    if (spacer || u.cols == 0u) {
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
        out.uv = vec2<f32>(0.0);
        out.fg = vec4<f32>(0.0);
        out.bg = vec4<f32>(0.0);
        out.local = corner;
        out.flags = 0u;
        out.slot = 0u;
        return out;
    }

    let col = inst % u.cols;
    let row = inst / u.cols;
    let wide = (cell.flags & 8u) != 0u;
    let span = select(select(1.0, 2.0, wide), 4.0, (cell.flags & 256u) != 0u);
    let tall = (cell.flags & 512u) != 0u;
    let height = select(u.cell_h, 2.0 * u.cell_h, tall);
    var px = u.pad_x + (f32(col) + corner.x * span) * u.cell_w;
    let py = u.pad_y + f32(row) * u.cell_h + corner.y * height;
    if ((cell.flags & 32u) != 0u) {
        px = px + (1.0 - corner.y) * u.cell_w * 0.22;
    }
    let ndc_x = px / max(u.surf_w, 1.0) * 2.0 - 1.0;
    let ndc_y = 1.0 - py / max(u.surf_h, 1.0) * 2.0;

    var uv = vec2<f32>(0.5 / f32(max(u.atlas_w, 1u)), 0.5 / f32(max(u.atlas_h, 1u)));
    if (cell.slot != 0u && (cell.flags & 128u) == 0u) {
        let glyph = glyphs[cell.slot];
        var glyph_y = corner.y;
        if (u._reserved != 0u || tall) {
            let glyph_h = select(f32(u._reserved), u.cell_h, tall);
            let top = floor((height - glyph_h) * 0.5);
            glyph_y = (corner.y * height - top) / glyph_h;
        }
        uv = vec2<f32>(
            (f32(glyph.x) + corner.x * f32(glyph.w)) / f32(u.atlas_w),
            (f32(glyph.y) + glyph_y * f32(glyph.h)) / f32(u.atlas_h),
        );
    }

    var fg = unpack_color(cell.fg);
    var bg = unpack_color(cell.bg);
    if ((cell.flags & 64u) != 0u) {
        fg = vec4<f32>(fg.rgb * 0.72, 1.0);
    }
    let on_cursor = u.cursor_on != 0u && col == u.cursor_col && row == u.cursor_row;
    if (on_cursor || (cell.flags & 1u) != 0u) {
        let swapped = fg;
        fg = bg;
        bg = swapped;
    }

    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = uv;
    out.fg = fg;
    out.bg = bg;
    out.local = corner;
    out.flags = cell.flags | select(0u, CURSOR_CELL, on_cursor);
    out.slot = cell.slot;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if ((in.flags & 128u) != 0u) {
        let icon_cols = select(2.0, 4.0, (in.flags & 256u) != 0u);
        let bounds = vec2<f32>(u.cell_w * icon_cols, u.cell_h);
        let side = min(bounds.x, bounds.y);
        let icon_pos = (in.local * bounds - (bounds - vec2<f32>(side)) * 0.5) / side;
        if (icon_pos.x < 0.0 || icon_pos.x > 1.0 || icon_pos.y < 0.0 || icon_pos.y > 1.0) {
            return tab_color(in.bg, in.local, in.flags);
        }
        let uv = vec2<f32>((f32(in.slot) + clamp(icon_pos.x, 0.001, 0.999)) / 7.0, clamp(icon_pos.y, 0.001, 0.999));
        let icon = textureSampleLevel(agent_icons, atlas_samp, uv, 0.0);
        let painted = vec4<f32>(mix(in.bg.rgb, icon.rgb, icon.a), 1.0);
        return mix(unpack_color(u.composer_accent), painted, tab_cover(in.local, in.flags));
    }
    let coverage = textureSampleLevel(atlas, atlas_samp, in.uv, 0.0).r;
    var ink = select(0.0, coverage, in.slot != 0u);
    let tall = (in.flags & 512u) != 0u;
    if (u._reserved != 0u || tall) {
        let height = select(u.cell_h, 2.0 * u.cell_h, tall);
        let glyph_h = select(f32(u._reserved), u.cell_h, tall);
        let top = floor((height - glyph_h) * 0.5);
        let pixel_y = in.local.y * height;
        if (pixel_y < top || pixel_y >= top + glyph_h) {
            ink = 0.0;
        }
    }
    var bg = tab_color(in.bg, in.local, in.flags);
    ink = ink * tab_cover(in.local, in.flags);
    if (u.composer.z != 0u && (in.flags & (CURSOR_CELL | 1u)) == 0u) {
        let pad = vec2<f32>(u.pad_x, u.pad_y);
        let cell_size = vec2<f32>(u.cell_w, u.cell_h);
        let edge = in.clip.xy - (pad + vec2<f32>(u.composer.xy) * cell_size);
        let far_edge = pad + vec2<f32>(u.composer.zw) * cell_size - in.clip.xy;
        if (all(edge >= vec2<f32>(0.0)) && all(far_edge > vec2<f32>(0.0))) {
            let accent = unpack_color(u.composer_accent);
            let rail = clamp(round(u.cell_w * 0.17), 1.0, 4.0);
            if (edge.x < rail) {
                bg = accent;
            } else if (min(min(edge.y, far_edge.y), far_edge.x) < 1.0) {
                bg = mix(bg, accent, 0.34);
            } else {
                bg = mix(bg, accent, 0.065);
            }
        }
    }
    var color = mix(bg, in.fg, ink);
    if ((in.flags & 4u) != 0u && in.local.y > 0.86) {
        color = in.fg;
    }
    if (u.scroll_h > 0.0) {
        let grid_right = u.pad_x + f32(u.cols) * u.cell_w;
        let grid_h = f32(u.rows) * u.cell_h;
        let y = (in.clip.y - u.pad_y) / max(grid_h, 1.0);
        if (in.clip.x >= grid_right - 5.0 && in.clip.x <= grid_right - 1.0 && y >= 0.0 && y <= 1.0) {
            color = mix(color, vec4<f32>(0.42, 0.44, 0.50, 1.0), 0.45);
            if (y >= u.scroll_top && y <= u.scroll_top + u.scroll_h) {
                color = vec4<f32>(0.86, 0.88, 0.93, 1.0);
            }
        }
    }
    return color;
}

const TAB: u32 = 1024u;
const TAB_ACTIVE: u32 = 8192u;

fn tab_color(fill: vec4<f32>, local: vec2<f32>, flags: u32) -> vec4<f32> {
    let cover = tab_cover(local, flags);
    if (cover >= 1.0) {
        return fill;
    }
    return mix(unpack_color(u.composer_accent), fill, cover);
}

fn tab_cover(local: vec2<f32>, flags: u32) -> f32 {
    if ((flags & TAB) == 0u) {
        return 1.0;
    }
    let span = select(select(1.0, 2.0, (flags & 8u) != 0u), 4.0, (flags & 256u) != 0u);
    let width = u.cell_w * span;
    let height = max(u.cell_h, 1.0);
    let from_left = f32((flags >> 14u) & 15u);
    let from_right = f32((flags >> 18u) & 15u);
    let selected = (flags & TAB_ACTIVE) != 0u;
    let gap = clamp(u.cell_w * 0.22, 3.0, 6.0);
    let slack = max((height - f32(u._reserved)) * 0.5, 0.0);
    let inset_y = select(min(slack, 4.0), 0.0, selected);
    let x0 = -from_left * u.cell_w + gap;
    let x1 = width + from_right * u.cell_w - gap;
    let y0 = inset_y;
    let y1 = height - inset_y;
    if (x1 - x0 < 1.0 || y1 - y0 < 1.0) {
        return 0.0;
    }
    let radius = min(height * 0.42, (x1 - x0) * 0.5);
    let p = vec2<f32>(local.x * width, local.y * height);
    let top = select(radius, 0.0, selected);
    let dist = sd_tab(p, vec2<f32>(x0, y0), vec2<f32>(x1, y1), top, top, radius, radius);
    return 1.0 - smoothstep(-0.6, 0.8, dist);
}

fn sd_tab(p: vec2<f32>, bmin: vec2<f32>, bmax: vec2<f32>, rtl: f32, rtr: f32, rbr: f32, rbl: f32) -> f32 {
    let center = (bmin + bmax) * 0.5;
    let half = (bmax - bmin) * 0.5;
    let q = p - center;
    var rad = rtl;
    if (q.x >= 0.0 && q.y < 0.0) {
        rad = rtr;
    } else if (q.x < 0.0 && q.y >= 0.0) {
        rad = rbl;
    } else if (q.x >= 0.0) {
        rad = rbr;
    }
    rad = min(rad, min(half.x, half.y));
    let d = abs(q) - half + vec2<f32>(rad, rad);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - rad;
}
