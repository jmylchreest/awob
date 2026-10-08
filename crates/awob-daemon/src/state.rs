//! Per-(source, event) history, bounded by age, entry count and identifier bytes.
//!
//! Expiry is ordered so updating a live key does not scan unrelated entries.
//! Shared identifiers keep the expiry and listener indexes from copying strings.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const HISTORY_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 4096;
const MAX_IDENTIFIER_BYTES: usize = 4 * 1024 * 1024;

type ExpiryKey = (Instant, Arc<str>, Arc<str>);

#[derive(Debug, Clone)]
pub struct Entry {
    pub event: Arc<str>,
    pub last_value: f64,
    pub last_max: f64,
    pub last_seen: Instant,
    pub listener_id: Option<Arc<str>>,
}

#[derive(Debug, Default)]
pub struct History {
    by_source: HashMap<Arc<str>, HashMap<Arc<str>, Entry>>,
    /// Reference counts preserve membership while any event belongs to the pair.
    by_listener: HashMap<Arc<str>, HashMap<Arc<str>, usize>>,
    /// Source/event break ties between records sharing an Instant.
    expiry: BTreeSet<ExpiryKey>,
    /// Conservatively charges each entry for its identifiers, even when shared.
    identifier_bytes: usize,
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
        self.record_at(source, listener_id, event, value, max, Instant::now())
    }

    fn record_at(
        &mut self,
        source: &str,
        listener_id: Option<&str>,
        event: &str,
        value: f64,
        max: f64,
        now: Instant,
    ) -> RecordOutcome {
        self.evict_expired_at(now);

        // Normal updates reuse identifiers and listener membership. In particular,
        // an existing duplicate must not produce another warning on every update.
        if let Some((source_key, events)) = self.by_source.get_key_value(source)
            && let Some(entry) = events.get(event)
        {
            let source_key = Arc::clone(source_key);
            let event_key = Arc::clone(&entry.event);
            let old_expiry = (
                entry.last_seen,
                Arc::clone(&source_key),
                Arc::clone(&event_key),
            );
            if entry.listener_id.as_deref() == listener_id {
                self.expiry.remove(&old_expiry);
                let entry = self
                    .by_source
                    .get_mut(source)
                    .expect("source exists")
                    .get_mut(event)
                    .expect("event exists");
                entry.last_value = value;
                entry.last_max = max;
                entry.last_seen = now;
                self.expiry.insert((now, source_key, event_key));
                return RecordOutcome::default();
            }
            self.remove(&old_expiry);
        }

        let bytes = source
            .len()
            .saturating_add(event.len())
            .saturating_add(listener_id.map_or(0, str::len));
        // The send itself still succeeds; oversized identifiers are not retained.
        if bytes > MAX_IDENTIFIER_BYTES {
            return RecordOutcome::default();
        }
        while self.expiry.len() >= MAX_ENTRIES
            || self.identifier_bytes + bytes > MAX_IDENTIFIER_BYTES
        {
            self.remove_oldest();
        }

        let source: Arc<str> = self
            .by_source
            .get_key_value(source)
            .map_or_else(|| Arc::from(source), |(key, _)| Arc::clone(key));
        let event: Arc<str> = Arc::from(event);
        let listener_id: Option<Arc<str>> = listener_id.map(|lid| {
            self.by_listener
                .get_key_value(lid)
                .map_or_else(|| Arc::from(lid), |(key, _)| Arc::clone(key))
        });
        let mut outcome = RecordOutcome::default();
        if let Some(lid) = &listener_id {
            let sources = self.by_listener.entry(Arc::clone(lid)).or_default();
            let count = sources.entry(Arc::clone(&source)).or_default();
            *count += 1;
            if *count == 1 && sources.len() > 1 {
                outcome.duplicate_listener = Some(DuplicateInfo {
                    listener_id: lid.to_string(),
                    sources: sources.keys().map(ToString::to_string).collect(),
                });
            }
        }
        self.expiry
            .insert((now, Arc::clone(&source), Arc::clone(&event)));
        self.by_source.entry(source).or_default().insert(
            Arc::clone(&event),
            Entry {
                event,
                last_value: value,
                last_max: max,
                last_seen: now,
                listener_id,
            },
        );
        self.identifier_bytes += bytes;
        outcome
    }

    pub fn get(&self, source: &str, event: &str) -> Option<&Entry> {
        self.get_at(source, event, Instant::now())
    }

    fn get_at(&self, source: &str, event: &str, now: Instant) -> Option<&Entry> {
        self.by_source
            .get(source)
            .and_then(|m| m.get(event))
            .filter(|entry| now.duration_since(entry.last_seen) < HISTORY_TTL)
    }

    fn remove(&mut self, key: &ExpiryKey) {
        self.expiry.remove(key);
        let (_, source, event) = key;
        let events = self
            .by_source
            .get_mut(source)
            .expect("indexed source exists");
        let entry = events.remove(event).expect("indexed event exists");
        if events.is_empty() {
            self.by_source.remove(source);
        }
        self.identifier_bytes -=
            source.len() + event.len() + entry.listener_id.as_deref().map_or(0, str::len);
        if let Some(lid) = entry.listener_id {
            let sources = self
                .by_listener
                .get_mut(&lid)
                .expect("indexed listener exists");
            let count = sources.get_mut(source).expect("listener source exists");
            *count -= 1;
            if *count == 0 {
                sources.remove(source);
            }
            if sources.is_empty() {
                self.by_listener.remove(&lid);
            }
        }
    }

    fn remove_oldest(&mut self) {
        if let Some(key) = self.expiry.first().cloned() {
            self.remove(&key);
        }
    }

    fn evict_expired_at(&mut self, now: Instant) {
        while self
            .expiry
            .first()
            .is_some_and(|(seen, _, _)| now.duration_since(*seen) >= HISTORY_TTL)
        {
            self.remove_oldest();
        }
    }

    pub fn entries(&mut self) -> impl Iterator<Item = (&str, &str, &Entry)> {
        self.entries_at(Instant::now())
    }

    fn entries_at(&mut self, now: Instant) -> impl Iterator<Item = (&str, &str, &Entry)> {
        self.evict_expired_at(now);
        self.by_source.iter().flat_map(|(s, events)| {
            events
                .iter()
                .map(move |(e, entry)| (s.as_ref(), e.as_ref(), entry))
        })
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
        assert_eq!(e.event.as_ref(), "volume");
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
    #[test]
    fn changing_listener_removes_orphan_membership() {
        let mut h = History::new();
        for i in 0..10_000 {
            h.record(
                "speaker",
                Some(&format!("listener-{i}")),
                "volume",
                1.0,
                1.0,
            );
        }
        assert_eq!(h.by_listener.len(), 1);
        assert!(
            h.record("other", Some("listener-0"), "volume", 1.0, 1.0)
                .duplicate_listener
                .is_none()
        );
    }

    #[test]
    fn changing_listener_preserves_other_events_membership() {
        let mut h = History::new();
        h.record("speaker", Some("old"), "volume", 0.5, 1.0);
        h.record("speaker", Some("old"), "mute", 0.0, 1.0);
        h.record("speaker", Some("new"), "volume", 0.6, 1.0);
        assert!(
            h.record("other", Some("old"), "mute", 0.0, 1.0)
                .duplicate_listener
                .is_some()
        );
        h.record("speaker", None, "mute", 0.0, 1.0);
        // Only "other" now belongs to old; reassigning speaker must detect it anew.
        assert!(
            h.record("speaker", Some("old"), "mute", 0.0, 1.0)
                .duplicate_listener
                .is_some()
        );
    }

    fn assert_indexes(h: &History) {
        let mut count = 0;
        let mut bytes = 0;
        let mut listeners: HashMap<Arc<str>, HashMap<Arc<str>, usize>> = HashMap::new();
        for (source, events) in &h.by_source {
            for (event, entry) in events {
                count += 1;
                bytes +=
                    source.len() + event.len() + entry.listener_id.as_deref().map_or(0, str::len);
                assert!(h.expiry.contains(&(
                    entry.last_seen,
                    Arc::clone(source),
                    Arc::clone(event)
                )));
                if let Some(lid) = &entry.listener_id {
                    *listeners
                        .entry(Arc::clone(lid))
                        .or_default()
                        .entry(Arc::clone(source))
                        .or_default() += 1;
                }
            }
        }
        assert_eq!(count, h.expiry.len());
        assert_eq!(bytes, h.identifier_bytes);
        assert_eq!(listeners, h.by_listener);
        assert!(count <= MAX_ENTRIES);
        assert!(bytes <= MAX_IDENTIFIER_BYTES);
    }

    #[test]
    fn ttl_boundary_applies_to_first_read_and_query_without_a_send() {
        let mut h = History::new();
        let start = Instant::now();
        h.record_at("source", Some("listener"), "volume", 0.5, 1.0, start);
        assert!(
            h.get_at(
                "source",
                "volume",
                start + HISTORY_TTL - Duration::from_nanos(1)
            )
            .is_some()
        );
        assert!(h.get_at("source", "volume", start + HISTORY_TTL).is_none());
        assert_eq!(h.entries_at(start + HISTORY_TTL).count(), 0);
        assert!(h.by_listener.is_empty());
        assert_indexes(&h);
    }

    #[test]
    fn refreshed_record_outlives_original_expiry() {
        let mut h = History::new();
        let start = Instant::now();
        h.record_at("source", Some("listener"), "volume", 0.5, 1.0, start);
        h.record_at(
            "source",
            Some("listener"),
            "volume",
            0.6,
            1.0,
            start + Duration::from_secs(1),
        );
        h.evict_expired_at(start + HISTORY_TTL);
        assert_eq!(
            h.get_at("source", "volume", start + HISTORY_TTL)
                .unwrap()
                .last_value,
            0.6
        );
        assert_indexes(&h);
    }

    #[test]
    fn expiry_removes_only_its_events_listener_membership() {
        let mut h = History::new();
        let start = Instant::now();
        h.record_at("source", Some("old"), "volume", 0.5, 1.0, start);
        h.record_at(
            "source",
            Some("new"),
            "mute",
            0.0,
            1.0,
            start + Duration::from_secs(1),
        );
        h.evict_expired_at(start + HISTORY_TTL);
        assert!(!h.by_listener.contains_key("old"));
        assert!(h.by_listener.contains_key("new"));
        assert_indexes(&h);
    }

    #[test]
    fn entry_limit_evicts_oldest_update_and_handles_equal_timestamps() {
        let mut h = History::new();
        let start = Instant::now();
        for i in 0..MAX_ENTRIES {
            h.record_at(
                &format!("source-{i:04}"),
                Some("listener"),
                "volume",
                0.5,
                1.0,
                start,
            );
        }
        h.record_at(
            "source-0000",
            Some("listener"),
            "volume",
            0.6,
            1.0,
            start + Duration::from_secs(1),
        );
        h.record_at(
            "new",
            None,
            "volume",
            0.5,
            1.0,
            start + Duration::from_secs(2),
        );
        assert!(
            h.get_at("source-0000", "volume", start + Duration::from_secs(2))
                .is_some()
        );
        assert!(
            h.get_at("source-0001", "volume", start + Duration::from_secs(2))
                .is_none()
        );
        assert_eq!(h.expiry.len(), MAX_ENTRIES);
        assert_indexes(&h);
    }

    #[test]
    fn bytes_limit_evicts_oldest_without_truncating_identifiers() {
        let mut h = History::new();
        let start = Instant::now();
        let source = "s".repeat(MAX_IDENTIFIER_BYTES - 1);
        h.record_at(&source, None, "v", 0.5, 1.0, start);
        assert_eq!(h.identifier_bytes, MAX_IDENTIFIER_BYTES);
        h.record_at(
            "new",
            Some("listener"),
            "volume",
            0.6,
            1.0,
            start + Duration::from_secs(1),
        );
        assert!(
            h.get_at(&source, "v", start + Duration::from_secs(1))
                .is_none()
        );
        assert!(
            h.get_at("new", "volume", start + Duration::from_secs(1))
                .is_some()
        );
        assert_indexes(&h);
    }

    #[test]
    fn oversized_replacement_forgets_old_record_without_evicting_other_keys() {
        let mut h = History::new();
        let start = Instant::now();
        h.record_at("source", Some("listener"), "volume", 0.5, 1.0, start);
        h.record_at("other", None, "volume", 0.5, 1.0, start);
        h.record_at(
            "source",
            Some(&"x".repeat(MAX_IDENTIFIER_BYTES)),
            "volume",
            0.6,
            1.0,
            start,
        );
        assert!(h.get_at("source", "volume", start).is_none());
        assert!(h.get_at("other", "volume", start).is_some());
        assert!(h.by_listener.is_empty());
        assert_indexes(&h);
    }

    #[test]
    fn duplicate_warns_only_when_membership_is_new() {
        let mut h = History::new();
        h.record("one", Some("listener"), "v", 1.0, 1.0);
        assert!(
            h.record("two", Some("listener"), "v", 1.0, 1.0)
                .duplicate_listener
                .is_some()
        );
        assert!(
            h.record("two", Some("listener"), "v", 0.5, 1.0)
                .duplicate_listener
                .is_none()
        );
        assert!(
            h.record("two", Some("listener"), "mute", 0.0, 1.0)
                .duplicate_listener
                .is_none()
        );
        assert_indexes(&h);
    }
}
