//! Per-element time-driven animations evaluated each frame during the
//! `show` window.
//!
//! Today's public-facing API is a small set of element attributes
//! (`pulse=true`, `pulse-rate="1Hz"`, `pulse-depth="40%"`) that desugar
//! to one [`ElementAnimation`]. The engine is intentionally
//! DSL-shaped so the eventual `@animate` blocks (FUTURES tier 2)
//! parse into the same struct without renderer changes — adding new
//! property variants, curves, and loop modes is purely additive.
//!
//! The renderer applies each animation to a per-frame `RenderState`
//! via [`ElementAnimation::evaluate`], called with the elapsed time
//! since the start of the show phase.

use std::time::Duration;

/// What an animation modulates on the element it's attached to.
///
/// Today only [`AnimProperty::Alpha`] is wired through the renderer
/// (multiplied into the element's final colour alpha). Future
/// tier-2 work adds Scale, Translate, and Color — same engine, more
/// `match` arms in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimProperty {
    /// Multiply into the element's per-frame alpha. 0.0 = fully
    /// transparent, 1.0 = fully opaque (no modulation).
    Alpha,
}

/// Easing curve mapping linear time progress (0..1) to an output
/// progress (0..1). Curves can overshoot 0..1 if the visual effect
/// calls for it; today's pulse uses `SineInOut` which stays within
/// the unit interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimCurve {
    /// `t` (no easing).
    Linear,
    /// `0.5 - 0.5 * cos(πt)` — symmetric, smooth at both ends. Makes
    /// `Alpha` pulses look organic rather than clipped.
    SineInOut,
}

/// What happens when the animation reaches the end of its duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    /// Plays once, then snaps to the `to` value.
    Once,
    /// Restarts from the beginning. Each iteration runs `from → to`.
    Loop,
    /// Runs `from → to → from → to → ...`. Each full cycle is
    /// `2 * duration`.
    PingPong,
}

/// One animation slot on an element. An element may carry multiple
/// independent animations (e.g. a future `breathe` that pulses both
/// Alpha and Scale at slightly different rates).
#[derive(Debug, Clone, PartialEq)]
pub struct ElementAnimation {
    pub target: AnimProperty,
    pub curve: AnimCurve,
    /// Output value at progress 0.0.
    pub from: f32,
    /// Output value at progress 1.0.
    pub to: f32,
    /// Time for one half-cycle (`from → to`). Total cycle in
    /// `PingPong` mode is `2 * duration`.
    pub duration: Duration,
    pub loop_mode: LoopMode,
    /// Time before the animation starts evaluating (returns `from`
    /// for `t < delay`).
    pub delay: Duration,
}

impl ElementAnimation {
    /// Evaluate the animation at elapsed-show-time `t`. Returns the
    /// scalar value to feed into the renderer's `RenderState`
    /// mutation logic.
    pub fn evaluate(&self, t: Duration) -> f32 {
        if t < self.delay {
            return self.from;
        }
        let elapsed = t - self.delay;
        let dur = self.duration.as_secs_f32().max(f32::EPSILON);
        let raw = elapsed.as_secs_f32() / dur;

        // Map raw progress through the loop mode → 0..1 normalized.
        let progress = match self.loop_mode {
            LoopMode::Once => raw.clamp(0.0, 1.0),
            LoopMode::Loop => raw.fract(),
            LoopMode::PingPong => {
                let phase = raw.rem_euclid(2.0);
                if phase <= 1.0 { phase } else { 2.0 - phase }
            }
        };

        let eased = match self.curve {
            AnimCurve::Linear => progress,
            AnimCurve::SineInOut => 0.5 - 0.5 * (std::f32::consts::PI * progress).cos(),
        };

        self.from + (self.to - self.from) * eased
    }

    /// True if the animation has finished and won't change again.
    /// Only `Once` mode reaches this state; loops never finish.
    pub fn is_done(&self, t: Duration) -> bool {
        match self.loop_mode {
            LoopMode::Once => t >= self.delay + self.duration,
            LoopMode::Loop | LoopMode::PingPong => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulse() -> ElementAnimation {
        // 1 Hz, 40 % depth, ping-pong → alpha cycles 0.6↔1.0.
        ElementAnimation {
            target: AnimProperty::Alpha,
            curve: AnimCurve::SineInOut,
            from: 0.6,
            to: 1.0,
            duration: Duration::from_millis(500), // half-period (1 Hz total)
            loop_mode: LoopMode::PingPong,
            delay: Duration::ZERO,
        }
    }

    #[test]
    fn pulse_endpoints() {
        let p = pulse();
        // t=0 → from
        assert!((p.evaluate(Duration::ZERO) - 0.6).abs() < 0.001);
        // t=duration → to (peak of half-period in ping-pong)
        assert!((p.evaluate(Duration::from_millis(500)) - 1.0).abs() < 0.001);
        // t=2*duration → back to from
        assert!((p.evaluate(Duration::from_millis(1000)) - 0.6).abs() < 0.001);
    }

    #[test]
    fn pulse_midpoint_is_average() {
        let p = pulse();
        // sin-in-out at progress 0.5 → 0.5 → midpoint of from/to
        let mid = p.evaluate(Duration::from_millis(250));
        let expected = (0.6 + 1.0) / 2.0;
        assert!((mid - expected).abs() < 0.01);
    }

    #[test]
    fn loop_modes_diverge() {
        let mut a = pulse();
        a.loop_mode = LoopMode::Loop;
        // Loop snaps from 1.0 back to 0.6 at t=duration.
        let just_after = a.evaluate(Duration::from_millis(501));
        assert!(just_after < 0.7);

        a.loop_mode = LoopMode::PingPong;
        let just_after = a.evaluate(Duration::from_millis(501));
        // PingPong stays near 1.0.
        assert!(just_after > 0.99);
    }

    #[test]
    fn delay_returns_from() {
        let mut p = pulse();
        p.delay = Duration::from_millis(200);
        assert_eq!(p.evaluate(Duration::from_millis(100)), 0.6);
        // After the delay it picks up.
        let after = p.evaluate(Duration::from_millis(450));
        assert!(after > 0.7);
    }

    #[test]
    fn is_done_only_for_once() {
        let mut p = pulse();
        p.loop_mode = LoopMode::Once;
        assert!(!p.is_done(Duration::from_millis(499)));
        assert!(p.is_done(Duration::from_millis(500)));
        assert!(p.is_done(Duration::from_secs(60)));

        p.loop_mode = LoopMode::PingPong;
        assert!(!p.is_done(Duration::from_secs(60_000)));
    }
}
