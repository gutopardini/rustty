/// Color packed as `0x00BBGGRR` for the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn pack(self) -> u32 {
        self.r as u32 | ((self.g as u32) << 8) | ((self.b as u32) << 16)
    }
}

pub const DEFAULT_FG: Rgb = Rgb::new(0xe6, 0xe6, 0xe6);
pub const DEFAULT_BG: Rgb = Rgb::new(0x12, 0x14, 0x1a);
const DANGER_DARK: Rgb = Rgb::new(0xf6, 0xa3, 0x9a);
const DANGER_LIGHT: Rgb = Rgb::new(0xa7, 0x36, 0x3d);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bar: Rgb,
    pub tab: Rgb,
    pub tab_fg: Rgb,
    pub panel: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub danger: Rgb,
}

pub const THEMES: &[Theme] = &[
    Theme {
        id: "rustty",
        name: "Rustty",
        fg: Rgb::new(0xed, 0xea, 0xe4),
        bg: Rgb::new(0x13, 0x16, 0x19),
        bar: Rgb::new(0x1b, 0x1e, 0x22),
        tab: Rgb::new(0x25, 0x2a, 0x2e),
        tab_fg: Rgb::new(0xb6, 0xb8, 0xbb),
        panel: Rgb::new(0x25, 0x2a, 0x2e),
        muted: Rgb::new(0xb6, 0xb8, 0xbb),
        accent: Rgb::new(0xf0, 0xa7, 0x7d),
        danger: DANGER_DARK,
    },
    Theme {
        id: "noite",
        name: "Night",
        fg: DEFAULT_FG,
        bg: DEFAULT_BG,
        bar: Rgb::new(0x10, 0x12, 0x16),
        tab: Rgb::new(0x2a, 0x2e, 0x3a),
        tab_fg: Rgb::new(0x9a, 0xa0, 0xb4),
        panel: Rgb::new(0x1c, 0x20, 0x2c),
        muted: Rgb::new(0x8b, 0x93, 0xa7),
        accent: Rgb::new(0x7a, 0xa2, 0xf7),
        danger: DANGER_DARK,
    },
    Theme {
        id: "tinta",
        name: "Ink",
        fg: Rgb::new(0xf2, 0xf2, 0xf2),
        bg: Rgb::new(0x0c, 0x0c, 0x0c),
        bar: Rgb::new(0x08, 0x08, 0x08),
        tab: Rgb::new(0x24, 0x24, 0x24),
        tab_fg: Rgb::new(0xa3, 0xa3, 0xa3),
        panel: Rgb::new(0x16, 0x16, 0x16),
        muted: Rgb::new(0x8a, 0x8a, 0x8a),
        accent: Rgb::new(0xe8, 0xe8, 0xe8),
        danger: DANGER_DARK,
    },
    Theme {
        id: "papel",
        name: "Paper",
        fg: Rgb::new(0x1c, 0x1e, 0x24),
        bg: Rgb::new(0xf4, 0xf1, 0xea),
        bar: Rgb::new(0xe7, 0xe2, 0xd8),
        tab: Rgb::new(0xd9, 0xd3, 0xc7),
        tab_fg: Rgb::new(0x5c, 0x57, 0x4e),
        panel: Rgb::new(0xfb, 0xf9, 0xf4),
        muted: Rgb::new(0x6e, 0x67, 0x5c),
        accent: Rgb::new(0x3d, 0x5a, 0x80),
        danger: DANGER_LIGHT,
    },
    Theme {
        id: "ambar",
        name: "Amber",
        fg: Rgb::new(0xe6, 0xb1, 0x5a),
        bg: Rgb::new(0x14, 0x0e, 0x08),
        bar: Rgb::new(0x10, 0x0c, 0x06),
        tab: Rgb::new(0x2a, 0x1c, 0x10),
        tab_fg: Rgb::new(0xa8, 0x84, 0x4a),
        panel: Rgb::new(0x1c, 0x14, 0x0c),
        muted: Rgb::new(0x8a, 0x6a, 0x3a),
        accent: Rgb::new(0xf0, 0xc1, 0x4d),
        danger: DANGER_DARK,
    },
    Theme {
        id: "mar",
        name: "Sea",
        fg: Rgb::new(0xc6, 0xe4, 0xe0),
        bg: Rgb::new(0x0c, 0x16, 0x18),
        bar: Rgb::new(0x08, 0x12, 0x14),
        tab: Rgb::new(0x16, 0x30, 0x33),
        tab_fg: Rgb::new(0x7e, 0xa8, 0xa4),
        panel: Rgb::new(0x10, 0x22, 0x26),
        muted: Rgb::new(0x6d, 0x8f, 0x8c),
        accent: Rgb::new(0x5f, 0xd0, 0xd0),
        danger: DANGER_DARK,
    },
];

pub fn theme(index: usize) -> Theme {
    THEMES[index.min(THEMES.len() - 1)]
}

pub fn theme_index(id: &str) -> usize {
    THEMES.iter().position(|theme| theme.id == id).unwrap_or(0)
}

const ANSI: [Rgb; 16] = [
    Rgb::new(0x1c, 0x1c, 0x1c),
    Rgb::new(0xcc, 0x55, 0x55),
    Rgb::new(0x4e, 0xa8, 0x4e),
    Rgb::new(0xc9, 0xa2, 0x27),
    Rgb::new(0x4b, 0x7f, 0xd6),
    Rgb::new(0xb1, 0x5b, 0x9e),
    Rgb::new(0x3a, 0xa8, 0xa8),
    Rgb::new(0xd0, 0xd0, 0xd0),
    Rgb::new(0x66, 0x66, 0x66),
    Rgb::new(0xff, 0x6b, 0x6b),
    Rgb::new(0x6e, 0xd3, 0x6e),
    Rgb::new(0xf0, 0xc1, 0x4d),
    Rgb::new(0x7a, 0xa2, 0xf7),
    Rgb::new(0xd6, 0x7a, 0xd6),
    Rgb::new(0x5f, 0xd0, 0xd0),
    Rgb::new(0xff, 0xff, 0xff),
];

pub fn ansi(index: u8) -> Rgb {
    ANSI[usize::from(index.min(15))]
}

/// 256-color palette: 16 ANSI colors, a 6×6×6 cube, and a gray ramp.
pub fn indexed(index: u8) -> Rgb {
    match index {
        0..=15 => ansi(index),
        16..=231 => {
            let n = index - 16;
            let r = n / 36;
            let g = (n / 6) % 6;
            let b = n % 6;
            let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            Rgb::new(step(r), step(g), step(b))
        }
        232..=255 => {
            let v = 8 + 10 * (index - 232);
            Rgb::new(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_starts_at_black_and_ends_near_white() {
        assert_eq!(indexed(16), Rgb::new(0, 0, 0));
        let last = indexed(231);
        assert!(last.r > 200 && last.g > 200 && last.b > 200);
    }

    #[test]
    fn branded_default_keeps_existing_theme_ids() {
        assert_eq!(theme(0).id, "rustty");
        for id in ["noite", "tinta", "papel", "ambar", "mar"] {
            assert_eq!(theme(theme_index(id)).id, id);
        }
    }
}
