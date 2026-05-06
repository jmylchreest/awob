//! Helpers for building well-behaved awob listeners.
//! [`ChangeFilter`] silences the first observation of each key;
//! [`wait_for_resource`] polls for an absent upstream instead of exiting.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

/// Tracks the last value seen per source and reports only **real**
/// changes — never the initial observation.
///
/// First call to [`changed`](Self::changed) for a given key returns
/// `false` and silently seeds the filter. Subsequent calls return
/// `true` only when the new value differs.
///
/// For single-source listeners use any unit-like key (`()` works).
/// For multi-source listeners (PipeWire nodes, multi-battery) use a
/// per-source key.
///
/// # Example
///
/// ```
/// use awob_client::listener::ChangeFilter;
///
/// let mut filter: ChangeFilter<(), u32> = ChangeFilter::new();
/// assert!(!filter.changed((), &50));
/// assert!(!filter.changed((), &50));
/// assert!( filter.changed((), &60));
/// assert!(!filter.changed((), &60));
/// ```
pub struct ChangeFilter<K, V>
where
    K: Eq + Hash,
    V: PartialEq,
{
    last: HashMap<K, V>,
}

impl<K, V> Default for ChangeFilter<K, V>
where
    K: Eq + Hash,
    V: PartialEq + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> ChangeFilter<K, V>
where
    K: Eq + Hash,
    V: PartialEq + Clone,
{
    pub fn new() -> Self {
        Self {
            last: HashMap::new(),
        }
    }

    /// First observation returns `false` and silently seeds.
    /// Subsequent calls return `true` only when `value` differs.
    pub fn changed(&mut self, key: K, value: &V) -> bool {
        match self.last.get(&key) {
            Some(prev) if prev == value => false,
            Some(_) => {
                self.last.insert(key, value.clone());
                true
            }
            None => {
                self.last.insert(key, value.clone());
                false
            }
        }
    }

    /// Drop the seed for `key`. Useful when a multi-source listener
    /// loses track of a source (PipeWire node removed, sysfs device
    /// unplugged) — the next observation for the same key resumes
    /// the silent-baseline policy as if it were brand new.
    pub fn forget(&mut self, key: &K) {
        self.last.remove(key);
    }
}

/// Block until `scan` returns `Some`, polling every `interval`.
///
/// Logs once at INFO when entering the wait loop and once when the
/// resource appears. Quiet retries log at DEBUG so a machine that
/// genuinely lacks the resource doesn't spam the journal.
///
/// `what` is a human label for those log lines, e.g. `"backlight"`,
/// `"battery"`, `"platform_profile"`.
pub fn wait_for_resource<F, T>(scan: F, what: &str, interval: Duration) -> T
where
    F: Fn() -> Option<T>,
{
    if let Some(t) = scan() {
        return t;
    }
    tracing::info!(
        what = %what,
        interval_secs = interval.as_secs(),
        "resource not present; will rescan for hot-plug"
    );
    loop {
        std::thread::sleep(interval);
        if let Some(t) = scan() {
            tracing::info!(what = %what, "resource appeared; resuming");
            return t;
        }
        tracing::debug!(
            what = %what,
            interval_secs = interval.as_secs(),
            "still absent; rescanning"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_filter_silent_on_first_observation() {
        let mut f: ChangeFilter<u32, i32> = ChangeFilter::new();
        assert!(!f.changed(1, &10));
        assert!(!f.changed(2, &20));
    }

    #[test]
    fn change_filter_silent_on_repeat() {
        let mut f: ChangeFilter<(), i32> = ChangeFilter::new();
        assert!(!f.changed((), &10));
        assert!(!f.changed((), &10));
    }

    #[test]
    fn change_filter_fires_on_real_change() {
        let mut f: ChangeFilter<(), i32> = ChangeFilter::new();
        assert!(!f.changed((), &10));
        assert!(f.changed((), &11));
        assert!(!f.changed((), &11));
        assert!(f.changed((), &12));
    }

    #[test]
    fn change_filter_keys_are_independent() {
        let mut f: ChangeFilter<u32, i32> = ChangeFilter::new();
        f.changed(1, &10);
        f.changed(2, &20);
        assert!(f.changed(1, &11));
        assert!(!f.changed(2, &20));
        assert!(f.changed(2, &21));
    }

    #[test]
    fn change_filter_forget_resets_baseline() {
        let mut f: ChangeFilter<u32, i32> = ChangeFilter::new();
        f.changed(1, &10);
        assert!(f.changed(1, &11));
        f.forget(&1);
        assert!(
            !f.changed(1, &11),
            "after forget, value 11 is a fresh baseline"
        );
        assert!(f.changed(1, &12));
    }

    #[test]
    fn change_filter_seed_advances_on_real_change() {
        let mut f: ChangeFilter<(), i32> = ChangeFilter::new();
        f.changed((), &10);
        assert!(f.changed((), &11));
        assert!(!f.changed((), &11), "seed should now be 11");
        assert!(f.changed((), &12));
    }
}
