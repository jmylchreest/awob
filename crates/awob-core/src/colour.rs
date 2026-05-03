//! RGBA colour value with parsers for the syntax accepted in scene files.

use std::fmt;


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colour {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Colour {
    pub const BLACK: Colour = Colour { r: 0, g: 0, b: 0, a: 255 };
    pub const TRANSPARENT: Colour = Colour { r: 0, g: 0, b: 0, a: 0 };

    pub fn rgb(r: u8, g: u8, b: u8) -> Self { Self { r, g, b, a: 255 } }
    pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self { Self { r, g, b, a } }

    /// Parse `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb(r,g,b)`, `rgba(r,g,b,a)`.
    pub fn parse(s: &str) -> Result<Self, ColourError> {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            return parse_hex(hex);
        }
        if let Some(rest) = s.strip_prefix("rgba(").and_then(|r| r.strip_suffix(')')) {
            return parse_rgb_fn(rest, true);
        }
        if let Some(rest) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
            return parse_rgb_fn(rest, false);
        }
        Err(ColourError::Unrecognised(s.to_string()))
    }
}

fn parse_hex(hex: &str) -> Result<Colour, ColourError> {
    let (r, g, b, a) = match hex.len() {
        3 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16)?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16)?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16)?;
            (r, g, b, 255)
        }
        4 => {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16)?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16)?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16)?;
            let a = u8::from_str_radix(&hex[3..4].repeat(2), 16)?;
            (r, g, b, a)
        }
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16)?;
            let g = u8::from_str_radix(&hex[2..4], 16)?;
            let b = u8::from_str_radix(&hex[4..6], 16)?;
            (r, g, b, 255)
        }
        8 => {
            let r = u8::from_str_radix(&hex[0..2], 16)?;
            let g = u8::from_str_radix(&hex[2..4], 16)?;
            let b = u8::from_str_radix(&hex[4..6], 16)?;
            let a = u8::from_str_radix(&hex[6..8], 16)?;
            (r, g, b, a)
        }
        _ => return Err(ColourError::BadHexLength(hex.len())),
    };
    Ok(Colour { r, g, b, a })
}

fn parse_rgb_fn(args: &str, with_alpha: bool) -> Result<Colour, ColourError> {
    let parts: Vec<_> = args.split(',').map(str::trim).collect();
    let expected = if with_alpha { 4 } else { 3 };
    if parts.len() != expected {
        return Err(ColourError::BadArgCount { want: expected, got: parts.len() });
    }
    let r = parts[0].parse::<u32>().map_err(|_| ColourError::BadComponent(parts[0].into()))?;
    let g = parts[1].parse::<u32>().map_err(|_| ColourError::BadComponent(parts[1].into()))?;
    let b = parts[2].parse::<u32>().map_err(|_| ColourError::BadComponent(parts[2].into()))?;
    let a = if with_alpha {
        let av: f64 = parts[3].parse().map_err(|_| ColourError::BadComponent(parts[3].into()))?;
        if (0.0..=1.0).contains(&av) {
            (av * 255.0).round() as u32
        } else {
            av as u32
        }
    } else {
        255
    };
    Ok(Colour {
        r: r.min(255) as u8,
        g: g.min(255) as u8,
        b: b.min(255) as u8,
        a: a.min(255) as u8,
    })
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.a == 255 {
            write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ColourError {
    #[error("unrecognised colour value: {0}")]
    Unrecognised(String),
    #[error("hex colour must be 3, 4, 6, or 8 chars, got {0}")]
    BadHexLength(usize),
    #[error("bad colour component: {0}")]
    BadComponent(String),
    #[error("expected {want} components, got {got}")]
    BadArgCount { want: usize, got: usize },
    #[error("hex parse: {0}")]
    Hex(#[from] std::num::ParseIntError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn hex6() { assert_eq!(Colour::parse("#f3e8d7").unwrap(), Colour::rgb(0xf3, 0xe8, 0xd7)); }
    #[test] fn hex8() { assert_eq!(Colour::parse("#1c1c23cc").unwrap(), Colour::rgba(0x1c, 0x1c, 0x23, 0xcc)); }
    #[test] fn hex3() { assert_eq!(Colour::parse("#abc").unwrap(), Colour::rgb(0xaa, 0xbb, 0xcc)); }
    #[test] fn rgba_floats() {
        let c = Colour::parse("rgba(28,28,35,0.85)").unwrap();
        assert_eq!(c.r, 28); assert_eq!(c.g, 28); assert_eq!(c.b, 35);
        assert!((c.a as i16 - (0.85 * 255.0) as i16).abs() <= 1);
    }
    #[test] fn rgb_int() {
        assert_eq!(Colour::parse("rgb(255, 128, 0)").unwrap(), Colour::rgb(255, 128, 0));
    }
    #[test] fn bad() { assert!(Colour::parse("not a color").is_err()); }
}
