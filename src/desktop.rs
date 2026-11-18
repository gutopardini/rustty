//! Title bar preferences of the desktop, read from GNOME's settings.
//!
//! The window draws its own title bar, so it follows the same keys GNOME uses for
//! the bars it draws: which buttons appear and on which side, and what a double,
//! middle or right click on the bar does.

use std::process::Command;
use std::time::Duration;

const WM_SCHEMA: &str = "org.gnome.desktop.wm.preferences";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Minimize,
    Maximize,
    Close,
}

/// Buttons at each end of the title bar, from left to right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonLayout {
    pub left: Vec<Button>,
    pub right: Vec<Button>,
}

impl Default for ButtonLayout {
    fn default() -> Self {
        Self::parse(":minimize,maximize,close")
    }
}

impl ButtonLayout {
    /// Reads GNOME's `button-layout`, such as `appmenu:close`. Names this bar does not draw,
    /// such as `appmenu`, `icon` or `spacer`, are skipped.
    pub fn parse(value: &str) -> Self {
        let (left, right) = value.split_once(':').unwrap_or(("", value));
        let side = |names: &str| {
            names
                .split(',')
                .filter_map(|name| match name.trim() {
                    "minimize" => Some(Button::Minimize),
                    "maximize" => Some(Button::Maximize),
                    "close" => Some(Button::Close),
                    _ => None,
                })
                .collect()
        };
        Self { left: side(left), right: side(right) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleAction {
    ToggleMaximize,
    Minimize,
    /// The window menu of the compositor.
    Menu,
    None,
}

impl TitleAction {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            // Half maximizing is not available to a client, so both axes go together.
            "toggle-maximize" | "toggle-maximize-horizontally" | "toggle-maximize-vertically" => {
                Self::ToggleMaximize
            }
            "minimize" => Self::Minimize,
            "menu" => Self::Menu,
            // A client cannot lower itself under other windows.
            "none" | "lower" => Self::None,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitlebarPrefs {
    pub buttons: ButtonLayout,
    pub double_click: TitleAction,
    pub middle_click: TitleAction,
    pub right_click: TitleAction,
    pub double_click_time: Duration,
}

impl Default for TitlebarPrefs {
    fn default() -> Self {
        Self {
            buttons: ButtonLayout::default(),
            double_click: TitleAction::ToggleMaximize,
            middle_click: TitleAction::None,
            right_click: TitleAction::Menu,
            double_click_time: Duration::from_millis(400),
        }
    }
}

impl TitlebarPrefs {
    /// GNOME's values, or the defaults when `gsettings` is not there.
    pub fn load() -> Self {
        let mut prefs = Self::default();
        if let Some(listing) = gsettings(&["list-recursively", WM_SCHEMA]) {
            prefs.apply(&listing);
        }
        if let Some(ms) = gsettings(&["get", "org.gnome.desktop.peripherals.mouse", "double-click"])
            .and_then(|value| value.split_whitespace().last()?.parse::<u64>().ok())
            .filter(|ms| (100..=2000).contains(ms))
        {
            prefs.double_click_time = Duration::from_millis(ms);
        }
        prefs
    }

    /// Takes the lines of `gsettings list-recursively`: schema, key, then the value.
    fn apply(&mut self, listing: &str) {
        for line in listing.lines() {
            let mut parts = line.splitn(3, ' ');
            let (Some(_schema), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let value = value.trim().trim_matches('\'');
            match key {
                "button-layout" => self.buttons = ButtonLayout::parse(value),
                "action-double-click-titlebar" => {
                    self.double_click = TitleAction::parse(value).unwrap_or(self.double_click);
                }
                "action-middle-click-titlebar" => {
                    self.middle_click = TitleAction::parse(value).unwrap_or(self.middle_click);
                }
                "action-right-click-titlebar" => {
                    self.right_click = TitleAction::parse(value).unwrap_or(self.right_click);
                }
                _ => {}
            }
        }
    }
}

fn gsettings(args: &[&str]) -> Option<String> {
    let output = Command::new("gsettings").args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gnome_layout_and_click_actions() {
        let mut prefs = TitlebarPrefs::default();
        prefs.apply(
            "org.gnome.desktop.wm.preferences button-layout 'appmenu:close'\n\
             org.gnome.desktop.wm.preferences action-double-click-titlebar 'minimize'\n\
             org.gnome.desktop.wm.preferences action-middle-click-titlebar 'lower'\n\
             org.gnome.desktop.wm.preferences action-right-click-titlebar 'menu'\n\
             org.gnome.desktop.wm.preferences titlebar-font 'Cantarell Bold 11'\n",
        );
        assert_eq!(prefs.buttons, ButtonLayout { left: vec![], right: vec![Button::Close] });
        assert_eq!(prefs.double_click, TitleAction::Minimize);
        assert_eq!(prefs.middle_click, TitleAction::None);
        assert_eq!(prefs.right_click, TitleAction::Menu);
    }

    #[test]
    fn layout_keeps_each_side_in_order() {
        let layout = ButtonLayout::parse("close,minimize:spacer,maximize");
        assert_eq!(layout.left, [Button::Close, Button::Minimize]);
        assert_eq!(layout.right, [Button::Maximize]);
        assert_eq!(ButtonLayout::parse("close").right, [Button::Close]);
    }
}
