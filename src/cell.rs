use bytemuck::{Pod, Zeroable};

/// Grid cell. 16 bytes, aligned for upload and SIMD hashing.
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
/// Interior cell of the bar: an RGBA icon index, not a Unicode character.
pub const AGENT_ICON: u32 = 1 << 7;
/// Interface icon that occupies four columns in the bar.
pub const QUAD: u32 = 1 << 8;
/// Button cell, two rows tall, with a centered glyph.
pub const TALL: u32 = 1 << 9;
/// Body of a tab. The shader draws the rounded rectangle.
pub const TAB: u32 = 1 << 10;
/// Active tab: the top stays square and meets the terminal background.
pub const TAB_ACTIVE: u32 = 1 << 13;
/// Cells to the left end, 4 bits.
pub const TAB_FROM_LEFT_SHIFT: u32 = 14;
/// Cells to the right end, 4 bits.
pub const TAB_FROM_RIGHT_SHIFT: u32 = 18;

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

/// Cell resolved for the GPU: `slot` is the atlas index.
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
    pub scroll_top: f32,
    pub scroll_h: f32,
    pub composer: [u32; 4],
    pub composer_accent: u32,
    pub fill_bg: u32,
    pub scroll_left: f32,
    pub scroll_right: f32,
    /// First selected cell, inclusive. Equal to `sel_hi` when nothing is selected.
    pub sel_lo: u32,
    /// One past the last selected cell.
    pub sel_hi: u32,
    /// Vertical span of the scrollbar track, in pixels. The grid can start lower.
    pub scroll_y0: f32,
    pub scroll_y1: f32,
    /// Nonzero in glass mode: a cell whose background is `glass_key` lets the window show through.
    pub glass: u32,
    /// The background that turns transparent in glass mode, packed as `0x00BBGGRR`.
    pub glass_key: u32,
    pub _glass_pad: [u32; 2],
}

const _: () = assert!(std::mem::size_of::<Uniforms>() % 16 == 0);
