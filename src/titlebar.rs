//! Top bar of the window, painted with the theme colors.

use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cell::{Cell, BOLD};
use crate::color::Theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleBar {
    pub cells: Vec<Cell>,
    pub minimize: Range<u16>,
    pub maximize: Range<u16>,
    pub close: Range<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleHit {
    Drag,
    Minimize,
    Maximize,
    Close,
}

pub fn render(title: &str, cols: u16, theme: Theme, maximized: bool) -> TitleBar {
    let cols = usize::from(cols);
    let bg = theme.bar.pack();
    let fg = theme.fg.pack();
    let mut cells = vec![Cell::blank(fg, bg); cols];
    let button = if cols >= 16 { 3 } else { 2 };
    let buttons = if cols >= button * 3 { 3 } else { 0 };
    let used = button * buttons;
    let origin = cols.saturating_sub(used);
    let minimize = origin..origin + button;
    let maximize = minimize.end..minimize.end + button;
    let close = maximize.end..maximize.end + button;
    if buttons == 3 {
        paint_button(&mut cells, minimize.clone(), '\u{2212}', fg, bg);
        let mark = if maximized { '\u{29C9}' } else { '\u{25A1}' };
        paint_button(&mut cells, maximize.clone(), mark, fg, bg);
        paint_button(&mut cells, close.clone(), '\u{00D7}', fg, bg);
    }
    let room = origin;
    let label = fit_title(if title.is_empty() { "Rustty" } else { title }, room);
    let label_w = label.width();
    let x = room.saturating_sub(label_w) / 2;
    paint(&mut cells[x..room], &label, fg, bg);
    TitleBar {
        cells,
        minimize: shrink(minimize, buttons == 3),
        maximize: shrink(maximize, buttons == 3),
        close: shrink(close, buttons == 3),
    }
}

pub fn hit(bar: &TitleBar, col: u16) -> TitleHit {
    if bar.close.contains(&col) {
        TitleHit::Close
    } else if bar.maximize.contains(&col) {
        TitleHit::Maximize
    } else if bar.minimize.contains(&col) {
        TitleHit::Minimize
    } else {
        TitleHit::Drag
    }
}

fn shrink(range: Range<usize>, keep: bool) -> Range<u16> {
    if keep {
        range.start as u16..range.end as u16
    } else {
        0..0
    }
}

fn paint_button(cells: &mut [Cell], range: Range<usize>, mark: char, fg: u32, bg: u32) {
    if range.end > cells.len() || range.is_empty() {
        return;
    }
    let at = range.start + (range.end - range.start) / 2;
    cells[at] = Cell {
        cp: u32::from(mark),
        fg,
        bg,
        flags: BOLD,
    };
}

fn fit_title(title: &str, width: usize) -> String {
    if title.width() <= width {
        return title.to_string();
    }
    if width == 0 {
        return String::new();
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
    if out.width() < title.width() && !out.is_empty() {
        out.pop();
        out.push('…');
    }
    out
}

fn paint(cells: &mut [Cell], text: &str, fg: u32, bg: u32) {
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
            flags: 0,
        };
        col += width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bar_uses_the_theme_and_keeps_the_buttons_on_the_right() {
        let noite = crate::color::theme(crate::color::theme_index("noite"));
        let papel = crate::color::theme(crate::color::theme_index("papel"));
        let bar = render("shell", 80, noite, false);
        assert_eq!(bar.cells[0].bg, noite.bar.pack());
        assert_ne!(bar.cells[0].bg, papel.bar.pack());
        assert_eq!(bar.cells[bar.close.start as usize + 1].cp, u32::from('\u{00D7}'));
        assert_eq!(bar.cells[bar.maximize.start as usize + 1].cp, u32::from('\u{25A1}'));
        assert!(bar.cells.iter().any(|cell| cell.cp == u32::from('s')));
        assert_eq!(hit(&bar, bar.close.start), TitleHit::Close);
        assert_eq!(hit(&bar, 4), TitleHit::Drag);
        let restored = render("shell", 80, noite, true);
        assert_eq!(restored.cells[restored.maximize.start as usize + 1].cp, u32::from('\u{29C9}'));
    }

    #[test]
    fn narrow_title_bar_drops_the_buttons_instead_of_overlapping_the_title() {
        let theme = crate::color::theme(0);
        let bar = render("Rustty", 4, theme, false);
        assert!(bar.close.is_empty());
        assert!(bar.cells.iter().any(|cell| cell.cp == u32::from('R')));
    }
}
