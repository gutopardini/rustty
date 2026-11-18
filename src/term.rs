use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Cell, BOLD, DIM, ITALIC, REVERSE, SPACER, UNDERLINE, WIDE};
use crate::color::{self, Rgb, DEFAULT_BG, DEFAULT_FG};
use crate::graphics::{decode_job, DecodeJob, Graphics, Image, Parser, Placement, Prepared, Update};

const MAX_SCROLLBACK: usize = 10_000;

#[derive(Clone, Copy)]
struct Pen {
    fg: Rgb,
    bg: Rgb,
    flags: u32,
}

impl Pen {
    fn reset(fg: Rgb, bg: Rgb) -> Self {
        Self { fg, bg, flags: 0 }
    }
}

#[derive(Clone, Copy)]
struct Cursor {
    row: u16,
    col: u16,
}

struct Screen {
    /// One owned row per visual line. Scrolling rotates these vectors, so a
    /// linefeed copies the row that leaves and not the rows that stay.
    lines: Vec<Vec<Cell>>,
    graphics: Graphics,
    cols: u16,
    rows: u16,
}

impl Screen {
    fn new(cols: u16, rows: u16, blank: Cell) -> Self {
        let cols_n = usize::from(cols);
        Self {
            lines: (0..rows).map(|_| vec![blank; cols_n]).collect(),
            graphics: Graphics::default(),
            cols,
            rows,
        }
    }
}

/// Snapshot of the visible area for rendering. Dirty rows avoid reuploading the whole grid.
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub full: bool,
    pub cells: Vec<Cell>,
    pub rows_dirty: Vec<(u16, Vec<Cell>)>,
    pub(crate) images: Vec<Placement>,
    pub cursor: Option<(u16, u16)>,
    pub(crate) composer: Option<[u16; 4]>,
    pub title: String,
    pub title_changed: bool,
    pub suspended: bool,
    pub changed: bool,
    /// Cells changed. A history view or a pet frame does not set this.
    pub content_changed: bool,
    pub scroll_top: f32,
    pub scroll_h: f32,
    /// Absolute line shown in the first visible row. See [`Term::view_top`].
    pub view_top: u64,
}

pub struct Term {
    primary: Screen,
    alt: Screen,
    alt_on: bool,
    pen: Pen,
    cursor: Cursor,
    saved: Cursor,
    primary_cursor: Cursor,
    scroll_top: u16,
    scroll_bot: u16,
    scrollback: VecDeque<Vec<Cell>>,
    line_pool: Vec<Vec<Cell>>,
    /// Packed scrollback records waiting for the NPU thread. Filled when a
    /// line leaves the top, so the reader never clones the row.
    pending: Vec<[u8; 64]>,
    view_offset: usize,
    /// Lines that left the front of the scrollback. Absolute line numbers stay
    /// valid while history is trimmed.
    dropped: u64,
    title: String,
    title_dirty: bool,
    dirty: Vec<bool>,
    graphics_dirty: bool,
    damage_all: bool,
    suspend: bool,
    autowrap: bool,
    cursor_visible: bool,
    last_snapshot_cursor: Option<(u16, u16)>,
    app_cursor: bool,
    origin_mode: bool,
    pending_wrap: bool,
    bracketed_paste: bool,
    mouse_normal: bool,
    mouse_button: bool,
    mouse_any: bool,
    mouse_sgr: bool,
    mouse_pixels: bool,
    /// Wheel sends cursor keys while the alternate screen is up.
    alt_scroll: bool,
    parser: Parser,
    replies: Vec<u8>,
    cols: u16,
    rows: u16,
    default_fg: Rgb,
    default_bg: Rgb,
}

impl Term {
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let blank = Cell::blank(DEFAULT_FG.pack(), DEFAULT_BG.pack());
        Self {
            primary: Screen::new(cols, rows, blank),
            alt: Screen::new(cols, rows, blank),
            alt_on: false,
            pen: Pen::reset(DEFAULT_FG, DEFAULT_BG),
            cursor: Cursor { row: 0, col: 0 },
            saved: Cursor { row: 0, col: 0 },
            primary_cursor: Cursor { row: 0, col: 0 },
            scroll_top: 0,
            scroll_bot: rows - 1,
            scrollback: VecDeque::new(),
            line_pool: Vec::new(),
            pending: Vec::new(),
            view_offset: 0,
            dropped: 0,
            title: "Rustty".to_string(),
            title_dirty: true,
            dirty: vec![true; usize::from(rows)],
            graphics_dirty: false,
            damage_all: true,
            suspend: false,
            autowrap: true,
            cursor_visible: true,
            last_snapshot_cursor: None,
            app_cursor: false,
            origin_mode: false,
            pending_wrap: false,
            bracketed_paste: false,
            mouse_normal: false,
            mouse_button: false,
            mouse_any: false,
            mouse_sgr: false,
            mouse_pixels: false,
            alt_scroll: false,
            parser: Parser::default(),
            replies: Vec::new(),
            cols,
            rows,
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
        }
    }

    pub fn restyle(&mut self, fg: Rgb, bg: Rgb) {
        let old_fg = self.default_fg.pack();
        let old_bg = self.default_bg.pack();
        let new_fg = fg.pack();
        let new_bg = bg.pack();
        let repaint = |cell: &mut Cell| {
            if cell.fg == old_fg {
                cell.fg = new_fg;
            }
            if cell.bg == old_bg {
                cell.bg = new_bg;
            }
        };
        for line in &mut self.primary.lines {
            for cell in line {
                repaint(cell);
            }
        }
        for line in &mut self.alt.lines {
            for cell in line {
                repaint(cell);
            }
        }
        for line in &mut self.scrollback {
            for cell in line {
                repaint(cell);
            }
        }
        if self.pen.fg == self.default_fg {
            self.pen.fg = fg;
        }
        if self.pen.bg == self.default_bg {
            self.pen.bg = bg;
        }
        self.default_fg = fg;
        self.default_bg = bg;
        self.damage_all();
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn app_cursor(&self) -> bool {
        self.app_cursor
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn view_offset(&self) -> usize {
        self.view_offset
    }

    /// Absolute number of the line in the first visible row. Line numbers count
    /// from the first line this tab ever kept, so they survive scrolling and trimming.
    pub fn view_top(&self) -> u64 {
        let history = self.scrollback.len();
        self.dropped + (history - self.view_offset.min(history)) as u64
    }

    /// Colored rectangle of the active Codex composer, in cells.
    fn codex_composer(&self) -> Option<[u16; 4]> {
        if !self.cursor_visible || self.suspend || self.view_offset != 0 {
            return None;
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        let bg = self.cell(row, col).bg;
        if bg == self.default_bg.pack() {
            return None;
        }

        let mut left = col;
        while left > 0 && self.cell(row, left - 1).bg == bg {
            left -= 1;
        }
        let mut right = col + 1;
        while right < self.cols && self.cell(row, right).bg == bg {
            right += 1;
        }
        if right - left < 8 {
            return None;
        }

        // Thumbnails and styled spans can use another color inside the box.
        let same_edges = |row| {
            self.cell(row, left).bg == bg
                && self.cell(row, right - 1).bg == bg
                && (left == 0 || self.cell(row, left - 1).bg != bg)
                && (right == self.cols || self.cell(row, right).bg != bg)
        };
        let mut top = row;
        while top > 0 && same_edges(top - 1) {
            top -= 1;
        }
        let mut bottom = row + 1;
        while bottom < self.rows && same_edges(bottom) {
            bottom += 1;
        }
        let prompt = (top..=row).any(|row| {
            (left..left + 3).any(|col| {
                let cell = self.cell(row, col);
                matches!(char::from_u32(cell.cp), Some('›' | '!')) && cell.flags & BOLD != 0
            })
        });
        prompt.then_some([left, top, right, bottom])
    }

    pub fn advance(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (used, job) = self.advance_until_image(bytes);
            if let Some(job) = job {
                let image = decode_job(&job);
                self.finish_image(job, image);
            }
            if used == 0 {
                break;
            }
            bytes = &bytes[used..];
        }
    }

    /// Parse until the next image payload. Deletes and text are applied immediately.
    pub(crate) fn advance_until_image(&mut self, mut bytes: &[u8]) -> (usize, Option<DecodeJob>) {
        let mut consumed = 0;
        while !bytes.is_empty() {
            let (used, command) = {
                let mut parser = std::mem::take(&mut self.parser);
                let (rest, command) = parser.advance(self, bytes);
                let used = bytes.len() - rest.len();
                self.parser = parser;
                (used, command)
            };
            if used == 0 {
                break;
            }
            consumed += used;
            if let Some(command) = command
                && let Some(job) = self.prepare_graphics(&command)
            {
                return (consumed, Some(job));
            }
            bytes = &bytes[used..];
        }
        (consumed, None)
    }

    pub(crate) fn decode_image(job: &DecodeJob) -> Option<Image> {
        decode_job(job)
    }

    fn prepare_graphics(&mut self, bytes: &[u8]) -> Option<DecodeJob> {
        let prepared = {
            let screen = if self.alt_on { &mut self.alt } else { &mut self.primary };
            screen
                .graphics
                .command(bytes, self.cursor.row, self.cursor.col, &mut self.replies)
        };
        match prepared {
            Prepared::Update(update) => {
                self.apply_graphics_update(update);
                None
            }
            Prepared::Decode(job) => Some(job),
        }
    }

    pub(crate) fn finish_image(&mut self, job: DecodeJob, image: Option<Image>) {
        let update = {
            let screen = if self.alt_on { &mut self.alt } else { &mut self.primary };
            screen.graphics.finish(job, image, &mut self.replies)
        };
        self.apply_graphics_update(update);
    }

    fn apply_graphics_update(&mut self, update: Update) {
        match update {
            Update::Unchanged => {}
            Update::Changed => self.graphics_dirty = true,
            Update::MoveCursor { cols, rows } => {
                self.graphics_dirty = true;
                self.move_to(
                    self.cursor.row.saturating_add(rows - 1),
                    self.cursor.col.saturating_add(cols),
                );
            }
        }
    }

    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Records for lines that left the top since the last take. Each record is
    /// the 64-byte NPU entry, already hashed.
    pub fn take_index_records(&mut self) -> Vec<[u8; 64]> {
        std::mem::take(&mut self.pending)
    }

    pub fn damage_all(&mut self) {
        self.damage_all = true;
        self.dirty.fill(true);
    }

    /// Keeps the rows from a complete frame the renderer has yet to consume.
    pub(crate) fn keep_pending_damage(&mut self, pending: &Snapshot) {
        if self.suspend {
            return;
        }
        if pending.full {
            self.damage_all();
        } else {
            for (row, _) in &pending.rows_dirty {
                self.mark(*row);
            }
        }
    }

    pub fn alt_screen(&self) -> bool {
        self.alt_on
    }

    fn mouse_enabled(&self) -> bool {
        self.mouse_normal || self.mouse_button || self.mouse_any
    }

    /// Bytes for the app that owns this screen. `None` means the wheel scrolls this tab's history.
    /// `steps` follows winit: positive reveals older lines. `col` and `row` are 1-based.
    pub fn wheel_bytes(&self, steps: isize, col: u16, row: u16, pixel: (u32, u32)) -> Option<Vec<u8>> {
        if steps == 0 || !self.mouse_enabled() {
            return None;
        }
        let up = steps > 0;
        let n = steps.unsigned_abs().min(40);
        let button: u16 = if up { 64 } else { 65 };
        let (x, y) = if self.mouse_pixels {
            (pixel.0.max(1), pixel.1.max(1))
        } else {
            (u32::from(col.max(1)), u32::from(row.max(1)))
        };
        let mut out = Vec::new();
        for _ in 0..n {
            if self.mouse_sgr || self.mouse_pixels {
                let mut text = String::new();
                let _ = std::fmt::Write::write_fmt(
                    &mut text,
                    format_args!("\x1b[<{button};{x};{y}M\x1b[<{button};{x};{y}m"),
                );
                out.extend(text.into_bytes());
            } else {
                let b = (button as u8).saturating_add(32);
                let cx = (x.min(223) as u8).saturating_add(32);
                let cy = (y.min(223) as u8).saturating_add(32);
                out.extend_from_slice(&[0x1b, b'[', b'M', b, cx, cy]);
            }
        }
        Some(out)
    }

    /// Cursor keys for an app that asked to turn the wheel into arrows.
    fn arrow_bytes(&self, steps: isize) -> Vec<u8> {
        let up = steps > 0;
        let seq: &[u8] = if self.app_cursor {
            if up { b"\x1bOA" } else { b"\x1bOB" }
        } else if up {
            b"\x1b[A"
        } else {
            b"\x1b[B"
        };
        let n = steps.unsigned_abs().min(40) as usize;
        let mut out = Vec::with_capacity(seq.len() * n);
        for _ in 0..n {
            out.extend_from_slice(seq);
        }
        out
    }

    /// What this tab's wheel should do. `Some` goes to the pty. `None` scrolls this tab's history.
    pub fn wheel_write(&self, steps: isize, col: u16, row: u16, pixel: (u32, u32)) -> Option<Vec<u8>> {
        if let Some(bytes) = self.wheel_bytes(steps, col, row, pixel) {
            return Some(bytes);
        }
        if self.alt_on && self.alt_scroll {
            return Some(self.arrow_bytes(steps));
        }
        None
    }

    pub fn scroll_view(&mut self, delta: isize) {
        self.set_view((self.view_offset as isize + delta).clamp(0, self.scrollback.len() as isize) as usize);
    }

    /// `from_top` 0 shows the oldest span. 1 returns to the end of the session.
    pub fn scroll_to(&mut self, from_top: f32) {
        let max = self.scrollback.len() as f32;
        let offset = ((1.0 - from_top.clamp(0.0, 1.0)) * max).round() as usize;
        self.set_view(offset.min(self.scrollback.len()));
    }

    /// Thumb top and height, as fractions of the track. With no history the thumb
    /// fills the track, so the bar stays on screen.
    pub fn scroll_metrics(&self) -> (f32, f32) {
        let history = self.scrollback.len();
        // With nothing to scroll, or while the program takes the wheel for its own
        // view, this history says nothing about what is left to read. No thumb is drawn.
        let app_scrolls = self.mouse_enabled() || (self.alt_on && self.alt_scroll);
        if history == 0 || (app_scrolls && self.view_offset == 0) {
            return (0.0, 0.0);
        }
        let rows = usize::from(self.rows).max(1);
        let total = history + rows;
        let height = (rows as f32 / total as f32).clamp(0.08, 1.0);
        let start = total - self.view_offset.min(history) - rows;
        let top = (start as f32 / total as f32).min(1.0 - height);
        (top, height)
    }

    fn set_view(&mut self, next: usize) {
        if next != self.view_offset {
            self.view_offset = next;
            self.damage_all = true;
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }
        let blank = self.blank();
        resize_screen(&mut self.primary, cols, rows, blank);
        resize_screen(&mut self.alt, cols, rows, blank);
        self.cols = cols;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bot = rows - 1;
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.saved.row = self.saved.row.min(rows - 1);
        self.saved.col = self.saved.col.min(cols - 1);
        self.primary_cursor.row = self.primary_cursor.row.min(rows - 1);
        self.primary_cursor.col = self.primary_cursor.col.min(cols - 1);
        self.pending_wrap = false;
        self.dirty = vec![true; usize::from(rows)];
        self.damage_all = true;
        self.view_offset = 0;
    }

    pub fn snapshot(&mut self) -> Snapshot {
        let title_changed = self.title_dirty;
        self.title_dirty = false;
        if self.suspend {
            return Snapshot {
                cols: self.cols,
                rows: self.rows,
                full: false,
                cells: Vec::new(),
                rows_dirty: Vec::new(),
                images: Vec::new(),
                cursor: None,
                composer: None,
                title: if title_changed { self.title.clone() } else { String::new() },
                title_changed,
                suspended: true,
                changed: true,
                content_changed: false,
                scroll_top: 0.0,
                scroll_h: 0.0,
                view_top: self.view_top(),
            };
        }

        let show_history = self.view_offset > 0;
        let cursor = (self.cursor_visible && !show_history).then_some((self.cursor.col, self.cursor.row));
        let mut full = self.damage_all || show_history;
        if !full && self.view_offset == 0 {
            let dirty_count = self.dirty.iter().filter(|flag| **flag).count();
            if dirty_count * 2 >= usize::from(self.rows) {
                full = true;
            }
        }
        let cols = usize::from(self.cols);
        let mut cells = Vec::new();
        let mut rows_dirty = Vec::new();
        if full {
            cells.reserve(cols * usize::from(self.rows));
            for row in 0..self.rows {
                self.append_visible(row, &mut cells);
            }
        } else {
            let mut dirty_rows = Vec::new();
            for (row, dirty) in self.dirty.iter().enumerate() {
                if *dirty {
                    dirty_rows.push(row as u16);
                }
            }
            for row in dirty_rows {
                let mut line = Vec::with_capacity(cols);
                self.append_visible(row, &mut line);
                rows_dirty.push((row, line));
            }
        }
        let images = if show_history {
            Vec::new()
        } else {
            self.screen().graphics.placements.iter().cloned().collect()
        };
        let changed = full
            || !rows_dirty.is_empty()
            || title_changed
            || self.graphics_dirty
            || cursor != self.last_snapshot_cursor;
        let content_changed = self.damage_all || self.dirty.iter().any(|flag| *flag);
        let (scroll_top, scroll_h) = self.scroll_metrics();
        self.last_snapshot_cursor = cursor;
        self.damage_all = false;
        self.graphics_dirty = false;
        self.dirty.fill(false);
        Snapshot {
            cols: self.cols,
            rows: self.rows,
            full,
            cells,
            rows_dirty,
            images,
            cursor,
            composer: self.codex_composer(),
            title: if title_changed { self.title.clone() } else { String::new() },
            title_changed,
            suspended: false,
            changed,
            content_changed,
            scroll_top,
            scroll_h,
            view_top: self.view_top(),
        }
    }

    /// Address or file path covering this cell of the visible screen.
    /// A trailing `:line` or `:line:column` stays clickable, and the returned path omits it.
    pub fn link_at(&self, row: u16, col: u16) -> Option<String> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let mut cells = Vec::with_capacity(usize::from(self.cols));
        self.append_visible(row, &mut cells);
        url_covering(&cells, usize::from(col))
    }

    /// Text between two absolute positions `(col, line)`, in reading order.
    /// Lines come from the history or the screen, wherever they are now. Trailing spaces are dropped.
    pub fn selected_text(&self, anchor: (u16, u64), focus: (u16, u64)) -> String {
        let last_col = self.cols.max(1) - 1;
        let (lo, hi) = if (anchor.1, anchor.0) <= (focus.1, focus.0) { (anchor, focus) } else { (focus, anchor) };
        let mut lines = Vec::new();
        let mut cells = Vec::with_capacity(usize::from(self.cols));
        for line in lo.1..=hi.1 {
            let from = if line == lo.1 { lo.0.min(last_col) } else { 0 };
            let to = if line == hi.1 { hi.0.min(last_col) } else { last_col };
            cells.clear();
            self.append_line(line, &mut cells);
            let mut text = String::new();
            for glyph in glyphs(&cells) {
                if glyph.end < from || glyph.start > to {
                    continue;
                }
                text.push(glyph.ch);
            }
            while text.ends_with(' ') {
                text.pop();
            }
            lines.push(text);
        }
        lines.join("\n")
    }

    /// The word under this cell, or the cell itself when it is not part of a word.
    pub fn word_span(&self, col: u16, row: u16) -> ((u16, u16), (u16, u16)) {
        let glyphs = self.glyphs(row);
        let Some(index) = glyphs.iter().position(|glyph| glyph.start <= col && col <= glyph.end) else {
            return ((col, row), (col, row));
        };
        if !is_word(glyphs[index].ch) {
            let glyph = &glyphs[index];
            return ((glyph.start, row), (glyph.end, row));
        }
        let mut from = index;
        let mut to = index;
        while from > 0 && is_word(glyphs[from - 1].ch) {
            from -= 1;
        }
        while to + 1 < glyphs.len() && is_word(glyphs[to + 1].ch) {
            to += 1;
        }
        ((glyphs[from].start, row), (glyphs[to].end, row))
    }

    fn glyphs(&self, row: u16) -> Vec<Glyph> {
        let mut cells = Vec::new();
        self.append_visible(row, &mut cells);
        glyphs(&cells)
    }

    /// Cells of an absolute line. A line that was trimmed or lies past the screen reads blank.
    fn append_line(&self, line: u64, dst: &mut Vec<Cell>) {
        let cols = usize::from(self.cols);
        let blank = Cell::blank(self.default_fg.pack(), self.default_bg.pack());
        let history = self.scrollback.len();
        let index = line.checked_sub(self.dropped).and_then(|index| usize::try_from(index).ok());
        match index {
            Some(index) if index < history => {
                let src = &self.scrollback[index];
                let n = src.len().min(cols);
                dst.extend_from_slice(&src[..n]);
                dst.resize(dst.len() + (cols - n), blank);
            }
            Some(index) if index - history < usize::from(self.rows) => {
                dst.extend_from_slice(&self.screen().lines[index - history]);
            }
            _ => dst.resize(dst.len() + cols, blank),
        }
    }

    fn append_visible(&self, row: u16, dst: &mut Vec<Cell>) {
        let cols = usize::from(self.cols);
        if self.view_offset == 0 {
            dst.extend_from_slice(&self.screen().lines[usize::from(row)]);
            return;
        }
        let blank = Cell::blank(self.default_fg.pack(), self.default_bg.pack());
        let history = self.scrollback.len();
        let total = history + usize::from(self.rows);
        let start = total - self.view_offset - usize::from(self.rows);
        let index = start + usize::from(row);
        if index < history {
            let src = &self.scrollback[index];
            let n = src.len().min(cols);
            dst.extend_from_slice(&src[..n]);
            dst.resize(dst.len() + (cols - n), blank);
        } else {
            let live = (index - history) as u16;
            dst.extend_from_slice(&self.screen().lines[usize::from(live)]);
        }
    }

    fn screen(&self) -> &Screen {
        if self.alt_on { &self.alt } else { &self.primary }
    }

    fn screen_mut(&mut self) -> &mut Screen {
        if self.alt_on {
            &mut self.alt
        } else {
            &mut self.primary
        }
    }

    fn blank(&self) -> Cell {
        Cell::blank(self.pen.fg.pack(), self.pen.bg.pack())
    }

    fn mark(&mut self, row: u16) {
        if let Some(flag) = self.dirty.get_mut(usize::from(row)) {
            *flag = true;
        }
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        self.screen().lines[usize::from(row)][usize::from(col)]
    }

    fn write_cell(&mut self, row: u16, col: u16, cell: Cell) {
        self.screen_mut().lines[usize::from(row)][usize::from(col)] = cell;
        self.mark(row);
    }

    fn move_to(&mut self, row: u16, col: u16) {
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
        self.pending_wrap = false;
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.pending_wrap = false;
    }

    fn linefeed(&mut self) {
        self.pending_wrap = false;
        if self.cursor.row == self.scroll_bot {
            self.scroll_region(self.scroll_top, self.scroll_bot, 1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn scroll_region(&mut self, top: u16, bot: u16, lines: i16) {
        if bot < top || lines == 0 {
            return;
        }
        let rows = self.rows;
        self.screen_mut().graphics.scroll(top, bot, lines, rows);
        let height = usize::from(bot - top) + 1;
        let shift = (lines.unsigned_abs() as usize).min(height);
        if shift == 0 {
            return;
        }
        let blank = self.blank();
        let cols = usize::from(self.cols);
        let start = usize::from(top);
        let up = lines > 0;
        let keep_history = up && top == 0;
        let mut fresh = Vec::new();
        if keep_history {
            fresh.reserve(shift);
            for _ in 0..shift {
                let mut line = self.alloc_line();
                line.resize(cols, blank);
                fresh.push(line);
            }
        }
        let mut leaving = Vec::new();
        {
            let band = &mut self.screen_mut().lines[start..start + height];
            if up {
                if keep_history {
                    for (slot, line) in band[..shift].iter_mut().zip(fresh) {
                        leaving.push(std::mem::replace(slot, line));
                    }
                }
                band.rotate_left(shift);
                if !keep_history {
                    for line in &mut band[height - shift..] {
                        line.fill(blank);
                    }
                }
            } else {
                band.rotate_right(shift);
                for line in &mut band[..shift] {
                    line.fill(blank);
                }
            }
        }
        for line in leaving {
            self.remember(line);
        }
        for row in top..=bot {
            self.mark(row);
        }
    }

    fn alloc_line(&mut self) -> Vec<Cell> {
        let mut line = self
            .line_pool
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(usize::from(self.cols)));
        line.clear();
        line
    }

    fn recycle(&mut self, mut line: Vec<Cell>) {
        if self.line_pool.len() < 128 {
            line.clear();
            self.line_pool.push(line);
        }
    }

    fn remember(&mut self, line: Vec<Cell>) {
        self.pending.push(crate::npu::pack_entry(&line));
        self.scrollback.push_back(line);
        while self.scrollback.len() > MAX_SCROLLBACK {
            if let Some(old) = self.scrollback.pop_front() {
                self.dropped += 1;
                self.recycle(old);
            }
        }
        // A scrolled view is a distance from the live bottom. A new history line
        // would slide that view toward the output unless the distance grows with it.
        if self.view_offset > 0 {
            self.view_offset = (self.view_offset + 1).min(self.scrollback.len());
        }
    }

    fn forget_history(&mut self) {
        while let Some(line) = self.scrollback.pop_front() {
            self.dropped += 1;
            self.recycle(line);
        }
        self.pending.clear();
        self.view_offset = 0;
    }

    fn clear_span(&mut self, row: u16, from: u16, to: u16) {
        if from >= to || row >= self.rows {
            return;
        }
        let blank = self.blank();
        let end_col = usize::from(to.min(self.cols));
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line[usize::from(from)..end_col].fill(blank);
        self.mark(row);
    }

    fn clear_wide_at(&mut self, row: u16, col: u16) {
        let cell = self.cell(row, col);
        let blank = self.blank();
        if cell.flags & WIDE != 0 && col + 1 < self.cols {
            self.write_cell(row, col, blank);
            self.write_cell(row, col + 1, blank);
        } else if cell.flags & SPACER != 0 && col > 0 {
            self.write_cell(row, col - 1, blank);
            self.write_cell(row, col, blank);
        }
    }

    fn put_char(&mut self, ch: char) {
        let width = columns(ch);
        if width == 0 {
            return;
        }
        if self.pending_wrap && self.autowrap {
            self.carriage_return();
            self.linefeed();
        }
        if self.cursor.col + width > self.cols {
            if self.autowrap {
                self.carriage_return();
                self.linefeed();
            } else {
                return;
            }
        }
        self.clear_wide_at(self.cursor.row, self.cursor.col);
        if width == 2 && self.cursor.col + 1 < self.cols {
            self.clear_wide_at(self.cursor.row, self.cursor.col + 1);
        }
        let mut flags = self.pen.flags;
        if width == 2 {
            flags |= WIDE;
        }
        self.write_cell(
            self.cursor.row,
            self.cursor.col,
            Cell {
                cp: ch as u32,
                fg: self.pen.fg.pack(),
                bg: self.pen.bg.pack(),
                flags,
            },
        );
        if width == 2 && self.cursor.col + 1 < self.cols {
            self.write_cell(
                self.cursor.row,
                self.cursor.col + 1,
                Cell {
                    cp: 0,
                    fg: self.pen.fg.pack(),
                    bg: self.pen.bg.pack(),
                    flags: SPACER,
                },
            );
        }
        let next = self.cursor.col + width;
        if next >= self.cols {
            self.cursor.col = self.cols - width;
            self.pending_wrap = self.autowrap;
        } else {
            self.cursor.col = next;
            self.pending_wrap = false;
        }
    }

    fn reset_soft(&mut self) {
        let blank = Cell::blank(self.default_fg.pack(), self.default_bg.pack());
        self.pen = Pen::reset(self.default_fg, self.default_bg);
        self.primary = Screen::new(self.cols, self.rows, blank);
        self.alt = Screen::new(self.cols, self.rows, blank);
        self.alt_on = false;
        self.cursor = Cursor { row: 0, col: 0 };
        self.saved = self.cursor;
        self.primary_cursor = self.cursor;
        self.scroll_top = 0;
        self.scroll_bot = self.rows - 1;
        self.autowrap = true;
        self.cursor_visible = true;
        self.app_cursor = false;
        self.origin_mode = false;
        self.pending_wrap = false;
        self.bracketed_paste = false;
        self.mouse_normal = false;
        self.mouse_button = false;
        self.mouse_any = false;
        self.mouse_sgr = false;
        self.mouse_pixels = false;
        self.alt_scroll = false;
        self.suspend = false;
        self.damage_all();
    }

    fn set_alt(&mut self, enable: bool) {
        if enable == self.alt_on {
            return;
        }
        if enable {
            self.primary_cursor = self.cursor;
            let blank = self.blank();
            self.alt = Screen::new(self.cols, self.rows, blank);
            self.alt_on = true;
            self.cursor = Cursor { row: 0, col: 0 };
        } else {
            self.alt_on = false;
            self.alt.graphics = Graphics::default();
            self.cursor = self.primary_cursor;
        }
        self.pending_wrap = false;
        self.scroll_top = 0;
        self.scroll_bot = self.rows - 1;
        self.damage_all();
    }

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.pen = Pen::reset(self.default_fg, self.default_bg);
            return;
        }
        let mut stored = [&[][..]; 32];
        let mut count = 0;
        for group in params.iter() {
            if count == stored.len() {
                break;
            }
            stored[count] = group;
            count += 1;
        }
        let groups = &stored[..count];
        let mut index = 0;
        while index < groups.len() {
            let code = groups[index].first().copied().unwrap_or(0);
            match code {
                0 => self.pen = Pen::reset(self.default_fg, self.default_bg),
                1 => self.pen.flags |= BOLD,
                2 => self.pen.flags |= DIM,
                3 => self.pen.flags |= ITALIC,
                4 => self.pen.flags |= UNDERLINE,
                7 => self.pen.flags |= REVERSE,
                22 => self.pen.flags &= !(BOLD | DIM),
                23 => self.pen.flags &= !ITALIC,
                24 => self.pen.flags &= !UNDERLINE,
                27 => self.pen.flags &= !REVERSE,
                30..=37 => self.pen.fg = color::ansi((code - 30) as u8),
                40..=47 => self.pen.bg = color::ansi((code - 40) as u8),
                90..=97 => self.pen.fg = color::ansi((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = color::ansi((code - 100 + 8) as u8),
                39 => self.pen.fg = self.default_fg,
                49 => self.pen.bg = self.default_bg,
                38 | 48 => {
                    let foreground = code == 38;
                    if groups[index].len() > 1 {
                        apply_color(&mut self.pen, foreground, &groups[index][1..]);
                        index += 1;
                        continue;
                    }
                    let mode = groups.get(index + 1).and_then(|g| g.first()).copied().unwrap_or(0);
                    if mode == 5 {
                        let value = groups.get(index + 2).and_then(|g| g.first()).copied().unwrap_or(0);
                        let rgb = color::indexed(value as u8);
                        if foreground {
                            self.pen.fg = rgb;
                        } else {
                            self.pen.bg = rgb;
                        }
                        index += 3;
                        continue;
                    } else if mode == 2 {
                        let r = groups.get(index + 2).and_then(|g| g.first()).copied().unwrap_or(0);
                        let g = groups.get(index + 3).and_then(|g| g.first()).copied().unwrap_or(0);
                        let b = groups.get(index + 4).and_then(|g| g.first()).copied().unwrap_or(0);
                        let rgb = Rgb::new(r as u8, g as u8, b as u8);
                        if foreground {
                            self.pen.fg = rgb;
                        } else {
                            self.pen.bg = rgb;
                        }
                        index += 5;
                        continue;
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn dec_mode(&mut self, params: &Params, enable: bool) {
        if params.is_empty() {
            return;
        }
        for group in params.iter() {
            let mode = group.first().copied().unwrap_or(0);
            match mode {
                1 => self.app_cursor = enable,
                6 => self.origin_mode = enable,
                7 => self.autowrap = enable,
                25 => self.cursor_visible = enable,
                47 | 1047 | 1049 => self.set_alt(enable),
                1000 => self.mouse_normal = enable,
                1002 => self.mouse_button = enable,
                1003 => self.mouse_any = enable,
                1006 => self.mouse_sgr = enable,
                1007 => self.alt_scroll = enable,
                1016 => self.mouse_pixels = enable,
                2004 => self.bracketed_paste = enable,
                2026 => self.suspend = enable,
                _ => {}
            }
        }
    }
}

impl Perform for Term {
    fn print(&mut self, ch: char) {
        if self.put_ascii(ch) {
            return;
        }
        self.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.linefeed(),
            b'\r' => self.carriage_return(),
            0x08 => {
                if self.pending_wrap {
                    self.pending_wrap = false;
                } else if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                }
            }
            b'\t' => {
                let next = ((self.cursor.col / 8) + 1) * 8;
                self.cursor.col = next.min(self.cols.saturating_sub(1));
                if next >= self.cols {
                    self.pending_wrap = self.autowrap;
                    self.cursor.col = self.cols.saturating_sub(1);
                }
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        if intermediates == [b'?'] {
            match action {
                'h' => self.dec_mode(params, true),
                'l' => self.dec_mode(params, false),
                _ => {}
            }
            return;
        }
        if !intermediates.is_empty() {
            return;
        }
        let p = |index: usize, default: u16| -> u16 {
            params
                .iter()
                .nth(index)
                .and_then(|group| group.first().copied())
                .filter(|value| *value != 0)
                .unwrap_or(default)
        };
        let raw = |index: usize| -> u16 {
            params
                .iter()
                .nth(index)
                .and_then(|group| group.first().copied())
                .unwrap_or(0)
        };
        match action {
            'A' => {
                let n = p(0, 1);
                let floor = if self.origin_mode { self.scroll_top } else { 0 };
                let row = self.cursor.row.saturating_sub(n).max(floor);
                self.move_to(row, self.cursor.col);
            }
            'B' => {
                let n = p(0, 1);
                let ceil = if self.origin_mode { self.scroll_bot } else { self.rows - 1 };
                let row = self.cursor.row.saturating_add(n).min(ceil);
                self.move_to(row, self.cursor.col);
            }
            'C' => {
                let n = p(0, 1);
                let col = self.cursor.col.saturating_add(n).min(self.cols - 1);
                self.move_to(self.cursor.row, col);
            }
            'D' => {
                let n = p(0, 1);
                let col = self.cursor.col.saturating_sub(n);
                self.move_to(self.cursor.row, col);
            }
            'E' => {
                let n = p(0, 1);
                let row = self.cursor.row.saturating_add(n).min(self.rows - 1);
                self.move_to(row, 0);
            }
            'F' => {
                let n = p(0, 1);
                let row = self.cursor.row.saturating_sub(n);
                self.move_to(row, 0);
            }
            'G' => {
                let col = p(0, 1).saturating_sub(1).min(self.cols - 1);
                self.move_to(self.cursor.row, col);
            }
            'H' | 'f' => {
                let mut row = p(0, 1).saturating_sub(1);
                let col = p(1, 1).saturating_sub(1).min(self.cols - 1);
                if self.origin_mode {
                    row = self.scroll_top.saturating_add(row).min(self.scroll_bot);
                } else {
                    row = row.min(self.rows - 1);
                }
                self.move_to(row, col);
            }
            'J' => {
                let mode = raw(0);
                match mode {
                    0 => {
                        self.clear_span(self.cursor.row, self.cursor.col, self.cols);
                        for row in self.cursor.row + 1..self.rows {
                            self.clear_span(row, 0, self.cols);
                        }
                    }
                    1 => {
                        for row in 0..self.cursor.row {
                            self.clear_span(row, 0, self.cols);
                        }
                        self.clear_span(self.cursor.row, 0, self.cursor.col.saturating_add(1));
                    }
                    2 | 3 => {
                        if mode == 3 {
                            self.forget_history();
                        }
                        let blank = self.blank();
                        let screen = self.screen_mut();
                        for line in &mut screen.lines {
                            line.fill(blank);
                        }
                        screen.graphics = Graphics::default();
                        self.dirty.fill(true);
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = raw(0);
                match mode {
                    0 => self.clear_span(self.cursor.row, self.cursor.col, self.cols),
                    1 => self.clear_span(self.cursor.row, 0, self.cursor.col.saturating_add(1)),
                    2 => self.clear_span(self.cursor.row, 0, self.cols),
                    _ => {}
                }
            }
            'L' => {
                if (self.scroll_top..=self.scroll_bot).contains(&self.cursor.row) {
                    self.scroll_region(self.cursor.row, self.scroll_bot, -(p(0, 1) as i16));
                }
            }
            'M' => {
                if (self.scroll_top..=self.scroll_bot).contains(&self.cursor.row) {
                    self.scroll_region(self.cursor.row, self.scroll_bot, p(0, 1) as i16);
                }
            }
            'P' => self.delete_cells(p(0, 1)),
            'S' => self.scroll_region(self.scroll_top, self.scroll_bot, p(0, 1) as i16),
            'T' => self.scroll_region(self.scroll_top, self.scroll_bot, -(p(0, 1) as i16)),
            'X' => {
                let n = p(0, 1);
                let end = self.cursor.col.saturating_add(n).min(self.cols);
                self.clear_span(self.cursor.row, self.cursor.col, end);
            }
            '@' => self.insert_cells(p(0, 1)),
            'd' => {
                let row = p(0, 1).saturating_sub(1).min(self.rows - 1);
                self.move_to(row, self.cursor.col);
            }
            'm' => self.sgr(params),
            'r' => {
                let top = p(0, 1).saturating_sub(1);
                let bot = params
                    .iter()
                    .nth(1)
                    .and_then(|group| group.first().copied())
                    .filter(|value| *value != 0)
                    .unwrap_or(self.rows)
                    .saturating_sub(1)
                    .min(self.rows - 1);
                if top < bot {
                    self.scroll_top = top;
                    self.scroll_bot = bot;
                    self.move_to(if self.origin_mode { top } else { 0 }, 0);
                }
            }
            's' => self.saved = self.cursor,
            'u' => self.move_to(self.saved.row, self.saved.col),
            'n' => {
                if raw(0) == 6 {
                    let row = self.cursor.row + 1;
                    let col = self.cursor.col + 1;
                    let reply = format!("\u{1b}[{row};{col}R");
                    self.replies.extend_from_slice(reply.as_bytes());
                }
            }
            'c' => self.replies.extend_from_slice(b"\x1b[?6c"),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = self.cursor,
            b'8' => self.move_to(self.saved.row, self.saved.col),
            b'c' => self.reset_soft(),
            b'D' => self.linefeed(),
            b'M' => {
                if self.cursor.row == self.scroll_top {
                    self.scroll_region(self.scroll_top, self.scroll_bot, -1);
                } else if self.cursor.row > 0 {
                    self.move_to(self.cursor.row - 1, self.cursor.col);
                }
            }
            b'E' => {
                self.carriage_return();
                self.linefeed();
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.len() < 2 {
            return;
        }
        let color = match params[0] {
            b"10" if params[1] == b"?" => Some((10, self.default_fg)),
            b"11" if params[1] == b"?" => Some((11, self.default_bg)),
            _ => None,
        };
        if let Some((code, color)) = color {
            let [r, g, b] = [color.r, color.g, color.b].map(|value| u16::from(value) * 257);
            self.replies.extend_from_slice(
                format!("\x1b]{code};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\").as_bytes(),
            );
        }
        if params[0] == b"0" || params[0] == b"2" {
            let title = String::from_utf8_lossy(params[1]);
            let title = title.trim().to_string();
            if !title.is_empty() && title != self.title {
                self.title = title;
                self.title_dirty = true;
            }
        }
    }
}

impl Term {
    fn delete_cells(&mut self, n: u16) {
        let row = self.cursor.row;
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols.saturating_sub(col));
        if n == 0 {
            return;
        }
        let blank = self.blank();
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line.copy_within(col + n..cols, col);
        line[cols - n..].fill(blank);
        self.mark(row);
    }

    fn insert_cells(&mut self, n: u16) {
        let row = self.cursor.row;
        let col = usize::from(self.cursor.col);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols.saturating_sub(col));
        if n == 0 {
            return;
        }
        let blank = self.blank();
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line.copy_within(col..cols - n, col + n);
        line[col..col + n].fill(blank);
        self.mark(row);
    }

    /// Common path: one-column ASCII, with no wrap and no wide glyph underneath.
    #[inline(always)]
    fn put_ascii(&mut self, ch: char) -> bool {
        let byte = ch as u32;
        if byte < 0x20 || byte >= 0x7f || self.pending_wrap {
            return false;
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        if col + 1 >= self.cols {
            return false;
        }
        let row_i = usize::from(row);
        let col_i = usize::from(col);
        if self.screen().lines[row_i][col_i].flags & (WIDE | SPACER) != 0 {
            return false;
        }
        self.screen_mut().lines[row_i][col_i] = Cell {
            cp: byte,
            fg: self.pen.fg.pack(),
            bg: self.pen.bg.pack(),
            flags: self.pen.flags,
        };
        self.cursor.col = col + 1;
        self.mark(row);
        true
    }
}

struct Glyph {
    start: u16,
    end: u16,
    ch: char,
}

fn glyphs(cells: &[Cell]) -> Vec<Glyph> {
    let mut glyphs = Vec::new();
    let mut col = 0u16;
    for cell in cells {
        if cell.flags & SPACER != 0 {
            continue;
        }
        let width: u16 = if cell.flags & WIDE != 0 { 2 } else { 1 };
        let ch = char::from_u32(cell.cp)
            .filter(|ch| cell.cp != 0 && !ch.is_control())
            .unwrap_or(' ');
        let end = col.saturating_add(width.saturating_sub(1));
        glyphs.push(Glyph { start: col, end, ch });
        col = col.saturating_add(width);
    }
    glyphs
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric()
        || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@' | '+' | '%' | '~' | '?' | '=' | '&' | '#')
}

/// Columns the glyph occupies. Private-use glyphs (Nerd Font / Oh My Posh) are
/// always 1: unicode-width marks that range as ambiguous, and the prompt counts
/// each diamond as one column.
fn columns(ch: char) -> u16 {
    if is_private_use(ch) {
        return 1;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0) as u16
}

fn url_covering(cells: &[Cell], col: usize) -> Option<String> {
    let mut text = String::new();
    let mut columns = Vec::new();
    let mut cursor = 0usize;
    for cell in cells {
        if cell.flags & SPACER != 0 {
            cursor += 1;
            continue;
        }
        let ch = char::from_u32(cell.cp).filter(|ch| !ch.is_control()).unwrap_or(' ');
        let width = if cell.flags & WIDE != 0 { 2 } else { 1 };
        columns.push((text.len(), cursor, cursor + width));
        text.push(ch);
        cursor += width;
    }
    let mut index = 0;
    while index < text.len() {
        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        if matches!(ch, '"' | '\'' | '`') {
            let start = index + ch.len_utf8();
            if let Some(rel) = text[start..].find(ch) {
                let end = start + rel;
                if let Some(target) = classify_link(&text[start..end]) {
                    if covers(&columns, start, end, col) {
                        return Some(target);
                    }
                }
                index = end + ch.len_utf8();
                continue;
            }
        }
        if is_link_delim(ch) {
            index += ch.len_utf8();
            continue;
        }
        let start = index;
        let mut end = start;
        for ch in text[start..].chars() {
            if is_link_delim(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        if let Some(target) = classify_link(&text[start..end]) {
            if covers(&columns, start, end, col) {
                return Some(target);
            }
        }
        index = end.max(start + 1);
    }
    None
}

fn is_link_delim(ch: char) -> bool {
    ch.is_whitespace()
        || ch.is_control()
        || matches!(ch, '<' | '>' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | ',' | ';')
}

fn covers(columns: &[(usize, usize, usize)], start: usize, end: usize, col: usize) -> bool {
    let mut from = None;
    let mut to = 0usize;
    for &(byte, col_from, col_to) in columns {
        if byte >= start && byte < end {
            if from.is_none() {
                from = Some(col_from);
            }
            to = col_to;
        }
    }
    from.is_some_and(|from| (from..to).contains(&col))
}

/// URL, or a file path with a trailing `:line` or `:line:column` removed.
fn classify_link(raw: &str) -> Option<String> {
    let token = raw.trim_end_matches(['.', ',', '!', '?', ';']);
    if token.starts_with("https://") || token.starts_with("http://") {
        let scheme = if token.starts_with("https://") { "https://".len() } else { "http://".len() };
        return (token.len() > scheme).then(|| token.to_string());
    }
    if let Some(path) = file_url_path(token) {
        return is_file_token(&path).then_some(path);
    }
    let located = strip_location(token);
    let path = located.trim_end_matches(['.', ',', '!', '?', ';', ':']);
    is_file_token(path).then(|| path.to_string())
}

fn strip_location(token: &str) -> &str {
    let once = strip_number(token);
    let twice = once.and_then(strip_number);
    if let Some(path) = twice {
        if is_file_token(path) {
            return path;
        }
    }
    if let Some(path) = once {
        if is_file_token(path) {
            return path;
        }
    }
    token
}

fn strip_number(token: &str) -> Option<&str> {
    let (head, tail) = token.rsplit_once(':')?;
    (!tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit())).then_some(head)
}

fn file_url_path(token: &str) -> Option<String> {
    let rest = token.strip_prefix("file://")?;
    let path = if let Some(stripped) = rest.strip_prefix("//") {
        stripped
    } else if rest.starts_with('/') {
        rest
    } else if let Some((host, path)) = rest.split_once('/') {
        if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
            path
        } else {
            return None;
        }
    } else {
        return None;
    };
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    Some(percent_decode(&path))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&input[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_file_token(path: &str) -> bool {
    if path.is_empty() || path.contains("://") || path.starts_with("//") || path.contains('\0') {
        return false;
    }
    if matches!(path, "." | ".." | "~" | "/") {
        return false;
    }
    if path.starts_with("~/") || path.starts_with('/') || path.contains('/') {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if let Some((stem, ext)) = name.rsplit_once('.') {
        if !stem.is_empty()
            && !ext.is_empty()
            && ext.len() <= 10
            && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return true;
        }
    }
    name.starts_with('.') && name.len() > 1 && !name.contains(['/', ' '])
}

fn is_private_use(ch: char) -> bool {
    matches!(
        ch,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
    )
}

fn apply_color(pen: &mut Pen, foreground: bool, spec: &[u16]) {
    let Some(mode) = spec.first().copied() else {
        return;
    };
    let rgb = if mode == 5 {
        color::indexed(spec.get(1).copied().unwrap_or(0) as u8)
    } else if mode == 2 {
        Rgb::new(
            spec.get(1).copied().unwrap_or(0) as u8,
            spec.get(2).copied().unwrap_or(0) as u8,
            spec.get(3).copied().unwrap_or(0) as u8,
        )
    } else {
        return;
    };
    if foreground {
        pen.fg = rgb;
    } else {
        pen.bg = rgb;
    }
}

fn resize_screen(screen: &mut Screen, cols: u16, rows: u16, blank: Cell) {
    let mut next = Screen::new(cols, rows, blank);
    let copy_rows = usize::from(screen.rows.min(rows));
    let copy_cols = usize::from(screen.cols.min(cols));
    for row in 0..copy_rows {
        next.lines[row][..copy_cols].copy_from_slice(&screen.lines[row][..copy_cols]);
    }
    next.graphics = std::mem::take(&mut screen.graphics);
    *screen = next;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_default_colors_for_terminal_applications() {
        let mut term = Term::new(8, 4);
        term.restyle(Rgb::new(237, 234, 228), Rgb::new(19, 22, 25));
        term.advance(b"\x1b]10;?\x1b\\\x1b]11;?\x07");
        assert_eq!(term.take_replies(), b"\x1b]10;rgb:eded/eaea/e4e4\x1b\\\x1b]11;rgb:1313/1616/1919\x1b\\");
    }

    #[test]
    fn codex_composer_tracks_the_field_with_a_preview_and_hides_with_the_cursor() {
        let mut term = Term::new(12, 5);
        term.advance(concat!(
            "\x1b[2;2H\x1b[48;2;45;47;51m         ",
            "\x1b[3;2H\x1b[1m›\x1b[22m        ",
            "\x1b[4;2H         ",
            "\x1b[2;4H\x1b[48;2;70;72;76m#\x1b[0m\x1b[4;7H",
        ).as_bytes());
        let visible = term.snapshot();
        assert_eq!(visible.composer, Some([1, 1, 10, 4]));
        term.advance(b"\x1b[?25l");
        let hidden = term.snapshot();
        assert_eq!(hidden.composer, None);
        assert_eq!(hidden.cursor, None);
        assert!(hidden.changed && !hidden.content_changed);
        term.advance(b"\x1b[?25h\x1b[5;1H");
        let moved = term.snapshot();
        assert_eq!(moved.composer, None);
        assert_eq!(moved.cursor, Some((0, 4)));
        assert!(moved.changed && !moved.content_changed);
    }

    fn record_first(entry: &[u8; 64]) -> u32 {
        u32::from_le_bytes(entry[12..16].try_into().unwrap())
    }

    fn cell_char(cell: &Cell) -> char {
        char::from_u32(cell.cp).filter(|_| cell.cp != 0).unwrap_or(' ')
    }

    fn text(term: &Term) -> String {
        let mut out = String::new();
        for row in 0..term.rows {
            for col in 0..term.cols {
                let cell = term.cell(row, col);
                if cell.flags & SPACER != 0 {
                    continue;
                }
                if let Some(ch) = char::from_u32(cell.cp) {
                    if cell.cp != 0 {
                        out.push(ch);
                    }
                }
            }
            if row + 1 != term.rows {
                out.push('\n');
            }
        }
        out.trim_end_matches('\n').to_string()
    }

    #[test]
    fn prints_and_wraps() {
        let mut term = Term::new(4, 3);
        term.advance(b"abcdef");
        assert_eq!(text(&term), "abcd\nef");
        assert_eq!(term.cursor.row, 1);
    }

    #[test]
    fn crlf_goes_to_next_line() {
        let mut term = Term::new(8, 3);
        term.advance(b"ab\r\ncd");
        assert_eq!(text(&term), "ab\ncd");
    }

    #[test]
    fn cup_and_sgr() {
        let mut term = Term::new(10, 4);
        term.advance(b"\x1b[2;3H\x1b[31mZ");
        assert_eq!(term.cell(1, 2).cp, u32::from(b'Z'));
        assert_eq!(term.cell(1, 2).fg, color::ansi(1).pack());
    }

    #[test]
    fn sgr_reset_returns_to_the_selected_theme() {
        let mut term = Term::new(10, 2);
        let theme = color::theme(0);
        term.restyle(theme.fg, theme.bg);
        term.advance(b"\x1b[31;44mA\x1b[0mB");
        assert_eq!(term.cell(0, 0).fg, color::ansi(1).pack());
        assert_eq!(term.cell(0, 1).fg, theme.fg.pack());
        assert_eq!(term.cell(0, 1).bg, theme.bg.pack());
    }

    #[test]
    fn truecolor_and_erase() {
        let mut term = Term::new(6, 2);
        term.advance(b"\x1b[38;2;1;2;3mABC\x1b[0K");
        assert_eq!(term.cell(0, 0).fg, Rgb::new(1, 2, 3).pack());
        assert_eq!(term.cell(0, 3).cp, 0);
    }

    #[test]
    fn alt_screen_scrolls_its_own_history() {
        let mut term = Term::new(4, 2);
        term.advance(b"\x1b[?1049haaaa\r\nbbbb\r\ncccc");
        assert!(term.alt_screen());
        assert_eq!(text(&term), "bbbb\ncccc");
        assert!(term.wheel_write(1, 1, 1, (1, 1)).is_none());
        term.scroll_view(1);
        let snap = term.snapshot();
        let row0: String = snap.cells.iter().take(4).map(cell_char).collect();
        assert_eq!(row0, "aaaa");
        assert!(snap.scroll_h > 0.0);
    }

    #[test]
    fn mouse_wheel_is_reported_for_the_program_that_asked() {
        let mut term = Term::new(8, 4);
        term.advance(b"\x1b[?1000h\x1b[?1006h");
        let up = term.wheel_write(1, 3, 2, (9, 8)).expect("sgr");
        assert_eq!(up, b"\x1b[<64;3;2M\x1b[<64;3;2m");
        term.advance(b"\x1b[?1006l");
        let legacy = term.wheel_write(-1, 1, 1, (1, 1)).expect("x10");
        assert_eq!(legacy, b"\x1b[Ma!!");
        term.advance(b"\x1b[?1000l\x1b[?1007h\x1b[?1049h");
        assert_eq!(term.wheel_write(2, 1, 1, (1, 1)).as_deref(), Some(&b"\x1b[A\x1b[A"[..]));
    }

    #[test]
    fn thumb_appears_with_history_and_shrinks_as_it_grows() {
        let mut term = Term::new(10, 4);
        assert_eq!(term.scroll_metrics(), (0.0, 0.0));
        assert_eq!(term.snapshot().scroll_h, 0.0);

        term.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8");
        let (top, short) = term.scroll_metrics();
        assert!((short - 0.5).abs() < 1e-6, "{short}");
        assert!((top - 0.5).abs() < 1e-6, "{top}");
        for _ in 0..40 {
            term.advance(b"\r\nline");
        }
        let (top, long) = term.scroll_metrics();
        assert!(long < short / 4.0, "{long}");
        assert!((top + long - 1.0).abs() < 1e-6, "{top} {long}");
    }

    #[test]
    fn no_thumb_while_the_program_scrolls_its_own_view() {
        let mut term = Term::new(4, 2);
        term.advance(b"aaaa\r\nbbbb\r\ncccc");
        assert!(term.scroll_metrics().1 > 0.0);
        term.advance(b"\x1b[?1049h\x1b[?1007h");
        assert_eq!(term.scroll_metrics(), (0.0, 0.0));
        term.scroll_view(1);
        assert!(term.scroll_metrics().1 > 0.0);
        term.scroll_view(-1);
        term.advance(b"\x1b[?1007l\x1b[?1049l\x1b[?1000h");
        assert_eq!(term.scroll_metrics(), (0.0, 0.0));
    }

    #[test]
    fn spinner_and_streamed_output_leave_the_selected_text_alone() {
        let mut term = Term::new(12, 3);
        term.advance("answer one\r\nanswer two\r\n⠋ Thinking".as_bytes());
        let selection = ((0, 0), (9, 1));
        assert_eq!(term.selected_text(selection.0, selection.1), "answer one\nanswer two");
        for frame in ["⠙", "⠹", "⠸"] {
            term.advance(format!("\r{frame} Thinking").as_bytes());
            assert_eq!(term.selected_text(selection.0, selection.1), "answer one\nanswer two");
        }
        term.advance(b"\r\x1b[Kmore\r\nlines\r\nhere");
        assert_eq!(term.view_top(), 2);
        assert_eq!(term.selected_text(selection.0, selection.1), "answer one\nanswer two");
    }

    #[test]
    fn selection_keeps_its_lines_while_the_view_scrolls_and_history_is_trimmed() {
        let mut term = Term::new(4, 2);
        term.advance(b"aaaa\r\nbbbb\r\ncccc");
        assert_eq!(term.view_top(), 1);
        term.scroll_view(1);
        assert_eq!(term.view_top(), 0);
        assert_eq!(term.selected_text((2, 0), (1, 2)), "aa\nbbbb\ncc");
        term.scroll_view(-1);
        term.advance(b"\x1b[3J\x1b[Hdddd");
        assert_eq!(term.view_top(), 1);
        assert_eq!(term.selected_text((0, 0), (3, 1)), "\ndddd");
    }

    #[test]
    fn streaming_output_leaves_a_scrolled_view_where_it_is() {
        let mut term = Term::new(4, 2);
        term.advance(b"aaaa\r\nbbbb\r\ncc");
        term.scroll_view(1);
        term.advance(b"\x1b[2;3H");
        assert_eq!(term.view_offset(), 1);
        term.advance(b"X");
        assert_eq!(term.view_offset(), 1);
        let snap = term.snapshot();
        let row0: String = snap.cells.iter().take(4).map(cell_char).collect();
        assert_eq!(row0, "aaaa");

        term.advance(b"\r\ndddd");
        assert_eq!(term.view_offset(), 2);
        let snap = term.snapshot();
        let row0: String = snap.cells.iter().take(4).map(cell_char).collect();
        assert_eq!(row0, "aaaa");

        term.scroll_to(1.0);
        assert_eq!(term.view_offset(), 0);
        assert_eq!(text(&term), "ccX\ndddd");
    }

    #[test]
    fn scroll_view_reveals_the_line_that_left() {
        let mut term = Term::new(4, 2);
        term.advance(b"aaaa\r\nbbbb\r\ncccc");
        assert_eq!(text(&term), "bbbb\ncccc");
        term.scroll_view(1);
        let snap = term.snapshot();
        let row0: String = snap.cells.iter().take(4).map(cell_char).collect();
        assert_eq!(row0, "aaaa");
        assert!(snap.scroll_h > 0.0);
        term.scroll_to(1.0);
        let snap = term.snapshot();
        let row0: String = snap.cells.iter().take(4).map(cell_char).collect();
        assert_eq!(row0, "bbbb");
        assert!(snap.scroll_top > 0.0);
    }

    #[test]
    fn scroll_pushes_history() {
        let mut term = Term::new(3, 2);
        term.advance(b"aaa\r\nbbb\r\nccc");
        assert_eq!(text(&term), "bbb\nccc");
        let indexed = term.take_index_records();
        assert_eq!(indexed.len(), 1);
        assert_eq!(record_first(&indexed[0]), u32::from(b'a'));
    }

    #[test]
    fn region_scroll_below_the_top_keeps_the_row_above() {
        let mut term = Term::new(3, 3);
        term.advance(b"aaa\r\nbbb\r\nccc\r\nddd");
        assert_eq!(text(&term), "bbb\nccc\nddd");
        term.advance(b"\x1b[2;3r\x1b[3;1H\x1b[S");
        assert_eq!(text(&term), "bbb\nddd");
        let indexed = term.take_index_records();
        assert_eq!(indexed.len(), 1);
        assert_eq!(record_first(&indexed[0]), u32::from(b'a'));
    }

    #[test]
    fn region_scroll_from_the_top_indexes_every_line_and_keeps_the_status() {
        let mut term = Term::new(6, 4);
        term.advance(b"\x1b[1;3r\x1b[4;1H######\x1b[1;1H");
        for _ in 0..300 {
            term.advance(b"ab\r\n");
        }
        assert_eq!(term.cell(3, 0).cp, u32::from(b'#'));
        assert_eq!(term.cell(3, 5).cp, u32::from(b'#'));
        let indexed = term.take_index_records();
        assert_eq!(indexed.len(), 298);
        assert_eq!(record_first(&indexed[0]), u32::from(b'a'));
    }

    #[test]
    fn insert_line_opens_a_blank_row_without_indexing_it() {
        let mut term = Term::new(4, 3);
        term.advance(b"aaaa\r\nbbbb\r\ncccc\x1b[1;1H\x1b[L");
        assert_eq!(text(&term), "\naaaa\nbbbb");
        assert!(term.take_index_records().is_empty());
    }

    #[test]
    fn selection_reads_across_lines_and_wide_characters() {
        let mut term = Term::new(6, 2);
        term.advance(b"ab cd\r\nxy");
        assert_eq!(term.selected_text((1, 0), (0, 1)), "b cd\nx");
        assert_eq!(term.word_span(3, 0), ((3, 0), (4, 0)));
        let mut wide = Term::new(4, 1);
        wide.advance("你a".as_bytes());
        assert_eq!(wide.selected_text((1, 0), (2, 0)), "你a");
        assert_eq!(wide.word_span(1, 0), ((0, 0), (2, 0)));
    }

    #[test]
    fn selection_reads_the_scrolled_line() {
        let mut term = Term::new(4, 1);
        term.advance(b"abcd\r\nefgh");
        term.scroll_view(1);
        assert_eq!(term.selected_text((0, 0), (3, 0)), "abcd");
    }

    #[test]
    fn bracketed_paste_follows_the_mode() {
        let mut term = Term::new(8, 2);
        assert!(!term.bracketed_paste());
        term.advance(b"\x1b[?2004h");
        assert!(term.bracketed_paste());
        term.advance(b"\x1b[?2004l");
        assert!(!term.bracketed_paste());
    }

    #[test]
    fn wide_char_uses_spacer() {
        let mut term = Term::new(4, 1);
        term.advance("你".as_bytes());
        assert!(term.cell(0, 0).flags & WIDE != 0);
        assert!(term.cell(0, 1).flags & SPACER != 0);
        assert_eq!(term.cursor.col, 2);
    }

    #[test]
    fn alt_screen_and_cursor_report() {
        let mut term = Term::new(8, 4);
        term.advance(b"hi\x1b[?1049h\x1b[6n");
        assert!(term.alt_on);
        assert_eq!(term.cell(0, 0).cp, 0);
        assert_eq!(term.take_replies(), b"\x1b[1;1R");
        term.advance(b"\x1b[?1049l");
        assert!(!term.alt_on);
        assert_eq!(term.cell(0, 0).cp, u32::from(b'h'));
    }

    #[test]
    fn click_on_a_url_returns_that_address() {
        let mut term = Term::new(80, 2);
        term.advance(b"veja https://github.com/gutopardini/rustty. e http://exemplo.test/a)\n");
        assert_eq!(term.link_at(0, 4), None);
        assert_eq!(
            term.link_at(0, 6).as_deref(),
            Some("https://github.com/gutopardini/rustty")
        );
        let second = term.link_at(0, 46).expect("second address");
        assert_eq!(second, "http://exemplo.test/a");
        assert_eq!(term.link_at(0, 70), None);
    }

    #[test]
    fn click_on_a_path_returns_the_file_without_the_line() {
        let mut term = Term::new(80, 2);
        term.advance(b"veja src/app.rs:120:4 e /tmp/notas.md.\n");
        assert_eq!(term.link_at(0, 5).as_deref(), Some("src/app.rs"));
        assert_eq!(term.link_at(0, 16).as_deref(), Some("src/app.rs"));
        assert_eq!(term.link_at(0, 24).as_deref(), Some("/tmp/notas.md"));
        term.advance(b"\r\n`README.md` \"~/proj/a b.rs\" file:///tmp/a%20b.txt");
        assert_eq!(term.link_at(1, 1).as_deref(), Some("README.md"));
        assert_eq!(term.link_at(1, 14).as_deref(), Some("~/proj/a b.rs"));
        assert_eq!(term.link_at(1, 32).as_deref(), Some("/tmp/a b.txt"));
    }

    #[test]
    fn osc_title() {
        let mut term = Term::new(8, 2);
        term.advance(b"\x1b]2;Rustty\x07");
        assert_eq!(term.title, "Rustty");
    }

    #[test]
    fn oh_my_posh_diamonds_keep_truecolor() {
        let mut term = Term::new(80, 4);
        // Bash consumes \[ \] before writing to the PTY. What arrives is SGR + PUA.
        term.advance(
            b"\x1b[32m\xee\x83\x87\x1b[0m\x1b[42m\x1b[38;2;255;255;255m rustty \x1b[0m\x1b[32m\xee\x83\x86\x1b[0m ",
        );
        assert_eq!(columns('\u{e0b6}'), 1);
        assert_eq!(columns('\u{e0c6}'), 1);
        assert_eq!(columns('\u{e0c7}'), 1);
        assert_eq!(term.cell(0, 0).cp, u32::from('\u{e0c7}'));
        assert_eq!(term.cell(0, 0).fg, color::ansi(2).pack());
        assert_eq!(term.cell(0, 0).bg, DEFAULT_BG.pack());
        assert_eq!(term.cell(0, 2).cp, u32::from(b'r'));
        assert_eq!(term.cell(0, 2).fg, Rgb::new(255, 255, 255).pack());
        assert_eq!(term.cell(0, 2).bg, color::ansi(2).pack());
        assert_eq!(term.cell(0, 9).cp, u32::from('\u{e0c6}'));
        assert_eq!(term.cell(0, 9).fg, color::ansi(2).pack());
        assert_eq!(term.cursor.col, 11);
    }
}
