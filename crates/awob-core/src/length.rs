//! Length values for scene element coordinates.
//!
//! Accepts `100`, `100px`, `50%`, `100%-60`, `100%+8`, and `center`.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Pixels.
    Px(f64),
    /// Percent of parent extent.
    Percent(f64),
    /// `<percent>% +/- <px>` shorthand for "p% of parent ± fixed px".
    Mixed { percent: f64, px: f64 },
    Center,
}

impl Length {
    pub fn resolve(self, parent: f64) -> f64 {
        match self {
            Length::Px(v) => v,
            Length::Percent(p) => parent * (p / 100.0),
            Length::Mixed { percent, px } => parent * (percent / 100.0) + px,
            Length::Center => parent / 2.0,
        }
    }

    pub fn parse(s: &str) -> Result<Self, LengthError> {
        let s = s.trim();
        if s == "center" {
            return Ok(Length::Center);
        }
        if let Some((p_part, rest)) = s.split_once('%') {
            let percent: f64 = p_part.parse().map_err(|_| LengthError::Bad(s.into()))?;
            let rest = rest.trim();
            if rest.is_empty() {
                return Ok(Length::Percent(percent));
            }
            let (sign, num_str) = match rest.chars().next() {
                Some('+') => (1.0, rest[1..].trim()),
                Some('-') => (-1.0, rest[1..].trim()),
                _ => return Err(LengthError::Bad(s.into())),
            };
            let px: f64 = num_str.trim_end_matches("px").parse()
                .map_err(|_| LengthError::Bad(s.into()))?;
            return Ok(Length::Mixed { percent, px: px * sign });
        }
        let stripped = s.trim_end_matches("px");
        let px: f64 = stripped.parse().map_err(|_| LengthError::Bad(s.into()))?;
        Ok(Length::Px(px))
    }
}

impl fmt::Display for Length {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Length::Px(v) => write!(f, "{v}px"),
            Length::Percent(p) => write!(f, "{p}%"),
            Length::Mixed { percent, px } => {
                if px >= 0.0 { write!(f, "{percent}%+{px}") }
                else { write!(f, "{percent}%{px}") }
            }
            Length::Center => write!(f, "center"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LengthError {
    #[error("invalid length: {0}")]
    Bad(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn px() { assert_eq!(Length::parse("100").unwrap(), Length::Px(100.0)); }
    #[test] fn px_suffix() { assert_eq!(Length::parse("100px").unwrap(), Length::Px(100.0)); }
    #[test] fn percent() { assert_eq!(Length::parse("50%").unwrap(), Length::Percent(50.0)); }
    #[test] fn mixed_minus() {
        assert_eq!(Length::parse("100%-60").unwrap(), Length::Mixed { percent: 100.0, px: -60.0 });
    }
    #[test] fn mixed_plus() {
        assert_eq!(Length::parse("100% + 8").unwrap(), Length::Mixed { percent: 100.0, px: 8.0 });
    }
    #[test] fn center() { assert_eq!(Length::parse("center").unwrap(), Length::Center); }

    #[test] fn resolve() {
        assert_eq!(Length::Mixed { percent: 100.0, px: -60.0 }.resolve(360.0), 300.0);
        assert_eq!(Length::Percent(50.0).resolve(360.0), 180.0);
        assert_eq!(Length::Px(42.0).resolve(360.0), 42.0);
        assert_eq!(Length::Center.resolve(64.0), 32.0);
    }
}
