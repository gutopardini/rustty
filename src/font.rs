use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

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
    /// Nerd Font Mono used when the chosen face has no glyph, or when its glyph
    /// does not fit the cell (icons from the proportional variant).
    symbols: Option<Font>,
    symbols_bold: Option<Font>,
    regular_ft: Option<Mutex<crate::ft::Face>>,
    bold_ft: Option<Mutex<crate::ft::Face>>,
    symbols_ft: Option<Mutex<crate::ft::Face>>,
    symbols_bold_ft: Option<Mutex<crate::ft::Face>>,
    px: f32,
    cell_w: u32,
    cell_h: u32,
    ascent: i32,
}

impl Face {
    pub fn load(px: f32) -> anyhow::Result<Self> {
        let regular_path = font_file(false)?;
        Self::load_file(&regular_path, px)
    }

    pub fn load_file(path: &Path, px: f32) -> anyhow::Result<Self> {
        let regular = load_font(path)?;
        let bold = bold_sibling(path).and_then(|candidate| load_font(&candidate).ok());
        let (symbols, symbols_bold) = load_symbols(path);
        let (symbols_ft, symbols_bold_ft) = open_symbol_ft(path);
        let mut face = Self {
            regular,
            bold,
            symbols,
            symbols_bold,
            regular_ft: open_ft(path),
            bold_ft: bold_sibling(path).and_then(|candidate| open_ft(&candidate)),
            symbols_ft,
            symbols_bold_ft,
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

    /// Rasterizes the glyph already positioned inside the cell (or two cells, if wide).
    pub fn raster_cell(&self, ch: char, bold: bool, wide: bool) -> Vec<u8> {
        let slots = if wide { 2 } else { 1 };
        let width = self.cell_w as usize * slots;
        let height = self.cell_h as usize;
        let mut pixels = vec![0u8; width * height];
        if is_symbol(ch) && self.draw_symbol(&mut pixels, width, height, ch, bold) {
            return pixels;
        }
        let Some((metrics, bitmap, synthetic_bold)) = self.choose_bitmap(ch, bold, width, height) else {
            return pixels;
        };
        if metrics.width == 0 || metrics.height == 0 || bitmap.is_empty() {
            return pixels;
        }
        if is_powerline_separator(ch) {
            stamp_fit(&mut pixels, width, height, &bitmap, metrics.width, metrics.height, Fit::Fill);
        } else if is_symbol(ch) && overflows(&metrics, self.ascent, width, height) {
            stamp_fit(&mut pixels, width, height, &bitmap, metrics.width, metrics.height, Fit::Contain);
        } else {
            stamp_baseline(&mut pixels, width, height, &bitmap, &metrics, self.ascent);
        }
        if synthetic_bold {
            smear(&mut pixels, width, height);
        }
        pixels
    }

    fn draw_symbol(&self, pixels: &mut [u8], width: usize, height: usize, ch: char, bold: bool) -> bool {
        let Some((image, synthetic)) = self.symbol_image(ch, bold, width, height) else {
            return false;
        };
        if image.width == 0 || image.height == 0 {
            return false;
        }
        if is_powerline_separator(ch) {
            stamp_fit(pixels, width, height, &image.pixels, image.width, image.height, Fit::Fill);
        } else if is_box_or_block(ch) {
            // These glyphs overflow the cell by 1px on purpose, so the lines meet.
            // Shrinking (Contain) opens a gap in the middle of the drawing.
            stamp_at(
                pixels,
                width,
                height,
                &image.pixels,
                image.width,
                image.height,
                image.left,
                self.ascent - image.top,
            );
        } else if placed_overflows(&image, self.ascent, width, height) {
            stamp_fit(pixels, width, height, &image.pixels, image.width, image.height, Fit::Contain);
        } else {
            stamp_at(
                pixels,
                width,
                height,
                &image.pixels,
                image.width,
                image.height,
                image.left,
                self.ascent - image.top,
            );
        }
        if synthetic {
            smear(pixels, width, height);
        }
        true
    }

    fn symbol_image(&self, ch: char, bold: bool, cell_w: usize, cell_h: usize) -> Option<(crate::ft::GlyphImage, bool)> {
        let primary = if bold { self.bold_ft.as_ref().or(self.regular_ft.as_ref()) } else { self.regular_ft.as_ref() };
        let synthetic_primary = bold && self.bold_ft.is_none();
        let fallback = if bold {
            self.symbols_bold_ft.as_ref().or(self.symbols_ft.as_ref())
        } else {
            self.symbols_ft.as_ref()
        };
        let synthetic_fallback = bold && self.symbols_bold_ft.is_none();
        let primary_image = primary.and_then(|face| render_locked(face, ch, self.px));
        if is_powerline_separator(ch) {
            if let Some(image) = primary_image {
                return Some((image, synthetic_primary));
            }
            return fallback
                .and_then(|face| render_locked(face, ch, self.px))
                .map(|image| (image, synthetic_fallback));
        }
        if let Some(image) = primary_image {
            if !placed_overflows(&image, self.ascent, cell_w, cell_h) {
                return Some((image, synthetic_primary));
            }
            if let Some(alt) = fallback.and_then(|face| render_locked(face, ch, self.px)) {
                if !placed_overflows(&alt, self.ascent, cell_w, cell_h) {
                    return Some((alt, synthetic_fallback));
                }
            }
            return Some((image, synthetic_primary));
        }
        fallback
            .and_then(|face| render_locked(face, ch, self.px))
            .map(|image| (image, synthetic_fallback))
    }

    fn choose_bitmap(&self, ch: char, bold: bool, cell_w: usize, cell_h: usize) -> Option<(fontdue::Metrics, Vec<u8>, bool)> {
        if let Some((font, synthetic)) = self.hit(ch, bold, true) {
            let raster = font.rasterize(ch, self.px);
            let symbol_overflow = is_symbol(ch) && !is_powerline_separator(ch) && overflows(&raster.0, self.ascent, cell_w, cell_h);
            if !symbol_overflow {
                return Some((raster.0, raster.1, synthetic));
            }
            if let Some((fallback, fallback_bold)) = self.hit(ch, bold, false) {
                let alt = fallback.rasterize(ch, self.px);
                if !overflows(&alt.0, self.ascent, cell_w, cell_h) {
                    return Some((alt.0, alt.1, fallback_bold));
                }
            }
            return Some((raster.0, raster.1, synthetic));
        }
        let (font, synthetic) = self.hit(ch, bold, false)?;
        let raster = font.rasterize(ch, self.px);
        Some((raster.0, raster.1, synthetic))
    }

    /// `primary` chooses the cell face. The other side is Nerd Font Mono.
    fn hit(&self, ch: char, bold: bool, primary: bool) -> Option<(&Font, bool)> {
        let (regular, bold_face) = if primary {
            (&self.regular, self.bold.as_ref())
        } else {
            (self.symbols.as_ref()?, self.symbols_bold.as_ref())
        };
        if bold {
            if let Some(font) = bold_face {
                if font.lookup_glyph_index(ch) != 0 {
                    return Some((font, false));
                }
            }
            if regular.lookup_glyph_index(ch) != 0 {
                return Some((regular, true));
            }
            return None;
        }
        if regular.lookup_glyph_index(ch) != 0 {
            Some((regular, false))
        } else {
            None
        }
    }
}

fn is_powerline_separator(ch: char) -> bool {
    matches!(ch, '\u{E0B0}'..='\u{E0D4}')
}

fn is_box_or_block(ch: char) -> bool {
    matches!(ch, '\u{2500}'..='\u{259F}')
}

fn is_symbol(ch: char) -> bool {
    matches!(
        ch,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}' | '\u{2190}'..='\u{2BFF}'
    )
}

fn overflows(metrics: &fontdue::Metrics, ascent: i32, cell_w: usize, cell_h: usize) -> bool {
    let top = ascent - metrics.ymin - metrics.height as i32;
    let left = metrics.xmin;
    left < 0
        || top < 0
        || left as i32 + metrics.width as i32 > cell_w as i32
        || top + metrics.height as i32 > cell_h as i32
}

fn render_locked(face: &Mutex<crate::ft::Face>, ch: char, px: f32) -> Option<crate::ft::GlyphImage> {
    let mut face = face.lock().unwrap_or_else(|err| err.into_inner());
    if !face.has(ch) {
        return None;
    }
    face.render(ch, px)
}

fn placed_overflows(image: &crate::ft::GlyphImage, ascent: i32, cell_w: usize, cell_h: usize) -> bool {
    let top = ascent - image.top;
    image.left < 0
        || top < 0
        || image.left + image.width as i32 > cell_w as i32
        || top + image.height as i32 > cell_h as i32
}

fn stamp_at(dst: &mut [u8], width: usize, height: usize, src: &[u8], src_w: usize, src_h: usize, left: i32, top: i32) {
    for y in 0..src_h {
        let dest_y = top + y as i32;
        if dest_y < 0 || dest_y as usize >= height {
            continue;
        }
        for x in 0..src_w {
            let dest_x = left + x as i32;
            if dest_x < 0 || dest_x as usize >= width {
                continue;
            }
            let coverage = src[y * src_w + x];
            let dest = dest_y as usize * width + dest_x as usize;
            dst[dest] = dst[dest].max(coverage);
        }
    }
}

fn stamp_baseline(dst: &mut [u8], width: usize, height: usize, bitmap: &[u8], metrics: &fontdue::Metrics, ascent: i32) {
    let top = ascent - metrics.ymin - metrics.height as i32;
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
            dst[dest] = dst[dest].max(coverage);
        }
    }
}

#[derive(Clone, Copy)]
enum Fit {
    /// Stretches the ink to the edges. Oh My Posh separators need to meet the cell.
    Fill,
    /// Keeps the proportion and centers. An icon that is too wide is no longer clipped.
    Contain,
}

fn stamp_fit(dst: &mut [u8], dst_w: usize, dst_h: usize, src: &[u8], src_w: usize, src_h: usize, fit: Fit) {
    let Some((x0, y0, x1, y1)) = ink_bounds(src, src_w, src_h) else {
        return;
    };
    let crop_w = x1 - x0;
    let crop_h = y1 - y0;
    let (origin_x, origin_y, draw_w, draw_h) = match fit {
        Fit::Fill => (0, 0, dst_w, dst_h),
        Fit::Contain => {
            let pad = 1.max(dst_w.min(dst_h) / 8);
            let avail_w = dst_w.saturating_sub(pad * 2).max(1);
            let avail_h = dst_h.saturating_sub(pad * 2).max(1);
            let scale = (avail_w as f32 / crop_w as f32).min(avail_h as f32 / crop_h as f32);
            let draw_w = ((crop_w as f32) * scale).round().clamp(1.0, avail_w as f32) as usize;
            let draw_h = ((crop_h as f32) * scale).round().clamp(1.0, avail_h as f32) as usize;
            ((dst_w - draw_w) / 2, (dst_h - draw_h) / 2, draw_w, draw_h)
        }
    };
    for y in 0..draw_h {
        for x in 0..draw_w {
            let u = if draw_w == 1 { 0.0 } else { x as f32 / (draw_w - 1) as f32 };
            let v = if draw_h == 1 { 0.0 } else { y as f32 / (draw_h - 1) as f32 };
            let sample_x = x0 as f32 + u * (crop_w.saturating_sub(1) as f32);
            let sample_y = y0 as f32 + v * (crop_h.saturating_sub(1) as f32);
            let coverage = match fit {
                Fit::Fill => sample_nearest(src, src_w, src_h, sample_x, sample_y),
                Fit::Contain => sample_coverage(src, src_w, src_h, sample_x, sample_y),
            };
            let dest = (origin_y + y) * dst_w + origin_x + x;
            if dest < dst.len() {
                dst[dest] = dst[dest].max(coverage);
            }
        }
    }
}

fn ink_bounds(src: &[u8], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            if src[y * width + x] > 0 {
                any = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    any.then_some((min_x, min_y, max_x + 1, max_y + 1))
}

fn sample_nearest(src: &[u8], width: usize, height: usize, x: f32, y: f32) -> u8 {
    if width == 0 || height == 0 {
        return 0;
    }
    let px = (x.round() as usize).min(width - 1);
    let py = (y.round() as usize).min(height - 1);
    src[py * width + px]
}

fn sample_coverage(src: &[u8], width: usize, height: usize, x: f32, y: f32) -> u8 {
    if width == 0 || height == 0 {
        return 0;
    }
    let x = x.clamp(0.0, (width - 1) as f32);
    let y = y.clamp(0.0, (height - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let at = |px: usize, py: usize| f32::from(src[py * width + px]);
    let value = at(x0, y0) * (1.0 - tx) * (1.0 - ty)
        + at(x1, y0) * tx * (1.0 - ty)
        + at(x0, y1) * (1.0 - tx) * ty
        + at(x1, y1) * tx * ty;
    value.round().clamp(0.0, 255.0) as u8
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

    pub fn discard(&mut self) {
        self.cursor_x = 1;
        self.cursor_y = 1;
        self.row_h = self.cell_h;
        self.next_slot = 1;
        self.map.clear();
        self.rects.truncate(1);
        self.rects[0] = GlyphRect { x: 0, y: 0, w: 0, h: 0 };
        self.uploads.clear();
        self.rects_dirty = true;
        self.generation = self.generation.wrapping_add(1);
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

    /// Rasterizes ASCII and the prompt glyphs before the first frame.
    pub fn warm(&mut self, face: &Face) {
        for cp in 0x21u32..=0x7e {
            self.prepare(face, cp, 0);
            self.prepare(face, cp, BOLD);
        }
        for cp in 0xE0B0u32..=0xE0D4 {
            self.prepare(face, cp, 0);
        }
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

#[derive(Clone, Debug)]
pub struct FontChoice {
    pub name: String,
    pub path: PathBuf,
}

pub fn font_choices() -> Vec<FontChoice> {
    let mut choices = Vec::new();
    if let Some(output) = Command::new("fc-list")
        .args([":spacing=100", "-f", "%{file}\t%{family}\t%{style}\n"])
        .output()
        .ok()
        .filter(|output| output.status.success())
    {
        if let Ok(text) = String::from_utf8(output.stdout) {
            let mut best: Vec<(String, i32, PathBuf)> = Vec::new();
            for line in text.lines() {
                let mut parts = line.split('\t');
                let Some(file) = parts.next() else { continue };
                let Some(family) = parts.next() else { continue };
                let style = parts.next().unwrap_or("");
                let path = PathBuf::from(file.trim());
                if !path.is_file() {
                    continue;
                }
                let family = family.split(',').next().unwrap_or(family).trim();
                if family.is_empty() {
                    continue;
                }
                let rank = style_rank(style);
                if let Some(slot) = best.iter_mut().find(|item| item.0 == family) {
                    if rank > slot.1 {
                        *slot = (family.to_string(), rank, path);
                    }
                } else {
                    best.push((family.to_string(), rank, path));
                }
            }
            best.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            for (name, _, path) in best.into_iter().take(18) {
                choices.push(FontChoice { name, path });
            }
        }
    }
    if choices.is_empty() {
        if let Ok(path) = font_file(false) {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("mono")
                .to_string();
            choices.push(FontChoice { name, path });
        }
    }
    choices
}

fn style_rank(style: &str) -> i32 {
    let style = style.to_lowercase();
    if style.contains("bold") || style.contains("italic") || style.contains("oblique") || style.contains("condensed")
    {
        return 0;
    }
    if style.contains("regular") || style.contains("roman") || style == "book" {
        return 3;
    }
    1
}

fn bold_sibling(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let bold = name.replace("Regular", "Bold").replace("-regular", "-Bold");
    if bold == name {
        return None;
    }
    let candidate = path.with_file_name(bold);
    candidate.is_file().then_some(candidate)
}

fn open_ft(path: &Path) -> Option<Mutex<crate::ft::Face>> {
    let bytes = std::fs::read(path).ok()?;
    crate::ft::Face::open(bytes).map(Mutex::new)
}

fn open_symbol_ft(primary: &Path) -> (Option<Mutex<crate::ft::Face>>, Option<Mutex<crate::ft::Face>>) {
    let Some(path) = symbol_font_path() else {
        return (None, None);
    };
    if path == primary {
        return (None, None);
    }
    let regular = open_ft(&path);
    let bold = bold_sibling(&path).and_then(|candidate| open_ft(&candidate));
    (regular, bold)
}

fn load_symbols(primary: &Path) -> (Option<Font>, Option<Font>) {
    let Some(path) = symbol_font_path() else {
        return (None, None);
    };
    if path == primary {
        return (None, None);
    }
    let regular = load_font(&path).ok();
    let bold = bold_sibling(&path).and_then(|candidate| load_font(&candidate).ok());
    (regular, bold)
}

fn symbol_font_path() -> Option<PathBuf> {
    for pattern in [
        "JetBrainsMono Nerd Font Mono:style=Regular",
        "Symbols Nerd Font Mono:style=Regular",
        "NerdFontsSymbols Nerd Font Mono:style=Regular",
    ] {
        if let Some(path) = fc_match(pattern) {
            return Some(path);
        }
    }
    None
}

fn load_font(path: &Path) -> anyhow::Result<Font> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
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
    fc_match(fallback).ok_or_else(|| anyhow!("fc-match found no monospace font"))
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
        let face = Face::load(32.0).expect("font");
        let pixels = face.raster_cell('A', false, false);
        let width = face.cell_w() as usize;
        let height = face.cell_h() as usize;
        assert_eq!(pixels.len(), width * height);
        let ink: usize = pixels.iter().map(|p| usize::from(*p > 0)).sum();
        assert!(ink > 10, "empty glyph");
        let upper: usize = pixels[..width * (height / 2)]
            .iter()
            .map(|p| usize::from(*p > 0))
            .sum();
        assert!(upper > 0, "baseline fell outside the cell");
    }

    #[test]
    fn nerd_font_draws_powerline_diamonds() {
        let face = Face::load(32.0).expect("font");
        for ch in ['\u{e0b6}', '\u{e0c6}', '\u{e0c7}'] {
            let pixels = face.raster_cell(ch, false, false);
            let ink: usize = pixels.iter().map(|p| usize::from(*p > 0)).sum();
            assert!(ink > 8, "U+{cp:04X} came out empty in the atlas font", cp = u32::from(ch));
        }
        let triangle = face.raster_cell('\u{e0b0}', false, false);
        let width = face.cell_w() as usize;
        let left: usize = (0..face.cell_h() as usize).map(|y| usize::from(triangle[y * width] > 16)).sum();
        assert!(left > 2, "the separator does not meet the cell edge");
    }

    #[test]
    fn adwaita_borrows_nerd_symbols_instead_of_drawing_notdef() {
        let path = std::path::Path::new("/usr/share/fonts/adwaita-mono-fonts/AdwaitaMono-Regular.ttf");
        if !path.is_file() {
            return;
        }
        let face = Face::load_file(path, 28.0).expect("adwaita");
        for ch in ['\u{e0c6}', '\u{f07b}', '\u{f126}', '\u{ea7b}'] {
            let pixels = face.raster_cell(ch, false, false);
            let ink: usize = pixels.iter().map(|p| usize::from(*p > 0)).sum();
            assert!(ink > 20, "U+{cp:04X} did not come from the symbol font", cp = u32::from(ch));
        }
        let missing = face.raster_cell('\u{0378}', false, false);
        assert!(missing.iter().all(|p| *p == 0), "a missing glyph drew the .notdef box");
    }

    #[test]
    fn proportional_nerd_icons_stay_inside_the_cell() {
        let wide = std::path::Path::new(
            "/home/gutopardini/.local/share/fonts/JetBrainsMonoNerdFont/JetBrainsMonoNerdFont-Regular.ttf",
        );
        let mono = std::path::Path::new(
            "/home/gutopardini/.local/share/fonts/JetBrainsMonoNerdFont/JetBrainsMonoNerdFontMono-Regular.ttf",
        );
        if !wide.is_file() || !mono.is_file() {
            return;
        }
        let wide_face = Face::load_file(wide, 28.0).expect("nerd");
        let mono_face = Face::load_file(mono, 28.0).expect("nerd mono");
        let raw = load_font(wide).unwrap().rasterize('\u{f07b}', wide_face.px()).0;
        assert!(raw.width > wide_face.cell_w() as usize, "the sample already fit the cell");
        assert_eq!(
            wide_face.raster_cell('\u{f07b}', false, false),
            mono_face.raster_cell('\u{f07b}', false, false),
            "the proportional icon was clipped instead of using the variant that fits the cell"
        );
    }

    #[test]
    fn box_drawing_meets_at_the_cell_edge() {
        let face = Face::load(19.0).expect("font");
        let width = face.cell_w() as usize;
        let height = face.cell_h() as usize;
        let vertical = face.raster_cell('\u{2502}', false, false);
        let top = vertical[..width].iter().any(|p| *p > 16);
        let bottom = vertical[(height - 1) * width..].iter().any(|p| *p > 16);
        assert!(top && bottom, "the vertical line does not cross the cell");
        let horizontal = face.raster_cell('\u{2500}', false, false);
        let row = horizontal
            .chunks(width)
            .find(|row| row.iter().any(|p| *p > 16))
            .expect("empty horizontal line");
        assert!(row[0] > 16 && row[width - 1] > 16, "the horizontal line does not run edge to edge");
        let block = face.raster_cell('\u{2588}', false, false);
        let ink = block.iter().filter(|p| **p > 16).count();
        assert!(ink * 10 > width * height * 8, "the full block was shrunk inside the cell");
    }
}
