//! Top bar of the window, painted with the theme colors.

use std::ops::Range;

use crate::agent::Agent;
use crate::cell::{AGENT_ICON, BOLD, Cell, SPACER, WIDE};
use crate::color::Theme;
use crate::desktop::{Button, ButtonLayout};
use crate::tabs;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleBar {
    pub cells: Vec<Cell>,
    pub minimize: Range<u16>,
    pub maximize: Range<u16>,
    pub close: Range<u16>,
    /// The settings gear, present when there is no tab bar to hold it.
    pub settings: Range<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleHit {
    Drag,
    Minimize,
    Maximize,
    Close,
    Settings,
}

pub fn render(
    title: &str,
    agent: Option<Agent>,
    cols: u16,
    theme: Theme,
    maximized: bool,
    layout: &ButtonLayout,
    settings: bool,
    glass: bool,
) -> TitleBar {
    let cols = usize::from(cols);
    let bg = theme.bar.pack();
    let fg = theme.fg.pack();
    let mut cells = vec![Cell::blank(fg, bg); cols];
    let button = if cols >= 16 { 3 } else { 2 };
    let count = layout.left.len() + layout.right.len();
    // The buttons go only when the title keeps at least the icon and a few letters.
    let show = count > 0 && cols >= button * count + 6.min(cols);
    let mut ranges = [0..0, 0..0, 0..0];
    let mut left_end = 0;
    let mut right_start = cols;
    if show {
        for &kind in &layout.left {
            ranges[kind as usize] = left_end..left_end + button;
            left_end += button;
        }
        right_start = cols - button * layout.right.len();
        let mut at = right_start;
        for &kind in &layout.right {
            ranges[kind as usize] = at..at + button;
            at += button;
        }
        for (kind, range) in [Button::Minimize, Button::Maximize, Button::Close].into_iter().zip(&ranges) {
            let mark = match kind {
                Button::Minimize => '\u{2212}',
                Button::Maximize if maximized => '\u{29C9}',
                Button::Maximize => '\u{25A1}',
                Button::Close => '\u{00D7}',
            };
            // On glass the marks are vector strokes drawn on the round buttons.
            if !glass {
                paint_button(&mut cells, range.clone(), mark, fg, bg);
            }
        }
    }
    // The settings gear is one more button in the row, next to the close button and the
    // same size as the others, so the circles keep an even spacing.
    let mut settings_range = 0..0;
    if settings && right_start - left_end >= button + 6 {
        let on_left = show && layout.left.contains(&Button::Close);
        if on_left {
            settings_range = left_end..left_end + button;
            left_end += button;
        } else {
            settings_range = right_start - button..right_start;
            right_start -= button;
        }
        // On glass the gear is a vector shape on its button. Otherwise the two-cell icon.
        if !glass {
            let at = settings_range.start + (button - 2) / 2;
            cells[at] = Cell { cp: crate::agent::SETTINGS_ICON, fg, bg, flags: AGENT_ICON | WIDE };
            cells[at + 1] = Cell { cp: 0, fg, bg, flags: SPACER };
        }
    }
    // The subject is centered on the window and moves aside only when a button is in the way.
    let room = right_start - left_end;
    let prefix = if room >= 6 { 3 } else { 0 };
    let (label, label_width) = tabs::fit_plain(title, room - prefix);
    let width = prefix + label_width;
    let start = ((cols - width) / 2).clamp(left_end, right_start - width);
    if prefix > 0 {
        tabs::paint_icon(&mut cells[start..right_start], agent, fg, bg);
    }
    tabs::paint(&mut cells[start + prefix..right_start], &label, fg, bg, BOLD);
    let [minimize, maximize, close] = ranges.map(|range| range.start as u16..range.end as u16);
    TitleBar { cells, minimize, maximize, close, settings: settings_range.start as u16..settings_range.end as u16 }
}

pub fn hit(bar: &TitleBar, col: u16) -> TitleHit {
    if bar.settings.contains(&col) {
        TitleHit::Settings
    } else if bar.close.contains(&col) {
        TitleHit::Close
    } else if bar.maximize.contains(&col) {
        TitleHit::Maximize
    } else if bar.minimize.contains(&col) {
        TitleHit::Minimize
    } else {
        TitleHit::Drag
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
        let night = crate::color::theme(crate::color::theme_index("night"));
        let paper = crate::color::theme(crate::color::theme_index("paper"));
        let bar = render("Codex subject", Some(Agent::Codex), 80, night, false, &ButtonLayout::default(), false, false);
        assert_eq!(bar.cells[0].bg, night.bar.pack());
        assert_ne!(bar.cells[0].bg, paper.bar.pack());
        assert_eq!(bar.cells[bar.close.start as usize + 1].cp, u32::from('\u{00D7}'));
        assert_eq!(bar.cells[bar.maximize.start as usize + 1].cp, u32::from('\u{25A1}'));
        let start = (80 - "Codex subject".len() - 3) / 2;
        assert_eq!(bar.cells[start].cp, Agent::Codex.icon());
        assert_eq!(bar.cells[start].flags, crate::cell::AGENT_ICON | crate::cell::WIDE);
        assert_eq!(bar.cells[start + 3].cp, u32::from('C'));
        assert_eq!(hit(&bar, bar.close.start), TitleHit::Close);
        assert_eq!(hit(&bar, 4), TitleHit::Drag);
        let restored = render("shell", None, 80, night, true, &ButtonLayout::default(), false, false);
        assert_eq!(restored.cells[restored.maximize.start as usize + 1].cp, u32::from('\u{29C9}'));
    }

    #[test]
    fn narrow_title_bar_drops_the_buttons_instead_of_overlapping_the_title() {
        let theme = crate::color::theme(0);
        let bar = render("Rustty", None, 4, theme, false, &ButtonLayout::default(), false, false);
        assert!(bar.close.is_empty());
        assert!(bar.cells.iter().any(|cell| cell.cp == u32::from('R')));
    }

    #[test]
    fn title_and_icon_fit_before_the_window_controls() {
        let bar = render("東京の長い話題", None, 16, crate::color::theme(0), false, &ButtonLayout::default(), false, false);
        assert_eq!(bar.cells[1].cp, crate::agent::RUSTTY_ICON);
        assert_eq!(bar.cells[4].cp, u32::from('東'));
        assert_eq!(bar.cells[5].flags, crate::cell::SPACER);
        assert_eq!(bar.cells[6].cp, u32::from('…'));
        assert_eq!(bar.minimize.start, 7);
    }

    #[test]
    fn buttons_follow_the_desktop_layout() {
        let theme = crate::color::theme(0);
        let gnome = render("shell", None, 80, theme, false, &ButtonLayout::parse("appmenu:close"), false, false);
        assert_eq!(gnome.close, 77..80);
        assert!(gnome.minimize.is_empty() && gnome.maximize.is_empty());
        assert_eq!(gnome.cells[78].cp, u32::from('\u{00D7}'));
        assert!(!gnome.cells.iter().any(|cell| cell.cp == u32::from('\u{2212}')));
        assert_eq!(hit(&gnome, 70), TitleHit::Drag);

        let left = render("shell", None, 80, theme, false, &ButtonLayout::parse("close,minimize:"), false, false);
        assert_eq!(left.close, 0..3);
        assert_eq!(left.minimize, 3..6);
        assert_eq!(left.cells[1].cp, u32::from('\u{00D7}'));
        assert_eq!(hit(&left, 4), TitleHit::Minimize);
        let start = (80 - "shell".len() - 3) / 2;
        assert_eq!(left.cells[start].cp, crate::agent::RUSTTY_ICON);

        let none = render("shell", None, 80, theme, false, &ButtonLayout::parse(""), false, false);
        assert!(none.close.is_empty());
    }

    #[test]
    fn gear_sits_next_to_close_with_the_same_width_as_the_buttons() {
        let theme = crate::color::theme(0);
        let bar = render("shell", None, 80, theme, false, &ButtonLayout::parse("appmenu:close"), true, false);
        assert_eq!(bar.close, 77..80);
        assert_eq!(bar.settings, 74..77);
        assert_eq!(bar.cells[74].cp, crate::agent::SETTINGS_ICON);
        assert_eq!(bar.cells[74].flags, AGENT_ICON | WIDE);
        assert_eq!(hit(&bar, 75), TitleHit::Settings);
        let glass = render("shell", None, 80, theme, false, &ButtonLayout::parse("appmenu:close"), true, true);
        assert_eq!(glass.settings, 74..77);
        assert_eq!(glass.cells[74].cp, 0);
        let full = render("shell", None, 80, theme, false, &ButtonLayout::default(), true, true);
        assert_eq!(full.settings.end, full.minimize.start);
        let left = render("shell", None, 80, theme, false, &ButtonLayout::parse("close,minimize:"), true, true);
        assert_eq!(left.settings, 6..9);
        let none = render("shell", None, 80, theme, false, &ButtonLayout::parse(""), true, false);
        assert_eq!(none.settings, 77..80);
    }
}
