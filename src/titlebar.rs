//! Top bar of the window, painted with the theme colors.

use std::ops::Range;

use crate::agent::Agent;
use crate::cell::{Cell, BOLD};
use crate::color::Theme;
use crate::tabs;

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

pub fn render(title: &str, agent: Option<Agent>, cols: u16, theme: Theme, maximized: bool) -> TitleBar {
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
    // Leave room for the subject before adding the two-cell icon and a gap.
    let prefix = if origin >= 6 { 3 } else { 0 };
    let (label, label_width) = tabs::fit_title(title, origin - prefix);
    let width = prefix + label_width;
    let start = ((cols - width) / 2).min(origin - width);
    if prefix > 0 {
        tabs::paint_icon(&mut cells[start..origin], agent, fg, bg);
    }
    tabs::paint(&mut cells[start + prefix..origin], &label, fg, bg, BOLD);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_bar_uses_the_theme_and_keeps_the_buttons_on_the_right() {
        let noite = crate::color::theme(crate::color::theme_index("noite"));
        let papel = crate::color::theme(crate::color::theme_index("papel"));
        let bar = render("Codex subject", Some(Agent::Codex), 80, noite, false);
        assert_eq!(bar.cells[0].bg, noite.bar.pack());
        assert_ne!(bar.cells[0].bg, papel.bar.pack());
        assert_eq!(bar.cells[bar.close.start as usize + 1].cp, u32::from('\u{00D7}'));
        assert_eq!(bar.cells[bar.maximize.start as usize + 1].cp, u32::from('\u{25A1}'));
        let start = (80 - "Codex subject".len() - 3) / 2;
        assert_eq!(bar.cells[start].cp, Agent::Codex.icon());
        assert_eq!(bar.cells[start].flags, crate::cell::AGENT_ICON | crate::cell::WIDE);
        assert_eq!(bar.cells[start + 3].cp, u32::from('C'));
        assert_eq!(hit(&bar, bar.close.start), TitleHit::Close);
        assert_eq!(hit(&bar, 4), TitleHit::Drag);
        let restored = render("shell", None, 80, noite, true);
        assert_eq!(restored.cells[restored.maximize.start as usize + 1].cp, u32::from('\u{29C9}'));
    }

    #[test]
    fn narrow_title_bar_drops_the_buttons_instead_of_overlapping_the_title() {
        let theme = crate::color::theme(0);
        let bar = render("Rustty", None, 4, theme, false);
        assert!(bar.close.is_empty());
        assert!(bar.cells.iter().any(|cell| cell.cp == u32::from('R')));
    }

    #[test]
    fn title_and_icon_fit_before_the_window_controls() {
        let bar = render("東京の長い話題", None, 16, crate::color::theme(0), false);
        assert_eq!(bar.cells[1].cp, crate::agent::RUSTTY_ICON);
        assert_eq!(bar.cells[4].cp, u32::from('東'));
        assert_eq!(bar.cells[5].flags, crate::cell::SPACER);
        assert_eq!(bar.cells[6].cp, u32::from('…'));
        assert_eq!(bar.minimize.start, 7);
    }
}
