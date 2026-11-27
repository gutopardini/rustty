//! Tab strip drawn as one row of cells at the bottom of the window.

use std::borrow::Cow;
use std::ops::Range;

use unicode_width::UnicodeWidthChar;

use crate::agent::Agent;
use crate::cell::{
    AGENT_ICON, BOLD, Cell, DIM, FADE_END, FADE_SINGLE, FADE_START, REVERSE, SPACER, WIDE,
};
use crate::color::Theme;

/// Chrome-inspired maximum width of a single tab in columns before it stops stretching.
pub const MAX_TAB_COLS: usize = 22;
/// Column width reserved for the `+` new-tab button immediately after the last tab.
pub const NEW_TAB_COLS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSpan {
    pub start: u16,
    pub end: u16,
    /// Visible close column when shown on the tab.
    pub close: Option<u16>,
    /// Column where the close button sits when shown, used for hover detection.
    pub(crate) close_slot: Option<u16>,
    /// First column of the title text, and how many columns that text may use.
    pub label_at: u16,
    pub label_cols: u16,
    /// True when the title overflowed `label_cols` and fades out before the close button.
    pub truncated: bool,
}

/// Pointer hover state on the tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarHover {
    Tab { tab: usize, close: bool },
    NewTab,
}

/// Floating tooltip showing the full text of a hovered tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabTooltip {
    pub cells: Vec<Cell>,
    pub cols: u16,
    pub rows: u16,
    /// Left column of the tab this tooltip is anchored above.
    pub anchor_col: u16,
}

/// Draft shown in one tab while its label is being typed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LabelEdit<'a> {
    pub index: usize,
    pub text: &'a str,
    pub cursor: usize,
    pub cursor_on: bool,
}

const MAX_LABEL_CHARS: usize = 128;

/// The text being typed into a tab label. `cursor` counts characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LabelDraft {
    pub text: String,
    pub cursor: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarHit {
    Select(usize),
    Close(usize),
    NewTab,
}

pub struct Bar {
    pub cells: Vec<Cell>,
    pub spans: Vec<TabSpan>,
    /// Column range of the `+` new-tab button right after the last tab.
    pub new_tab: Option<Range<u16>>,
    /// Index of the active tab in `spans`.
    pub selected: usize,
}

/// Renders the Chrome-inspired bottom tab bar. Cells keep `theme.bar` as their background key
/// so the vector strip, active tab contour, dividers, hover highlights and `+` button draw underneath.
pub fn render_bar(
    titles: &[String],
    agents: &[Option<Agent>],
    active: usize,
    cols: u16,
    theme: Theme,
    glass: bool,
) -> Bar {
    render_bar_in(titles, agents, active, cols, theme, glass, None, None, &[], None)
}

/// `track` optionally constrains the usable column range inside the bar.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_bar_in(
    titles: &[String],
    agents: &[Option<Agent>],
    active: usize,
    cols: u16,
    theme: Theme,
    _glass: bool,
    track: Option<(u16, u16)>,
    edit: Option<LabelEdit<'_>>,
    custom: &[bool],
    hover: Option<BarHover>,
) -> Bar {
    let cols = usize::from(cols);
    if titles.is_empty() || cols == 0 {
        return Bar { cells: Vec::new(), spans: Vec::new(), new_tab: None, selected: 0 };
    }
    // Keep one column of breathing room on the edges so the active tab's left concave fillet
    // clears the window's bottom rounded corners.
    let (origin, tabs_end) = if let Some((start, end)) = track {
        let start = usize::from(start).min(cols.saturating_sub(1));
        let end = usize::from(end).clamp(start.saturating_add(1), cols);
        (start, end)
    } else if cols >= 12 {
        (1, cols - 1)
    } else {
        (0, cols)
    };
    let bg = theme.bar.pack();
    let mut cells = vec![Cell::blank(theme.muted.pack(), bg); cols];
    let count = titles.len();
    let room = tabs_end - origin;
    let new_tab_w = if room >= count + NEW_TAB_COLS {
        NEW_TAB_COLS
    } else if room >= count + 2 {
        2
    } else {
        0
    };
    let tabs_room = room - new_tab_w;
    let (base, extra) = if count * MAX_TAB_COLS <= tabs_room {
        (MAX_TAB_COLS, 0)
    } else {
        (tabs_room / count, tabs_room % count)
    };

    let mut spans = Vec::with_capacity(count);
    let mut start = origin;
    for (index, title) in titles.iter().enumerate() {
        let width = base + usize::from(index < extra);
        if width == 0 || start >= tabs_end {
            break;
        }
        let end = (start + width).min(tabs_end);
        if end <= start {
            break;
        }
        let active_tab = index == active;
        let hovered_tab = matches!(hover, Some(BarHover::Tab { tab, .. }) if tab == index);
        let fg = if active_tab || hovered_tab { theme.fg.pack() } else { theme.tab_fg.pack() };
        let flags = if active_tab { BOLD } else { 0 };
        for cell in &mut cells[start..end] {
            *cell = Cell { cp: 0, fg, bg, flags: 0 };
        }
        let span = end - start;
        let (lead, tail) = if span >= 8 { (1, 1) } else { (0, 0) };
        let icon_gap = 1;
        let close_slot = if span >= lead + 2 + tail + 3 {
            Some((end - tail - 1) as u16)
        } else {
            None
        };
        // Wide tabs show a subtle close mark like Chrome; narrow inactive tabs reveal it on hover.
        let show_close = active_tab || hovered_tab || span >= 12;
        let close = close_slot.filter(|_| show_close);
        let reserve = if close_slot.is_some() && (span >= 12 || show_close) {
            tail + 2
        } else {
            tail
        };
        let label_end = end.saturating_sub(reserve);
        let editing = edit.as_ref().copied().filter(|item| item.index == index);
        let custom_label = custom.get(index).copied().unwrap_or(false);

        let mut text_start = (start + lead).min(label_end);
        if text_start + 2 <= label_end {
            paint_icon(&mut cells[text_start..label_end], agents.get(index).copied().flatten(), fg, bg);
            text_start += 2;
            if text_start < label_end {
                text_start = (text_start + icon_gap).min(label_end);
            }
        }
        let label_room = label_end.saturating_sub(text_start);
        let (fitted, used, truncated, cursor_col) = if let Some(edit) = editing {
            let window = edit_window(edit.text, label_room, edit.cursor);
            let cursor_col = edit.cursor_on.then_some(window.cursor_col);
            let used = window.visible.chars().filter_map(UnicodeWidthChar::width).sum();
            (Cow::Owned(window.visible), used, false, cursor_col)
        } else {
            let (text, used, truncated) = fit_clipped(title, label_room, !custom_label);
            (text, used, truncated, None)
        };
        paint(&mut cells[text_start..label_end], &fitted, fg, bg, flags);
        if truncated && used > 0 {
            apply_fade(&mut cells[text_start..(text_start + used).min(label_end)]);
        }
        if let Some(cursor_col) = cursor_col {
            paint_caret(&mut cells[text_start..label_end], cursor_col, fg, bg, flags);
        }
        if let Some(col) = close {
            let close_hot = matches!(hover, Some(BarHover::Tab { tab, close: true }) if tab == index);
            let (close_fg, close_flags) = if close_hot {
                (theme.fg.pack(), BOLD)
            } else if active_tab || hovered_tab {
                (theme.tab_fg.pack(), 0)
            } else {
                (theme.muted.pack(), DIM)
            };
            cells[col as usize] = Cell {
                cp: u32::from('×'),
                fg: close_fg,
                bg,
                flags: close_flags,
            };
        }
        spans.push(TabSpan {
            start: start as u16,
            end: end as u16,
            close,
            close_slot,
            label_at: text_start as u16,
            label_cols: label_room as u16,
            truncated,
        });
        start = end;
    }
    let new_tab = (new_tab_w > 0 && start + new_tab_w <= tabs_end)
        .then_some(start as u16..(start + new_tab_w) as u16);
    Bar { cells, spans, new_tab, selected: active }
}

pub fn hit(bar: &Bar, col: u16) -> Option<BarHit> {
    if bar.new_tab.as_ref().is_some_and(|range| range.contains(&col)) {
        return Some(BarHit::NewTab);
    }
    let index = bar.spans.iter().position(|span| (span.start..span.end).contains(&col))?;
    let span = bar.spans[index];
    if span.close == Some(col) {
        Some(BarHit::Close(index))
    } else {
        Some(BarHit::Select(index))
    }
}

/// Tab or `+` button under `col`, and whether the pointer sits over a tab's close slot.
pub(crate) fn hover(bar: &Bar, col: u16) -> Option<BarHover> {
    if bar.new_tab.as_ref().is_some_and(|range| range.contains(&col)) {
        return Some(BarHover::NewTab);
    }
    let index = bar.spans.iter().position(|span| (span.start..span.end).contains(&col))?;
    let span = bar.spans[index];
    Some(BarHover::Tab {
        tab: index,
        close: span.close_slot == Some(col),
    })
}

/// Tab under `col`, including the margin past the first and last tabs.
/// A column in the margin has no hit, but a drag still lands on the end tab.
pub fn drop_index(bar: &Bar, col: u16) -> Option<usize> {
    let (Some(first), Some(last)) = (bar.spans.first(), bar.spans.last()) else {
        return None;
    };
    if col < first.start {
        return Some(0);
    }
    if col >= last.end {
        return Some(bar.spans.len() - 1);
    }
    bar.spans.iter().position(|span| (span.start..span.end).contains(&col))
}

/// Builds a floating tooltip containing the full tab title, wrapped across lines if needed.
pub fn render_tooltip(title: &str, max_cols: u16, anchor_col: u16, theme: Theme) -> Option<TabTooltip> {
    let text = title.trim();
    if text.is_empty() || max_cols == 0 {
        return None;
    }
    let limit = usize::from(max_cols).clamp(1, 72);
    let lines = wrap_lines(text, limit, 4);
    let cols = lines
        .iter()
        .map(|line| line.chars().filter_map(UnicodeWidthChar::width).sum::<usize>())
        .max()
        .unwrap_or(0)
        .max(1);
    let rows = lines.len();
    let fg = theme.fg.pack();
    let bg = theme.bg.pack();
    let mut cells = vec![Cell::blank(fg, bg); cols * rows];
    for (row, line) in lines.iter().enumerate() {
        paint(&mut cells[row * cols..(row + 1) * cols], line, fg, bg, 0);
    }
    Some(TabTooltip {
        cells,
        cols: cols as u16,
        rows: rows as u16,
        anchor_col,
    })
}

fn wrap_lines(text: &str, max_width: usize, max_lines: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut cur_w = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let ch = if ch.is_control() { ' ' } else { ch };
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if w == 0 {
            continue;
        }
        if cur_w + w > max_width && !current.is_empty() {
            if lines.len() + 1 >= max_lines {
                // Keep the last line cleanly bounded with an ellipsis only if >4 lines.
                let mut trimmed = String::new();
                let mut used = 0;
                for existing in current.chars() {
                    let ew = UnicodeWidthChar::width(existing).unwrap_or(0);
                    if used + ew >= max_width {
                        break;
                    }
                    trimmed.push(existing);
                    used += ew;
                }
                trimmed.push('…');
                lines.push(trimmed);
                return lines;
            }
            if let Some(split) = current.rfind(' ')
                && split > 0
            {
                let tail = current[split + 1..].to_string();
                current.truncate(split);
                lines.push(current);
                current = tail;
                cur_w = current.chars().filter_map(UnicodeWidthChar::width).sum();
                if ch == ' ' {
                    continue;
                }
            } else {
                lines.push(current);
                current = String::new();
                cur_w = 0;
                if ch == ' ' {
                    continue;
                }
            }
        }
        if cur_w == 0 && ch == ' ' && !lines.is_empty() {
            continue;
        }
        if cur_w + w <= max_width || current.is_empty() {
            current.push(ch);
            cur_w += w;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(text.to_string());
    }
    lines
}

pub(crate) fn paint_icon(cells: &mut [Cell], agent: Option<Agent>, fg: u32, bg: u32) {
    let cp = agent.map_or(crate::agent::RUSTTY_ICON, Agent::icon);
    cells[0] = Cell { cp, fg, bg, flags: AGENT_ICON | WIDE };
    cells[1] = Cell { cp: 0, fg, bg, flags: SPACER };
}

fn fit_clipped(title: &str, width: usize, strip_status: bool) -> (Cow<'_, str>, usize, bool) {
    if width == 0 {
        return (Cow::Borrowed(""), 0, !title.trim().is_empty());
    }
    let stripped;
    let title = if strip_status {
        stripped = without_status(title);
        if stripped.is_empty() { "shell" } else { stripped }
    } else {
        title
    };
    let title_width: usize = title.chars().filter_map(UnicodeWidthChar::width).sum();
    if title_width <= width {
        return (Cow::Borrowed(title), title_width, false);
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in title.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            continue;
        }
        if used + ch_width > width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    (Cow::Owned(out), used, true)
}

/// Marks the trailing columns of a clipped title so the fragment shader fades them smoothly.
fn apply_fade(cells: &mut [Cell]) {
    let len = cells.len();
    if len == 0 {
        return;
    }
    if len == 1 {
        if cells[0].flags & SPACER == 0 {
            cells[0].flags |= FADE_SINGLE;
        }
        return;
    }
    let last = len - 1;
    if cells[last].flags & SPACER != 0 {
        // Wide glyph covers the last two columns: fade across it smoothly.
        cells[last - 1].flags |= FADE_SINGLE;
        return;
    }
    cells[last].flags |= FADE_END;
    let prev = last - 1;
    if cells[prev].flags & SPACER != 0 {
        if prev > 0 {
            cells[prev - 1].flags |= FADE_START;
        }
    } else {
        cells[prev].flags |= FADE_START;
    }
}

/// Like `fit_title`, but keeps a name the user typed, including a leading mark.
pub(crate) fn fit_plain(title: &str, width: usize) -> (Cow<'_, str>, usize) {
    fit_text(title, width, false)
}

fn fit_text(title: &str, width: usize, strip_status: bool) -> (Cow<'_, str>, usize) {
    if width == 0 {
        return (Cow::Borrowed(""), 0);
    }
    let stripped;
    let title = if strip_status {
        stripped = without_status(title);
        if stripped.is_empty() { "shell" } else { stripped }
    } else {
        title
    };
    // Count the same cells as paint, including when the title has emoji or wide characters.
    let title_width = title.chars().filter_map(UnicodeWidthChar::width).sum();
    if title_width <= width {
        return (Cow::Borrowed(title), title_width);
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in title.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            continue;
        }
        if used + ch_width >= width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    (Cow::Owned(out), used + 1)
}

/// Drops the activity mark an agent puts before its title, such as Claude's `✳` or a
/// Braille spinner. The tab already shows the agent's icon, and the mark only pushed the text away from it.
pub(crate) fn display_title(title: &str) -> &str {
    let shown = without_status(title);
    if shown.is_empty() { "shell" } else { shown }
}

/// `None` clears the custom name so the shell title shows again.
pub(crate) fn committed_label(draft: &str, shell_title: &str) -> Option<String> {
    let text = draft.trim();
    if text.is_empty() || text == display_title(shell_title) {
        None
    } else {
        Some(text.to_string())
    }
}

impl LabelDraft {
    pub(crate) fn new(text: String) -> Self {
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    pub(crate) fn insert(&mut self, extra: &str) {
        let extra: String = extra.chars().filter(|ch| !ch.is_control()).collect();
        if extra.is_empty() {
            return;
        }
        let mut chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor.min(chars.len());
        let room = MAX_LABEL_CHARS.saturating_sub(chars.len());
        let add: Vec<char> = extra.chars().take(room).collect();
        let added = add.len();
        chars.splice(cursor..cursor, add);
        self.cursor = cursor + added;
        self.text = chars.into_iter().collect();
    }

    pub(crate) fn backspace(&mut self, word: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        if self.cursor == 0 || chars.is_empty() {
            return;
        }
        let cursor = self.cursor.min(chars.len());
        let start = if word { word_left(&chars, cursor) } else { cursor - 1 };
        let mut next = chars;
        next.drain(start..cursor);
        self.cursor = start;
        self.text = next.into_iter().collect();
    }

    pub(crate) fn delete_forward(&mut self, word: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        if self.cursor >= chars.len() {
            return;
        }
        let end = if word { word_right(&chars, self.cursor) } else { self.cursor + 1 };
        let mut next = chars;
        next.drain(self.cursor..end);
        self.text = next.into_iter().collect();
    }

    pub(crate) fn left(&mut self, word: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor.min(chars.len());
        self.cursor = if word { word_left(&chars, cursor) } else { cursor.saturating_sub(1) };
    }

    pub(crate) fn right(&mut self, word: bool) {
        let chars: Vec<char> = self.text.chars().collect();
        let cursor = self.cursor.min(chars.len());
        self.cursor = if word { word_right(&chars, cursor) } else { (cursor + 1).min(chars.len()) };
    }

    pub(crate) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    pub(crate) fn click(&mut self, width: usize, col: usize) {
        self.cursor = edit_cursor_at(&self.text, width, self.cursor, col);
    }
}

fn word_left(chars: &[char], cursor: usize) -> usize {
    let mut index = cursor.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

fn word_right(chars: &[char], cursor: usize) -> usize {
    let mut index = cursor.min(chars.len());
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    index
}

struct EditWindow {
    visible: String,
    cursor_col: usize,
    origin: usize,
}

/// The slice of a label that fits in `width` columns, with the cursor kept on screen.
fn edit_window(text: &str, width: usize, cursor: usize) -> EditWindow {
    let chars: Vec<char> = text.chars().collect();
    let widths: Vec<usize> = chars.iter().map(|ch| UnicodeWidthChar::width(*ch).unwrap_or(0)).collect();
    let cursor = cursor.min(chars.len());
    if width == 0 {
        return EditWindow { visible: String::new(), cursor_col: 0, origin: cursor };
    }
    let caret_extra = usize::from(cursor == chars.len());
    let total = widths.iter().sum::<usize>() + caret_extra;
    if total <= width {
        return EditWindow {
            visible: text.to_string(),
            cursor_col: widths[..cursor].iter().sum(),
            origin: 0,
        };
    }
    if cursor < chars.len() && widths[cursor] >= width {
        let visible = chars[cursor].to_string();
        return EditWindow {
            visible,
            cursor_col: 0,
            origin: cursor,
        };
    }
    // One column stays free for a leading ellipsis when the start scrolls away.
    let budget = width - 1;
    let mut start = cursor;
    let mut used = caret_extra;
    if cursor < chars.len() {
        used += widths[cursor];
    }
    while start > 0 {
        let next = widths[start - 1];
        if used + next > budget {
            break;
        }
        used += next;
        start -= 1;
    }
    let mut end = if cursor < chars.len() { cursor + 1 } else { cursor };
    while end < chars.len() {
        let next = widths[end];
        if used + next > budget {
            break;
        }
        used += next;
        end += 1;
    }
    let body: String = chars[start..end].iter().collect();
    let cursor_col = widths[start..cursor].iter().sum();
    if start == 0 {
        EditWindow { cursor_col, origin: 0, visible: body }
    } else {
        let mut visible = String::from("…");
        visible.push_str(&body);
        EditWindow {
            cursor_col: 1 + cursor_col,
            origin: start,
            visible,
        }
    }
}

pub(crate) fn field_view(text: &str, width: usize, cursor: usize) -> (String, usize) {
    let window = edit_window(text, width, cursor);
    (window.visible, window.cursor_col)
}

fn edit_cursor_at(text: &str, width: usize, cursor: usize, col: usize) -> usize {
    let window = edit_window(text, width, cursor);
    if width == 0 {
        return window.origin;
    }
    let ellipsis = window.origin > 0 && window.visible.starts_with('…');
    if ellipsis && col == 0 {
        return window.origin;
    }
    let target = col.saturating_sub(usize::from(ellipsis));
    let mut used = 0;
    let mut index = window.origin;
    for ch in window.visible.chars().skip(usize::from(ellipsis)) {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            index += 1;
            continue;
        }
        if target < used + ch_width {
            return index;
        }
        used += ch_width;
        index += 1;
    }
    index
}

fn paint_caret(cells: &mut [Cell], col: usize, fg: u32, bg: u32, flags: u32) {
    if col >= cells.len() || cells[col].flags & SPACER != 0 {
        return;
    }
    if cells[col].cp == 0 {
        cells[col] = Cell { cp: u32::from(' '), fg, bg, flags };
    }
    cells[col].flags |= REVERSE;
}

fn without_status(title: &str) -> &str {
    let title = title.trim_start();
    let mut chars = title.chars();
    let Some(first) = chars.next() else {
        return title;
    };
    let status = matches!(u32::from(first), 0x25A0..=0x27BF | 0x2800..=0x28FF) || matches!(first, '·' | '•' | '∙' | '*');
    let rest = chars.as_str();
    if status && rest.starts_with(char::is_whitespace) && !rest.trim_start().is_empty() {
        rest.trim_start()
    } else {
        title
    }
}

pub(crate) fn paint(cells: &mut [Cell], text: &str, fg: u32, bg: u32, flags: u32) {
    let mut col = 0;
    for ch in text.chars() {
        if col >= cells.len() {
            break;
        }
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            continue;
        }
        if col + width > cells.len() {
            break;
        }
        cells[col] = Cell {
            cp: ch as u32,
            fg,
            bg,
            flags: flags | if width == 2 { WIDE } else { 0 },
        };
        if width == 2 {
            cells[col + 1] = Cell {
                cp: 0,
                fg,
                bg,
                flags: SPACER,
            };
        }
        col += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_use_chrome_max_width_with_new_tab_button_and_no_numbers() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["alpha".into(), "beta".into()], &[], 0, 100, theme, false);
        assert_eq!(bar.spans.len(), 2);
        assert_eq!(bar.spans[0].start, 1);
        assert_eq!(bar.spans[0].end - bar.spans[0].start, MAX_TAB_COLS as u16);
        assert_eq!(bar.spans[1].start, bar.spans[0].end);
        assert_eq!(bar.spans[1].end - bar.spans[1].start, MAX_TAB_COLS as u16);
        let new_tab = bar.new_tab.clone().expect("new tab button");
        assert_eq!(new_tab.start, bar.spans[1].end);
        assert_eq!(new_tab.end - new_tab.start, NEW_TAB_COLS as u16);
        assert_eq!(hit(&bar, new_tab.start), Some(BarHit::NewTab));
        assert_eq!(hover(&bar, new_tab.start), Some(BarHover::NewTab));
        // Clicking empty space past '+' returns None.
        assert_eq!(hit(&bar, new_tab.end + 5), None);
        // No tab numbers anywhere.
        assert!(!bar.cells.iter().any(|cell| cell.cp == u32::from('1') || cell.cp == u32::from('2')));
        let close = bar.spans[0].close.expect("close column");
        assert_eq!(bar.cells[close as usize].cp, u32::from('×'));
        assert_eq!(hit(&bar, bar.spans[0].start), Some(BarHit::Select(0)));
        assert_eq!(hit(&bar, close), Some(BarHit::Close(0)));
        assert_eq!(hit(&bar, bar.spans[1].start), Some(BarHit::Select(1)));
        assert_eq!(drop_index(&bar, close), Some(0));
        assert_eq!(drop_index(&bar, 99), Some(1));
        assert!(!bar.cells.iter().any(|cell| cell.cp == crate::agent::SETTINGS_ICON));
    }

    #[test]
    fn long_title_clips_and_fades_without_ellipsis() {
        let bar = render_bar(&["a title too long for this tab".into(), "beta".into()], &[], 0, 40, crate::color::theme(0), false);
        let span = bar.spans[0];
        assert!(span.truncated);
        let label_cells = &bar.cells[usize::from(span.label_at)..usize::from(span.label_at + span.label_cols)];
        let text: String = label_cells
            .iter()
            .filter_map(|cell| char::from_u32(cell.cp))
            .filter(|ch| *ch != '\0')
            .collect();
        assert!(!text.contains('…'), "{text}");
        assert!(text.starts_with("a title"), "{text}");
        let last = label_cells.last().expect("last cell");
        let prev = &label_cells[label_cells.len() - 2];
        assert_eq!(last.flags & (3 << crate::cell::FADE_SHIFT), FADE_END);
        assert_eq!(prev.flags & (3 << crate::cell::FADE_SHIFT), FADE_START);
        let close = usize::from(span.close.expect("close column"));
        assert_eq!(bar.cells[close - 1].cp, 0);
    }

    #[test]
    fn tabs_shrink_when_many_while_keeping_new_tab_button() {
        let theme = crate::color::theme(0);
        let two = render_bar(&["a".into(), "b".into()], &[], 0, 80, theme, false);
        let titles: Vec<String> = (0..6).map(|index| format!("tab {index}")).collect();
        let many = render_bar(&titles, &[], 0, 80, theme, false);
        let two_w = two.spans[0].end - two.spans[0].start;
        let many_w = many.spans[0].end - many.spans[0].start;
        assert_eq!(two_w, MAX_TAB_COLS as u16);
        assert!(many_w < two_w);
        for pair in many.spans.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        let widths: Vec<u16> = many.spans.iter().map(|span| span.end - span.start).collect();
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1);
        let plus = many.new_tab.expect("new tab button");
        assert_eq!(plus.start, many.spans.last().expect("tabs").end);
        assert_eq!(plus.end, 79);
    }

    #[test]
    fn all_agents_use_the_same_two_cell_icon_area() {
        let agents = [Agent::Claude, Agent::Codex, Agent::Kiro, Agent::Antigravity, Agent::Cursor];
        let titles = vec!["shell".to_string(); agents.len()];
        let icons: Vec<_> = agents.iter().copied().map(Some).collect();
        let bar = render_bar(&titles, &icons, 0, 120, crate::color::theme(0), false);
        for (index, span) in bar.spans.iter().enumerate() {
            let icon = &bar.cells[usize::from(span.start) + 1];
            assert_eq!(icon.cp, agents[index].icon());
            assert_eq!(icon.flags & (AGENT_ICON | WIDE), AGENT_ICON | WIDE);
            assert_eq!(bar.cells[usize::from(span.start) + 2].flags, SPACER);
        }
    }

    #[test]
    fn agent_status_mark_does_not_widen_the_gap_after_the_icon() {
        let theme = crate::color::theme(0);
        let titles = vec!["✳ Problema".to_string(), "⠐ Problema".to_string(), "Problema".to_string()];
        let agents = [Some(Agent::Claude), Some(Agent::Claude), Some(Agent::Codex)];
        let bar = render_bar(&titles, &agents, 0, 120, theme, false);
        for span in &bar.spans {
            let start = usize::from(span.start);
            assert_eq!(bar.cells[start + 2].flags, SPACER);
            assert_eq!(bar.cells[start + 3].cp, 0);
            assert_eq!(bar.cells[start + 4].cp, u32::from('P'));
        }
        assert_eq!(without_status("~/Git ✳ x"), "~/Git ✳ x");
        assert_eq!(without_status("* "), "* ");
        assert_eq!(without_status("*.rs files"), "*.rs files");
    }

    #[test]
    fn shell_tab_uses_rustty_icon_when_no_agent_is_running() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["shell".into(), "work".into()], &[None, None], 0, 80, theme, false);
        let start = usize::from(bar.spans[0].start);
        assert_eq!(bar.cells[start + 1].cp, crate::agent::RUSTTY_ICON);
        assert_eq!(bar.cells[start + 1].flags & (AGENT_ICON | WIDE), AGENT_ICON | WIDE);
        assert_eq!(bar.cells[start + 2].flags, SPACER);
    }

    #[test]
    fn bar_cells_keep_bar_background_for_chrome_vector_rendering() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string()];
        let bar = render_bar(&titles, &[], 1, 80, theme, true);
        assert!(bar.cells.iter().all(|cell| cell.bg == theme.bar.pack()));
        assert_eq!(bar.spans[0].start, 1);
        assert_eq!(hit(&bar, bar.spans[1].start), Some(BarHit::Select(1)));
        let active_text = bar.cells[usize::from(bar.spans[1].start)..usize::from(bar.spans[1].end)]
            .iter()
            .find(|cell| cell.cp == u32::from('b'))
            .expect("title");
        assert_eq!(active_text.fg, theme.fg.pack());
    }

    #[test]
    fn narrow_inactive_tab_reveals_close_on_hover_and_wide_tabs_keep_close_aligned() {
        let theme = crate::color::theme(0);
        let titles: Vec<String> = (0..8).map(|i| format!("session {i}")).collect();
        let bar = render_bar_in(&titles, &[], 0, 82, theme, true, Some((1, 81)), None, &[], None);
        // Narrow inactive tabs (< 12 cols) hide close until hovered.
        assert!(bar.spans[1].end - bar.spans[1].start < 12);
        assert!(bar.spans[1].close.is_none());
        let slot = bar.spans[1].close_slot.expect("close slot");
        assert_eq!(hover(&bar, slot), Some(BarHover::Tab { tab: 1, close: true }));
        let hovered = render_bar_in(
            &titles,
            &[],
            0,
            82,
            theme,
            true,
            Some((1, 81)),
            None,
            &[],
            Some(BarHover::Tab { tab: 1, close: true }),
        );
        assert_eq!(hovered.spans[1].close, Some(slot));
        assert_eq!(hovered.cells[usize::from(slot)].cp, u32::from('×'));
        assert_eq!(hovered.cells[usize::from(slot)].fg, theme.fg.pack());
        assert_eq!(hit(&hovered, slot), Some(BarHit::Close(1)));
    }

    #[test]
    fn tooltip_renders_full_title_and_wraps_long_text() {
        let theme = crate::color::theme(0);
        let short = render_tooltip("Cursor Agent", 60, 23, theme).expect("tooltip");
        assert_eq!(short.rows, 1);
        assert_eq!(short.cols, 12);
        assert_eq!(short.anchor_col, 23);

        let long_title = "Very long tab title describing a deep project repository and current agent task";
        let wrapped = render_tooltip(long_title, 30, 1, theme).expect("wrapped");
        assert!(wrapped.rows >= 2 && wrapped.rows <= 4);
        assert!(wrapped.cols <= 30);
        let joined: String = (0..usize::from(wrapped.rows))
            .map(|row| {
                let start = row * usize::from(wrapped.cols);
                wrapped.cells[start..start + usize::from(wrapped.cols)]
                    .iter()
                    .filter_map(|c| char::from_u32(c.cp))
                    .filter(|ch| *ch != '\0')
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("Very long"), "{joined}");
        assert!(joined.contains("agent task"), "{joined}");
    }

    #[test]
    fn editing_a_label_paints_the_draft_and_a_block_cursor() {
        let theme = crate::color::theme(0);
        let edit = LabelEdit { index: 1, text: "beta", cursor: 4, cursor_on: true };
        let bar = render_bar_in(
            &["alpha".into(), "old".into()],
            &[],
            1,
            80,
            theme,
            false,
            None,
            Some(edit),
            &[],
            None,
        );
        let span = bar.spans[1];
        let text: String = bar.cells[span.label_at as usize..(span.label_at + span.label_cols) as usize]
            .iter()
            .filter_map(|cell| char::from_u32(cell.cp))
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(text.starts_with("beta"), "{text}");
        let caret = bar.cells.iter().find(|cell| cell.flags & REVERSE != 0).expect("caret");
        assert_eq!(caret.cp, u32::from(' '));
        let middle = LabelEdit { index: 0, text: "beta", cursor: 1, cursor_on: true };
        let bar = render_bar_in(&["beta".into(), "old".into()], &[], 0, 80, theme, false, None, Some(middle), &[], None);
        let span = bar.spans[0];
        let caret = bar.cells[span.label_at as usize..]
            .iter()
            .find(|cell| cell.flags & REVERSE != 0)
            .expect("caret on the letter");
        assert_eq!(caret.cp, u32::from('e'));
    }

    #[test]
    fn a_custom_label_keeps_a_leading_mark() {
        let theme = crate::color::theme(0);
        let bar = render_bar_in(&["* notes".into()], &[], 0, 40, theme, false, None, None, &[true], None);
        let text: String = bar.cells.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(text.contains('*'), "{text}");
    }

    #[test]
    fn the_editor_keeps_the_cursor_inside_a_narrow_tab() {
        let window = edit_window("abcdefghijklmnopqrstuvwxyz", 5, 26);
        assert!(window.visible.starts_with('…'));
        assert!(window.visible.ends_with("yz"));
        assert!(window.cursor_col < 5);
        assert_eq!(edit_cursor_at("abcdef", 20, 6, 0), 0);
        assert_eq!(edit_cursor_at("abcdef", 20, 6, 3), 3);
        assert_eq!(edit_cursor_at("abcdef", 20, 6, 6), 6);
        assert_eq!(edit_cursor_at("abcdef", 20, 6, 9), 6);
    }

    #[test]
    fn committing_the_shell_title_clears_the_custom_name() {
        assert_eq!(committed_label("  build  ", "shell"), Some("build".into()));
        assert_eq!(committed_label("shell", "shell"), None);
        assert_eq!(committed_label("   ", "rustty"), None);
        assert_eq!(committed_label("Problema", "✳ Problema"), None);
        let mut draft = LabelDraft::new("one two".into());
        draft.left(true);
        assert_eq!(draft.cursor, 4);
        draft.backspace(true);
        assert_eq!(draft.text, "two");
        draft.insert("\nab");
        assert_eq!(draft.text, "abtwo");
    }
}
