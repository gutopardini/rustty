use std::collections::VecDeque;
use std::sync::Arc;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Cell, BOLD, DIM, ITALIC, REVERSE, SPACER, UNDERLINE, WIDE};
use crate::color::{self, Rgb, DEFAULT_BG, DEFAULT_FG};
use crate::graphics::{decode_job, DecodeJob, Graphics, Image, Parser, Placement, Prepared, Update};

const MAX_SCROLLBACK: usize = 10_000;
const MAX_LINK_WRAP_LINES: usize = 64;
const MAX_OSC8_URI_LEN: usize = 8192;

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

#[derive(Clone, Debug, PartialEq, Eq)]
struct HyperlinkSpan {
    start: u16,
    end: u16,
    uri: Arc<str>,
}

struct Line {
    cells: Vec<Cell>,
    /// True when output autowrapped from the end of this line onto the next line.
    wrapped: bool,
    /// Explicit OSC 8 hyperlink spans, sorted and non-overlapping. Empty on plain lines.
    links: Vec<HyperlinkSpan>,
}

impl Line {
    fn new(cols: usize, blank: Cell) -> Self {
        Self {
            cells: vec![blank; cols],
            wrapped: false,
            links: Vec::new(),
        }
    }

    fn fill(&mut self, blank: Cell) {
        self.cells.fill(blank);
        self.wrapped = false;
        self.links.clear();
    }

    fn link_at(&self, col: u16) -> Option<&str> {
        self.links
            .iter()
            .find(|span| span.start <= col && col < span.end)
            .map(|span| span.uri.as_ref())
    }

    fn clear_links(&mut self, from: u16, to: u16) {
        if self.links.is_empty() || from >= to {
            return;
        }
        let mut index = 0;
        while index < self.links.len() {
            let span = &self.links[index];
            if span.end <= from || span.start >= to {
                index += 1;
                continue;
            }
            if span.start >= from && span.end <= to {
                self.links.remove(index);
            } else if span.start < from && span.end > to {
                let tail = HyperlinkSpan {
                    start: to,
                    end: span.end,
                    uri: Arc::clone(&span.uri),
                };
                self.links[index].end = from;
                self.links.insert(index + 1, tail);
                index += 2;
            } else if span.start < from {
                self.links[index].end = from;
                index += 1;
            } else {
                self.links[index].start = to;
                index += 1;
            }
        }
    }

    fn set_link(&mut self, from: u16, to: u16, uri: &Arc<str>) {
        if from >= to {
            return;
        }
        self.clear_links(from, to);
        if let Some(last) = self.links.last_mut()
            && last.end == from
            && last.uri == *uri
        {
            last.end = to;
            return;
        }
        let pos = self.links.partition_point(|span| span.start < from);
        self.links.insert(
            pos,
            HyperlinkSpan {
                start: from,
                end: to,
                uri: Arc::clone(uri),
            },
        );
        if pos + 1 < self.links.len()
            && self.links[pos].end == self.links[pos + 1].start
            && self.links[pos].uri == self.links[pos + 1].uri
        {
            self.links[pos].end = self.links[pos + 1].end;
            self.links.remove(pos + 1);
        }
        if pos > 0
            && self.links[pos - 1].end == self.links[pos].start
            && self.links[pos - 1].uri == self.links[pos].uri
        {
            self.links[pos - 1].end = self.links[pos].end;
            self.links.remove(pos);
        }
    }

    fn shift_links_left(&mut self, col: u16, n: u16) {
        if self.links.is_empty() || n == 0 {
            return;
        }
        self.clear_links(col, col.saturating_add(n));
        for span in &mut self.links {
            if span.start >= col.saturating_add(n) {
                span.start -= n;
                span.end -= n;
            }
        }
        self.merge_adjacent_links();
    }

    fn shift_links_right(&mut self, col: u16, n: u16, cols: u16) {
        if self.links.is_empty() || n == 0 {
            return;
        }
        let mut next = Vec::with_capacity(self.links.len() + 1);
        for span in self.links.drain(..) {
            if span.end <= col {
                next.push(span);
            } else if span.start < col {
                next.push(HyperlinkSpan {
                    start: span.start,
                    end: col,
                    uri: Arc::clone(&span.uri),
                });
                let start = col.saturating_add(n);
                let end = span.end.saturating_add(n).min(cols);
                if start < end {
                    next.push(HyperlinkSpan {
                        start,
                        end,
                        uri: span.uri,
                    });
                }
            } else {
                let start = span.start.saturating_add(n);
                let end = span.end.saturating_add(n).min(cols);
                if start < end {
                    next.push(HyperlinkSpan {
                        start,
                        end,
                        uri: span.uri,
                    });
                }
            }
        }
        self.links = next;
    }

    fn merge_adjacent_links(&mut self) {
        let mut index = 0;
        while index + 1 < self.links.len() {
            if self.links[index].end == self.links[index + 1].start
                && self.links[index].uri == self.links[index + 1].uri
            {
                self.links[index].end = self.links[index + 1].end;
                self.links.remove(index + 1);
            } else {
                index += 1;
            }
        }
    }
}

struct Screen {
    /// One owned row per visual line. Scrolling rotates these vectors, so a
    /// linefeed copies the row that leaves and not the rows that stay.
    lines: Vec<Line>,
    graphics: Graphics,
    cols: u16,
    rows: u16,
}

impl Screen {
    fn new(cols: u16, rows: u16, blank: Cell) -> Self {
        let cols_n = usize::from(cols);
        Self {
            lines: (0..rows).map(|_| Line::new(cols_n, blank)).collect(),
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
    scrollback: VecDeque<Line>,
    line_pool: Vec<Line>,
    /// Active OSC 8 hyperlink URI applied to printed cells until closed.
    hyperlink: Option<Arc<str>>,
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
    insert_mode: bool,
    pending_wrap: bool,
    last_char: Option<char>,
    tab_stops: Vec<bool>,
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
            hyperlink: None,
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
            insert_mode: false,
            pending_wrap: false,
            last_char: None,
            tab_stops: default_tab_stops(cols),
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
            for cell in &mut line.cells {
                repaint(cell);
            }
        }
        for line in &mut self.alt.lines {
            for cell in &mut line.cells {
                repaint(cell);
            }
        }
        for line in &mut self.scrollback {
            for cell in &mut line.cells {
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

    /// The live screen belongs to the program. A selection dragged past the grid has to
    /// scroll that program: Claude keeps the rest of the transcript and only redraws the
    /// viewport, so this tab's history does not contain the hidden lines.
    /// `None` means `scroll_view` is the right move, including while the user is already
    /// reading this tab's own history.
    pub fn selection_scroll(&self, steps: isize, col: u16, row: u16, pixel: (u32, u32)) -> Option<Vec<u8>> {
        if steps == 0 || self.view_offset != 0 {
            return None;
        }
        if !(self.mouse_enabled() || (self.alt_on && self.alt_scroll)) {
            return None;
        }
        self.wheel_write(steps, col, row, pixel)
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
        resize_tab_stops(&mut self.tab_stops, cols);
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
    /// Follows OSC 8 hyperlinks, soft-wrapped logical lines, and TUI-wrapped multi-line URLs.
    pub fn link_at(&self, row: u16, col: u16) -> Option<String> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let target = self.visible_index(row);
        let line = self.line_at(target)?;
        if let Some(uri) = line.link_at(col) {
            return classify_osc8(uri);
        }
        let (first, last) = self.soft_wrapped_span(target);
        if first < last {
            let mut combined = Vec::new();
            let mut target_col = None;
            for idx in first..=last {
                let Some(part) = self.line_at(idx) else {
                    return None;
                };
                let mut slice = part.cells.as_slice();
                if idx < last && slice.last().is_some_and(|cell| cell.is_empty()) {
                    slice = &slice[..slice.len() - 1];
                }
                if idx == target {
                    let col = usize::from(col);
                    if col >= slice.len() {
                        return None;
                    }
                    target_col = Some(combined.len() + col);
                }
                combined.extend_from_slice(slice);
            }
            return target_col.and_then(|offset| url_covering(&combined, offset));
        }
        if let Some(url) = self.hard_wrapped_url_at(target, usize::from(col)) {
            return Some(url);
        }
        url_covering(&line.cells, usize::from(col))
    }

    fn total_lines(&self) -> usize {
        self.scrollback.len() + usize::from(self.rows)
    }

    fn visible_index(&self, row: u16) -> usize {
        let history = self.scrollback.len();
        let start = history.saturating_sub(self.view_offset);
        start + usize::from(row)
    }

    fn line_at(&self, index: usize) -> Option<&Line> {
        let history = self.scrollback.len();
        if index < history {
            self.scrollback.get(index)
        } else {
            self.screen().lines.get(index - history)
        }
    }

    fn soft_wrapped_span(&self, target: usize) -> (usize, usize) {
        let mut first = target;
        let min_first = target.saturating_sub(MAX_LINK_WRAP_LINES);
        while first > min_first {
            if self.line_at(first - 1).is_some_and(|line| line.wrapped) {
                first -= 1;
            } else {
                break;
            }
        }
        let total = self.total_lines();
        let max_last = (target + MAX_LINK_WRAP_LINES).min(total.saturating_sub(1));
        let mut last = target;
        while last < max_last {
            if self.line_at(last).is_some_and(|line| line.wrapped) {
                last += 1;
            } else {
                break;
            }
        }
        (first, last)
    }

    /// Reconstructs a multi-line `http://` / `https://` URL wrapped explicitly by a TUI
    /// (with cursor moves or newlines rather than terminal autowrap), including indented
    /// or box-bordered layouts.
    fn hard_wrapped_url_at(&self, target: usize, col: usize) -> Option<String> {
        let target_line = self.line_at(target)?;
        let (_, hovered_ch) = cell_char_at(&target_line.cells, col)?;
        if is_link_delim(hovered_ch) || is_vertical_border(target_line.cells[col]) {
            return None;
        }
        let left_border = (0..col)
            .rev()
            .find(|&c| is_vertical_border(target_line.cells[c]))
            .map(|c| (c, target_line.cells[c].cp));
        let right_border = ((col + 1)..target_line.cells.len())
            .find(|&c| is_vertical_border(target_line.cells[c]))
            .map(|c| (c, target_line.cells[c].cp));
        let region_lo = left_border.map_or(0, |(c, _)| c + 1);
        let region_hi = right_border.map_or(target_line.cells.len(), |(c, _)| c);
        if region_hi <= region_lo + 8 {
            return None;
        }

        let target_frame = frame_slice(target_line, region_lo, region_hi, left_border, right_border)?;
        if col < target_frame.start_abs || col >= target_frame.end_abs {
            return None;
        }
        let (tok_start, tok_end) =
            token_covering_col(&target_line.cells, target_frame.start_abs, target_frame.end_abs, col)?;
        let tok_text = token_text(&target_line.cells, tok_start, tok_end);

        let (start_idx, url_start_abs) = if starts_with_scheme(&tok_text) && tok_end == target_frame.end_abs {
            (target, tok_start)
        } else if is_scheme_prefix(&tok_text) && tok_end == target_frame.end_abs {
            let next_line = self.line_at(target + 1)?;
            let next_frame = frame_slice(next_line, region_lo, region_hi, left_border, right_border)?;
            let next_tok_end = leading_token_end(&next_line.cells, next_frame.start_abs, next_frame.end_abs)?;
            let next_head = token_text(&next_line.cells, next_frame.start_abs, next_tok_end);
            if starts_with_scheme(&format!("{tok_text}{next_head}")) {
                (target, tok_start)
            } else {
                return None;
            }
        } else if tok_start == target_frame.start_abs && !starts_with_scheme(&tok_text) {
            let cont_indent = target_frame.indent;
            let mut cur = target;
            let mut cur_head = tok_text;
            let mut found = None;
            while cur > 0 && target - cur < MAX_LINK_WRAP_LINES {
                let prev_idx = cur - 1;
                let Some(prev_line) = self.line_at(prev_idx) else {
                    break;
                };
                if prev_line.wrapped {
                    break;
                }
                let Some(prev_frame) =
                    frame_slice(prev_line, region_lo, region_hi, left_border, right_border)
                else {
                    break;
                };
                let Some(prev_tok_start) =
                    trailing_token_start(&prev_line.cells, prev_frame.start_abs, prev_frame.end_abs)
                else {
                    break;
                };
                let prev_tail = token_text(&prev_line.cells, prev_tok_start, prev_frame.end_abs);
                let starts_here = starts_with_scheme(&prev_tail)
                    || (is_scheme_prefix(&prev_tail)
                        && starts_with_scheme(&format!("{prev_tail}{cur_head}")));
                if starts_here {
                    let indent_ok =
                        region_lo + cont_indent <= prev_tok_start || prev_frame.indent == cont_indent;
                    if indent_ok {
                        found = Some((prev_idx, prev_tok_start));
                    }
                    break;
                }
                if prev_frame.indent == cont_indent && prev_tok_start == prev_frame.start_abs {
                    cur_head = prev_tail;
                    cur = prev_idx;
                } else {
                    break;
                }
            }
            found?
        } else {
            return None;
        };

        let start_line = self.line_at(start_idx)?;
        if start_line.wrapped {
            return None;
        }
        let start_frame = frame_slice(start_line, region_lo, region_hi, left_border, right_border)?;
        let mut wrap_col = start_frame.content_end;

        let second_idx = start_idx + 1;
        let second_line = self.line_at(second_idx)?;
        let second_frame = frame_slice(second_line, region_lo, region_hi, left_border, right_border)?;
        let cont_indent = second_frame.indent;
        if !(region_lo + cont_indent <= url_start_abs || start_frame.indent == cont_indent) {
            return None;
        }
        let mut last_tok_end =
            leading_token_end(&second_line.cells, second_frame.start_abs, second_frame.end_abs)?;
        let second_head = token_text(&second_line.cells, second_frame.start_abs, last_tok_end);
        if starts_with_scheme(&second_head) {
            return None;
        }
        if last_tok_end == second_frame.end_abs && second_frame.content_end > wrap_col {
            wrap_col = second_frame.content_end;
        }

        let mut frames = vec![(start_idx, start_frame), (second_idx, second_frame)];
        while frames.len() <= MAX_LINK_WRAP_LINES {
            let (cur_idx, cur_frame) = *frames.last().expect("non-empty");
            let Some(cur_line) = self.line_at(cur_idx) else {
                break;
            };
            let next_idx = cur_idx + 1;
            let Some(next_line) = self.line_at(next_idx) else {
                break;
            };
            let Some(next_frame) =
                frame_slice(next_line, region_lo, region_hi, left_border, right_border)
            else {
                break;
            };
            if last_tok_end != cur_frame.end_abs
                || cur_line.wrapped
                || next_frame.indent != cont_indent
            {
                break;
            }
            let Some(next_tok_end) =
                leading_token_end(&next_line.cells, next_frame.start_abs, next_frame.end_abs)
            else {
                break;
            };
            let next_head = token_text(&next_line.cells, next_frame.start_abs, next_tok_end);
            if starts_with_scheme(&next_head) {
                break;
            }
            let reached_wrap = cur_frame.content_end.abs_diff(wrap_col) <= 2
                || broke_at_url_sep(cur_line, cur_frame, &next_head, wrap_col);
            if !reached_wrap {
                break;
            }
            if next_tok_end == next_frame.end_abs && next_frame.content_end > wrap_col {
                wrap_col = next_frame.content_end;
            }
            frames.push((next_idx, next_frame));
            last_tok_end = next_tok_end;
        }

        let end_idx = frames.last()?.0;
        if target < start_idx || target > end_idx {
            return None;
        }

        // Verify the hovered column lies inside the URL span on `target`.
        let hovered_in_url = if target == start_idx {
            col >= url_start_abs && col < start_frame.end_abs
        } else if target == end_idx {
            let end_frame = frames.last()?.1;
            col >= end_frame.start_abs && col < last_tok_end
        } else {
            let mid_frame = frames[target - start_idx].1;
            col >= mid_frame.start_abs && col < mid_frame.end_abs
        };
        if !hovered_in_url {
            return None;
        }

        // Anti-false-positive wrap margin check: all wrapped lines must reach the wrap margin
        // (or break at a URL separator where the next segment did not fit), and that margin must
        // either reach the region edge or repeat across 3+ lines.
        let wrapped_lines = &frames[..frames.len() - 1];
        for (pos, &(idx, frame)) in wrapped_lines.iter().enumerate() {
            if frame.content_end.abs_diff(wrap_col) <= 2 {
                continue;
            }
            let cur_line = self.line_at(idx)?;
            let (next_idx, next_frame) = frames[pos + 1];
            let next_line = self.line_at(next_idx)?;
            let next_end = if pos + 1 == frames.len() - 1 {
                last_tok_end
            } else {
                next_frame.end_abs
            };
            let next_head = token_text(&next_line.cells, next_frame.start_abs, next_end);
            if !broke_at_url_sep(cur_line, frame, &next_head, wrap_col) {
                return None;
            }
        }
        let tail_tok_end_rel = last_tok_end.saturating_sub(region_lo);
        if tail_tok_end_rel > wrap_col + 2 {
            return None;
        }
        let region_width = region_hi - region_lo;
        let hits_edge = if right_border.is_some() {
            wrap_col + cont_indent.min(4) + 2 >= region_width
        } else if cont_indent == 0 {
            wrap_col + 1 >= region_width
        } else {
            wrap_col + cont_indent + 2 >= region_width
        };
        let repeated_column = frames.len() >= 3
            && wrap_col.saturating_sub(cont_indent) >= 40
            && wrapped_lines
                .iter()
                .filter(|(_, frame)| frame.content_end.abs_diff(wrap_col) <= 1)
                .count()
                >= 2;
        if !hits_edge && !repeated_column {
            return None;
        }

        let mut joined = String::new();
        for (pos, &(idx, frame)) in frames.iter().enumerate() {
            let line = self.line_at(idx)?;
            let from = if pos == 0 { url_start_abs } else { frame.start_abs };
            let to = if pos + 1 == frames.len() { last_tok_end } else { frame.end_abs };
            joined.push_str(&token_text(&line.cells, from, to));
        }
        classify_link(&joined).filter(|url| starts_with_scheme(url))
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

    /// Plain text of the recent viewport and scrollback tail (up to `max_lines` lines), used to
    /// capture on-screen quota summaries (e.g. `agy /usage`) even if they scroll up slightly.
    pub fn recent_text(&self, max_lines: usize) -> String {
        let bottom = self.dropped + (self.scrollback.len() + usize::from(self.rows)).saturating_sub(1) as u64;
        let span = (max_lines.max(usize::from(self.rows)) as u64).saturating_sub(1);
        let start = bottom.saturating_sub(span).max(self.dropped);
        let last_col = self.cols.saturating_sub(1);
        self.selected_text((0, start), (last_col, bottom))
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
                let src = &self.scrollback[index].cells;
                let n = src.len().min(cols);
                dst.extend_from_slice(&src[..n]);
                dst.resize(dst.len() + (cols - n), blank);
            }
            Some(index) if index - history < usize::from(self.rows) => {
                dst.extend_from_slice(&self.screen().lines[index - history].cells);
            }
            _ => dst.resize(dst.len() + cols, blank),
        }
    }

    fn append_visible(&self, row: u16, dst: &mut Vec<Cell>) {
        let cols = usize::from(self.cols);
        if self.view_offset == 0 {
            dst.extend_from_slice(&self.screen().lines[usize::from(row)].cells);
            return;
        }
        let blank = Cell::blank(self.default_fg.pack(), self.default_bg.pack());
        let history = self.scrollback.len();
        let total = history + usize::from(self.rows);
        let start = total - self.view_offset - usize::from(self.rows);
        let index = start + usize::from(row);
        if index < history {
            let src = &self.scrollback[index].cells;
            let n = src.len().min(cols);
            dst.extend_from_slice(&src[..n]);
            dst.resize(dst.len() + (cols - n), blank);
        } else {
            let live = (index - history) as u16;
            dst.extend_from_slice(&self.screen().lines[usize::from(live)].cells);
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
        self.screen().lines[usize::from(row)].cells[usize::from(col)]
    }

    fn write_cell(&mut self, row: u16, col: u16, cell: Cell) {
        self.screen_mut().lines[usize::from(row)].cells[usize::from(col)] = cell;
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
        let row = usize::from(self.cursor.row);
        self.screen_mut().lines[row].wrapped = false;
        self.linefeed_inner();
    }

    fn wrap_line(&mut self) {
        let row = usize::from(self.cursor.row);
        self.screen_mut().lines[row].wrapped = true;
        self.carriage_return();
        self.linefeed_inner();
    }

    fn linefeed_inner(&mut self) {
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
                line.cells.resize(cols, blank);
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

    fn alloc_line(&mut self) -> Line {
        let mut line = self.line_pool.pop().unwrap_or_else(|| Line {
            cells: Vec::with_capacity(usize::from(self.cols)),
            wrapped: false,
            links: Vec::new(),
        });
        line.cells.clear();
        line.wrapped = false;
        line.links.clear();
        line
    }

    fn recycle(&mut self, mut line: Line) {
        if self.line_pool.len() < 128 {
            line.cells.clear();
            line.wrapped = false;
            line.links.clear();
            self.line_pool.push(line);
        }
    }

    fn remember(&mut self, line: Line) {
        self.pending.push(crate::npu::pack_entry(&line.cells));
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
        let end_col = to.min(self.cols);
        let cols = self.cols;
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line.cells[usize::from(from)..usize::from(end_col)].fill(blank);
        line.clear_links(from, end_col);
        if end_col >= cols {
            line.wrapped = false;
        }
        self.mark(row);
    }

    fn clear_wide_at(&mut self, row: u16, col: u16) {
        let cell = self.cell(row, col);
        let blank = self.blank();
        if cell.flags & WIDE != 0 && col + 1 < self.cols {
            self.write_cell(row, col, blank);
            self.write_cell(row, col + 1, blank);
            self.screen_mut().lines[usize::from(row)].clear_links(col, col + 2);
        } else if cell.flags & SPACER != 0 && col > 0 {
            self.write_cell(row, col - 1, blank);
            self.write_cell(row, col, blank);
            self.screen_mut().lines[usize::from(row)].clear_links(col - 1, col + 1);
        }
    }

    fn put_char(&mut self, ch: char) {
        let width = columns(ch);
        if width == 0 {
            return;
        }
        if self.pending_wrap && self.autowrap {
            self.wrap_line();
        }
        if self.cursor.col + width > self.cols {
            if self.autowrap {
                self.wrap_line();
            } else {
                return;
            }
        }
        if self.insert_mode {
            self.insert_cells(width);
        }
        self.clear_wide_at(self.cursor.row, self.cursor.col);
        if width == 2 && self.cursor.col + 1 < self.cols {
            self.clear_wide_at(self.cursor.row, self.cursor.col + 1);
        }
        let mut flags = self.pen.flags;
        if width == 2 {
            flags |= WIDE;
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        self.write_cell(
            row,
            col,
            Cell {
                cp: ch as u32,
                fg: self.pen.fg.pack(),
                bg: self.pen.bg.pack(),
                flags,
            },
        );
        if width == 2 && col + 1 < self.cols {
            self.write_cell(
                row,
                col + 1,
                Cell {
                    cp: 0,
                    fg: self.pen.fg.pack(),
                    bg: self.pen.bg.pack(),
                    flags: SPACER,
                },
            );
        }
        let link_end = col.saturating_add(width).min(self.cols);
        if let Some(uri) = self.hyperlink.clone() {
            self.screen_mut().lines[usize::from(row)].set_link(col, link_end, &uri);
        } else {
            self.screen_mut().lines[usize::from(row)].clear_links(col, link_end);
        }
        let next = col + width;
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
        self.hyperlink = None;
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
        self.insert_mode = false;
        self.pending_wrap = false;
        self.last_char = None;
        self.tab_stops = default_tab_stops(self.cols);
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
        self.hyperlink = None;
        self.last_char = None;
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
    fn soft_reset_modes(&mut self) {
        self.pen = Pen::reset(self.default_fg, self.default_bg);
        self.hyperlink = None;
        self.last_char = None;
        self.scroll_top = 0;
        self.scroll_bot = self.rows - 1;
        self.autowrap = true;
        self.cursor_visible = true;
        self.app_cursor = false;
        self.origin_mode = false;
        self.insert_mode = false;
        self.pending_wrap = false;
        self.tab_stops = default_tab_stops(self.cols);
    }

    fn tab_forward(&mut self, count: u16) {
        self.pending_wrap = false;
        let last_col = self.cols.saturating_sub(1);
        for _ in 0..count.max(1) {
            if self.cursor.col >= last_col {
                self.cursor.col = last_col;
                break;
            }
            let mut next = usize::from(self.cursor.col) + 1;
            let limit = usize::from(last_col);
            while next < limit && !self.tab_stops.get(next).copied().unwrap_or(false) {
                next += 1;
            }
            self.cursor.col = next as u16;
        }
    }

    fn tab_backward(&mut self, count: u16) {
        self.pending_wrap = false;
        for _ in 0..count.max(1) {
            if self.cursor.col == 0 {
                break;
            }
            let mut prev = usize::from(self.cursor.col) - 1;
            while prev > 0 && !self.tab_stops.get(prev).copied().unwrap_or(false) {
                prev -= 1;
            }
            self.cursor.col = prev as u16;
        }
    }
}

impl Perform for Term {
    fn print(&mut self, ch: char) {
        self.last_char = Some(ch);
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
            b'\t' => self.tab_forward(1),
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
        if intermediates == [b'!'] && action == 'p' {
            self.soft_reset_modes();
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
                let floor = if self.origin_mode || self.cursor.row >= self.scroll_top {
                    self.scroll_top
                } else {
                    0
                };
                let row = self.cursor.row.saturating_sub(n).max(floor);
                self.move_to(row, self.cursor.col);
            }
            'B' | 'e' => {
                let n = p(0, 1);
                let ceil = if self.origin_mode || self.cursor.row <= self.scroll_bot {
                    self.scroll_bot
                } else {
                    self.rows - 1
                };
                let row = self.cursor.row.saturating_add(n).min(ceil);
                self.move_to(row, self.cursor.col);
            }
            'C' | 'a' => {
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
            'G' | '`' => {
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
            'I' => self.tab_forward(p(0, 1)),
            'Z' => self.tab_backward(p(0, 1)),
            'b' => {
                if let Some(ch) = self.last_char {
                    let max = usize::from(self.cols).saturating_mul(usize::from(self.rows));
                    let n = usize::from(p(0, 1)).min(max);
                    for _ in 0..n {
                        self.print(ch);
                    }
                }
            }
            'g' => match raw(0) {
                0 => {
                    if let Some(slot) = self.tab_stops.get_mut(usize::from(self.cursor.col)) {
                        *slot = false;
                    }
                }
                3 => self.tab_stops.fill(false),
                _ => {}
            },
            'h' | 'l' => {
                let enable = action == 'h';
                for group in params.iter() {
                    if group.first().copied() == Some(4) {
                        self.insert_mode = enable;
                    }
                }
            }
            'J' => {
                self.pending_wrap = false;
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
                self.pending_wrap = false;
                let mode = raw(0);
                match mode {
                    0 => self.clear_span(self.cursor.row, self.cursor.col, self.cols),
                    1 => self.clear_span(self.cursor.row, 0, self.cursor.col.saturating_add(1)),
                    2 => self.clear_span(self.cursor.row, 0, self.cols),
                    _ => {}
                }
            }
            'L' => {
                self.pending_wrap = false;
                if (self.scroll_top..=self.scroll_bot).contains(&self.cursor.row) {
                    self.cursor.col = 0;
                    self.scroll_region(self.cursor.row, self.scroll_bot, -(p(0, 1) as i16));
                }
            }
            'M' => {
                self.pending_wrap = false;
                if (self.scroll_top..=self.scroll_bot).contains(&self.cursor.row) {
                    self.cursor.col = 0;
                    self.scroll_region(self.cursor.row, self.scroll_bot, p(0, 1) as i16);
                }
            }
            'P' => {
                self.pending_wrap = false;
                self.delete_cells(p(0, 1));
            }
            'S' => self.scroll_region(self.scroll_top, self.scroll_bot, p(0, 1) as i16),
            'T' => self.scroll_region(self.scroll_top, self.scroll_bot, -(p(0, 1) as i16)),
            'X' => {
                self.pending_wrap = false;
                let n = p(0, 1);
                let end = self.cursor.col.saturating_add(n).min(self.cols);
                self.clear_span(self.cursor.row, self.cursor.col, end);
            }
            '@' => {
                self.pending_wrap = false;
                self.insert_cells(p(0, 1));
            }
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
            b'H' => {
                if let Some(slot) = self.tab_stops.get_mut(usize::from(self.cursor.col)) {
                    *slot = true;
                }
            }
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
        if params[0] == b"8" {
            self.hyperlink = parse_osc8_uri(params);
            return;
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
        let col_u16 = self.cursor.col;
        let col = usize::from(col_u16);
        let cols = usize::from(self.cols);
        let n = usize::from(n).min(cols.saturating_sub(col));
        if n == 0 {
            return;
        }
        let blank = self.blank();
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line.cells.copy_within(col + n..cols, col);
        line.cells[cols - n..].fill(blank);
        line.shift_links_left(col_u16, n as u16);
        line.wrapped = false;
        self.mark(row);
    }

    fn insert_cells(&mut self, n: u16) {
        let row = self.cursor.row;
        let col_u16 = self.cursor.col;
        let col = usize::from(col_u16);
        let cols_u16 = self.cols;
        let cols = usize::from(cols_u16);
        let n = usize::from(n).min(cols.saturating_sub(col));
        if n == 0 {
            return;
        }
        let blank = self.blank();
        let line = &mut self.screen_mut().lines[usize::from(row)];
        line.cells.copy_within(col..cols - n, col + n);
        line.cells[col..col + n].fill(blank);
        line.shift_links_right(col_u16, n as u16, cols_u16);
        line.wrapped = false;
        self.mark(row);
    }

    /// Common path: one-column ASCII, with no wrap and no wide glyph underneath.
    #[inline(always)]
    fn put_ascii(&mut self, ch: char) -> bool {
        let byte = ch as u32;
        if byte < 0x20 || byte >= 0x7f || self.pending_wrap || self.insert_mode || self.hyperlink.is_some() {
            return false;
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        if col + 1 >= self.cols {
            return false;
        }
        let row_i = usize::from(row);
        let col_i = usize::from(col);
        let line = &self.screen().lines[row_i];
        if line.cells[col_i].flags & (WIDE | SPACER) != 0 || !line.links.is_empty() {
            return false;
        }
        self.screen_mut().lines[row_i].cells[col_i] = Cell {
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
    for (index, cell) in cells.iter().enumerate() {
        if cell.flags & SPACER != 0 {
            continue;
        }
        let ch = char::from_u32(cell.cp).filter(|ch| !ch.is_control()).unwrap_or(' ');
        let width = if cell.flags & WIDE != 0 { 2 } else { 1 };
        columns.push((text.len(), index, index + width));
        text.push(ch);
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

#[derive(Clone, Copy)]
struct FrameLine {
    indent: usize,
    content_end: usize,
    start_abs: usize,
    end_abs: usize,
}

fn is_blank_cell(cell: Cell) -> bool {
    (cell.cp == 0 || cell.cp == u32::from(b' ')) && (cell.flags & (WIDE | SPACER) == 0)
}

fn is_vertical_border(cell: Cell) -> bool {
    matches!(char::from_u32(cell.cp), Some('│' | '┃' | '║'))
}

fn cell_char_at(cells: &[Cell], col: usize) -> Option<(usize, char)> {
    let cell = *cells.get(col)?;
    let pos = if cell.flags & SPACER != 0 {
        col.checked_sub(1)?
    } else {
        col
    };
    let cell = *cells.get(pos)?;
    if cell.cp == 0 {
        return None;
    }
    let ch = char::from_u32(cell.cp).filter(|ch| !ch.is_control())?;
    Some((pos, ch))
}

fn frame_slice(
    line: &Line,
    region_lo: usize,
    region_hi: usize,
    left_border: Option<(usize, u32)>,
    right_border: Option<(usize, u32)>,
) -> Option<FrameLine> {
    if let Some((col, cp)) = left_border
        && !line.cells.get(col).is_some_and(|cell| cell.cp == cp)
    {
        return None;
    }
    if let Some((col, cp)) = right_border
        && !line.cells.get(col).is_some_and(|cell| cell.cp == cp)
    {
        return None;
    }
    let hi = region_hi.min(line.cells.len());
    if region_lo >= hi {
        return None;
    }
    let mut start_abs = region_lo;
    while start_abs < hi && is_blank_cell(line.cells[start_abs]) {
        start_abs += 1;
    }
    let mut end_abs = hi;
    while end_abs > start_abs && is_blank_cell(line.cells[end_abs - 1]) {
        end_abs -= 1;
    }
    if start_abs >= end_abs {
        return None;
    }
    Some(FrameLine {
        indent: start_abs - region_lo,
        content_end: end_abs - region_lo,
        start_abs,
        end_abs,
    })
}

fn is_non_delim_at(cells: &[Cell], col: usize) -> bool {
    cell_char_at(cells, col).is_some_and(|(_, ch)| !is_link_delim(ch) && !is_vertical_border(cells[col]))
}

fn token_covering_col(cells: &[Cell], start_abs: usize, end_abs: usize, col: usize) -> Option<(usize, usize)> {
    let (pos, ch) = cell_char_at(cells, col)?;
    if pos < start_abs || pos >= end_abs || is_link_delim(ch) {
        return None;
    }
    let mut tok_start = pos;
    while tok_start > start_abs {
        let prev = tok_start - 1;
        let check = if cells[prev].flags & SPACER != 0 && prev > start_abs {
            prev - 1
        } else {
            prev
        };
        if !is_non_delim_at(cells, check) {
            break;
        }
        tok_start = check;
    }
    let mut tok_end = pos + if cells[pos].flags & WIDE != 0 { 2 } else { 1 };
    tok_end = tok_end.min(end_abs);
    while tok_end < end_abs {
        if !is_non_delim_at(cells, tok_end) {
            break;
        }
        let step = if cells[tok_end].flags & WIDE != 0 { 2 } else { 1 };
        tok_end = (tok_end + step).min(end_abs);
    }
    Some((tok_start, tok_end))
}

fn trailing_token_start(cells: &[Cell], start_abs: usize, end_abs: usize) -> Option<usize> {
    if start_abs >= end_abs {
        return None;
    }
    let last = end_abs - 1;
    let check = if cells[last].flags & SPACER != 0 && last > start_abs {
        last - 1
    } else {
        last
    };
    if !is_non_delim_at(cells, check) {
        return None;
    }
    let (tok_start, _) = token_covering_col(cells, start_abs, end_abs, check)?;
    Some(tok_start)
}

fn leading_token_end(cells: &[Cell], start_abs: usize, end_abs: usize) -> Option<usize> {
    if start_abs >= end_abs || !is_non_delim_at(cells, start_abs) {
        return None;
    }
    let (_, tok_end) = token_covering_col(cells, start_abs, end_abs, start_abs)?;
    Some(tok_end)
}

fn token_text(cells: &[Cell], from: usize, to: usize) -> String {
    let mut out = String::new();
    for cell in &cells[from..to.min(cells.len())] {
        if cell.flags & SPACER != 0 {
            continue;
        }
        if let Some(ch) = char::from_u32(cell.cp).filter(|ch| cell.cp != 0 && !ch.is_control()) {
            out.push(ch);
        }
    }
    out
}

fn starts_with_scheme(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

fn is_scheme_prefix(text: &str) -> bool {
    !text.is_empty()
        && text.len() < "https://".len()
        && ("https://".starts_with(text) || "http://".starts_with(text))
}

/// True when `line` ended early at a URL break character (`-`, `/`, `&`, `?`, `=`)
/// because the next unbreakable segment of the URL did not fit before `wrap_col`.
fn broke_at_url_sep(line: &Line, frame: FrameLine, next_head: &str, wrap_col: usize) -> bool {
    let Some((_, last_ch)) = cell_char_at(&line.cells, frame.end_abs.saturating_sub(1)) else {
        return false;
    };
    if !matches!(last_ch, '-' | '/' | '&' | '?' | '=') {
        return false;
    }
    let next_chunk_len = next_head
        .find(['-', '/', '&', '?', '='])
        .map_or(next_head.chars().count(), |byte| next_head[..byte].chars().count() + 1);
    next_chunk_len > 0 && frame.content_end + next_chunk_len > wrap_col
}

fn parse_osc8_uri(params: &[&[u8]]) -> Option<Arc<str>> {
    if params.len() < 3 {
        return None;
    }
    let mut uri = String::new();
    for (index, part) in params[2..].iter().enumerate() {
        if index > 0 {
            uri.push(';');
        }
        uri.push_str(&String::from_utf8_lossy(part));
        if uri.len() > MAX_OSC8_URI_LEN {
            return None;
        }
    }
    if uri.is_empty() || uri.chars().any(|ch| ch.is_control()) {
        return None;
    }
    Some(Arc::from(uri))
}

fn classify_osc8(uri: &str) -> Option<String> {
    if starts_with_scheme(uri) {
        let scheme = if uri.starts_with("https://") { "https://".len() } else { "http://".len() };
        return (uri.len() > scheme).then(|| uri.to_string());
    }
    if let Some(path) = file_url_path(uri) {
        return is_file_token(&path).then_some(path);
    }
    None
}

fn default_tab_stops(cols: u16) -> Vec<bool> {
    (0..usize::from(cols)).map(|col| col > 0 && col % 8 == 0).collect()
}

fn resize_tab_stops(stops: &mut Vec<bool>, cols: u16) {
    let target = usize::from(cols);
    let old = stops.len();
    stops.resize(target, false);
    for col in old..target {
        stops[col] = col > 0 && col % 8 == 0;
    }
}

fn resize_screen(screen: &mut Screen, cols: u16, rows: u16, blank: Cell) {
    let mut next = Screen::new(cols, rows, blank);
    let copy_rows = usize::from(screen.rows.min(rows));
    let copy_cols = usize::from(screen.cols.min(cols));
    let same_width = cols == screen.cols;
    for row in 0..copy_rows {
        next.lines[row].cells[..copy_cols].copy_from_slice(&screen.lines[row].cells[..copy_cols]);
        next.lines[row].wrapped = same_width && screen.lines[row].wrapped;
        let limit = copy_cols as u16;
        for span in &screen.lines[row].links {
            if span.start < limit {
                next.lines[row].links.push(HyperlinkSpan {
                    start: span.start,
                    end: span.end.min(limit),
                    uri: Arc::clone(&span.uri),
                });
            }
        }
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
    fn selection_drag_past_the_live_screen_scrolls_the_program() {
        let mut term = Term::new(8, 4);
        term.advance(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        let up = term.selection_scroll(1, 2, 1, (4, 2)).expect("older lines");
        assert_eq!(up, b"\x1b[<64;2;1M\x1b[<64;2;1m");
        let down = term.selection_scroll(-2, 2, 4, (4, 40)).expect("newer lines");
        assert_eq!(down, b"\x1b[<65;2;4M\x1b[<65;2;4m\x1b[<65;2;4M\x1b[<65;2;4m");
        assert_eq!(term.view_offset(), 0);

        let mut arrows = Term::new(8, 4);
        arrows.advance(b"\x1b[?1049h\x1b[?1007h");
        assert_eq!(arrows.selection_scroll(1, 1, 1, (1, 1)).as_deref(), Some(&b"\x1b[A"[..]));

        let mut shell = Term::new(4, 2);
        shell.advance(b"aaaa\r\nbbbb\r\ncccc");
        assert!(shell.selection_scroll(1, 1, 1, (1, 1)).is_none());
        shell.scroll_view(1);
        shell.advance(b"\x1b[?1000h\x1b[?1006h");
        assert!(shell.selection_scroll(1, 1, 1, (1, 1)).is_none());
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
    fn soft_wrapped_multiline_url_is_clickable_on_every_line() {
        let url = concat!(
            "https://accounts.google.com/o/oauth2/v2/auth?",
            "access_type=offline&client_id=1234567890-abcdef.apps.googleusercontent.com",
            "&redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fcallback",
            "&response_type=code&scope=openid%20email%20profile&state=xyz987654321"
        );
        let mut term = Term::new(40, 8);
        term.advance(format!("Abra {url}. Depois volte.").as_bytes());
        assert_eq!(term.link_at(0, 0), None);
        // Every wrapped row of the URL (0..=6) returns the complete address.
        assert_eq!(term.link_at(0, 6).as_deref(), Some(url));
        assert_eq!(term.link_at(1, 0).as_deref(), Some(url));
        assert_eq!(term.link_at(2, 20).as_deref(), Some(url));
        assert_eq!(term.link_at(3, 39).as_deref(), Some(url));
        assert_eq!(term.link_at(5, 10).as_deref(), Some(url));
        // Tail row: inside the URL returns the full URL; prose after it returns None.
        let tail_row = ((5 + url.len()) / 40) as u16;
        let tail_end_col = ((5 + url.len()) % 40) as u16;
        assert_eq!(term.link_at(tail_row, 0).as_deref(), Some(url));
        assert_eq!(term.link_at(tail_row, tail_end_col.saturating_sub(1)).as_deref(), Some(url));
        assert_eq!(term.link_at(tail_row, (tail_end_col + 3).min(39)), None);
    }

    #[test]
    fn soft_wrapped_url_stays_clickable_across_scrollback_boundary() {
        let url = concat!(
            "https://example.com/very/long/path/with/segments/",
            "alpha/beta/gamma/delta/epsilon/zeta/eta/theta/iota"
        );
        let mut term = Term::new(30, 3);
        term.advance(format!("{url}\r\nfinal").as_bytes());
        // Top line(s) of the URL are now in scrollback; visible rows still resolve the full URL.
        assert_eq!(term.link_at(0, 5).as_deref(), Some(url));
        assert_eq!(term.link_at(1, 0).as_deref(), Some(url));
        assert_eq!(term.link_at(2, 0), None);
        // Scroll up so the tail of the URL is below the visible screen.
        term.scroll_view(2);
        assert_eq!(term.link_at(0, 10).as_deref(), Some(url));
    }

    #[test]
    fn soft_wrapped_long_path_is_clickable_on_wrapped_lines() {
        let path = "/home/gutopardini/Git/personal/rustty/src/graphics/very_long_module_name.rs";
        let mut term = Term::new(30, 4);
        term.advance(format!("erro em {path}:128:9").as_bytes());
        assert_eq!(term.link_at(0, 10).as_deref(), Some(path));
        assert_eq!(term.link_at(1, 5).as_deref(), Some(path));
        assert_eq!(term.link_at(2, 15).as_deref(), Some(path));
    }

    #[test]
    fn osc8_hyperlink_works_across_lines_and_sgr_reset() {
        let uri = "https://example.com/auth;v=1?token=abcdef1234567890";
        let mut term = Term::new(20, 4);
        term.advance(
            format!("\x1b]8;id=1;{uri}\x1b\\\x1b[36mClick this very\x1b[0m long link\x1b]8;;\x1b\\ plain")
                .as_bytes(),
        );
        assert_eq!(term.link_at(0, 0).as_deref(), Some(uri));
        assert_eq!(term.link_at(0, 19).as_deref(), Some(uri));
        assert_eq!(term.link_at(1, 0).as_deref(), Some(uri));
        assert_eq!(term.link_at(1, 4).as_deref(), Some(uri));
        assert_eq!(term.link_at(1, 7), None);

        // Overwriting a cell clears its OSC 8 link span.
        term.advance(b"\x1b[1;1HXXXXX");
        assert_eq!(term.link_at(0, 0), None);
        assert_eq!(term.link_at(0, 6).as_deref(), Some(uri));
    }

    #[test]
    fn tui_hard_wrapped_urls_are_clickable_on_all_lines() {
        let url = concat!(
            "https://accounts.google.com/o/oauth2/v2/auth?access_type=offline",
            "&client_id=681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com",
            "&redirect_uri=https://localhost:43215/oauth2/callback&response_type=code&scope=openid"
        );
        // 1. Full-width CUP wrap (no terminal autowrap).
        let mut cup = Term::new(60, 6);
        for (row, chunk) in url.as_bytes().chunks(60).enumerate() {
            cup.advance(format!("\x1b[{};1H", row + 1).as_bytes());
            cup.advance(chunk);
        }
        for row in 0..4u16 {
            assert_eq!(cup.link_at(row, 5).as_deref(), Some(url), "cup row {row}");
        }

        // 2. Fixed-column indented wrap (width 76 inside a 160-col terminal) with trailing text line.
        let mut indented = Term::new(160, 8);
        let mut row = 1;
        for chunk in url.as_bytes().chunks(74) {
            indented.advance(format!("\x1b[{row};1H  ").as_bytes());
            indented.advance(chunk);
            row += 1;
        }
        indented.advance(format!("\x1b[{row};1H  Press Enter to continue").as_bytes());
        for r in 0..4u16 {
            assert_eq!(indented.link_at(r, 4).as_deref(), Some(url), "indented row {r}");
        }
        // Unrelated indented line below must NOT be part of the URL.
        assert_eq!(indented.link_at(4, 4), None);

        // 3. Box-bordered wrap (`│  ...  │`).
        let mut boxed = Term::new(80, 8);
        boxed.advance("  ╭──────────────────────────────────────────────────────────────╮\r\n".as_bytes());
        let inner = 60usize;
        for chunk in url.as_bytes().chunks(inner) {
            let text = std::str::from_utf8(chunk).unwrap();
            boxed.advance(format!("  │ {text:<inner$} │\r\n").as_bytes());
        }
        boxed.advance("  ╰──────────────────────────────────────────────────────────────╯".as_bytes());
        for r in 1..=4u16 {
            assert_eq!(boxed.link_at(r, 5).as_deref(), Some(url), "boxed row {r}");
        }
        // Borders and space outside the URL are not clickable.
        assert_eq!(boxed.link_at(1, 2), None);
        assert_eq!(boxed.link_at(5, 10), None);

        // 4. Hyphen-broken TUI wrap (e.g. `textwrap` breaking at `-` before the right edge).
        let mut hyphen = Term::new(80, 6);
        let part0 = "https://accounts.google.com/o/oauth2/v2/auth?client_id=681255809395-";
        let part1 = "oo8ft2oprdrnp9e3aqf6av3hmdib135j.apps.googleusercontent.com&redirect_uri=htt";
        let part2 = "p%3A%2F%2Flocalhost%3A43215%2Fcallback";
        let expected = format!("{part0}{part1}{part2}");
        hyphen.advance(format!("  {part0}\r\n  {part1}\r\n  {part2}").as_bytes());
        for r in 0..3u16 {
            assert_eq!(hyphen.link_at(r, 5).as_deref(), Some(expected.as_str()), "hyphen row {r}");
        }
    }

    #[test]
    fn separate_lines_do_not_merge_into_false_multiline_links() {
        let mut term = Term::new(80, 6);
        term.advance(
            concat!(
                "https://github.com/gutopardini/rustty\r\n",
                "https://github.com/gutopardini/wayshot\r\n",
                "  https://example.com/short\r\n",
                "  Next step: run cargo test\r\n",
            )
            .as_bytes(),
        );
        assert_eq!(term.link_at(0, 10).as_deref(), Some("https://github.com/gutopardini/rustty"));
        assert_eq!(term.link_at(1, 10).as_deref(), Some("https://github.com/gutopardini/wayshot"));
        assert_eq!(term.link_at(2, 10).as_deref(), Some("https://example.com/short"));
        assert_eq!(term.link_at(3, 5), None);
    }

    #[test]
    fn wide_emoji_before_url_keeps_click_columns_aligned() {
        let mut term = Term::new(80, 2);
        term.advance("🔗 https://example.com/docs".as_bytes());
        // Emoji occupies cols 0..2, space at col 2, URL starts at col 3.
        assert_eq!(term.link_at(0, 0), None);
        assert_eq!(term.link_at(0, 2), None);
        assert_eq!(term.link_at(0, 3).as_deref(), Some("https://example.com/docs"));
        assert_eq!(term.link_at(0, 26).as_deref(), Some("https://example.com/docs"));
        assert_eq!(term.link_at(0, 27), None);
    }

    #[test]
    fn rep_repeats_characters_and_spaces_used_by_tui_diff_renderers() {
        let mut term = Term::new(40, 2);
        term.advance(b"w\x1b[15b");
        assert_eq!(text(&term), "wwwwwwwwwwwwwwww");
        assert_eq!(term.cursor.col, 16);

        // Overwrite with spaces via REP so no stray characters remain behind.
        term.advance(b"\x1b[1;5H \x1b[11b");
        assert_eq!(text(&term), "wwww            ");
        assert_eq!(term.cursor.col, 16);
    }

    #[test]
    fn tab_stops_and_cbt_cht_never_trigger_spurious_line_wrap() {
        let mut term = Term::new(16, 2);
        // Tabbing to the right margin stops at col 15 without setting pending_wrap.
        term.advance(b"ab\t\tZ");
        assert_eq!(term.cursor.row, 0);
        assert_eq!(term.cell(0, 15).cp, u32::from(b'Z'));

        // CHT (CSI I) and CBT (CSI Z) move cleanly between 8-column stops.
        term.advance(b"\x1b[1;1H\x1b[2IX");
        assert_eq!(term.cell(0, 15).cp, u32::from(b'X'));
        term.advance(b"\x1b[ZY");
        assert_eq!(term.cell(0, 8).cp, u32::from(b'Y'));

        // Insert mode (CSI 4 h / l) shifts characters right when printing.
        term.advance(b"\x1b[2;1Habcd\x1b[2;2H\x1b[4hX\x1b[4l");
        assert_eq!(term.cell(1, 0).cp, u32::from(b'a'));
        assert_eq!(term.cell(1, 1).cp, u32::from(b'X'));
        assert_eq!(term.cell(1, 2).cp, u32::from(b'b'));
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
