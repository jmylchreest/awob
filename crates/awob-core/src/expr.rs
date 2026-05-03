//! Tiny expression language used inside scene attribute values.
//!
//! Grammar (informal):
//!
//! ```text
//! expr     = ternary
//! ternary  = coalesce ('?' expr ':' expr)?
//! coalesce = compare ('??' compare)*
//! compare  = add (('=='|'!='|'<'|'<='|'>'|'>=') add)?
//! add      = mul (('+'|'-') mul)*
//! mul      = unary (('*'|'/'|'%') unary)*
//! unary    = ('-' | '!')? primary
//! primary  = NUMBER | STRING | '$' IDENT | IDENT '(' args? ')' | '(' expr ')'
//! args     = expr (',' expr)*
//! ```
//!
//! Strings inside an attribute value can also use `{interpolation}` segments;
//! see [`Template`].

use crate::bindings::{Bindings, Value};
use crate::colour::Colour;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    Str(String),
    Var(String),
    Call(String, Vec<Expr>),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Tern(Box<Expr>, Box<Expr>, Box<Expr>),
    Coalesce(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, thiserror::Error)]
pub enum ExprError {
    #[error("parse error at position {pos}: {msg}")]
    Parse { msg: String, pos: usize },
    #[error("unknown function: {0}")]
    UnknownCall(String),
    #[error("type error: {0}")]
    Type(String),
}

pub fn parse(s: &str) -> Result<Expr, ExprError> {
    let mut p = Parser { src: s, pos: 0 };
    let e = p.parse_ternary()?;
    p.skip_ws();
    if p.pos < p.src.len() {
        return Err(ExprError::Parse {
            msg: format!("unexpected `{}`", &p.src[p.pos..]),
            pos: p.pos,
        });
    }
    Ok(e)
}

pub fn eval(e: &Expr, b: &Bindings) -> Result<Value, ExprError> {
    Ok(match e {
        Expr::Number(n) => Value::Number(*n),
        Expr::Str(s) => Value::String(s.clone()),
        Expr::Var(name) => b.get(name),
        Expr::Neg(x) => match eval(x, b)? {
            Value::Number(n) => Value::Number(-n),
            v => return Err(ExprError::Type(format!("negate non-number {v:?}"))),
        },
        Expr::Not(x) => Value::Bool(!eval(x, b)?.truthy()),
        Expr::Coalesce(a, fb) => {
            let av = eval(a, b)?;
            if av.is_null() { eval(fb, b)? } else { av }
        }
        Expr::Tern(c, t, f) => {
            if eval(c, b)?.truthy() {
                eval(t, b)?
            } else {
                eval(f, b)?
            }
        }
        Expr::Bin(op, l, r) => {
            let lv = eval(l, b)?;
            let rv = eval(r, b)?;
            apply_binop(*op, lv, rv)?
        }
        Expr::Call(name, args) => call_builtin(name, args, b)?,
    })
}

fn apply_binop(op: BinOp, l: Value, r: Value) -> Result<Value, ExprError> {
    use BinOp::*;
    let nums = || -> Result<(f64, f64), ExprError> {
        let ln = l
            .as_number()
            .ok_or_else(|| ExprError::Type(format!("not a number: {l:?}")))?;
        let rn = r
            .as_number()
            .ok_or_else(|| ExprError::Type(format!("not a number: {r:?}")))?;
        Ok((ln, rn))
    };
    Ok(match op {
        Add => match (&l, &r) {
            (Value::String(_), _) | (_, Value::String(_)) => {
                Value::String(format!("{}{}", l.as_string(), r.as_string()))
            }
            _ => {
                let (a, b) = nums()?;
                Value::Number(a + b)
            }
        },
        Sub => {
            let (a, b) = nums()?;
            Value::Number(a - b)
        }
        Mul => {
            let (a, b) = nums()?;
            Value::Number(a * b)
        }
        Div => {
            let (a, b) = nums()?;
            Value::Number(if b == 0.0 { 0.0 } else { a / b })
        }
        Mod => {
            let (a, b) = nums()?;
            Value::Number(if b == 0.0 { 0.0 } else { a % b })
        }
        Eq => Value::Bool(l.as_string() == r.as_string()),
        Ne => Value::Bool(l.as_string() != r.as_string()),
        Lt => {
            let (a, b) = nums()?;
            Value::Bool(a < b)
        }
        Le => {
            let (a, b) = nums()?;
            Value::Bool(a <= b)
        }
        Gt => {
            let (a, b) = nums()?;
            Value::Bool(a > b)
        }
        Ge => {
            let (a, b) = nums()?;
            Value::Bool(a >= b)
        }
    })
}

fn call_builtin(name: &str, args: &[Expr], b: &Bindings) -> Result<Value, ExprError> {
    let vs: Vec<Value> = args.iter().map(|a| eval(a, b)).collect::<Result<_, _>>()?;
    match name {
        "icon" => {
            let event = vs.first().map(|v| v.as_string()).unwrap_or_default();
            Ok(Value::String(default_icon(&event).to_string()))
        }
        "label" => {
            let event = vs.first().map(|v| v.as_string()).unwrap_or_default();
            Ok(Value::String(default_label(&event).to_string()))
        }
        "clamp" => {
            let v = vs.first().and_then(Value::as_number).unwrap_or(0.0);
            let lo = vs
                .get(1)
                .and_then(Value::as_number)
                .unwrap_or(f64::NEG_INFINITY);
            let hi = vs
                .get(2)
                .and_then(Value::as_number)
                .unwrap_or(f64::INFINITY);
            Ok(Value::Number(v.clamp(lo, hi)))
        }
        "lerp" => {
            let a = vs.first().and_then(Value::as_number).unwrap_or(0.0);
            let bv = vs.get(1).and_then(Value::as_number).unwrap_or(0.0);
            let t = vs.get(2).and_then(Value::as_number).unwrap_or(0.0);
            Ok(Value::Number(a + (bv - a) * t))
        }
        "min" => Ok(Value::Number(
            vs.iter()
                .filter_map(Value::as_number)
                .fold(f64::INFINITY, f64::min),
        )),
        "max" => Ok(Value::Number(
            vs.iter()
                .filter_map(Value::as_number)
                .fold(f64::NEG_INFINITY, f64::max),
        )),
        "int" => Ok(Value::Number(
            vs.first()
                .and_then(Value::as_number)
                .map(|n| n.trunc())
                .unwrap_or(0.0),
        )),
        "round" => Ok(Value::Number(
            vs.first()
                .and_then(Value::as_number)
                .map(f64::round)
                .unwrap_or(0.0),
        )),
        "upper" => Ok(Value::String(
            vs.first()
                .map(Value::as_string)
                .unwrap_or_default()
                .to_uppercase(),
        )),
        "lower" => Ok(Value::String(
            vs.first()
                .map(Value::as_string)
                .unwrap_or_default()
                .to_lowercase(),
        )),
        "capitalize" => {
            let s = vs.first().map(Value::as_string).unwrap_or_default();
            let mut chars = s.chars();
            let result = match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            };
            Ok(Value::String(result))
        }
        "truncate" => {
            // truncate(s, n)         → first n chars, suffix "…" if cut
            // truncate(s, n, suffix) → first n chars, custom suffix if cut
            // n counts whole Unicode code points; suffix is appended *after*
            // the cut so total visible length may exceed n by suffix.len().
            let s = vs.first().map(Value::as_string).unwrap_or_default();
            let n = vs.get(1).and_then(Value::as_number).unwrap_or(0.0).max(0.0) as usize;
            let suffix = vs
                .get(2)
                .map(Value::as_string)
                .unwrap_or_else(|| "\u{2026}".into());
            let total = s.chars().count();
            if total <= n {
                Ok(Value::String(s))
            } else {
                let mut out: String = s.chars().take(n).collect();
                out.push_str(&suffix);
                Ok(Value::String(out))
            }
        }
        _ => Err(ExprError::UnknownCall(name.to_string())),
    }
}

fn default_icon(event: &str) -> &'static str {
    match event {
        "volume" => "audio-volume-high",
        "volume-low" => "audio-volume-low",
        "volume-medium" => "audio-volume-medium",
        "volume-muted" => "audio-volume-muted",
        "brightness" => "display-brightness",
        "mic" => "microphone-sensitivity-high",
        "mic-muted" => "microphone-disabled",
        "battery" => "battery",
        "battery-low" => "battery-caution",
        "caps" => "input-keyboard",
        _ => "dialog-information",
    }
}

fn default_label(event: &str) -> &'static str {
    match event {
        "volume" => "Volume",
        "brightness" => "Brightness",
        "battery" => "Battery",
        "mic" => "Microphone",
        "caps" => "Caps Lock",
        _ => "",
    }
}

/// String-with-interpolation. Each segment is either literal text or an `Expr`
/// captured from a `{ ... }` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq)]
enum Segment {
    Literal(String),
    Expr(Expr),
}

impl Template {
    pub fn parse(src: &str) -> Result<Self, ExprError> {
        let mut segs = Vec::new();
        let bytes = src.as_bytes();
        let mut i = 0;
        let mut lit = String::new();
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                lit.push('{');
                i += 2;
            } else if c == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                lit.push('}');
                i += 2;
            } else if c == b'{' {
                if !lit.is_empty() {
                    segs.push(Segment::Literal(std::mem::take(&mut lit)));
                }
                let mut depth = 1;
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() {
                    match bytes[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(ExprError::Parse {
                        msg: "unclosed `{`".into(),
                        pos: i,
                    });
                }
                let inner = &src[start..j];
                segs.push(Segment::Expr(parse(inner)?));
                i = j + 1;
            } else if c == b'$' && i + 1 < bytes.len() && is_ident_start(bytes[i + 1] as char) {
                if !lit.is_empty() {
                    segs.push(Segment::Literal(std::mem::take(&mut lit)));
                }
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && is_ident_cont(bytes[j] as char) {
                    j += 1;
                }
                let name = src[start..j].to_string();
                segs.push(Segment::Expr(Expr::Var(name)));
                i = j;
            } else {
                lit.push(c as char);
                i += 1;
            }
        }
        if !lit.is_empty() {
            segs.push(Segment::Literal(lit));
        }
        Ok(Template { segments: segs })
    }

    pub fn render(&self, b: &Bindings) -> Result<String, ExprError> {
        let mut out = String::new();
        for s in &self.segments {
            match s {
                Segment::Literal(t) => out.push_str(t),
                Segment::Expr(e) => out.push_str(&eval(e, b)?.as_string()),
            }
        }
        Ok(out)
    }

    /// Returns `true` if the template has no expression segments.
    pub fn is_static(&self) -> bool {
        !self.segments.iter().any(|s| matches!(s, Segment::Expr(_)))
    }
}

/// Convenience: try to interpret a value as a colour. Falls back through:
/// 1. direct colour string via `Colour::parse`
/// 2. palette lookup if value is a string identifier
pub fn value_as_color(v: &Value, b: &Bindings) -> Option<Colour> {
    match v {
        Value::Colour(c) => Some(*c),
        Value::String(s) => {
            if let Ok(c) = Colour::parse(s) {
                return Some(c);
            }
            b.palette.get(s).copied()
        }
        _ => None,
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_cont(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

// -- parser internals --

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }
    fn eat(&mut self, s: &str) -> bool {
        self.skip_ws();
        if self.src[self.pos..].starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }
    fn err(&self, msg: impl Into<String>) -> ExprError {
        ExprError::Parse {
            msg: msg.into(),
            pos: self.pos,
        }
    }

    fn parse_ternary(&mut self) -> Result<Expr, ExprError> {
        let cond = self.parse_coalesce()?;
        if self.eat("?") {
            let t = self.parse_ternary()?;
            if !self.eat(":") {
                return Err(self.err("expected `:` in ternary"));
            }
            let f = self.parse_ternary()?;
            return Ok(Expr::Tern(Box::new(cond), Box::new(t), Box::new(f)));
        }
        Ok(cond)
    }

    fn parse_coalesce(&mut self) -> Result<Expr, ExprError> {
        let mut left = self.parse_compare()?;
        while self.eat("??") {
            let r = self.parse_compare()?;
            left = Expr::Coalesce(Box::new(left), Box::new(r));
        }
        Ok(left)
    }

    fn parse_compare(&mut self) -> Result<Expr, ExprError> {
        let l = self.parse_add()?;
        let op = if self.eat("==") {
            Some(BinOp::Eq)
        } else if self.eat("!=") {
            Some(BinOp::Ne)
        } else if self.eat("<=") {
            Some(BinOp::Le)
        } else if self.eat(">=") {
            Some(BinOp::Ge)
        } else if self.eat("<") {
            Some(BinOp::Lt)
        } else if self.eat(">") {
            Some(BinOp::Gt)
        } else {
            None
        };
        if let Some(op) = op {
            let r = self.parse_add()?;
            return Ok(Expr::Bin(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn parse_add(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.parse_mul()?;
        loop {
            let op = if self.eat("+") {
                Some(BinOp::Add)
            } else if self.eat("-") {
                Some(BinOp::Sub)
            } else {
                None
            };
            match op {
                Some(o) => {
                    let r = self.parse_mul()?;
                    l = Expr::Bin(o, Box::new(l), Box::new(r));
                }
                None => break,
            }
        }
        Ok(l)
    }

    fn parse_mul(&mut self) -> Result<Expr, ExprError> {
        let mut l = self.parse_unary()?;
        loop {
            let op = if self.eat("*") {
                Some(BinOp::Mul)
            } else if self.eat("/") {
                Some(BinOp::Div)
            } else if self.eat("%") {
                Some(BinOp::Mod)
            } else {
                None
            };
            match op {
                Some(o) => {
                    let r = self.parse_unary()?;
                    l = Expr::Bin(o, Box::new(l), Box::new(r));
                }
                None => break,
            }
        }
        Ok(l)
    }

    fn parse_unary(&mut self) -> Result<Expr, ExprError> {
        self.skip_ws();
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.parse_unary()?)));
        }
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ExprError> {
        self.skip_ws();
        let c = self
            .peek()
            .ok_or_else(|| self.err("unexpected end of expression"))?;
        if c == '(' {
            self.pos += 1;
            let e = self.parse_ternary()?;
            if !self.eat(")") {
                return Err(self.err("expected `)`"));
            }
            return Ok(e);
        }
        if c == '$' {
            self.pos += 1;
            return Ok(Expr::Var(self.parse_ident()?));
        }
        if c == '\'' || c == '"' {
            self.pos += 1;
            let mut s = String::new();
            while let Some(ch) = self.peek() {
                self.pos += ch.len_utf8();
                if ch == c {
                    return Ok(Expr::Str(s));
                }
                if ch == '\\' {
                    if let Some(esc) = self.peek() {
                        self.pos += esc.len_utf8();
                        s.push(esc);
                        continue;
                    }
                }
                s.push(ch);
            }
            return Err(self.err("unterminated string"));
        }
        if c.is_ascii_digit() || c == '.' {
            return self.parse_number();
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let ident = self.parse_ident()?;
            if self.eat("(") {
                let mut args = Vec::new();
                if !self.eat(")") {
                    loop {
                        args.push(self.parse_ternary()?);
                        if self.eat(")") {
                            break;
                        }
                        if !self.eat(",") {
                            return Err(self.err("expected `,` or `)`"));
                        }
                    }
                }
                return Ok(Expr::Call(ident, args));
            }
            return match ident.as_str() {
                "true" => Ok(Expr::Number(1.0)),
                "false" => Ok(Expr::Number(0.0)),
                "null" => Ok(Expr::Var("__null".into())),
                _ => Ok(Expr::Str(ident)),
            };
        }
        Err(self.err(format!("unexpected `{c}`")))
    }

    fn parse_number(&mut self) -> Result<Expr, ExprError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let n: f64 = self.src[start..self.pos]
            .parse()
            .map_err(|_| self.err("bad number"))?;
        Ok(Expr::Number(n))
    }

    fn parse_ident(&mut self) -> Result<String, ExprError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(self.err("expected identifier"));
        }
        Ok(self.src[start..self.pos].to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Bindings {
        let mut b = Bindings::new();
        b.set("value", Value::Number(50.0));
        b.set("max", Value::Number(100.0));
        b.set("event", Value::String("volume".into()));
        b.set("app", Value::Null);
        b.set("lastValue", Value::Null);
        b
    }

    #[test]
    fn number() {
        assert_eq!(
            eval(&parse("42").unwrap(), &b()).unwrap(),
            Value::Number(42.0)
        );
    }
    #[test]
    fn var() {
        assert_eq!(
            eval(&parse("$value").unwrap(), &b()).unwrap(),
            Value::Number(50.0)
        );
    }
    #[test]
    fn arith() {
        assert_eq!(
            eval(&parse("$value / $max * 100").unwrap(), &b()).unwrap(),
            Value::Number(50.0)
        );
    }
    #[test]
    fn coalesce() {
        assert_eq!(
            eval(&parse("$app ?? $event").unwrap(), &b()).unwrap(),
            Value::String("volume".into())
        );
    }
    #[test]
    fn ternary() {
        assert_eq!(
            eval(&parse("$value > 0 ? $value : $max").unwrap(), &b()).unwrap(),
            Value::Number(50.0)
        );
    }
    #[test]
    fn builtin_icon() {
        assert_eq!(
            eval(&parse("icon($event)").unwrap(), &b()).unwrap(),
            Value::String("audio-volume-high".into())
        );
    }
    #[test]
    fn builtin_upper() {
        assert_eq!(
            eval(&parse("upper(\"volume\")").unwrap(), &b()).unwrap(),
            Value::String("VOLUME".into())
        );
    }
    #[test]
    fn builtin_lower() {
        assert_eq!(
            eval(&parse("lower(\"VoLuMe\")").unwrap(), &b()).unwrap(),
            Value::String("volume".into())
        );
    }
    #[test]
    fn builtin_capitalize() {
        assert_eq!(
            eval(&parse("capitalize(\"volume\")").unwrap(), &b()).unwrap(),
            Value::String("Volume".into())
        );
        // capitalize leaves the tail as-is (JS / Python `str.capitalize`
        // semantics, NOT title-case).
        assert_eq!(
            eval(&parse("capitalize(\"vOLUME\")").unwrap(), &b()).unwrap(),
            Value::String("VOLUME".into())
        );
        // Empty string stays empty, doesn't panic.
        assert_eq!(
            eval(&parse("capitalize(\"\")").unwrap(), &b()).unwrap(),
            Value::String("".into())
        );
    }
    #[test]
    fn builtin_truncate() {
        // No truncation needed.
        assert_eq!(
            eval(&parse("truncate(\"volume\", 10)").unwrap(), &b()).unwrap(),
            Value::String("volume".into())
        );
        // Default suffix is the single-char ellipsis.
        assert_eq!(
            eval(&parse("truncate(\"brightness\", 5)").unwrap(), &b()).unwrap(),
            Value::String("brigh\u{2026}".into())
        );
        // Custom suffix.
        assert_eq!(
            eval(
                &parse("truncate(\"brightness\", 5, \"...\")").unwrap(),
                &b()
            )
            .unwrap(),
            Value::String("brigh...".into())
        );
        // Counts Unicode code points, not bytes.
        assert_eq!(
            eval(&parse("truncate(\"héllo\", 4)").unwrap(), &b()).unwrap(),
            Value::String("héll\u{2026}".into())
        );
    }
    #[test]
    fn builtin_label() {
        assert_eq!(
            eval(&parse("label($event)").unwrap(), &b()).unwrap(),
            Value::String("Volume".into())
        );
    }
    #[test]
    fn template_literal() {
        let t = Template::parse("hello").unwrap();
        assert!(t.is_static());
        assert_eq!(t.render(&b()).unwrap(), "hello");
    }
    #[test]
    fn template_interp() {
        let t = Template::parse("{$app ?? label($event)}").unwrap();
        assert_eq!(t.render(&b()).unwrap(), "Volume");
    }
    #[test]
    fn template_mixed() {
        let t = Template::parse("[{int($value)}/{int($max)}]").unwrap();
        assert_eq!(t.render(&b()).unwrap(), "[50/100]");
    }
    #[test]
    fn template_bare_var() {
        let t = Template::parse("$value").unwrap();
        assert_eq!(t.render(&b()).unwrap(), "50");
    }
    #[test]
    fn template_bare_var_in_text() {
        let t = Template::parse("v=$value m=$max").unwrap();
        assert_eq!(t.render(&b()).unwrap(), "v=50 m=100");
    }
    #[test]
    fn template_dollar_dollar_escape() {
        // bare `$` not followed by ident is literal
        let t = Template::parse("$ alone").unwrap();
        assert_eq!(t.render(&b()).unwrap(), "$ alone");
    }
}
