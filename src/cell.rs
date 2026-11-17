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
/// Célula interna da barra: índice de um ícone RGBA, não um caractere Unicode.
pub const AGENT_ICON: u32 = 1 << 7;
/// Ícone de interface que ocupa quatro colunas na barra.
pub const QUAD: u32 = 1 << 8;
/// Célula de botão com altura de duas linhas e glifo centralizado.
pub const TALL: u32 = 1 << 9;
/// Corpo de uma aba. O shader desenha o retângulo arredondado.
pub const TAB: u32 = 1 << 10;
/// Aba ativa: o topo fica reto e encosta no fundo do terminal.
pub const TAB_ACTIVE: u32 = 1 << 13;
/// Células até a ponta esquerda, 4 bits.
pub const TAB_FROM_LEFT_SHIFT: u32 = 14;
/// Células até a ponta direita, 4 bits.
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
    pub scroll_top: f32,
    pub scroll_h: f32,
    pub composer: [u32; 4],
    pub composer_accent: u32,
    pub _composer_padding: [u32; 3],
}
