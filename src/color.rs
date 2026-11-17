/// Cor empacotada em `0x00BBGGRR` para o shader.
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

/// Paleta 256: 16 ANSI, cubo 6×6×6 e rampa de cinza.
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
}
