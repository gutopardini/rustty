use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context};
use fontdue::{Font, FontSettings};

use crate::cell::{GlyphRect, BOLD, WIDE};

pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
}

pub struct Face {
    regular: Font,
    bold: Option<Font>,
    px: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: i32,
}

impl Face {
    pub fn load(px: f32) -> anyhow::Result<Self> {
        let regular_path = font_file(false)?;
        let bold_path = font_file(true).unwrap_or_else(|_| regular_path.clone());
        let regular = load_font(&regular_path)?;
        let bold = load_font(&bold_path).ok();
        let mut face = Self {
            regular,
            bold,
            px: 0.0,
            cell_w: 1,
            cell_h: 1,
            ascent: 1,
        };
        face.set_px(px);
        Ok(face)
    }

    pub fn set_px(&mut self, px: f32) {
        let px = px.clamp(8.0, 64.0);
        let metrics = self
            .regular
            .horizontal_line_metrics(px)
            .unwrap_or(fontdue::LineMetrics {
                ascent: px * 0.8,
                descent: -px * 0.2,
                line_gap: 0.0,
                new_line_size: px,
            });
        let advance = self.regular.rasterize('M', px).0.advance_width;
        self.px = px;
        self.cell_w = advance.round().max(1.0) as u32;
        self.cell_h = metrics.new_line_size.round().max(1.0) as u32;
        self.ascent = metrics.ascent.round() as i32;
    }

    pub fn px(&self) -> f32 {
        self.px
    }

    pub fn cell_w(&self) -> u32 {
        self.cell_w
    }

    pub fn cell_h(&self) -> u32 {
        self.cell_h
    }

    /// Rasteriza o glifo já posicionado dentro da célula (ou de duas, se for wide).
    pub fn raster_cell(&self, ch: char, bold: bool, wide: bool) -> Vec<u8> {
        let owned_bold = bold && self.bold.is_some();
        let font = if owned_bold { self.bold.as_ref().unwrap() } else { &self.regular };
        let (metrics, bitmap) = font.rasterize(ch, self.px);
        let slots = if wide { 2 } else { 1 };
        let width = self.cell_w as usize * slots;
        let height = self.cell_h as usize;
        let mut pixels = vec![0u8; width * height];
        if metrics.width == 0 || metrics.height == 0 {
            return pixels;
        }
        let top = self.ascent - metrics.ymin - metrics.height as i32;
        let left = metrics.xmin.max(0) as usize;
        for row in 0..metrics.height {
            let dest_y = top + row as i32;
            if dest_y < 0 || dest_y as usize >= height {
                continue;
            }
            for col in 0..metrics.width {
                let dest_x = left + col;
                if dest_x >= width {
                    break;
                }
                let coverage = bitmap[row * metrics.width + col];
                let dest = dest_y as usize * width + dest_x;
                pixels[dest] = pixels[dest].max(coverage);
            }
        }
        if bold && !owned_bold {
            smear(&mut pixels, width, height);
        }
        pixels
    }
}

fn smear(pixels: &mut [u8], width: usize, height: usize) {
    let source = pixels.to_vec();
    for y in 0..height {
        for x in 0..width {
            let mut value = source[y * width + x];
            if x > 0 {
                value = value.max(source[y * width + x - 1]);
            }
            pixels[y * width + x] = value;
        }
    }
}

pub struct Atlas {
    size: u32,
    cell_w: u32,
    cell_h: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_h: u32,
    next_slot: u32,
    map: HashMap<(u32, u32), u32>,
    rects: Vec<GlyphRect>,
    uploads: Vec<GlyphUpload>,
    rects_dirty: bool,
    generation: u64,
}

impl Atlas {
    pub const SIZE: u32 = 2048;
    const MAX_SLOTS: u32 = 8192;

    pub fn new(cell_w: u32, cell_h: u32) -> Self {
        let mut atlas = Self {
            size: Self::SIZE,
            cell_w,
            cell_h,
            cursor_x: 1,
            cursor_y: 1,
            row_h: 0,
            next_slot: 1,
            map: HashMap::new(),
            rects: vec![GlyphRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            }],
            uploads: Vec::new(),
            rects_dirty: true,
            generation: 1,
        };
        atlas.sync_cell(cell_w, cell_h);
        atlas
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cell_w(&self) -> u32 {
        self.cell_w
    }

    pub fn cell_h(&self) -> u32 {
        self.cell_h
    }

    pub fn sync_cell(&mut self, cell_w: u32, cell_h: u32) {
        if self.cell_w == cell_w && self.cell_h == cell_h && self.generation > 0 && self.map.is_empty()
        {
            self.cell_w = cell_w;
            self.cell_h = cell_h;
            return;
        }
        if self.cell_w == cell_w && self.cell_h == cell_h && !self.map.is_empty() {
            return;
        }
        self.cell_w = cell_w.max(1);
        self.cell_h = cell_h.max(1);
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = self.cell_h;
        self.next_slot = 1;
        self.map.clear();
        self.rects.truncate(1);
        self.rects[0] = GlyphRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        self.uploads.clear();
        self.rects_dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn prepare(&mut self, face: &Face, cp: u32, flags: u32) -> u32 {
        if cp == 0 || cp == u32::from(b' ') {
            return 0;
        }
        let wide = flags & WIDE != 0;
        let bold = flags & BOLD != 0;
        let key_flags = (flags & BOLD) | if wide { WIDE } else { 0 };
        let key = (cp, key_flags);
        if let Some(slot) = self.map.get(&key) {
            return *slot;
        }
        if self.next_slot >= Self::MAX_SLOTS {
            self.reset_pack();
        }
        let ch = char::from_u32(cp).unwrap_or(' ');
        let pixels = face.raster_cell(ch, bold, wide);
        let slot_w = self.cell_w * if wide { 2 } else { 1 };
        let slot_h = self.cell_h;
        let (x, y) = match self.alloc(slot_w, slot_h) {
            Some(origin) => origin,
            None => {
                self.reset_pack();
                match self.alloc(slot_w, slot_h) {
                    Some(origin) => origin,
                    None => return 0,
                }
            }
        };
        let slot = self.next_slot;
        self.next_slot += 1;
        self.map.insert(key, slot);
        self.rects.push(GlyphRect {
            x,
            y,
            w: slot_w,
            h: slot_h,
        });
        self.uploads.push(GlyphUpload {
            x,
            y,
            w: slot_w,
            h: slot_h,
            pixels,
        });
        self.rects_dirty = true;
        slot
    }

    pub fn take_uploads(&mut self) -> Vec<GlyphUpload> {
        std::mem::take(&mut self.uploads)
    }

    pub fn rects_if_dirty(&mut self) -> Option<&[GlyphRect]> {
        if self.rects_dirty {
            self.rects_dirty = false;
            Some(&self.rects)
        } else {
            None
        }
    }

    fn reset_pack(&mut self) {
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = self.cell_h;
        self.next_slot = 1;
        self.map.clear();
        self.rects.truncate(1);
        self.generation = self.generation.wrapping_add(1);
        self.rects_dirty = true;
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w + 1 >= self.size || h + 1 >= self.size {
            return None;
        }
        if self.cursor_x + w + 1 > self.size {
            self.cursor_x = 1;
            self.cursor_y += self.row_h + 1;
            self.row_h = h;
        }
        if self.cursor_y + h + 1 > self.size {
            return None;
        }
        let origin = (self.cursor_x, self.cursor_y);
        self.cursor_x += w + 1;
        self.row_h = self.row_h.max(h);
        Some(origin)
    }
}

fn load_font(path: &Path) -> anyhow::Result<Font> {
    let bytes = std::fs::read(path).with_context(|| format!("lendo {}", path.display()))?;
    Font::from_bytes(bytes, FontSettings::default()).map_err(|err| anyhow!("{err}"))
}

fn font_file(bold: bool) -> anyhow::Result<PathBuf> {
    if let Ok(path) = std::env::var("RUSTTY_FONT") {
        if !bold {
            return Ok(PathBuf::from(path));
        }
    }
    if bold {
        if let Ok(path) = std::env::var("RUSTTY_FONT_BOLD") {
            return Ok(PathBuf::from(path));
        }
    }
    let pattern = if bold {
        "JetBrainsMono Nerd Font Mono:style=Bold"
    } else {
        "JetBrainsMono Nerd Font Mono:style=Regular"
    };
    if let Some(path) = fc_match(pattern) {
        return Ok(path);
    }
    let fallback = if bold {
        "Liberation Mono:style=Bold"
    } else {
        "monospace"
    };
    fc_match(fallback).ok_or_else(|| anyhow!("nenhuma fonte monoespaçada via fc-match"))
}

fn fc_match(pattern: &str) -> Option<PathBuf> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(text.trim());
    if path.is_file() { Some(path) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capital_a_puts_ink_in_the_upper_half() {
        let face = Face::load(32.0).expect("fonte");
        let pixels = face.raster_cell('A', false, false);
        let width = face.cell_w() as usize;
        let height = face.cell_h() as usize;
        assert_eq!(pixels.len(), width * height);
        let ink: usize = pixels.iter().map(|p| usize::from(*p > 0)).sum();
        assert!(ink > 10, "glifo vazio");
        let upper: usize = pixels[..width * (height / 2)]
            .iter()
            .map(|p| usize::from(*p > 0))
            .sum();
        assert!(upper > 0, "baseline ficou fora da célula");
    }
}
