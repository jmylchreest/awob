//! Per-(source, event) history map.
//!
//! History is keyed by `(source, event)` so that distinct events on the same
//! source — e.g. `volume` then `mute` on a single PipeWire node — don't
//! cross-contaminate `$lastValue`. If a send omits `source`, no history is
//! recorded and `$lastValue`/`$lastMax` are `Null` when the theme renders.
//!
//! TTL eviction runs lazily on each insert: any entry whose `last_seen` is
//! older than [`HISTORY_TTL`] is dropped. Bounds the map without a separate
//! cleanup thread.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub const HISTORY_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct Entry {
    pub event: String,
    pub last_value: f64,
    pub last_max: f64,
    pub last_seen: Instant,
    pub listener_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct History {
    /// Two-level map: `source -> event -> Entry`. Lookup uses native
    /// HashMap borrow rules so callers can pass `&str` for both keys.
    by_source: HashMap<String, HashMap<String, Entry>>,
    /// Reverse index: listener_id → set of distinct source IDs currently
    /// active. Each (listener_id, source) is treated as one independent
    /// listener instance. Multiple events under one (listener_id, source)
    /// are not flagged — that's just one process publishing multiple
    /// metrics for the same node.
    by_listener: HashMap<String, HashSet<String>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(
        &mut self,
        source: &str,
        listener_id: Option<&str>,
        event: &str,
        value: f64,
        max: f64,
    ) -> RecordOutcome {
        self.evict_expired();

        let mut outcome = RecordOutcome::default();
        if let Some(lid) = listener_id {
            let set = self.by_listener.entry(lid.to_string()).or_default();
            // Newly seen source for this listener_id? Multiple distinct
            // sources under one listener_id == multiple processes running
            // the same listener.
            if set.insert(source.to_string()) && set.len() > 1 {
                outcome.duplicate_listener = Some(DuplicateInfo {
                    listener_id: lid.to_string(),
                    sources: set.iter().cloned().collect(),
                });
            }
        }

        self.by_source
            .entry(source.to_string())
            .or_default()
            .insert(
                event.to_string(),
                Entry {
                    event: event.to_string(),
                    last_value: value,
                    last_max: max,
                    last_seen: Instant::now(),
                    listener_id: listener_id.map(|s| s.to_string()),
                },
            );
        outcome
    }

    /// Look up the last recorded entry for `(source, event)`. Returns
    /// `None` if either is unknown.
    pub fn get(&self, source: &str, event: &str) -> Option<&Entry> {
        self.by_source.get(source).and_then(|m| m.get(event))
    }

    pub fn evict_expired(&mut self) {
        let now = Instant::now();
        // Collect stale `(source, event, listener_id)` triples first to
        // avoid mutating while iterating.
        let mut stale: Vec<(String, String, Option<String>)> = Vec::new();
        for (src, events) in &self.by_source {
            for (evt, entry) in events {
                if now.duration_since(entry.last_seen) >= HISTORY_TTL {
                    stale.push((src.clone(), evt.clone(), entry.listener_id.clone()));
                }
            }
        }
        for (src, evt, lid) in stale {
            if let Some(events) = self.by_source.get_mut(&src) {
                events.remove(&evt);
                let source_now_empty = events.is_empty();
                if source_now_empty {
                    self.by_source.remove(&src);
                }
                if source_now_empty {
                    if let Some(lid) = lid {
                        if let Some(set) = self.by_listener.get_mut(&lid) {
                            set.remove(&src);
                            if set.is_empty() {
                                self.by_listener.remove(&lid);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Iterate every `(source, event, entry)` triple in the map.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &String, &Entry)> {
        self.by_source
            .iter()
            .flat_map(|(s, events)| events.iter().map(move |(e, entry)| (s, e, entry)))
    }
}

#[derive(Debug, Default)]
pub struct RecordOutcome {
    pub duplicate_listener: Option<DuplicateInfo>,
}

#[derive(Debug, Clone)]
pub struct DuplicateInfo {
    pub listener_id: String,
    pub sources: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_get() {
        let mut h = History::new();
        h.record(
            "pipewire-7a3f",
            Some("awob-listener-pipewire"),
            "volume",
            50.0,
            100.0,
        );
        let e = h.get("pipewire-7a3f", "volume").unwrap();
        assert_eq!(e.event, "volume");
        assert_eq!(e.last_value, 50.0);
        assert_eq!(e.last_max, 100.0);
        assert_eq!(e.listener_id.as_deref(), Some("awob-listener-pipewire"));
    }

    #[test]
    fn distinct_events_on_same_source_do_not_cross_contaminate() {
        let mut h = History::new();
        h.record(
            "speaker",
            Some("awob-listener-pipewire"),
            "volume",
            0.6,
            1.0,
        );
        h.record("speaker", Some("awob-listener-pipewire"), "mute", 1.0, 1.0);
        // After the mute send, the volume history must still report 0.6 —
        // a regression in the old single-key map would have it report 1.0
        // (the mute value bleeding into the volume slot).
        assert_eq!(h.get("speaker", "volume").unwrap().last_value, 0.6);
        assert_eq!(h.get("speaker", "mute").unwrap().last_value, 1.0);
    }

    #[test]
    fn missing_returns_none() {
        let h = History::new();
        assert!(h.get("nope", "volume").is_none());
    }

    #[test]
    fn missing_event_returns_none_even_when_source_known() {
        let mut h = History::new();
        h.record("speaker", None, "volume", 0.5, 1.0);
        assert!(h.get("speaker", "mute").is_none());
    }

    #[test]
    fn duplicate_listener_detected_when_two_processes_share_listener_id() {
        let mut h = History::new();
        let r1 = h.record(
            "aaaa",
            Some("awob-listener-pipewire-speaker"),
            "volume",
            10.0,
            100.0,
        );
        assert!(r1.duplicate_listener.is_none());
        let r2 = h.record(
            "bbbb",
            Some("awob-listener-pipewire-speaker"),
            "volume",
            20.0,
            100.0,
        );
        let dup = r2.duplicate_listener.expect("expected duplicate detection");
        assert_eq!(dup.listener_id, "awob-listener-pipewire-speaker");
        assert_eq!(dup.sources.len(), 2);
    }

    #[test]
    fn different_listener_ids_are_independent() {
        let mut h = History::new();
        let r1 = h.record(
            "aaaa",
            Some("awob-listener-pipewire-speaker"),
            "volume",
            50.0,
            100.0,
        );
        assert!(r1.duplicate_listener.is_none());
        let r2 = h.record(
            "aaaa",
            Some("awob-listener-pipewire-mic"),
            "mic",
            80.0,
            100.0,
        );
        assert!(
            r2.duplicate_listener.is_none(),
            "different listener_ids should never trigger duplicate detection, even with the same source"
        );
    }

    #[test]
    fn no_duplicate_when_listener_id_missing() {
        let mut h = History::new();
        h.record("a", None, "v", 10.0, 100.0);
        let r = h.record("b", None, "v", 20.0, 100.0);
        assert!(r.duplicate_listener.is_none());
    }

    #[test]
    fn re_record_same_source_event_no_duplicate() {
        let mut h = History::new();
        h.record("aaaa", Some("battery"), "battery", 50.0, 100.0);
        let r = h.record("aaaa", Some("battery"), "battery", 49.0, 100.0);
        assert!(r.duplicate_listener.is_none());
    }

    #[test]
    fn multiple_events_one_source_one_listener_no_duplicate() {
        // One PipeWire process publishing both `volume` and `mute` for the
        // same source must not be flagged as a duplicate listener.
        let mut h = History::new();
        let r1 = h.record(
            "speaker",
            Some("awob-listener-pipewire"),
            "volume",
            0.6,
            1.0,
        );
        let r2 = h.record("speaker", Some("awob-listener-pipewire"), "mute", 1.0, 1.0);
        assert!(r1.duplicate_listener.is_none());
        assert!(r2.duplicate_listener.is_none());
    }
}
