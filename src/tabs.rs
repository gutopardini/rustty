//! Tab strip drawn as one row of cells at the bottom of the window.

use std::borrow::Cow;

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
    render_bar_in(titles, agents, active, cols, theme, glass, None)
}

/// `track` is the column range under the glass capsule. Labels sit in that range,
/// split the same way the pill splits the capsule.
pub(crate) fn render_bar_in(
    titles: &[String],
    agents: &[Option<Agent>],
    active: usize,
    cols: u16,
    theme: Theme,
    glass: bool,
    track: Option<(u16, u16)>,
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
        // On glass the number, icon and title sit in the middle of the segment, as the
        // labels of a segmented control. They move left only when the close mark needs room.
        let lead = if glass {
            let number = (index + 1).to_string().len();
            let fixed = number + 1 + 2 + icon_gap;
            let (_, title_width) = fit_title(title, label_end.saturating_sub(start + lead + fixed));
            let content = fixed + title_width;
            let centered = span.saturating_sub(content) / 2;
            centered.min(label_end.saturating_sub(start + content)).max(lead)
        } else {
            lead
        };
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
        if !glass {
            mark_tab(&mut cells[start..end], active_tab);
        }
        spans.push(TabSpan {
            start: start as u16,
            end: end as u16,
            close,
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
        assert_eq!(bar.cells[4].cp, u32::from('1'));
        assert_eq!(bar.cells[4].fg, theme.accent.pack());
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
    fn glass_labels_sit_in_the_middle_of_their_segment() {
        let theme = crate::color::theme(0);
        let bar = render_bar(&["alpha".into(), "beta".into()], &[None, None], 0, 100, theme, true);
        for span in &bar.spans {
            let (start, end) = (usize::from(span.start), usize::from(span.end));
            let painted: Vec<usize> = (start..end)
                .filter(|&col| ![0, u32::from(' '), u32::from('×')].contains(&bar.cells[col].cp))
                .collect();
            let left = painted[0] - start;
            let right = end - 1 - painted[painted.len() - 1];
            assert!(left.abs_diff(right) <= 2, "{left} {right}");
        }
    }

    #[test]
    fn glass_labels_follow_the_capsule_columns() {
        let theme = crate::color::theme(0);
        let titles = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        let bar = render_bar_in(&titles, &[], 0, 100, theme, true, Some((2, 90)));
        assert_eq!(bar.spans[0].start, 2);
        assert_eq!(bar.spans[2].end, 90);
        assert_eq!(bar.spans[0].end, bar.spans[1].start);
        assert_eq!(bar.spans[1].end, bar.spans[2].start);
    }
}
