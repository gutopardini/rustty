use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

use crate::cell::{Cell, BOLD, DIM, ITALIC, REVERSE, SPACER, UNDERLINE, WIDE};
use crate::color::{self, Rgb, DEFAULT_BG, DEFAULT_FG};
use crate::graphics::{Graphics, Parser, Placement, Update};

const MAX_SCROLLBACK: usize = 10_000;
const MAX_PENDING: usize = 256;

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
    cells: Vec<Cell>,
    graphics: Graphics,
    cols: u16,
    rows: u16,
    /// Linha física que aparece no topo. Rolar a tela inteira gira este índice
    /// em vez de deslocar `rows * cols` células.
    origin: u16,
}

impl Screen {
    fn new(cols: u16, rows: u16, blank: Cell) -> Self {
        Self {
            cells: vec![blank; usize::from(cols) * usize::from(rows)],
            graphics: Graphics::default(),
            cols,
            rows,
            origin: 0,
        }
    }

    #[inline]
    fn physical_row(&self, row: u16) -> usize {
        (usize::from(self.origin) + usize::from(row)) % usize::from(self.rows)
    }

    #[inline]
    fn index(&self, row: u16, col: u16) -> usize {
        self.physical_row(row) * usize::from(self.cols) + usize::from(col)
    }

    #[inline]
    fn line_start(&self, row: u16) -> usize {
        self.physical_row(row) * usize::from(self.cols)
    }

    fn linearize(&mut self) {
        if self.origin == 0 {
            return;
        }
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let origin = usize::from(self.origin);
        let mut rotated = vec![self.cells[0]; rows * cols];
        let tail = (rows - origin) * cols;
        rotated[..tail].copy_from_slice(&self.cells[origin * cols..]);
        rotated[tail..].copy_from_slice(&self.cells[..origin * cols]);
        self.cells = rotated;
        self.origin = 0;
    }
}

/// Foto da área visível para o render. Linhas sujas evitam reenviar a grade inteira.
pub struct Snapshot {
    pub cols: u16,
    pub rows: u16,
    pub full: bool,
    pub cells: Vec<Cell>,
    pub rows_dirty: Vec<(u16, Vec<Cell>)>,
    pub(crate) images: Vec<Placement>,
    pub cursor: Option<(u16, u16)>,
    pub title: String,
    pub title_changed: bool,
    pub suspended: bool,
    pub changed: bool,
    pub scroll_top: f32,
    pub scroll_h: f32,
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
    pending_count: usize,
    view_offset: usize,
    title: String,
    title_dirty: bool,
    dirty: Vec<bool>,
    graphics_dirty: bool,
    damage_all: bool,
    suspend: bool,
    autowrap: bool,
    cursor_visible: bool,
    app_cursor: bool,
    origin_mode: bool,
    pending_wrap: bool,
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
            pending_count: 0,
            view_offset: 0,
            title: "Rustty".to_string(),
            title_dirty: true,
            dirty: vec![true; usize::from(rows)],
            graphics_dirty: false,
            damage_all: true,
            suspend: false,
            autowrap: true,
            cursor_visible: true,
            app_cursor: false,
            origin_mode: false,
            pending_wrap: false,
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
        for cell in &mut self.primary.cells {
            repaint(cell);
        }
        for cell in &mut self.alt.cells {
            repaint(cell);
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

    /// Retorna o retângulo colorido do composer ativo do Codex, em células.
    pub(crate) fn codex_composer(&self) -> Option<[u16; 4]> {
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

        // Miniaturas e trechos estilizados podem ter outra cor dentro da caixa.
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

    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    pub(crate) fn graphics_command(&mut self, bytes: &[u8]) {
        let screen = if self.alt_on { &mut self.alt } else { &mut self.primary };
        match screen.graphics.command(bytes, self.cursor.row, self.cursor.col, &mut self.replies) {
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

    pub fn take_indexed_lines(&mut self) -> Vec<Vec<Cell>> {
        let n = self.pending_count.min(self.scrollback.len());
        self.pending_count = 0;
        if n == 0 {
            return Vec::new();
        }
        let start = self.scrollback.len() - n;
        self.scrollback.range(start..).cloned().collect()
    }

    pub fn damage_all(&mut self) {
        self.damage_all = true;
        self.dirty.fill(true);
    }

    pub fn alt_screen(&self) -> bool {
        self.alt_on
    }

    pub fn scroll_view(&mut self, delta: isize) {
        self.set_view((self.view_offset as isize + delta).clamp(0, self.scrollback.len() as isize) as usize);
    }

    /// `from_top` 0 mostra o trecho mais antigo. 1 volta para o final da conversa.
    pub fn scroll_to(&mut self, from_top: f32) {
        let max = self.scrollback.len() as f32;
        let offset = ((1.0 - from_top.clamp(0.0, 1.0)) * max).round() as usize;
        self.set_view(offset.min(self.scrollback.len()));
    }

    /// Topo e altura do polegar, frações da grade. Sem histórico, os dois ficam em 0.
    pub fn scroll_metrics(&self) -> (f32, f32) {
        if self.alt_on {
            return (0.0, 0.0);
        }
        let history = self.scrollback.len();
        if history == 0 {
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
                title: if title_changed { self.title.clone() } else { String::new() },
                title_changed,
                suspended: true,
                changed: title_changed,
                scroll_top: 0.0,
                scroll_h: 0.0,
            };
        }

        let cursor = self.cursor_visible.then_some((self.cursor.col, self.cursor.row));
        let show_history = self.view_offset > 0;
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
        let changed = full || !rows_dirty.is_empty() || title_changed || self.graphics_dirty;
        let (scroll_top, scroll_h) = self.scroll_metrics();
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
            cursor: if show_history { None } else { cursor },
            title: if title_changed { self.title.clone() } else { String::new() },
            title_changed,
            suspended: false,
            changed,
            scroll_top,
            scroll_h,
        }
    }

    fn append_visible(&self, row: u16, dst: &mut Vec<Cell>) {
        let cols = usize::from(self.cols);
        if self.view_offset == 0 {
            let screen = self.screen();
            let start = screen.line_start(row);
            dst.extend_from_slice(&screen.cells[start..start + cols]);
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
            let screen = self.screen();
            let start = screen.line_start(live);
            dst.extend_from_slice(&screen.cells[start..start + cols]);
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

    fn touch_output(&mut self) {
        if self.view_offset != 0 {
            self.view_offset = 0;
            self.damage_all = true;
        }
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        let screen = self.screen();
        screen.cells[screen.index(row, col)]
    }

    fn write_cell(&mut self, row: u16, col: u16, cell: Cell) {
        let index = self.screen().index(row, col);
        self.screen_mut().cells[index] = cell;
        self.mark(row);
    }

    fn move_to(&mut self, row: u16, col: u16) {
        let old = self.cursor.row;
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
        self.pending_wrap = false;
        self.mark(old);
        self.mark(self.cursor.row);
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.pending_wrap = false;
        self.mark(self.cursor.row);
    }

    fn linefeed(&mut self) {
        self.pending_wrap = false;
        if self.cursor.row == self.scroll_bot {
            self.scroll_region(self.scroll_top, self.scroll_bot, 1);
        } else if self.cursor.row + 1 < self.rows {
            let old = self.cursor.row;
            self.cursor.row += 1;
            self.mark(old);
            self.mark(self.cursor.row);
        }
    }

    fn scroll_region(&mut self, top: u16, bot: u16, lines: i16) {
        if bot < top || lines == 0 {
            return;
        }
        let rows = self.rows;
        self.screen_mut().graphics.scroll(top, bot, lines, rows);
        if top == 0 && bot + 1 == self.rows {
            self.scroll_full(lines);
            return;
        }
        self.screen_mut().linearize();
        let cols = usize::from(self.cols);
        let height = usize::from(bot - top) + 1;
        let shift_rows = (lines.unsigned_abs() as usize).min(height);
        let shift = shift_rows * cols;
        let start = usize::from(top) * cols;
        let end = start + height * cols;
        let blank = self.blank();
        if lines > 0 && !self.alt_on && top == 0 {
            for row in 0..shift_rows {
                let mut line = self.alloc_line();
                let from = row * cols;
                line.extend_from_slice(&self.screen().cells[from..from + cols]);
                self.remember(line);
            }
        }
        {
            let cells = &mut self.screen_mut().cells;
            if lines > 0 {
                if shift < end - start {
                    cells.copy_within(start + shift..end, start);
                    cells[end - shift..end].fill(blank);
                } else {
                    cells[start..end].fill(blank);
                }
            } else if shift < end - start {
                cells.copy_within(start..end - shift, start + shift);
                cells[start..start + shift].fill(blank);
            } else {
                cells[start..end].fill(blank);
            }
        }
        for row in top..=bot {
            self.mark(row);
        }
    }

    /// Rola a tela inteira girando a origem. Uma linha custa uma cópia de `cols` células.
    fn scroll_full(&mut self, lines: i16) {
        let shift_rows = (lines.unsigned_abs() as usize).min(usize::from(self.rows));
        if shift_rows == 0 {
            return;
        }
        let blank = self.blank();
        let height = self.rows;
        let rows = usize::from(height);
        let cols = usize::from(self.cols);
        if lines > 0 {
            for _ in 0..shift_rows {
                if !self.alt_on {
                    let mut line = self.alloc_line();
                    let start = self.screen().line_start(0);
                    line.extend_from_slice(&self.screen().cells[start..start + cols]);
                    self.remember(line);
                }
                let screen = self.screen_mut();
                screen.origin = ((usize::from(screen.origin) + 1) % rows) as u16;
                let cleared = screen.line_start(height - 1);
                screen.cells[cleared..cleared + cols].fill(blank);
            }
        } else {
            let screen = self.screen_mut();
            for _ in 0..shift_rows {
                screen.origin = ((usize::from(screen.origin) + rows - 1) % rows) as u16;
                let cleared = screen.line_start(0);
                screen.cells[cleared..cleared + cols].fill(blank);
            }
        }
        self.dirty.fill(true);
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
        self.scrollback.push_back(line);
        while self.scrollback.len() > MAX_SCROLLBACK {
            if let Some(old) = self.scrollback.pop_front() {
                self.recycle(old);
            }
        }
        self.pending_count = (self.pending_count + 1).min(MAX_PENDING);
    }

    fn forget_history(&mut self) {
        while let Some(line) = self.scrollback.pop_front() {
            self.recycle(line);
        }
        self.pending_count = 0;
        self.view_offset = 0;
    }

    fn clear_span(&mut self, row: u16, from: u16, to: u16) {
        if from >= to || row >= self.rows {
            return;
        }
        let blank = self.blank();
        let end_col = to.min(self.cols);
        let start = self.screen().line_start(row) + usize::from(from);
        let end = self.screen().line_start(row) + usize::from(end_col);
        self.screen_mut().cells[start..end].fill(blank);
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
        self.touch_output();
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
                25 => {
                    self.cursor_visible = enable;
                    self.mark(self.cursor.row);
                }
                47 | 1047 | 1049 => self.set_alt(enable),
                2004 => {}
                2026 => {
                    self.suspend = enable;
                    if !enable {
                        self.damage_all = true;
                    }
                }
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
        self.touch_output();
        match byte {
            b'\n' | 0x0b | 0x0c => self.linefeed(),
            b'\r' => self.carriage_return(),
            0x08 => {
                if self.pending_wrap {
                    self.pending_wrap = false;
                } else if self.cursor.col > 0 {
                    self.cursor.col -= 1;
                    self.mark(self.cursor.row);
                }
            }
            b'\t' => {
                let next = ((self.cursor.col / 8) + 1) * 8;
                self.cursor.col = next.min(self.cols.saturating_sub(1));
                if next >= self.cols {
                    self.pending_wrap = self.autowrap;
                    self.cursor.col = self.cols.saturating_sub(1);
                }
                self.mark(self.cursor.row);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        self.touch_output();
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
                        screen.cells.fill(blank);
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
        let start = self.screen().line_start(row);
        {
            let line = &mut self.screen_mut().cells[start..start + cols];
            line.copy_within(col + n..cols, col);
            line[cols - n..].fill(blank);
        }
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
        let start = self.screen().line_start(row);
        {
            let line = &mut self.screen_mut().cells[start..start + cols];
            line.copy_within(col..cols - n, col + n);
            line[col..col + n].fill(blank);
        }
        self.mark(row);
    }

    /// Caminho comum: ASCII de uma coluna, sem wrap e sem glifo largo por baixo.
    #[inline(always)]
    fn put_ascii(&mut self, ch: char) -> bool {
        let byte = ch as u32;
        if byte < 0x20 || byte >= 0x7f || self.view_offset != 0 || self.pending_wrap {
            return false;
        }
        let row = self.cursor.row;
        let col = self.cursor.col;
        if col + 1 >= self.cols {
            return false;
        }
        let index = self.screen().index(row, col);
        if self.screen().cells[index].flags & (WIDE | SPACER) != 0 {
            return false;
        }
        self.screen_mut().cells[index] = Cell {
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

/// Colunas que o glifo ocupa. Uso privado (Nerd Font / Oh My Posh) é sempre 1:
/// o unicode-width marca essa faixa como ambígua e o prompt conta cada diamante
/// como uma coluna.
fn columns(ch: char) -> u16 {
    if is_private_use(ch) {
        return 1;
    }
    UnicodeWidthChar::width(ch).unwrap_or(0) as u16
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
    let copy_rows = screen.rows.min(rows);
    let copy_cols = screen.cols.min(cols);
    for row in 0..copy_rows {
        for col in 0..copy_cols {
            let src = screen.index(row, col);
            let dst = next.index(row, col);
            next.cells[dst] = screen.cells[src];
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
        assert_eq!(term.codex_composer(), Some([1, 1, 10, 4]));
        term.advance(b"\x1b[?25l");
        assert_eq!(term.codex_composer(), None);
        term.advance(b"\x1b[?25h\x1b[5;1H");
        assert_eq!(term.codex_composer(), None);
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
        let indexed = term.take_indexed_lines();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0][0].cp, u32::from(b'a'));
    }

    #[test]
    fn ring_scroll_keeps_visual_order_after_the_origin_wraps() {
        let mut term = Term::new(3, 3);
        term.advance(b"aaa\r\nbbb\r\nccc\r\nddd");
        assert_eq!(text(&term), "bbb\nccc\nddd");
        term.advance(b"\x1b[2;3r\x1b[3;1H\x1b[S");
        assert_eq!(text(&term), "bbb\nddd");
        let indexed = term.take_indexed_lines();
        assert_eq!(indexed.len(), 1);
        assert_eq!(indexed[0][0].cp, u32::from(b'a'));
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
    fn osc_title() {
        let mut term = Term::new(8, 2);
        term.advance(b"\x1b]2;Rustty\x07");
        assert_eq!(term.title, "Rustty");
    }

    #[test]
    fn oh_my_posh_diamonds_keep_truecolor() {
        let mut term = Term::new(80, 4);
        // O bash consome \[ \] antes de escrever no PTY. O que chega é SGR + PUA.
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
