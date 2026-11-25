//! Tab strip drawn as one row of cells at the bottom of the window.

use std::borrow::Cow;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::Agent;
use crate::cell::{
    AGENT_ICON, BOLD, Cell, DIM, QUAD, REVERSE, SPACER, TAB, TAB_ACTIVE, TAB_FROM_LEFT_SHIFT,
    TAB_FROM_RIGHT_SHIFT, WIDE,
};
use crate::color::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSpan {
    pub start: u16,
    pub end: u16,
    /// Visible close column when the tab is active or hovered.
    pub close: Option<u16>,
    /// Column where the close button sits when shown, used for hover detection.
    pub(crate) close_slot: Option<u16>,
    /// First column of the title text, and how many columns that text may use.
    pub label_at: u16,
    pub label_cols: u16,
}

/// Pointer hover state on the tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BarHover {
    pub tab: usize,
    pub close: bool,
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
}

pub struct Bar {
    pub cells: Vec<Cell>,
    pub spans: Vec<TabSpan>,
    /// Index of the active tab in `spans`.
    pub selected: usize,
}

/// In glass mode every cell keeps the bar color, which the shader makes see-through, and the
/// tabs carry no shape of their own: the glass capsule and pill are drawn under them.
/// The labels occupy the columns under that capsule.
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

/// `track` is the column range under the glass capsule. Labels sit in that range,
/// split the same way the pill splits the capsule.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_bar_in(
    titles: &[String],
    agents: &[Option<Agent>],
    active: usize,
    cols: u16,
    theme: Theme,
    glass: bool,
    track: Option<(u16, u16)>,
    edit: Option<LabelEdit<'_>>,
    custom: &[bool],
    hover: Option<BarHover>,
) -> Bar {
    let cols = usize::from(cols);
    if titles.is_empty() || cols == 0 {
        return Bar { cells: Vec::new(), spans: Vec::new(), selected: 0 };
    }
    // The settings gear lives in the title bar, next to the window buttons.
    let (origin, tabs_end) = if glass {
        if let Some((start, end)) = track {
            let start = usize::from(start).min(cols.saturating_sub(1));
            let end = usize::from(end).clamp(start.saturating_add(1), cols);
            (start, end)
        } else if cols >= 12 {
            // One column on each side when the caller has no pixel track yet.
            (1, cols - 1)
        } else {
            (0, cols)
        }
    } else {
        (0, cols)
    };
    let mut cells = vec![Cell::blank(theme.muted.pack(), theme.bar.pack()); cols];
    // The strip is split across the tabs. The remainder goes to the first tabs, so the
    // last one always reaches the end of the row.
    let count = titles.len();
    let room = tabs_end - origin;
    let base = room / count;
    let extra = room % count;
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
        let bg = if glass {
            theme.bar.pack()
        } else if active_tab {
            theme.bg.pack()
        } else {
            theme.tab.pack()
        };
        let fg = if active_tab { theme.fg.pack() } else { theme.tab_fg.pack() };
        let flags = if active_tab { BOLD } else { 0 };
        for cell in &mut cells[start..end] {
            *cell = Cell {
                cp: 0,
                fg,
                bg,
                flags: 0,
            };
        }
        let span = end - start;
        // Compact left-aligned padding keeps titles readable without truncating early,
        // and anchors the close button neatly inside the right edge.
        let (lead, tail) = if span >= 16 { (2, 2) } else { (1, 1) };
        let icon_gap = 1;
        let close_slot = if span >= lead + icon_gap + tail + 6 {
            Some((end - tail - 1) as u16)
        } else {
            None
        };
        let show_close = active_tab || hover.is_some_and(|item| item.tab == index);
        let close = close_slot.filter(|_| show_close);
        // Reserve the close slot consistently so hovering a tab never shifts its title.
        let reserve = if close_slot.is_some() { tail + 2 } else { tail };
        let label_end = end.saturating_sub(reserve);
        let editing = edit.as_ref().copied().filter(|item| item.index == index);
        let custom_label = custom.get(index).copied().unwrap_or(false);
        let prefix = format!("{:lead$}{} ", "", index + 1, lead = lead);
        let can_fit_prefix_and_icon = start + prefix.width() + 2 <= label_end;
        let mut text_start = start;
        if can_fit_prefix_and_icon {
            paint(&mut cells[start..label_end], &prefix, fg, bg, flags);
            text_start = (start + prefix.width()).min(label_end);
        }
        if text_start + 2 <= label_end {
            paint_icon(&mut cells[text_start..label_end], agents.get(index).copied().flatten(), fg, bg);
            text_start += 2;
            if text_start < label_end {
                text_start = (text_start + icon_gap).min(label_end);
            }
        }
        let room = label_end.saturating_sub(text_start);
        let (fitted, cursor_col) = if let Some(edit) = editing {
            let window = edit_window(edit.text, room, edit.cursor);
            let cursor_col = edit.cursor_on.then_some(window.cursor_col);
            (Cow::Owned(window.visible), cursor_col)
        } else if custom_label {
            (fit_plain(title, room).0, None)
        } else {
            (fit_title(title, room).0, None)
        };
        paint(&mut cells[text_start..label_end], &fitted, fg, bg, flags);
        if let Some(cursor_col) = cursor_col {
            paint_caret(&mut cells[text_start..label_end], cursor_col, fg, bg, flags);
        }
        let number_at = start + lead;
        if can_fit_prefix_and_icon && number_at < label_end {
            let prefix_end = (number_at + (index + 1).to_string().len()).min(label_end);
            let (num_fg, num_flags) = if active_tab {
                (theme.accent.pack(), BOLD)
            } else {
                (theme.muted.pack(), DIM)
            };
            for cell in &mut cells[number_at..prefix_end] {
                cell.fg = num_fg;
                cell.flags = num_flags;
            }
        }
        if let Some(col) = close {
            let close_hot = hover.is_some_and(|item| item.tab == index && item.close);
            let (close_fg, close_flags) = if close_hot {
                (theme.fg.pack(), BOLD)
            } else {
                (theme.tab_fg.pack(), 0)
            };
            cells[col as usize] = Cell {
                cp: u32::from('×'),
                fg: close_fg,
                bg,
                flags: close_flags,
            };
        }
        // The shader rounds the ends. The gap between them separates tabs of the same color.
        if !glass {
            mark_tab(&mut cells[start..end], active_tab);
        }
        spans.push(TabSpan {
            start: start as u16,
            end: end as u16,
            close,
            close_slot,
            label_at: text_start as u16,
            label_cols: room as u16,
        });
        start = end;
    }
    Bar { cells, spans, selected: active }
}

pub fn hit(bar: &Bar, col: u16) -> Option<BarHit> {
    let index = drop_index(bar, col)?;
    let span = bar.spans.get(index)?;
    if span.close == Some(col) {
        Some(BarHit::Close(index))
    } else if (span.start..span.end).contains(&col) {
        Some(BarHit::Select(index))
    } else {
        None
    }
}

/// Tab under `col` and whether the pointer sits over its close slot.
pub(crate) fn hover(bar: &Bar, col: u16) -> Option<BarHover> {
    let index = bar.spans.iter().position(|span| (span.start..span.end).contains(&col))?;
    let span = bar.spans[index];
    Some(BarHover {
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

pub(crate) fn paint_icon(cells: &mut [Cell], agent: Option<Agent>, fg: u32, bg: u32) {
    let cp = agent.map_or(crate::agent::RUSTTY_ICON, Agent::icon);
    cells[0] = Cell { cp, fg, bg, flags: AGENT_ICON | WIDE };
    cells[1] = Cell { cp: 0, fg, bg, flags: SPACER };
}

pub(crate) fn fit_title(title: &str, width: usize) -> (Cow<'_, str>, usize) {
    fit_text(title, width, true)
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

fn mark_tab(cells: &mut [Cell], active: bool) {
    let shape = TAB | if active { TAB_ACTIVE } else { 0 };
    let len = cells.len();
    for (index, cell) in cells.iter_mut().enumerate() {
        if cell.flags & SPACER != 0 {
            continue;
        }
        let span = if cell.flags & QUAD != 0 {
            4
        } else if cell.flags & WIDE != 0 {
            2
        } else {
            1
        };
        let from_left = index.min(15) as u32;
        let from_right = len.saturating_sub(index + span).min(15) as u32;
        cell.flags |= shape | (from_left << TAB_FROM_LEFT_SHIFT) | (from_right << TAB_FROM_RIGHT_SHIFT);
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
    fn tabs_cover_the_row_and_the_cross_closes() {
        let bar = render_bar(&["alpha".into(), "beta".into()], &[], 0, 40, crate::color::theme(0), false);
        assert_eq!(bar.spans.len(), 2);
        assert_eq!(bar.spans[0].start, 0);
        assert_eq!(bar.spans[0].end, bar.spans[1].start);
        assert_eq!(bar.spans[1].end, 40);
        let close = bar.spans[0].close.expect("close column");
        assert_eq!(bar.cells[close as usize].cp, u32::from('×'));
        assert_ne!(bar.cells[bar.spans[0].start as usize].bg, bar.cells[bar.spans[1].start as usize].bg);
        assert_eq!(hit(&bar, bar.spans[0].start), Some(BarHit::Select(0)));
        assert_eq!(hit(&bar, close), Some(BarHit::Close(0)));
        assert_eq!(hit(&bar, bar.spans[1].start), Some(BarHit::Select(1)));
        assert_eq!(drop_index(&bar, close), Some(0));
        assert_eq!(drop_index(&bar, 40), Some(1));
        assert!(!bar.cells.iter().any(|cell| cell.cp == crate::agent::SETTINGS_ICON));
    }

    #[test]
    fn long_title_is_cut() {
        let bar = render_bar(&["a title too long for this tab".into()], &[], 0, 16, crate::color::theme(0), false);
        let cells = &bar.cells;
        let spans = &bar.spans;
        let text: String = cells
            .iter()
            .filter_map(|cell| char::from_u32(cell.cp))
            .filter(|ch| *ch != '\0')
            .collect();
        assert!(text.contains('…'), "{text}");
        let close = usize::from(spans[0].close.expect("close column"));
        assert_eq!(cells[close - 1].cp, 0);
    }

    #[test]
    fn single_tab_fills_the_row_and_its_number_is_accented() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["shell".into()], &[], 0, 100, theme, false);
        assert_eq!(bar.spans[0].start, 0);
        assert_eq!(bar.spans[0].end, 100);
        assert_eq!(bar.cells[2].cp, u32::from('1'));
        assert_eq!(bar.cells[2].fg, theme.accent.pack());
        assert_eq!(bar.cells[50].bg, theme.bg.pack());
        assert_eq!(hit(&bar, 50), Some(BarHit::Select(0)));
        assert_eq!(hit(&bar, 99), Some(BarHit::Select(0)));
    }

    #[test]
    fn tabs_shrink_to_share_the_row() {
        let theme = crate::color::theme(0);
        let one = render_bar(&["shell".into()], &[], 0, 40, theme, false);
        let titles: Vec<String> = (0..6).map(|index| format!("tab {index}")).collect();
        let many = render_bar(&titles, &[], 0, 40, theme, false);
        assert_eq!(one.spans[0].end, 40);
        assert_eq!(many.spans.last().expect("tabs").end, 40);
        assert!(many.spans[0].end < one.spans[0].end);
        for pair in many.spans.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        let widths: Vec<u16> = many.spans.iter().map(|span| span.end - span.start).collect();
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1);
    }

    #[test]
    fn all_agents_use_the_same_two_cell_icon_area() {
        let agents = [Agent::Claude, Agent::Codex, Agent::Kiro, Agent::Antigravity, Agent::Cursor];
        let titles = vec!["shell".to_string(); agents.len()];
        let icons: Vec<_> = agents.iter().copied().map(Some).collect();
        let bar = render_bar(&titles, &icons, 0, 120, crate::color::theme(0), false);
        for (index, span) in bar.spans.iter().enumerate() {
            let icon = &bar.cells[usize::from(span.start) + 4];
            assert_eq!(icon.cp, agents[index].icon());
            let shape = TAB | if index == 0 { TAB_ACTIVE } else { 0 };
            assert_eq!(icon.flags & (AGENT_ICON | WIDE | TAB | TAB_ACTIVE), AGENT_ICON | WIDE | shape);
            assert_eq!(bar.cells[usize::from(span.start) + 5].flags, SPACER);
        }
    }

    #[test]
    fn tabs_round_the_outer_cells_and_keep_the_active_one_attached() {
        let theme = crate::color::theme(0);
        let bar = render_bar(
            &["alpha".into(), "beta".into(), "gamma".into()],
            &[],
            1,
            60,
            theme,
            false,
        );
        for (index, span) in bar.spans.iter().enumerate() {
            let start = usize::from(span.start);
            let end = usize::from(span.end - 1);
            assert_eq!(bar.cells[start].flags & TAB, TAB);
            assert_eq!((bar.cells[start].flags >> TAB_FROM_LEFT_SHIFT) & 15, 0);
            assert_eq!((bar.cells[end].flags >> TAB_FROM_RIGHT_SHIFT) & 15, 0);
            assert_eq!((bar.cells[start].flags & TAB_ACTIVE) != 0, index == 1);
            if end > start {
                assert_eq!((bar.cells[start + 1].flags >> TAB_FROM_LEFT_SHIFT) & 15, 1);
            }
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
            assert_eq!(bar.cells[start + 5].flags, SPACER);
            assert_eq!(bar.cells[start + 6].cp, 0);
            assert_eq!(bar.cells[start + 7].cp, u32::from('P'));
        }
        assert_eq!(without_status("~/Git ✳ x"), "~/Git ✳ x");
        assert_eq!(without_status("* "), "* ");
        assert_eq!(without_status("*.rs files"), "*.rs files");
    }

    #[test]
    fn shell_tab_uses_rustty_icon_when_no_agent_is_running() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["shell".into()], &[None], 0, 80, theme, false);
        assert_eq!(bar.cells[4].cp, crate::agent::RUSTTY_ICON);
        assert_eq!(
            bar.cells[4].flags & (AGENT_ICON | WIDE | TAB | TAB_ACTIVE),
            AGENT_ICON | WIDE | TAB | TAB_ACTIVE
        );
        assert_eq!(bar.cells[5].flags, SPACER);
    }

    #[test]
    fn glass_bar_leaves_the_cells_see_through_and_room_for_the_capsule() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string()];
        let bar = render_bar(&titles, &[], 1, 80, theme, true);
        assert!(bar.cells.iter().all(|cell| cell.bg == theme.bar.pack()));
        assert!(bar.cells.iter().all(|cell| cell.flags & (TAB | TAB_ACTIVE) == 0));
        assert_eq!(bar.spans[0].start, 1);
        assert_eq!(bar.spans.last().expect("tabs").end, 79);
        assert_eq!(hit(&bar, bar.spans[1].start), Some(BarHit::Select(1)));
        let active_text = bar.cells[usize::from(bar.spans[1].start)..usize::from(bar.spans[1].end)]
            .iter()
            .find(|cell| cell.cp == u32::from('b'))
            .expect("title");
        assert_eq!(active_text.fg, theme.fg.pack());
    }

    #[test]
    fn glass_tabs_left_align_and_show_close_on_active_or_hover_only() {
        let theme = crate::color::theme(0);
        let titles = vec!["personal".to_string(), "Claude Code".to_string(), "Cursor Agent".to_string()];
        let bar = render_bar_in(&titles, &[], 2, 92, theme, true, Some((1, 91)), None, &[], None);
        // Active tab (2) shows its close mark and keeps the full title without truncating.
        assert!(bar.spans[0].close.is_none());
        assert!(bar.spans[1].close.is_none());
        let active_close = bar.spans[2].close.expect("active close");
        assert_eq!(bar.cells[usize::from(active_close)].cp, u32::from('×'));
        assert_eq!(bar.spans[2].end - 1 - active_close, 2);
        // Left-aligned: number sits 2 cols from the start of each tab.
        assert_eq!(bar.cells[usize::from(bar.spans[0].start) + 2].cp, u32::from('1'));
        assert_eq!(bar.cells[usize::from(bar.spans[2].start) + 2].cp, u32::from('3'));
        let active_label: String = bar.cells
            [usize::from(bar.spans[2].label_at)..usize::from(bar.spans[2].label_at + bar.spans[2].label_cols)]
            .iter()
            .filter_map(|cell| char::from_u32(cell.cp))
            .filter(|ch| *ch != '\0')
            .collect();
        assert_eq!(active_label.trim_end(), "Cursor Agent");

        // Hovering inactive tab 0 reveals its close button without shifting its title.
        let slot = bar.spans[0].close_slot.expect("close slot");
        assert_eq!(hover(&bar, slot), Some(BarHover { tab: 0, close: true }));
        let hovered = render_bar_in(
            &titles,
            &[],
            2,
            92,
            theme,
            true,
            Some((1, 91)),
            None,
            &[],
            Some(BarHover { tab: 0, close: true }),
        );
        assert_eq!(hovered.spans[0].close, Some(slot));
        assert_eq!(hovered.spans[0].label_at, bar.spans[0].label_at);
        assert_eq!(hovered.spans[0].label_cols, bar.spans[0].label_cols);
        assert_eq!(hovered.cells[usize::from(slot)].cp, u32::from('×'));
        assert_eq!(hovered.cells[usize::from(slot)].fg, theme.fg.pack());
        assert_eq!(hit(&hovered, slot), Some(BarHit::Close(0)));
    }

    #[test]
    fn glass_labels_follow_the_capsule_columns() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let bar = render_bar_in(&titles, &[], 0, 100, theme, true, Some((2, 90)), None, &[], None);
        assert_eq!(bar.spans[0].start, 2);
        assert_eq!(bar.spans[2].end, 90);
        assert_eq!(bar.spans[0].end, bar.spans[1].start);
        assert_eq!(bar.spans[1].end, bar.spans[2].start);
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
