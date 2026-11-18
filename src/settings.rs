//! Appearance panel and the file that stores the choice.

use std::path::PathBuf;

use crate::cell::Cell;
use crate::color::{self, Theme, THEMES};
use crate::font::{self, FontChoice};

#[derive(Clone, Debug)]
pub struct Ui {
    pub open: bool,
    pub size: f32,
    pub style: usize,
    pub fonts: Vec<FontChoice>,
    pub font: usize,
    /// Directory where each new tab starts its shell.
    pub cwd: Option<PathBuf>,
}

impl Ui {
    pub fn load() -> Self {
        let mut ui = Self {
            open: false,
            size: std::env::var("RUSTTY_FONT_SIZE")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(15.0f32)
                .clamp(11.0, 28.0),
            style: 0,
            fonts: font::font_choices(),
            font: 0,
            cwd: None,
        };
        let Some(text) = std::fs::read_to_string(config_path()).ok() else {
            return ui;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            match key.trim() {
                "size" => {
                    if let Ok(size) = value.trim().parse::<f32>() {
                        ui.size = size.clamp(11.0, 28.0);
                    }
                }
                "style" => ui.style = color::theme_index(value.trim()),
                "font" => ui.select_path(PathBuf::from(value.trim())),
                "cwd" => {
                    let path = PathBuf::from(value.trim());
                    if path.is_dir() {
                        ui.cwd = Some(path);
                    }
                }
                _ => {}
            }
        }
        ui
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let font = self.fonts.get(self.font).map(|font| font.path.display().to_string()).unwrap_or_default();
        let cwd = self
            .cwd
            .as_ref()
            .map(|path| format!("cwd={}\n", path.display()))
            .unwrap_or_default();
        let text = format!(
            "size={}\nstyle={}\nfont={}\n{cwd}",
            self.size,
            color::theme(self.style).id,
            font
        );
        if let Err(err) = std::fs::write(&path, text) {
            tracing::warn!(%err, path = %path.display(), "appearance was not saved");
        }
    }

    pub fn font_name(&self) -> &str {
        self.fonts.get(self.font).map(|font| font.name.as_str()).unwrap_or("mono")
    }

    pub fn font_path(&self) -> Option<&std::path::Path> {
        self.fonts.get(self.font).map(|font| font.path.as_path())
    }

    pub fn nudge_size(&mut self, delta: f32) {
        self.size = (self.size + delta).clamp(11.0, 28.0);
    }

    pub fn cycle_font(&mut self, delta: isize) {
        if self.fonts.is_empty() {
            return;
        }
        let len = self.fonts.len() as isize;
        self.font = (self.font as isize + delta).rem_euclid(len) as usize;
    }

    fn select_path(&mut self, path: PathBuf) {
        if let Some(index) = self.fonts.iter().position(|font| font.path == path) {
            self.font = index;
            return;
        }
        if path.is_file() {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("mono")
                .to_string();
            self.fonts.insert(0, FontChoice { name, path });
            self.font = 0;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelHit {
    Close,
    Smaller,
    Larger,
    PrevFont,
    NextFont,
    Style(usize),
}

#[derive(Clone, Copy, Debug)]
struct Zone {
    col: u16,
    end: u16,
    row: u16,
    action: PanelHit,
}

pub struct Panel {
    pub origin_col: u16,
    pub origin_row: u16,
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<Cell>,
    zones: Vec<Zone>,
}

impl Panel {
    pub fn hit(&self, col: u16, row: u16) -> Option<PanelHit> {
        let local_col = col.checked_sub(self.origin_col)?;
        let local_row = row.checked_sub(self.origin_row)?;
        if local_col >= self.cols || local_row >= self.rows {
            return None;
        }
        self.zones
            .iter()
            .find(|zone| zone.row == local_row && (zone.col..zone.end).contains(&local_col))
            .map(|zone| zone.action)
    }

    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.origin_col
            && row >= self.origin_row
            && col < self.origin_col.saturating_add(self.cols)
            && row < self.origin_row.saturating_add(self.rows)
    }
}

pub fn render_panel(grid_cols: u16, grid_rows: u16, theme: Theme, font_name: &str, size: f32, style: usize) -> Option<Panel> {
    let cols = 34u16.min(grid_cols.saturating_sub(1)).max(0);
    let rows = (7 + THEMES.len() as u16).min(grid_rows.saturating_sub(1));
    if cols < 20 || rows < 8 || grid_cols < 21 || grid_rows < 9 {
        return None;
    }
    let origin_col = grid_cols.saturating_sub(cols);
    let origin_row = grid_rows.saturating_sub(rows);
    let mut cells = vec![Cell::blank(theme.fg.pack(), theme.panel.pack()); usize::from(cols) * usize::from(rows)];
    let mut zones = Vec::new();
    let width = usize::from(cols);

    paint(&mut cells, width, 0, 1, "Appearance", theme.fg.pack(), theme.panel.pack(), 0);
    let close_col = cols - 2;
    put(&mut cells, width, close_col, 0, '×', theme.muted.pack(), theme.panel.pack(), 0);
    zones.push(Zone { col: close_col, end: cols, row: 0, action: PanelHit::Close });

    paint(&mut cells, width, 2, 1, "Font", theme.muted.pack(), theme.panel.pack(), 0);
    paint(&mut cells, width, 3, 1, "[<]", theme.accent.pack(), theme.panel.pack(), 0);
    paint(&mut cells, width, 3, cols - 4, "[>]", theme.accent.pack(), theme.panel.pack(), 0);
    let fitted = fit(font_name, usize::from(cols).saturating_sub(10));
    paint(&mut cells, width, 3, 5, &fitted, theme.fg.pack(), theme.panel.pack(), 0);
    zones.push(Zone { col: 0, end: 5, row: 3, action: PanelHit::PrevFont });
    zones.push(Zone { col: cols - 5, end: cols, row: 3, action: PanelHit::NextFont });

    paint(&mut cells, width, 4, 1, "[-]", theme.accent.pack(), theme.panel.pack(), 0);
    paint(&mut cells, width, 4, cols - 4, "[+]", theme.accent.pack(), theme.panel.pack(), 0);
    let size_label = if cols >= 24 { format!("Size: {size:.0} px") } else { format!("{size:.0} px") };
    let size_col = cols.saturating_sub(size_label.chars().count() as u16) / 2;
    paint(&mut cells, width, 4, size_col, &size_label, theme.fg.pack(), theme.panel.pack(), 0);
    zones.push(Zone { col: 0, end: 5, row: 4, action: PanelHit::Smaller });
    zones.push(Zone { col: cols - 5, end: cols, row: 4, action: PanelHit::Larger });

    paint(&mut cells, width, 6, 1, "Style", theme.muted.pack(), theme.panel.pack(), 0);
    for (index, item) in THEMES.iter().enumerate() {
        let row = 7 + index as u16;
        if row >= rows {
            break;
        }
        let selected = index == style;
        let mark = if selected { "●" } else { "○" };
        let fg = if selected { theme.accent.pack() } else { theme.fg.pack() };
        paint(
            &mut cells,
            width,
            row,
            1,
            &format!("{mark} {}", item.name),
            fg,
            theme.panel.pack(),
            0,
        );
        zones.push(Zone { col: 0, end: cols, row, action: PanelHit::Style(index) });
    }

    Some(Panel { origin_col, origin_row, cols, rows, cells, zones })
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("rustty").join("ui.conf")
}

fn fit(text: &str, width: usize) -> String {
    let mut out = String::new();
    for ch in text.chars().take(width) {
        out.push(ch);
    }
    out
}

fn paint(cells: &mut [Cell], width: usize, row: u16, col: u16, text: &str, fg: u32, bg: u32, flags: u32) {
    let mut cursor = usize::from(col);
    for ch in text.chars() {
        if cursor >= width {
            break;
        }
        put_at(cells, width, cursor, row, ch, fg, bg, flags);
        cursor += 1;
    }
}

fn put(cells: &mut [Cell], width: usize, col: u16, row: u16, ch: char, fg: u32, bg: u32, flags: u32) {
    put_at(cells, width, usize::from(col), row, ch, fg, bg, flags);
}

fn put_at(cells: &mut [Cell], width: usize, col: usize, row: u16, ch: char, fg: u32, bg: u32, flags: u32) {
    if col >= width {
        return;
    }
    let index = usize::from(row) * width + col;
    if let Some(cell) = cells.get_mut(index) {
        *cell = Cell { cp: ch as u32, fg, bg, flags };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_row_and_size_buttons_are_hits() {
        let theme = color::theme(0);
        let panel = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0).expect("panel");
        assert_eq!(panel.hit(panel.origin_col + 1, panel.origin_row + 4), Some(PanelHit::Smaller));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 2, panel.origin_row + 4), Some(PanelHit::Larger));
        assert_eq!(panel.hit(panel.origin_col + 4, panel.origin_row + 4), Some(PanelHit::Smaller));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 5, panel.origin_row + 4), Some(PanelHit::Larger));
        let size_row = &panel.cells[usize::from(panel.cols) * 4..usize::from(panel.cols) * 5];
        let size_text: String = size_row.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(size_text.contains("Size: 15 px"), "{size_text}");
        assert_eq!(panel.hit(panel.origin_col + 2, panel.origin_row + 7), Some(PanelHit::Style(0)));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 1, panel.origin_row), Some(PanelHit::Close));
        assert!(panel.contains(panel.origin_col, panel.origin_row));
        assert!(!panel.contains(0, panel.origin_row) || panel.origin_col == 0);
    }
}
