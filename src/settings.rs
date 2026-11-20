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
    /// Translucent window with the desktop blurred behind it, and glass controls in the bars.
    pub glass: bool,
    /// Frost, shadow, pill, sheets and spacing of glass mode. Each field is a key in the file.
    pub glass_tune: GlassTune,
}

/// Window tint used when `ui.conf` does not set `glass_opacity`.
pub const DEFAULT_GLASS_OPACITY: f32 = GlassTune::DEFAULT.opacity;

impl Ui {
    pub fn load() -> Self {
        let text = std::fs::read_to_string(config_path()).unwrap_or_default();
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
            glass: false,
            glass_tune: GlassTune::default(),
        };
        ui.apply_text(&text);
        // The calibration keys are written once, so the file shows what can be tuned.
        if !text.contains("glass_body_top=") {
            ui.save();
        }
        ui
    }

    fn apply_text(&mut self, text: &str) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || key.starts_with('#') {
                continue;
            }
            match key {
                "size" => {
                    if let Ok(size) = value.parse::<f32>() {
                        self.size = size.clamp(11.0, 28.0);
                    }
                }
                "style" => self.style = color::theme_index(value),
                "font" => self.select_path(PathBuf::from(value)),
                "glass" => self.glass = matches!(value, "on" | "true" | "1" | "yes"),
                "cwd" => {
                    let path = PathBuf::from(value);
                    if path.is_dir() {
                        self.cwd = Some(path);
                    }
                }
                _ => self.glass_tune.apply(key, value),
            }
        }
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
        let glass = if self.glass { "on" } else { "off" };
        let text = format!(
            "size={}\nstyle={}\nfont={}\nglass={glass}\n{}{cwd}",
            self.size,
            color::theme(self.style).id,
            font,
            self.glass_tune.text()
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

/// One glass amount the appearance panel can nudge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlassControl {
    Opacity,
    BodyTop,
    BodyBottom,
    Lit,
    Shadow,
    ShadowBlur,
    ShadowY,
    PillTop,
    PillBottom,
    PillRim,
    PillRimSize,
    PillSoft,
    Groove,
    GrooveStrength,
    Sheet,
    Dialog,
    Danger,
    SheetShadow,
    SheetSpread,
    SheetY,
    FrostTop,
    FrostBottom,
    Ink,
    InkAlpha,
    InkLit,
    Bar,
    BarPad,
    Inset,
    Gap,
    Margin,
    Radius,
    DialogPad,
    ButtonInset,
}

impl GlassControl {
    pub const ALL: [GlassControl; 33] = [
        GlassControl::Opacity,
        GlassControl::BodyTop,
        GlassControl::BodyBottom,
        GlassControl::Lit,
        GlassControl::Shadow,
        GlassControl::ShadowBlur,
        GlassControl::ShadowY,
        GlassControl::PillTop,
        GlassControl::PillBottom,
        GlassControl::PillRim,
        GlassControl::PillRimSize,
        GlassControl::PillSoft,
        GlassControl::Groove,
        GlassControl::GrooveStrength,
        GlassControl::Sheet,
        GlassControl::Dialog,
        GlassControl::Danger,
        GlassControl::SheetShadow,
        GlassControl::SheetSpread,
        GlassControl::SheetY,
        GlassControl::FrostTop,
        GlassControl::FrostBottom,
        GlassControl::Ink,
        GlassControl::InkAlpha,
        GlassControl::InkLit,
        GlassControl::Bar,
        GlassControl::BarPad,
        GlassControl::Inset,
        GlassControl::Gap,
        GlassControl::Margin,
        GlassControl::Radius,
        GlassControl::DialogPad,
        GlassControl::ButtonInset,
    ];

    fn label(self) -> &'static str {
        match self {
            GlassControl::Opacity => "Tint",
            GlassControl::BodyTop => "Frost top",
            GlassControl::BodyBottom => "Frost bottom",
            GlassControl::Lit => "Hover",
            GlassControl::Shadow => "Shadow",
            GlassControl::ShadowBlur => "Shadow blur",
            GlassControl::ShadowY => "Shadow shift",
            GlassControl::PillTop => "Tab top",
            GlassControl::PillBottom => "Tab bottom",
            GlassControl::PillRim => "Tab rim",
            GlassControl::PillRimSize => "Rim size",
            GlassControl::PillSoft => "Tab edge",
            GlassControl::Groove => "Groove",
            GlassControl::GrooveStrength => "Groove depth",
            GlassControl::Sheet => "Panel",
            GlassControl::Dialog => "Dialog",
            GlassControl::Danger => "Close",
            GlassControl::SheetShadow => "Sheet shadow",
            GlassControl::SheetSpread => "Sheet spread",
            GlassControl::SheetY => "Sheet shift",
            GlassControl::FrostTop => "Sheet frost",
            GlassControl::FrostBottom => "Sheet fade",
            GlassControl::Ink => "Icon",
            GlassControl::InkAlpha => "Icon fade",
            GlassControl::InkLit => "Icon hover",
            GlassControl::Bar => "Bar height",
            GlassControl::BarPad => "Bar pad",
            GlassControl::Inset => "Inset",
            GlassControl::Gap => "Gap",
            GlassControl::Margin => "Margin",
            GlassControl::Radius => "Corner",
            GlassControl::DialogPad => "Dialog pad",
            GlassControl::ButtonInset => "Button",
        }
    }

    fn step(self) -> f32 {
        match self.kind() {
            Knob::Unit => 0.01,
            Knob::Soft => 0.1,
            _ => 1.0,
        }
    }

    fn clamp(self, value: f32) -> f32 {
        match self.kind() {
            Knob::Unit => unit(value),
            Knob::Pixel => px(value),
            Knob::Signed => signed(value),
            Knob::Soft => soft(value),
            Knob::Bar => bar_height(value),
        }
    }

    fn kind(self) -> Knob {
        match self {
            GlassControl::Opacity
            | GlassControl::BodyTop
            | GlassControl::BodyBottom
            | GlassControl::Lit
            | GlassControl::Shadow
            | GlassControl::PillTop
            | GlassControl::PillBottom
            | GlassControl::PillRim
            | GlassControl::GrooveStrength
            | GlassControl::Sheet
            | GlassControl::Dialog
            | GlassControl::Danger
            | GlassControl::SheetShadow
            | GlassControl::FrostTop
            | GlassControl::FrostBottom
            | GlassControl::Ink
            | GlassControl::InkAlpha
            | GlassControl::InkLit => Knob::Unit,
            GlassControl::ShadowY | GlassControl::SheetY => Knob::Signed,
            GlassControl::PillSoft => Knob::Soft,
            GlassControl::Bar => Knob::Bar,
            _ => Knob::Pixel,
        }
    }

    fn is_tone(self) -> bool {
        !matches!(
            self,
            GlassControl::Opacity
                | GlassControl::ShadowBlur
                | GlassControl::ShadowY
                | GlassControl::PillRimSize
                | GlassControl::PillSoft
                | GlassControl::Groove
                | GlassControl::Sheet
                | GlassControl::Dialog
                | GlassControl::Danger
                | GlassControl::SheetSpread
                | GlassControl::SheetY
                | GlassControl::InkLit
                | GlassControl::Bar
                | GlassControl::BarPad
                | GlassControl::Inset
                | GlassControl::Gap
                | GlassControl::Margin
                | GlassControl::Radius
                | GlassControl::DialogPad
                | GlassControl::ButtonInset
        )
    }
}

enum Knob {
    Unit,
    Pixel,
    Signed,
    Soft,
    Bar,
}

/// One amount for a dark theme and the same amount for a light theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    pub dark: f32,
    pub light: f32,
}

impl Tone {
    const fn new(dark: f32, light: f32) -> Self {
        Self { dark, light }
    }

    pub fn pick(self, light: bool) -> f32 {
        if light { self.light } else { self.dark }
    }
}

/// The glass look. Opacities are 0 to 1. Distances are logical pixels.
///
/// A `Tone` is stored as `dark,light` in `ui.conf`. One number sets both themes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassTune {
    /// Tint of the window behind the text.
    pub opacity: f32,
    /// Frost of the capsule and the round buttons, at the top edge and the bottom edge.
    pub body_top: Tone,
    pub body_bottom: Tone,
    /// Extra frost on a hovered or open control.
    pub lit: Tone,
    /// Shadow under the capsule and the buttons: strength, blur and downward offset.
    pub shadow: Tone,
    pub shadow_blur: f32,
    pub shadow_y: f32,
    /// The selected tab. `pill_rim` is the dark band along its top, `pill_soft` its edge.
    pub pill_top: Tone,
    pub pill_bottom: Tone,
    pub pill_rim: Tone,
    pub pill_rim_size: f32,
    pub pill_soft: f32,
    /// Groove under the top edge of the capsule.
    pub groove: f32,
    pub groove_strength: Tone,
    /// Settings sheet, close dialog, and the red close button.
    pub sheet: f32,
    pub dialog: f32,
    pub danger: f32,
    pub sheet_shadow: Tone,
    pub sheet_spread: f32,
    pub sheet_y: f32,
    /// White frost over a sheet, at its top and its bottom.
    pub frost_top: Tone,
    pub frost_bottom: Tone,
    /// Icons on the glass buttons.
    pub ink: Tone,
    pub ink_alpha: Tone,
    pub ink_lit: f32,
    /// Bar height, and how much it grows past the text.
    pub bar: f32,
    pub bar_pad: f32,
    /// Gap inside the bar, gap inside the pill, room for the shadow, sheet corner, dialog pad,
    /// and how far the round buttons sit inside the bar.
    pub inset: f32,
    pub gap: f32,
    pub margin: f32,
    pub radius: f32,
    pub dialog_pad: f32,
    pub button_inset: f32,
}

impl Default for GlassTune {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl GlassTune {
    pub const DEFAULT: Self = Self {
        opacity: 0.78,
        body_top: Tone::new(0.035, 0.22),
        body_bottom: Tone::new(0.11, 0.40),
        lit: Tone::new(0.06, 0.10),
        shadow: Tone::new(0.28, 0.12),
        shadow_blur: 7.0,
        shadow_y: 3.0,
        pill_top: Tone::new(0.42, 0.62),
        pill_bottom: Tone::new(0.26, 0.78),
        pill_rim: Tone::new(0.30, 0.10),
        pill_rim_size: 6.0,
        pill_soft: 1.2,
        groove: 9.0,
        groove_strength: Tone::new(0.26, 0.08),
        sheet: 0.86,
        dialog: 0.94,
        danger: 0.9,
        sheet_shadow: Tone::new(0.40, 0.18),
        sheet_spread: 16.0,
        sheet_y: 6.0,
        frost_top: Tone::new(0.06, 0.10),
        frost_bottom: Tone::new(0.015, 0.03),
        ink: Tone::new(0.86, 0.22),
        ink_alpha: Tone::new(0.90, 0.85),
        ink_lit: 0.14,
        bar: 44.0,
        bar_pad: 20.0,
        inset: 4.0,
        gap: 4.0,
        margin: 10.0,
        radius: 14.0,
        dialog_pad: 8.0,
        button_inset: 2.0,
    };

    /// Reads the glass keys and leaves the rest of the file alone.
    pub fn from_text(text: &str) -> Self {
        let mut tune = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
            let key = key.trim();
            if key.is_empty() || key.starts_with('#') {
                continue;
            }
            tune.apply(key, value.trim());
        }
        tune
    }

    /// The glass block of `ui.conf`, comments included, ending in a newline.
    pub fn text(self) -> String {
        format!(
            "\
# Glass mode. A pair is the dark theme, then the light theme.
# Opacities run from 0 to 1. Distances are logical pixels and follow the display scale.
# glass_margin is the room left around a shape for its shadow. Raise it with the blur.
glass_opacity={opacity}
# Capsule and round buttons, frosted from the top edge toward the bottom.
glass_body_top={body_top}
glass_body_bottom={body_bottom}
glass_lit={lit}
# Shadow under the capsule and the buttons.
glass_shadow={shadow}
glass_shadow_blur={shadow_blur}
glass_shadow_y={shadow_y}
# The selected tab, sunk into the capsule.
glass_pill_top={pill_top}
glass_pill_bottom={pill_bottom}
glass_pill_rim={pill_rim}
glass_pill_rim_size={pill_rim_size}
glass_pill_soft={pill_soft}
# Groove under the top edge of the capsule.
glass_groove={groove}
glass_groove_strength={groove_strength}
# Settings sheet, the close dialog, and the red close button.
glass_sheet={sheet}
glass_dialog={dialog}
glass_danger={danger}
glass_sheet_shadow={sheet_shadow}
glass_sheet_spread={sheet_spread}
glass_sheet_y={sheet_y}
glass_frost_top={frost_top}
glass_frost_bottom={frost_bottom}
# Icons drawn on the glass buttons.
glass_ink={ink}
glass_ink_alpha={ink_alpha}
glass_ink_lit={ink_lit}
# Bar height, and the gaps around the capsule, the pill and the sheets.
glass_bar={bar}
glass_bar_pad={bar_pad}
glass_inset={inset}
glass_gap={gap}
glass_margin={margin}
glass_radius={radius}
glass_dialog_pad={dialog_pad}
glass_button_inset={button_inset}
",
            opacity = num(self.opacity),
            body_top = pair(self.body_top),
            body_bottom = pair(self.body_bottom),
            lit = pair(self.lit),
            shadow = pair(self.shadow),
            shadow_blur = num(self.shadow_blur),
            shadow_y = num(self.shadow_y),
            pill_top = pair(self.pill_top),
            pill_bottom = pair(self.pill_bottom),
            pill_rim = pair(self.pill_rim),
            pill_rim_size = num(self.pill_rim_size),
            pill_soft = num(self.pill_soft),
            groove = num(self.groove),
            groove_strength = pair(self.groove_strength),
            sheet = num(self.sheet),
            dialog = num(self.dialog),
            danger = num(self.danger),
            sheet_shadow = pair(self.sheet_shadow),
            sheet_spread = num(self.sheet_spread),
            sheet_y = num(self.sheet_y),
            frost_top = pair(self.frost_top),
            frost_bottom = pair(self.frost_bottom),
            ink = pair(self.ink),
            ink_alpha = pair(self.ink_alpha),
            ink_lit = num(self.ink_lit),
            bar = num(self.bar),
            bar_pad = num(self.bar_pad),
            inset = num(self.inset),
            gap = num(self.gap),
            margin = num(self.margin),
            radius = num(self.radius),
            dialog_pad = num(self.dialog_pad),
            button_inset = num(self.button_inset),
        )
    }

    fn apply(&mut self, key: &str, value: &str) {
        match key {
            "glass_opacity" => self.opacity = unit_of(value).unwrap_or(self.opacity),
            "glass_body_top" => self.body_top = tone(value).unwrap_or(self.body_top),
            "glass_body_bottom" => self.body_bottom = tone(value).unwrap_or(self.body_bottom),
            "glass_lit" => self.lit = tone(value).unwrap_or(self.lit),
            "glass_shadow" => self.shadow = tone(value).unwrap_or(self.shadow),
            "glass_shadow_blur" => self.shadow_blur = px_of(value).unwrap_or(self.shadow_blur),
            "glass_shadow_y" => self.shadow_y = signed_of(value).unwrap_or(self.shadow_y),
            "glass_pill_top" => self.pill_top = tone(value).unwrap_or(self.pill_top),
            "glass_pill_bottom" => self.pill_bottom = tone(value).unwrap_or(self.pill_bottom),
            "glass_pill_rim" => self.pill_rim = tone(value).unwrap_or(self.pill_rim),
            "glass_pill_rim_size" => self.pill_rim_size = px_of(value).unwrap_or(self.pill_rim_size),
            "glass_pill_soft" => self.pill_soft = soft_of(value).unwrap_or(self.pill_soft),
            "glass_groove" => self.groove = px_of(value).unwrap_or(self.groove),
            "glass_groove_strength" => self.groove_strength = tone(value).unwrap_or(self.groove_strength),
            "glass_sheet" => self.sheet = unit_of(value).unwrap_or(self.sheet),
            "glass_dialog" => self.dialog = unit_of(value).unwrap_or(self.dialog),
            "glass_danger" => self.danger = unit_of(value).unwrap_or(self.danger),
            "glass_sheet_shadow" => self.sheet_shadow = tone(value).unwrap_or(self.sheet_shadow),
            "glass_sheet_spread" => self.sheet_spread = px_of(value).unwrap_or(self.sheet_spread),
            "glass_sheet_y" => self.sheet_y = signed_of(value).unwrap_or(self.sheet_y),
            "glass_frost_top" => self.frost_top = tone(value).unwrap_or(self.frost_top),
            "glass_frost_bottom" => self.frost_bottom = tone(value).unwrap_or(self.frost_bottom),
            "glass_ink" => self.ink = tone(value).unwrap_or(self.ink),
            "glass_ink_alpha" => self.ink_alpha = tone(value).unwrap_or(self.ink_alpha),
            "glass_ink_lit" => self.ink_lit = unit_of(value).unwrap_or(self.ink_lit),
            "glass_bar" => self.bar = bar_of(value).unwrap_or(self.bar),
            "glass_bar_pad" => self.bar_pad = px_of(value).unwrap_or(self.bar_pad),
            "glass_inset" => self.inset = px_of(value).unwrap_or(self.inset),
            "glass_gap" => self.gap = px_of(value).unwrap_or(self.gap),
            "glass_margin" => self.margin = px_of(value).unwrap_or(self.margin),
            "glass_radius" => self.radius = px_of(value).unwrap_or(self.radius),
            "glass_dialog_pad" => self.dialog_pad = px_of(value).unwrap_or(self.dialog_pad),
            "glass_button_inset" => self.button_inset = px_of(value).unwrap_or(self.button_inset),
            _ => {}
        }
    }

    /// The amount on screen for the current style. A pair keeps the other style untouched.
    pub fn get(self, control: GlassControl, light: bool) -> f32 {
        let tone = |tone: Tone| tone.pick(light);
        match control {
            GlassControl::Opacity => self.opacity,
            GlassControl::BodyTop => tone(self.body_top),
            GlassControl::BodyBottom => tone(self.body_bottom),
            GlassControl::Lit => tone(self.lit),
            GlassControl::Shadow => tone(self.shadow),
            GlassControl::ShadowBlur => self.shadow_blur,
            GlassControl::ShadowY => self.shadow_y,
            GlassControl::PillTop => tone(self.pill_top),
            GlassControl::PillBottom => tone(self.pill_bottom),
            GlassControl::PillRim => tone(self.pill_rim),
            GlassControl::PillRimSize => self.pill_rim_size,
            GlassControl::PillSoft => self.pill_soft,
            GlassControl::Groove => self.groove,
            GlassControl::GrooveStrength => tone(self.groove_strength),
            GlassControl::Sheet => self.sheet,
            GlassControl::Dialog => self.dialog,
            GlassControl::Danger => self.danger,
            GlassControl::SheetShadow => tone(self.sheet_shadow),
            GlassControl::SheetSpread => self.sheet_spread,
            GlassControl::SheetY => self.sheet_y,
            GlassControl::FrostTop => tone(self.frost_top),
            GlassControl::FrostBottom => tone(self.frost_bottom),
            GlassControl::Ink => tone(self.ink),
            GlassControl::InkAlpha => tone(self.ink_alpha),
            GlassControl::InkLit => self.ink_lit,
            GlassControl::Bar => self.bar,
            GlassControl::BarPad => self.bar_pad,
            GlassControl::Inset => self.inset,
            GlassControl::Gap => self.gap,
            GlassControl::Margin => self.margin,
            GlassControl::Radius => self.radius,
            GlassControl::DialogPad => self.dialog_pad,
            GlassControl::ButtonInset => self.button_inset,
        }
    }

    pub fn nudge(&mut self, control: GlassControl, dir: i32, light: bool) {
        if dir == 0 {
            return;
        }
        let dir = if dir > 0 { 1.0 } else { -1.0 };
        let value = control.clamp(self.get(control, light) + dir * control.step());
        if control.is_tone() {
            let mut tone = match control {
                GlassControl::BodyTop => self.body_top,
                GlassControl::BodyBottom => self.body_bottom,
                GlassControl::Lit => self.lit,
                GlassControl::Shadow => self.shadow,
                GlassControl::PillTop => self.pill_top,
                GlassControl::PillBottom => self.pill_bottom,
                GlassControl::PillRim => self.pill_rim,
                GlassControl::GrooveStrength => self.groove_strength,
                GlassControl::SheetShadow => self.sheet_shadow,
                GlassControl::FrostTop => self.frost_top,
                GlassControl::FrostBottom => self.frost_bottom,
                GlassControl::Ink => self.ink,
                GlassControl::InkAlpha => self.ink_alpha,
                _ => return,
            };
            if light { tone.light = value } else { tone.dark = value }
            match control {
                GlassControl::BodyTop => self.body_top = tone,
                GlassControl::BodyBottom => self.body_bottom = tone,
                GlassControl::Lit => self.lit = tone,
                GlassControl::Shadow => self.shadow = tone,
                GlassControl::PillTop => self.pill_top = tone,
                GlassControl::PillBottom => self.pill_bottom = tone,
                GlassControl::PillRim => self.pill_rim = tone,
                GlassControl::GrooveStrength => self.groove_strength = tone,
                GlassControl::SheetShadow => self.sheet_shadow = tone,
                GlassControl::FrostTop => self.frost_top = tone,
                GlassControl::FrostBottom => self.frost_bottom = tone,
                GlassControl::Ink => self.ink = tone,
                GlassControl::InkAlpha => self.ink_alpha = tone,
                _ => {}
            }
            return;
        }
        match control {
            GlassControl::Opacity => self.opacity = value,
            GlassControl::ShadowBlur => self.shadow_blur = value,
            GlassControl::ShadowY => self.shadow_y = value,
            GlassControl::PillRimSize => self.pill_rim_size = value,
            GlassControl::PillSoft => self.pill_soft = value,
            GlassControl::Groove => self.groove = value,
            GlassControl::Sheet => self.sheet = value,
            GlassControl::Dialog => self.dialog = value,
            GlassControl::Danger => self.danger = value,
            GlassControl::SheetSpread => self.sheet_spread = value,
            GlassControl::SheetY => self.sheet_y = value,
            GlassControl::InkLit => self.ink_lit = value,
            GlassControl::Bar => self.bar = value,
            GlassControl::BarPad => self.bar_pad = value,
            GlassControl::Inset => self.inset = value,
            GlassControl::Gap => self.gap = value,
            GlassControl::Margin => self.margin = value,
            GlassControl::Radius => self.radius = value,
            GlassControl::DialogPad => self.dialog_pad = value,
            GlassControl::ButtonInset => self.button_inset = value,
            _ => {}
        }
    }
}

fn tone(value: &str) -> Option<Tone> {
    let mut parts = value.split([',', ' ']).filter(|part| !part.is_empty());
    let dark = parts.next()?.parse().ok()?;
    let light = match parts.next() {
        Some(part) => part.parse().ok()?,
        None => dark,
    };
    Some(Tone { dark: unit(dark), light: unit(light) })
}

fn unit(value: f32) -> f32 {
    value.clamp(0.0, 1.0)
}

fn unit_of(value: &str) -> Option<f32> {
    value.parse().ok().map(unit)
}

fn px(value: f32) -> f32 {
    value.clamp(0.0, 96.0)
}

fn px_of(value: &str) -> Option<f32> {
    value.parse().ok().map(px)
}

fn signed(value: f32) -> f32 {
    value.clamp(-96.0, 96.0)
}

fn signed_of(value: &str) -> Option<f32> {
    value.parse().ok().map(signed)
}

fn bar_height(value: f32) -> f32 {
    value.clamp(16.0, 160.0)
}

fn bar_of(value: &str) -> Option<f32> {
    value.parse().ok().map(bar_height)
}

fn soft(value: f32) -> f32 {
    value.clamp(0.05, 32.0)
}

fn soft_of(value: &str) -> Option<f32> {
    value.parse().ok().map(soft)
}

fn num(value: f32) -> String {
    let mut text = format!("{value:.4}");
    if text.contains('.') {
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
    }
    if text.is_empty() || text == "-" { "0".to_string() } else { text }
}

fn pair(tone: Tone) -> String {
    format!("{},{}", num(tone.dark), num(tone.light))
}

/// Modification time of `ui.conf`, so a running window can pick up a calibration.
pub fn config_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path()).ok().and_then(|meta| meta.modified().ok())
}

/// Glass keys currently in `ui.conf`.
pub fn read_glass_tune() -> Option<GlassTune> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    Some(GlassTune::from_text(&text))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelHit {
    Close,
    Smaller,
    Larger,
    PrevFont,
    NextFont,
    Glass,
    Style(usize),
    /// Opens the glass controls.
    GlassLook,
    /// Returns from the glass controls to appearance.
    GlassBack,
    /// Puts every glass value back to its original amount.
    GlassReset,
    /// `dir` is -1 or +1.
    GlassNudge(GlassControl, i8),
    /// Moves the glass list by this many rows.
    GlassScroll(i16),
    /// Opens the shortcut map.
    Keys,
    /// Returns from the shortcut map.
    KeysBack,
    /// Moves the shortcut map by this many rows.
    KeysScroll(i16),
}

/// Shortcuts Rustty handles itself. Anything else goes to the shell.
const KEYS: &[(&str, &str)] = &[
    ("Ctrl+Shift+T", "new tab"),
    ("Ctrl+Shift+W", "close tab"),
    ("Ctrl+Shift+Q", "close tab"),
    ("Ctrl+Tab", "next tab"),
    ("Ctrl+Shift+Tab", "previous tab"),
    ("Ctrl+Shift+←", "previous tab"),
    ("Ctrl+Shift+→", "next tab"),
    ("Ctrl+PageUp", "previous tab"),
    ("Ctrl+PageDown", "next tab"),
    ("Ctrl+Shift+1–9", "jump to a tab"),
    ("Shift+PageUp", "scroll up"),
    ("Shift+PageDown", "scroll down"),
    ("Ctrl+Shift+C", "copy"),
    ("Ctrl+Shift+V", "paste"),
    ("middle-click", "paste selection"),
    ("drag", "select text"),
    ("Shift+click", "extend selection"),
    ("double-click", "select a word"),
    ("triple-click", "select the line"),
    ("drag a tab", "move the tab"),
    ("click a link", "open it"),
    ("Ctrl+,", "appearance"),
    ("Ctrl+Enter", "insert a newline"),
    ("Shift+Enter", "insert a newline"),
    ("Esc", "back or close"),
];

/// The glass controls, showing the values for the style on screen.
#[derive(Clone, Copy)]
pub struct GlassPage {
    pub tune: GlassTune,
    pub light: bool,
    pub scroll: u16,
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

/// Row of the glass switch, and the first row of the style list below it.
const GLASS_ROW: u16 = 6;
const STYLE_LIST: u16 = 9;

pub fn render_panel(
    grid_cols: u16,
    grid_rows: u16,
    theme: Theme,
    font_name: &str,
    size: f32,
    style: usize,
    glass: bool,
    page: Option<GlassPage>,
    keys: Option<u16>,
) -> Option<Panel> {
    if let Some(scroll) = keys {
        return render_keys(grid_cols, grid_rows, theme, scroll);
    }
    if let Some(page) = page {
        return render_glass(grid_cols, grid_rows, theme, page);
    }
    let cols = 34u16.min(grid_cols.saturating_sub(1)).max(0);
    let rows = (STYLE_LIST + THEMES.len() as u16).min(grid_rows.saturating_sub(1));
    if cols < 20 || rows < STYLE_LIST + 1 || grid_cols < 21 || grid_rows < STYLE_LIST + 2 {
        return None;
    }
    let origin_col = grid_cols.saturating_sub(cols);
    let origin_row = grid_rows.saturating_sub(rows);
    let mut cells = vec![Cell::blank(theme.fg.pack(), theme.panel.pack()); usize::from(cols) * usize::from(rows)];
    let mut zones = Vec::new();
    let width = usize::from(cols);

    paint(&mut cells, width, 0, 1, "Appearance", theme.fg.pack(), theme.panel.pack(), 0);
    paint_info(&mut cells, &mut zones, width, cols, 0, theme);
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

    paint(&mut cells, width, GLASS_ROW, 1, "Glass mode", theme.fg.pack(), theme.panel.pack(), 0);
    let switch = if glass { "[ on]" } else { "[off]" };
    let switch_fg = if glass { theme.accent.pack() } else { theme.muted.pack() };
    paint(&mut cells, width, GLASS_ROW, cols - 6, switch, switch_fg, theme.panel.pack(), 0);
    zones.push(Zone { col: 0, end: cols, row: GLASS_ROW, action: PanelHit::Glass });

    if glass {
        paint(&mut cells, width, GLASS_ROW + 1, 1, "Look", theme.fg.pack(), theme.panel.pack(), 0);
        paint(&mut cells, width, GLASS_ROW + 1, cols - 4, "[>]", theme.accent.pack(), theme.panel.pack(), 0);
        zones.push(Zone { col: 0, end: cols, row: GLASS_ROW + 1, action: PanelHit::GlassLook });
    }

    paint(&mut cells, width, STYLE_LIST - 1, 1, "Style", theme.muted.pack(), theme.panel.pack(), 0);
    for (index, item) in THEMES.iter().enumerate() {
        let row = STYLE_LIST + index as u16;
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

/// How far the glass list can scroll for a grid of `grid_rows`.
pub fn glass_scroll_max(grid_rows: u16) -> u16 {
    let (_, visible, _) = glass_body(grid_rows.saturating_sub(1));
    (GlassControl::ALL.len() as u16).saturating_sub(visible)
}

/// `(first control row, visible controls, footer row present)`.
fn glass_body(rows: u16) -> (u16, u16, bool) {
    let room = rows.saturating_sub(3);
    let count = GlassControl::ALL.len() as u16;
    if room == 0 {
        return (3, 0, false);
    }
    if count <= room {
        (3, count, false)
    } else {
        (3, room.saturating_sub(1).max(1), true)
    }
}

fn render_glass(grid_cols: u16, grid_rows: u16, theme: Theme, page: GlassPage) -> Option<Panel> {
    let cols = 34u16.min(grid_cols.saturating_sub(1)).max(0);
    let rows = grid_rows.saturating_sub(1);
    if cols < 20 || rows < 5 || grid_cols < 21 {
        return None;
    }
    let (list_row, visible, footer) = glass_body(rows);
    let max_scroll = (GlassControl::ALL.len() as u16).saturating_sub(visible);
    let scroll = page.scroll.min(max_scroll);
    let origin_col = grid_cols.saturating_sub(cols);
    let origin_row = grid_rows.saturating_sub(rows);
    let mut cells = vec![Cell::blank(theme.fg.pack(), theme.panel.pack()); usize::from(cols) * usize::from(rows)];
    let mut zones = Vec::new();
    let width = usize::from(cols);
    let bg = theme.panel.pack();

    paint(&mut cells, width, 0, 1, "[<]", theme.accent.pack(), bg, 0);
    paint(&mut cells, width, 0, 5, "Glass", theme.fg.pack(), bg, 0);
    zones.push(Zone { col: 0, end: 8, row: 0, action: PanelHit::GlassBack });
    paint_info(&mut cells, &mut zones, width, cols, 0, theme);
    let close_col = cols - 2;
    put(&mut cells, width, close_col, 0, '×', theme.muted.pack(), bg, 0);
    zones.push(Zone { col: close_col, end: cols, row: 0, action: PanelHit::Close });
    paint(&mut cells, width, 1, 1, "For this style", theme.muted.pack(), bg, 0);
    paint(&mut cells, width, 2, 1, "Restore default", theme.accent.pack(), bg, 0);
    zones.push(Zone { col: 0, end: cols, row: 2, action: PanelHit::GlassReset });

    for slot in 0..visible {
        let index = usize::from(scroll) + usize::from(slot);
        let Some(control) = GlassControl::ALL.get(index).copied() else { break };
        let row = list_row + slot;
        let value = num(page.tune.get(control, page.light));
        paint_knob(
            &mut cells,
            &mut zones,
            width,
            cols,
            row,
            control.label(),
            &value,
            PanelHit::GlassNudge(control, -1),
            PanelHit::GlassNudge(control, 1),
            theme,
        );
    }

    if footer {
        let row = rows - 1;
        let up = scroll > 0;
        let down = scroll < max_scroll;
        let up_fg = if up { theme.accent.pack() } else { theme.muted.pack() };
        let down_fg = if down { theme.accent.pack() } else { theme.muted.pack() };
        paint(&mut cells, width, row, 1, "[↑]", up_fg, bg, 0);
        paint(&mut cells, width, row, cols - 4, "[↓]", down_fg, bg, 0);
        let place = format!("{}–{}", scroll + 1, scroll + visible);
        let place_col = cols.saturating_sub(place.chars().count() as u16) / 2;
        paint(&mut cells, width, row, place_col, &place, theme.muted.pack(), bg, 0);
        if up {
            zones.push(Zone { col: 0, end: 5, row, action: PanelHit::GlassScroll(-(visible as i16)) });
        }
        if down {
            zones.push(Zone { col: cols - 5, end: cols, row, action: PanelHit::GlassScroll(visible as i16) });
        }
    }

    Some(Panel { origin_col, origin_row, cols, rows, cells, zones })
}

/// How far the shortcut list can scroll for a grid of `grid_rows`.
pub fn keys_scroll_max(grid_rows: u16) -> u16 {
    let (_, visible, _) = keys_body(grid_rows.saturating_sub(1));
    (KEYS.len() as u16).saturating_sub(visible)
}

fn keys_body(rows: u16) -> (u16, u16, bool) {
    let count = KEYS.len() as u16;
    let room = rows.saturating_sub(1);
    if room == 0 {
        return (1, 0, false);
    }
    if count <= room {
        (1, count, false)
    } else {
        (1, room.saturating_sub(1).max(1), true)
    }
}

fn render_keys(grid_cols: u16, grid_rows: u16, theme: Theme, scroll: u16) -> Option<Panel> {
    let cols = 42u16.min(grid_cols.saturating_sub(1)).max(0);
    let rows = grid_rows.saturating_sub(1);
    if cols < 20 || rows < 4 || grid_cols < 21 {
        return None;
    }
    let (list_row, visible, footer) = keys_body(rows);
    let max_scroll = (KEYS.len() as u16).saturating_sub(visible);
    let scroll = scroll.min(max_scroll);
    let origin_col = grid_cols.saturating_sub(cols);
    let origin_row = grid_rows.saturating_sub(rows);
    let mut cells = vec![Cell::blank(theme.fg.pack(), theme.panel.pack()); usize::from(cols) * usize::from(rows)];
    let mut zones = Vec::new();
    let width = usize::from(cols);
    let bg = theme.panel.pack();

    paint(&mut cells, width, 0, 1, "[<]", theme.accent.pack(), bg, 0);
    paint(&mut cells, width, 0, 5, "Keys", theme.fg.pack(), bg, 0);
    zones.push(Zone { col: 0, end: 8, row: 0, action: PanelHit::KeysBack });
    let close_col = cols - 2;
    put(&mut cells, width, close_col, 0, '×', theme.muted.pack(), bg, 0);
    zones.push(Zone { col: close_col, end: cols, row: 0, action: PanelHit::Close });

    for slot in 0..visible {
        let index = usize::from(scroll) + usize::from(slot);
        let Some((shortcut, action)) = KEYS.get(index) else { break };
        let row = list_row + slot;
        paint(&mut cells, width, row, 1, shortcut, theme.accent.pack(), bg, 0);
        let action_col = 1 + shortcut.chars().count() as u16 + 2;
        let room = usize::from(cols.saturating_sub(action_col + 1));
        paint(&mut cells, width, row, action_col, &fit(action, room), theme.fg.pack(), bg, 0);
    }

    if footer {
        let row = rows - 1;
        let up = scroll > 0;
        let down = scroll < max_scroll;
        let up_fg = if up { theme.accent.pack() } else { theme.muted.pack() };
        let down_fg = if down { theme.accent.pack() } else { theme.muted.pack() };
        paint(&mut cells, width, row, 1, "[↑]", up_fg, bg, 0);
        paint(&mut cells, width, row, cols - 4, "[↓]", down_fg, bg, 0);
        if up {
            zones.push(Zone { col: 0, end: 5, row, action: PanelHit::KeysScroll(-(visible as i16)) });
        }
        if down {
            zones.push(Zone { col: cols - 5, end: cols, row, action: PanelHit::KeysScroll(visible as i16) });
        }
    }

    Some(Panel { origin_col, origin_row, cols, rows, cells, zones })
}

fn paint_info(cells: &mut [Cell], zones: &mut Vec<Zone>, width: usize, cols: u16, row: u16, theme: Theme) {
    let col = cols.saturating_sub(4);
    put(cells, width, col, row, 'i', theme.accent.pack(), theme.panel.pack(), 0);
    zones.push(Zone { col: col.saturating_sub(1), end: col + 1, row, action: PanelHit::Keys });
}

fn paint_knob(
    cells: &mut [Cell],
    zones: &mut Vec<Zone>,
    width: usize,
    cols: u16,
    row: u16,
    label: &str,
    value: &str,
    down: PanelHit,
    up: PanelHit,
    theme: Theme,
) {
    let bg = theme.panel.pack();
    paint(cells, width, row, 1, "[-]", theme.accent.pack(), bg, 0);
    paint(cells, width, row, cols - 4, "[+]", theme.accent.pack(), bg, 0);
    let value_col = cols.saturating_sub(5 + value.chars().count() as u16);
    paint(cells, width, row, value_col, value, theme.fg.pack(), bg, 0);
    let room = usize::from(value_col.saturating_sub(6));
    paint(cells, width, row, 5, &fit(label, room), theme.fg.pack(), bg, 0);
    zones.push(Zone { col: 0, end: 5, row, action: down });
    zones.push(Zone { col: cols.saturating_sub(5), end: cols, row, action: up });
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
        let panel = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0, false, None, None).expect("panel");
        assert_eq!(panel.hit(panel.origin_col + 1, panel.origin_row + 4), Some(PanelHit::Smaller));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 2, panel.origin_row + 4), Some(PanelHit::Larger));
        assert_eq!(panel.hit(panel.origin_col + 4, panel.origin_row + 4), Some(PanelHit::Smaller));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 5, panel.origin_row + 4), Some(PanelHit::Larger));
        let size_row = &panel.cells[usize::from(panel.cols) * 4..usize::from(panel.cols) * 5];
        let size_text: String = size_row.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(size_text.contains("Size: 15 px"), "{size_text}");
        assert_eq!(panel.hit(panel.origin_col + 2, panel.origin_row + STYLE_LIST), Some(PanelHit::Style(0)));
        assert_eq!(panel.hit(panel.origin_col + 2, panel.origin_row + GLASS_ROW), Some(PanelHit::Glass));
        let glass_row = &panel.cells[usize::from(panel.cols) * 6..usize::from(panel.cols) * 7];
        let glass_text: String = glass_row.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(glass_text.contains("Glass mode") && glass_text.contains("[off]"), "{glass_text}");
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 1, panel.origin_row), Some(PanelHit::Close));
        assert_eq!(panel.hit(panel.origin_col + panel.cols - 4, panel.origin_row), Some(PanelHit::Keys));
        assert!(panel.contains(panel.origin_col, panel.origin_row));
        assert!(!panel.contains(0, panel.origin_row) || panel.origin_col == 0);
    }

    #[test]
    fn glass_keys_roundtrip_and_one_number_covers_both_themes() {
        let defaults = GlassTune::default();
        let parsed = GlassTune::from_text(&defaults.text());
        assert_eq!(parsed.text(), defaults.text());
        assert!((parsed.opacity - 0.78).abs() < 1e-4);
        assert!((parsed.body_top.dark - 0.035).abs() < 1e-4);
        assert!((parsed.body_top.light - 0.22).abs() < 1e-4);

        let custom = GlassTune::from_text(
            "glass_opacity=0.4\nglass_body_top=0.2\nglass_inset=9\nglass_shadow_y=-2\n# glass_gap=1\n",
        );
        assert!((custom.opacity - 0.4).abs() < 1e-4);
        assert!((custom.body_top.dark - 0.2).abs() < 1e-4 && (custom.body_top.light - 0.2).abs() < 1e-4);
        assert!((custom.inset - 9.0).abs() < 1e-4);
        assert!((custom.shadow_y + 2.0).abs() < 1e-4);
        assert!((custom.gap - defaults.gap).abs() < 1e-4);
        assert!((custom.radius - 14.0).abs() < 1e-4);

        let clamped = GlassTune::from_text("glass_opacity=4\nglass_shadow_blur=-3\nglass_bar=4\n");
        assert!((clamped.opacity - 1.0).abs() < 1e-4);
        assert_eq!(clamped.shadow_blur, 0.0);
        assert_eq!(clamped.bar, 16.0);
        assert!((clamped.sheet - defaults.sheet).abs() < 1e-4, "a bad line leaves the other keys");
    }

    #[test]
    fn look_row_opens_the_glass_controls_for_the_style_on_screen() {
        let theme = color::theme(0);
        let off = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0, false, None, None).expect("panel");
        assert_eq!(off.hit(off.origin_col + 2, off.origin_row + GLASS_ROW + 1), None);
        let on = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0, true, None, None).expect("panel");
        assert_eq!(on.hit(on.origin_col + 2, on.origin_row + GLASS_ROW + 1), Some(PanelHit::GlassLook));

        let page = GlassPage { tune: GlassTune::default(), light: false, scroll: 0 };
        let glass = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0, true, Some(page), None).expect("glass");
        assert_eq!(glass.hit(glass.origin_col + 1, glass.origin_row), Some(PanelHit::GlassBack));
        assert_eq!(
            glass.hit(glass.origin_col + 1, glass.origin_row + 2),
            Some(PanelHit::GlassReset)
        );
        let restore = &glass.cells[usize::from(glass.cols) * 2..usize::from(glass.cols) * 3];
        let restore_text: String = restore.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(restore_text.contains("Restore default"), "{restore_text}");
        assert_eq!(
            glass.hit(glass.origin_col + 1, glass.origin_row + 3),
            Some(PanelHit::GlassNudge(GlassControl::Opacity, -1))
        );
        assert_eq!(
            glass.hit(glass.origin_col + glass.cols - 2, glass.origin_row + 3),
            Some(PanelHit::GlassNudge(GlassControl::Opacity, 1))
        );
        let row = &glass.cells[usize::from(glass.cols) * 3..usize::from(glass.cols) * 4];
        let text: String = row.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(text.contains("Tint") && text.contains("0.78"), "{text}");
        assert_eq!(
            glass.hit(glass.origin_col + glass.cols - 2, glass.origin_row + glass.rows - 1),
            Some(PanelHit::GlassScroll(i16::try_from(glass_body(glass.rows).1).unwrap()))
        );

        let mut tune = GlassTune::default();
        tune.nudge(GlassControl::Opacity, 1, false);
        tune.nudge(GlassControl::BodyTop, 1, false);
        tune.nudge(GlassControl::BodyTop, -1, true);
        let keys = render_panel(80, 24, theme, "JetBrains Mono", 15.0, 0, false, None, Some(0)).expect("keys");
        assert_eq!(keys.hit(keys.origin_col + 1, keys.origin_row), Some(PanelHit::KeysBack));
        let key_row = &keys.cells[usize::from(keys.cols)..usize::from(keys.cols) * 2];
        let key_text: String = key_row.iter().filter_map(|cell| char::from_u32(cell.cp)).collect();
        assert!(key_text.contains("Ctrl+Shift+T") && key_text.contains("new tab"), "{key_text}");
        assert!((tune.opacity - 0.79).abs() < 1e-4);
        assert!((tune.body_top.dark - 0.045).abs() < 1e-4);
        assert!((tune.body_top.light - 0.21).abs() < 1e-4);
    }
}
