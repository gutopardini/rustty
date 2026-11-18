//! Tab strip drawn as one row of cells at the bottom of the window.

use std::borrow::Cow;
use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::Agent;
use crate::cell::{
    AGENT_ICON, BOLD, Cell, QUAD, SPACER, TAB, TAB_ACTIVE, TAB_FROM_LEFT_SHIFT, TAB_FROM_RIGHT_SHIFT, WIDE,
};
use crate::color::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSpan {
    pub start: u16,
    pub end: u16,
    pub close: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarHit {
    Select(usize),
    Close(usize),
    Settings,
}

pub struct Bar {
    pub cells: Vec<Cell>,
    pub spans: Vec<TabSpan>,
    pub settings: Option<Range<u16>>,
}

pub fn render_bar(titles: &[String], agents: &[Option<Agent>], active: usize, cols: u16, theme: Theme, settings_lit: bool) -> Bar {
    let cols = usize::from(cols);
    if titles.is_empty() || cols == 0 {
        return Bar { cells: Vec::new(), spans: Vec::new(), settings: None };
    }
    let settings_width = if cols >= 10 {
        6
    } else if cols >= 7 {
        4
    } else {
        0
    };
    let usable = cols - settings_width;
    let mut cells = vec![Cell::blank(theme.muted.pack(), theme.bar.pack()); cols];
    // The usable strip is split across the tabs. The remainder goes to the first
    // tabs, so the last one always meets the settings button.
    let count = titles.len();
    let base = usable / count;
    let extra = usable % count;
    let mut spans = Vec::with_capacity(count);
    let mut start = 0usize;
    for (index, title) in titles.iter().enumerate() {
        let width = base + usize::from(index < extra);
        if width == 0 || start >= usable {
            break;
        }
        let end = (start + width).min(usable);
        if end <= start {
            break;
        }
        let active_tab = index == active;
        let bg = if active_tab { theme.bg.pack() } else { theme.tab.pack() };
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
        // Wide tabs get more air at the edges. The icon keeps one cell to each side,
        // the same as the number before it.
        let (lead, tail) = if span >= 20 { (4, 4) } else { (1, 1) };
        let icon_gap = 1;
        let close = if span >= lead + icon_gap + tail + 6 {
            Some((end - tail - 1) as u16)
        } else {
            None
        };
        // One breathing cell separates the title from the close button.
        let reserve = if close.is_some() { tail + 2 } else { 0 };
        let label_end = end.saturating_sub(reserve);
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
        let (fitted, _) = fit_title(title, label_end.saturating_sub(text_start));
        paint(&mut cells[text_start..label_end], &fitted, fg, bg, flags);
        let number_at = start + lead;
        if active_tab && number_at < end.saturating_sub(reserve) {
            // The number also marks the active tab when the bar is full.
            let prefix_end = (number_at + (index + 1).to_string().len()).min(end.saturating_sub(reserve));
            for cell in &mut cells[number_at..prefix_end] {
                cell.fg = theme.accent.pack();
            }
        }
        if let Some(col) = close {
            cells[col as usize] = Cell {
                cp: u32::from('×'),
                fg,
                bg,
                flags,
            };
        }
        // The shader rounds the ends. The gap between them separates tabs of the same color.
        mark_tab(&mut cells[start..end], active_tab);
        spans.push(TabSpan {
            start: start as u16,
            end: end as u16,
            close,
        });
        start = end;
    }
    let settings = if settings_width > 0 {
        let bg = if settings_lit { theme.tab.pack() } else { theme.bar.pack() };
        let fg = theme.fg.pack();
        for cell in &mut cells[usable..] {
            *cell = Cell::blank(fg, bg);
        }
        let icon_col = usable + (settings_width - 4) / 2;
        cells[icon_col] = Cell { cp: crate::agent::SETTINGS_ICON, fg, bg, flags: AGENT_ICON | QUAD };
        for cell in &mut cells[icon_col + 1..icon_col + 4] {
            *cell = Cell { cp: 0, fg, bg, flags: SPACER };
        }
        Some(usable as u16..cols as u16)
    } else {
        None
    };
    Bar { cells, spans, settings }
}

pub fn hit(bar: &Bar, col: u16) -> Option<BarHit> {
    if bar.settings.as_ref().is_some_and(|range| range.contains(&col)) {
        return Some(BarHit::Settings);
    }
    for (index, span) in bar.spans.iter().enumerate() {
        if (span.start..span.end).contains(&col) {
            if span.close == Some(col) {
                return Some(BarHit::Close(index));
            }
            return Some(BarHit::Select(index));
        }
    }
    None
}

pub(crate) fn paint_icon(cells: &mut [Cell], agent: Option<Agent>, fg: u32, bg: u32) {
    let cp = agent.map_or(crate::agent::RUSTTY_ICON, Agent::icon);
    cells[0] = Cell { cp, fg, bg, flags: AGENT_ICON | WIDE };
    cells[1] = Cell { cp: 0, fg, bg, flags: SPACER };
}

pub(crate) fn fit_title(title: &str, width: usize) -> (Cow<'_, str>, usize) {
    if width == 0 {
        return (Cow::Borrowed(""), 0);
    }
    let title = without_status(title);
    let title = if title.is_empty() { "shell" } else { title };
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
        let settings = bar.settings.as_ref().expect("settings button");
        assert!(bar.spans[1].end <= settings.start);
        assert_eq!(settings.end, 40);
        let close = bar.spans[0].close.expect("close column");
        assert_eq!(bar.cells[close as usize].cp, u32::from('×'));
        assert_ne!(bar.cells[bar.spans[0].start as usize].bg, bar.cells[bar.spans[1].start as usize].bg);
        assert_eq!(hit(&bar, bar.spans[0].start), Some(BarHit::Select(0)));
        assert_eq!(hit(&bar, close), Some(BarHit::Close(0)));
        assert_eq!(hit(&bar, bar.spans[1].start), Some(BarHit::Select(1)));
        for col in settings.clone() {
            assert_eq!(hit(&bar, col), Some(BarHit::Settings));
        }
        assert_eq!(settings.end - settings.start, 6);
        let icon = &bar.cells[usize::from(settings.start) + 1];
        assert_eq!(icon.cp, crate::agent::SETTINGS_ICON);
        assert_eq!(icon.flags, AGENT_ICON | QUAD);
        assert_eq!(bar.cells[usize::from(settings.start) + 2].flags, SPACER);
        assert_eq!(bar.cells[usize::from(settings.start) + 3].flags, SPACER);
        assert_eq!(bar.cells[usize::from(settings.start) + 4].flags, SPACER);
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
    fn single_tab_fills_up_to_settings_and_its_number_is_accented() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["shell".into()], &[], 0, 100, theme, false);
        let settings = bar.settings.as_ref().expect("settings button");
        assert_eq!(bar.spans[0].start, 0);
        assert_eq!(bar.spans[0].end, settings.start);
        assert_eq!(bar.cells[4].cp, u32::from('1'));
        assert_eq!(bar.cells[4].fg, theme.accent.pack());
        assert_eq!(bar.cells[50].bg, theme.bg.pack());
        assert_eq!(hit(&bar, 50), Some(BarHit::Select(0)));
        assert_eq!(hit(&bar, 99), Some(BarHit::Settings));
    }

    #[test]
    fn tabs_shrink_to_share_the_row() {
        let theme = crate::color::theme(0);
        let one = render_bar(&["shell".into()], &[], 0, 40, theme, false);
        let titles: Vec<String> = (0..6).map(|index| format!("tab {index}")).collect();
        let many = render_bar(&titles, &[], 0, 40, theme, false);
        let settings = many.settings.as_ref().expect("settings button");
        assert_eq!(one.spans[0].end, settings.start);
        assert_eq!(many.spans.last().expect("tabs").end, settings.start);
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
            let icon = &bar.cells[usize::from(span.start) + 6];
            assert_eq!(icon.cp, agents[index].icon());
            let shape = TAB | if index == 0 { TAB_ACTIVE } else { 0 };
            assert_eq!(icon.flags & (AGENT_ICON | WIDE | TAB | TAB_ACTIVE), AGENT_ICON | WIDE | shape);
            assert_eq!(bar.cells[usize::from(span.start) + 7].flags, SPACER);
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
        let settings = bar.settings.as_ref().expect("settings button");
        assert_eq!(bar.cells[usize::from(settings.start)].flags & TAB, 0);
    }

    #[test]
    fn agent_status_mark_does_not_widen_the_gap_after_the_icon() {
        let theme = crate::color::theme(0);
        let titles = vec!["✳ Problema".to_string(), "⠐ Problema".to_string(), "Problema".to_string()];
        let agents = [Some(Agent::Claude), Some(Agent::Claude), Some(Agent::Codex)];
        let bar = render_bar(&titles, &agents, 0, 120, theme, false);
        for span in &bar.spans {
            let start = usize::from(span.start);
            assert_eq!(bar.cells[start + 7].flags, SPACER);
            assert_eq!(bar.cells[start + 8].cp, 0);
            assert_eq!(bar.cells[start + 9].cp, u32::from('P'));
        }
        assert_eq!(without_status("~/Git ✳ x"), "~/Git ✳ x");
        assert_eq!(without_status("* "), "* ");
        assert_eq!(without_status("*.rs files"), "*.rs files");
    }

    #[test]
    fn shell_tab_uses_rustty_icon_when_no_agent_is_running() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["shell".into()], &[None], 0, 80, theme, false);
        assert_eq!(bar.cells[6].cp, crate::agent::RUSTTY_ICON);
        assert_eq!(
            bar.cells[6].flags & (AGENT_ICON | WIDE | TAB | TAB_ACTIVE),
            AGENT_ICON | WIDE | TAB | TAB_ACTIVE
        );
        assert_eq!(bar.cells[7].flags, SPACER);
    }
}
