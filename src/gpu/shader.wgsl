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
    _pad0: f32,
    _pad1: f32,
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
    let span = select(1.0, 2.0, wide);
    var px = u.pad_x + (f32(col) + corner.x * span) * u.cell_w;
    let py = u.pad_y + (f32(row) + corner.y) * u.cell_h;
    if ((cell.flags & 32u) != 0u) {
        px = px + (1.0 - corner.y) * u.cell_w * 0.22;
    }
    let ndc_x = px / max(u.surf_w, 1.0) * 2.0 - 1.0;
    let ndc_y = 1.0 - py / max(u.surf_h, 1.0) * 2.0;

    var uv = vec2<f32>(0.5 / f32(max(u.atlas_w, 1u)), 0.5 / f32(max(u.atlas_h, 1u)));
    if (cell.slot != 0u) {
        let glyph = glyphs[cell.slot];
        uv = vec2<f32>(
            (f32(glyph.x) + corner.x * f32(glyph.w)) / f32(u.atlas_w),
            (f32(glyph.y) + corner.y * f32(glyph.h)) / f32(u.atlas_h),
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
    out.flags = cell.flags;
    out.slot = cell.slot;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let coverage = textureSample(atlas, atlas_samp, in.uv).r;
    let ink = select(0.0, coverage, in.slot != 0u);
    var color = mix(in.bg, in.fg, ink);
    if ((in.flags & 4u) != 0u && in.local.y > 0.86) {
        color = in.fg;
    }
    return color;
}
