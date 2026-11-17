use bytemuck::{Pod, Zeroable};

/// Célula da grade. 16 bytes, alinhada para upload e hash SIMD.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Pod, Zeroable)]
pub struct Cell {
    pub cp: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u32,
}

pub const REVERSE: u32 = 1 << 0;
pub const BOLD: u32 = 1 << 1;
pub const UNDERLINE: u32 = 1 << 2;
pub const WIDE: u32 = 1 << 3;
pub const SPACER: u32 = 1 << 4;
pub const ITALIC: u32 = 1 << 5;
pub const DIM: u32 = 1 << 6;

impl Cell {
    pub const fn blank(fg: u32, bg: u32) -> Self {
        Self {
            cp: 0,
            fg,
            bg,
            flags: 0,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.cp == 0 && self.flags & (WIDE | SPACER) == 0
    }
}

/// Célula já resolvida para a GPU: `slot` é o índice no atlas.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuCell {
    pub slot: u32,
    pub fg: u32,
    pub bg: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GlyphRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Uniforms {
    pub cols: u32,
    pub rows: u32,
    pub cursor_col: u32,
    pub cursor_row: u32,
    pub cursor_on: u32,
    pub atlas_w: u32,
    pub atlas_h: u32,
    pub _reserved: u32,
    pub pad_x: f32,
    pub pad_y: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub surf_w: f32,
    pub surf_h: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}
