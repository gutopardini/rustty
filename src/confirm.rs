//! Confirmation so several sessions are not closed by accident.

use std::ops::Range;

use crate::cell::{BOLD, Cell, SPACER, TALL};
use crate::color::Theme;

const MAX_COLS: u16 = 44;
const MAX_ROWS: u16 = 10;
pub(crate) const MAX_CELLS: usize = MAX_COLS as usize * MAX_ROWS as usize;
const CANCEL: &str = "Cancel";
const CLOSE_ALL: &str = "Close tabs";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Cancel,
    Close,
}

struct Zone {
    cols: Range<u16>,
    rows: Range<u16>,
    choice: Choice,
}

pub struct Dialog {
    pub origin_col: u16,
    pub origin_row: u16,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    zones: Vec<Zone>,
}

impl Dialog {
    pub fn hit(&self, col: u16, row: u16) -> Option<Choice> {
        let col = col.checked_sub(self.origin_col)?;
        let row = row.checked_sub(self.origin_row)?;
        self.zones
            .iter()
            .find(|zone| zone.cols.contains(&col) && zone.rows.contains(&row))
            .map(|zone| zone.choice)
    }

    fn row(&mut self, row: u16) -> &mut [Cell] {
        let width = usize::from(self.cols);
        let start = usize::from(row) * width;
        &mut self.cells[start..start + width]
    }

    fn button(&mut self, zone: Zone, label: &str, fg: u32, bg: u32) {
        let cols = usize::from(zone.cols.start)..usize::from(zone.cols.end);
        let tall = zone.rows.end - zone.rows.start == 2;
        let center = (cols.len() - label.chars().count()) / 2;
        let flags = if tall { TALL } else { 0 };
        let line = &mut self.row(zone.rows.start)[cols.clone()];
        line.fill(Cell {
            flags,
            ..Cell::blank(fg, bg)
        });
        paint(&mut line[center..], label, fg, BOLD);
        if tall {
            self.row(zone.rows.start + 1)[cols].fill(Cell {
                flags: SPACER,
                ..Cell::blank(fg, bg)
            });
        }
        self.zones.push(zone);
    }
}

pub fn render(grid_cols: u16, grid_rows: u16, theme: Theme, tabs: usize) -> Option<Dialog> {
    let cols = grid_cols.min(MAX_COLS);
    let height = grid_rows.min(MAX_ROWS);
    if cols == 0 || height == 0 {
        return None;
    }

    let gutter = if cols >= 24 { 2 } else { u16::from(cols >= 12) };
    let content = cols - 2 * gutter;
    let wide_actions = content > 2 * (CLOSE_ALL.len() as u16 + 4);
    let compact_pair_fits = content > 2 * (CANCEL.len() as u16 + 2);
    let labels = if wide_actions {
        [CANCEL, if tabs == 1 { "Close tab" } else { CLOSE_ALL }]
    } else if usize::from(content) >= CANCEL.len() && (height >= 3 || compact_pair_fits) {
        [CANCEL, "Close"]
    } else {
        ["Esc", "Enter"]
    };
    let label_width = labels[0].len().max(labels[1].len()) as u16;
    let button_padding = if wide_actions { 2 } else { 1 };
    let keyboard_labels = labels[0] == "Esc";
    let min_button_width = if keyboard_labels {
        label_width
    } else {
        label_width + 2 * button_padding
    };
    let grouped = content > 2 * min_button_width;
    let action_rows = if grouped { 1 } else { 2 };
    let can_draw_actions = height > action_rows && content >= label_width;
    let spare = if can_draw_actions {
        height - 1 - action_rows
    } else {
        0
    };
    let warning = [
        &["Open sessions will be closed."][..],
        &["Open sessions", "will be closed."][..],
        &["Sessions", "will close."][..],
    ]
    .into_iter()
    .find(|lines| {
        lines.len() <= usize::from(spare)
            && lines.iter().all(|line| line.chars().count() <= usize::from(content))
    })
    .unwrap_or(&[]);

    let spare = spare - warning.len() as u16;
    let gap = u16::from(spare >= 1);
    let extra = u16::from(spare >= 2);
    let tall = if grouped { extra } else { 0 };
    let padding = u16::from(spare - gap - extra >= 2);
    let shortcuts = grouped && !keyboard_labels && spare - gap - extra - padding * 2 >= 1;
    let rows = if can_draw_actions {
        1 + warning.len() as u16 + gap + action_rows + extra + padding * 2 + u16::from(shortcuts)
    } else {
        1
    };

    let mut dialog = Dialog {
        origin_col: (grid_cols - cols) / 2,
        origin_row: (grid_rows - rows) / 2,
        cols,
        rows,
        cells: vec![
            Cell::blank(theme.fg.pack(), theme.panel.pack());
            usize::from(cols) * usize::from(rows)
        ],
        zones: Vec::new(),
    };

    let title = match tabs {
        1 => String::from("Close this tab?"),
        _ => format!("Close {tabs} tabs?"),
    };
    let left = usize::from(gutter);
    let right = usize::from(cols - gutter);
    let title = if !can_draw_actions {
        "Close? Esc/Enter"
    } else if title.chars().count() > usize::from(content) {
        "Close?"
    } else {
        &title
    };
    paint(&mut dialog.row(padding)[left..right], title, theme.fg.pack(), BOLD);

    if !can_draw_actions {
        return Some(dialog);
    }

    for (offset, line) in warning.iter().enumerate() {
        paint(
            &mut dialog.row(padding + 1 + offset as u16)[left..right],
            line,
            theme.tab_fg.pack(),
            0,
        );
    }

    let button_width = if grouped {
        (label_width + button_padding * 2).min((content - 1) / 2)
    } else {
        content
    };
    let close_col = cols - gutter - button_width;
    let cancel_col = if grouped {
        close_col - 1 - button_width
    } else {
        close_col
    };
    let cancel_row = padding + 1 + warning.len() as u16 + gap;
    let close_row = if grouped {
        cancel_row
    } else {
        cancel_row + 1 + extra
    };
    let button_rows = 1 + tall;
    dialog.button(
        Zone {
            cols: cancel_col..cancel_col + button_width,
            rows: cancel_row..cancel_row + button_rows,
            choice: Choice::Cancel,
        },
        labels[0],
        theme.fg.pack(),
        theme.bar.pack(),
    );
    dialog.button(
        Zone {
            cols: close_col..close_col + button_width,
            rows: close_row..close_row + button_rows,
            choice: Choice::Close,
        },
        labels[1],
        theme.bg.pack(),
        theme.danger.pack(),
    );

    if shortcuts {
        let row = dialog.row(close_row + button_rows);
        let esc = usize::from(cancel_col + (button_width - 3) / 2);
        let enter = usize::from(close_col + (button_width - 5) / 2);
        paint(&mut row[esc..], "Esc", theme.tab_fg.pack(), 0);
        paint(&mut row[enter..], "Enter", theme.tab_fg.pack(), 0);
    }

    Some(dialog)
}

fn paint(cells: &mut [Cell], text: &str, fg: u32, flags: u32) {
    for (cell, ch) in cells.iter_mut().zip(text.chars()) {
        cell.cp = ch as u32;
        cell.fg = fg;
        cell.flags |= flags;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(dialog: &Dialog, choice: Choice) -> Vec<(u16, u16)> {
        let mut found = Vec::new();
        for row in 0..dialog.rows {
            for col in 0..dialog.cols {
                if dialog.hit(dialog.origin_col + col, dialog.origin_row + row) == Some(choice) {
                    found.push((col, row));
                }
            }
        }
        found
    }

    #[test]
    fn both_button_rows_are_clickable_and_stay_inside_the_dialog() {
        let dialog = render(80, 24, crate::color::theme(0), 3).unwrap();
        let cancel = hits(&dialog, Choice::Cancel);
        let close = hits(&dialog, Choice::Close);
        assert!(!cancel.is_empty());
        assert!(!close.is_empty());
        let cancel_rows: Vec<u16> = cancel.iter().map(|hit| hit.1).collect();
        let close_rows: Vec<u16> = close.iter().map(|hit| hit.1).collect();
        assert_eq!(cancel_rows.iter().min(), close_rows.iter().min());
        assert_eq!(
            cancel_rows.iter().max().unwrap() - cancel_rows.iter().min().unwrap(),
            1
        );
        let gap_col = cancel.iter().map(|hit| hit.0).max().unwrap() + 1;
        let gap_row = *cancel_rows.iter().min().unwrap();
        assert_eq!(
            dialog.hit(dialog.origin_col + gap_col, dialog.origin_row + gap_row),
            None
        );
        assert_eq!(
            dialog.hit(
                dialog.origin_col + dialog.cols - 2,
                dialog.origin_row + gap_row
            ),
            None
        );
        assert_eq!(
            dialog.hit(
                dialog.origin_col + close[0].0,
                dialog.origin_row + close_rows.iter().max().unwrap() + 1
            ),
            None
        );
        assert_eq!(dialog.hit(0, 0), None);
    }

    #[test]
    fn narrow_dialog_keeps_both_actions() {
        let dialog = render(16, 8, crate::color::theme(0), 2).unwrap();
        let cancel = hits(&dialog, Choice::Cancel);
        let close = hits(&dialog, Choice::Close);
        assert!(!cancel.is_empty());
        assert!(!close.is_empty());
        assert!(cancel.iter().all(|hit| hit.1 < close.iter().map(|hit| hit.1).min().unwrap()));
        let between = cancel.iter().map(|hit| hit.1).max().unwrap() + 1;
        assert!(between < close.iter().map(|hit| hit.1).min().unwrap());
        assert_eq!(
            dialog.hit(dialog.origin_col + cancel[0].0, dialog.origin_row + between),
            None
        );
        let compact = render(16, 2, crate::color::theme(0), 2).unwrap();
        assert_eq!(compact.hit(2, 1), Some(Choice::Cancel));
        assert_eq!(compact.hit(13, 1), Some(Choice::Close));
    }
}
