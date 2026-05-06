//! Per-element time-driven animations evaluated each frame during the
//! `show` window. The pulse attribute family (`pulse=true`,
//! `pulse-rate=…`, `pulse-depth=…`) desugars to one [`ElementAnimation`].

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimProperty {
    Alpha,
}

/// Easing curve mapping linear time progress to output progress.
/// Curves may overshoot 0..1 if the effect calls for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimCurve {
    Linear,
    /// `0.5 - 0.5 * cos(πt)` — symmetric, smooth at both ends.
    SineInOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Once,
    Loop,
    /// `from → to → from → to → ...` — full cycle is `2 * duration`.
    PingPong,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementAnimation {
    pub target: AnimProperty,
    pub curve: AnimCurve,
    pub from: f32,
    pub to: f32,
    /// Time for one half-cycle (`from → to`).
    pub duration: Duration,
    pub loop_mode: LoopMode,
    pub delay: Duration,
}

impl ElementAnimation {
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

    /// Only reachable in `Once` mode; loops never finish.
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
