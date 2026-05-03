//! Variables exposed to scene expressions.
//!
//! `Bindings` is the value space the expression evaluator queries. The daemon
//! populates it from a [`SendPayload`][awob_protocol::SendPayload] plus tracked
//! per-source history before invoking the renderer.

use std::collections::HashMap;
use std::time::Duration;

use crate::colour::Colour;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    String(String),
    Colour(Colour),
    Bool(bool),
    Null,
}

impl Value {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            Value::String(s) => s.parse().ok(),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
    pub fn as_string(&self) -> String {
        match self {
            Value::Number(n) => {
                if (n.fract()).abs() < 1e-9 { format!("{}", *n as i64) } else { format!("{n}") }
            }
            Value::String(s) => s.clone(),
            Value::Colour(c) => c.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => String::new(),
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => *n != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::Colour(_) => true,
        }
    }
    pub fn is_null(&self) -> bool { matches!(self, Value::Null) }
}

/// Expression evaluation context.
///
/// Holds:
/// * `vars` — runtime values populated per send (`value`, `lastValue`, `event`, …).
/// * `palette` — named colours from the theme palette block, looked up via
///   `$name` when the var lookup misses.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub vars: HashMap<String, Value>,
    pub palette: HashMap<String, Colour>,
}

impl Bindings {
    pub fn new() -> Self { Self::default() }

    pub fn set(&mut self, name: impl Into<String>, value: Value) {
        self.vars.insert(name.into(), value);
    }

    /// Lookup a `$name` value, falling back to palette as a Colour.
    pub fn get(&self, name: &str) -> Value {
        if let Some(v) = self.vars.get(name) { return v.clone(); }
        if let Some(c) = self.palette.get(name) { return Value::Colour(*c); }
        Value::Null
    }
}

/// Build the standard binding set from a `SendPayload` plus tracked history.
pub fn build(
    payload: &awob_protocol::SendPayload,
    last_value: Option<f64>,
    last_max: Option<f64>,
    last_seen: Option<Duration>,
) -> Bindings {
    use Value::*;
    let mut b = Bindings::new();
    b.set("event", String(payload.event.clone()));
    b.set("value", Number(payload.value));
    b.set("max", Number(payload.max));
    let progress = if payload.max > 0.0 { payload.value / payload.max } else { 0.0 };
    b.set("progress", Number(progress));

    match last_value {
        Some(v) => b.set("lastValue", Number(v)),
        None => b.set("lastValue", Null),
    }
    match last_max {
        Some(v) => b.set("lastMax", Number(v)),
        None => b.set("lastMax", Null),
    }

    let delta = match last_value {
        Some(prev) => payload.value - prev,
        None => 0.0,
    };
    b.set("delta", Number(delta));
    let direction = if last_value.is_none() { "flat" }
        else if delta > 0.0 { "up" }
        else if delta < 0.0 { "down" }
        else { "flat" };
    b.set("direction", String(direction.into()));

    b.set("valueAge", Number(last_seen.map(|d| d.as_secs_f64()).unwrap_or(f64::INFINITY)));

    match &payload.app {
        Some(s) => b.set("app", String(s.clone())),
        None => b.set("app", Null),
    }
    match &payload.icon {
        Some(s) => b.set("icon", String(s.clone())),
        None => b.set("icon", Null),
    }
    match &payload.style {
        Some(s) => b.set("style", String(s.clone())),
        None => b.set("style", Null),
    }
    match &payload.accent {
        Some(s) => b.set("accent", String(s.clone())),
        None => b.set("accent", Null),
    }

    b
}
