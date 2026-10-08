//! Reuse short shaped labels without tying cache validity to mutable scene IDs.
//!
//! Entries are private to Renderer, whose font database is not publicly mutable.
//! The source limits only determine cache eligibility; longer runs still draw.

use std::collections::VecDeque;
use std::mem::size_of;

use cosmic_text::Buffer;

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 512;
const MAX_FONT_BYTES: usize = 256;

pub(crate) struct LayoutEntry {
    text: String,
    font: Option<String>,
    pub(crate) buffer: Buffer,
    bytes: usize,
}

impl LayoutEntry {
    pub(crate) fn new(text: String, font: Option<String>, buffer: Buffer) -> Self {
        Self {
            text,
            font,
            buffer,
            bytes: 0,
        }
    }

    fn cache_weight(&self) -> usize {
        let font_bytes = self.font.as_ref().map_or(0, String::capacity);
        let mut bytes = size_of::<Self>()
            + self.text.capacity()
            + font_bytes
            + vector_bytes(&self.buffer.lines);
        for line in &self.buffer.lines {
            // cosmic-text exposes shaping/layout vector capacities but not
            // String/AttrsList capacities. Bound input sizes and reserve a
            // conservative allowance for these small per-line internals.
            bytes += 4096 + 2 * line.text().len() + 4 * font_bytes;
            if let Some(shape) = line.shape_opt() {
                bytes += vector_bytes(&shape.spans);
                for span in &shape.spans {
                    bytes += vector_bytes(&span.words) + vector_bytes(&span.decoration_spans);
                    for word in &span.words {
                        bytes += vector_bytes(&word.glyphs);
                    }
                }
            }
            if let Some(layouts) = line.layout_opt() {
                bytes += vector_bytes(layouts);
                for layout in layouts {
                    bytes += vector_bytes(&layout.glyphs) + vector_bytes(&layout.decorations);
                }
            }
        }
        bytes
    }
}

fn vector_bytes<T>(values: &Vec<T>) -> usize {
    values.capacity() * size_of::<T>()
}

#[derive(Default)]
pub(crate) struct LayoutCache {
    entries: VecDeque<LayoutEntry>,
    bytes: usize,
}

impl LayoutCache {
    pub(crate) fn take(&mut self, text: &str, font: Option<&str>) -> Option<LayoutEntry> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.text == text && entry.font.as_deref() == font)?;
        let entry = self
            .entries
            .remove(index)
            .expect("index came from this cache");
        self.bytes -= entry.bytes;
        Some(entry)
    }

    pub(crate) fn insert(&mut self, mut entry: LayoutEntry) {
        if entry.text.len() > MAX_TEXT_BYTES
            || entry
                .font
                .as_ref()
                .is_some_and(|font| font.len() > MAX_FONT_BYTES)
        {
            return;
        }
        entry.bytes = entry.cache_weight();
        if entry.bytes > MAX_BYTES {
            return;
        }
        while self.bytes + entry.bytes > MAX_BYTES || self.entries.len() >= MAX_ENTRIES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.bytes;
            }
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::Metrics;

    fn entry(text: &str) -> LayoutEntry {
        LayoutEntry::new(
            text.into(),
            None,
            Buffer::new_empty(Metrics::new(14.0, 17.5)),
        )
    }

    #[test]
    fn lru_preserves_hot_labels_and_rejects_large_cache_keys() {
        let mut cache = LayoutCache::default();
        for n in 0..MAX_ENTRIES {
            cache.insert(entry(&n.to_string()));
        }
        let hot = cache.take("0", None).unwrap();
        cache.insert(hot);
        cache.insert(entry("new"));
        assert!(cache.take("1", None).is_none());
        assert!(cache.take("0", None).is_some());
        cache.insert(entry(&"x".repeat(MAX_TEXT_BYTES + 1)));
        let mut long_font = entry("font");
        long_font.font = Some("x".repeat(MAX_FONT_BYTES + 1));
        cache.insert(long_font);
        assert!(cache.take("font", None).is_none());
        assert!(cache.len() <= MAX_ENTRIES);
        assert!(cache.bytes <= MAX_BYTES);
    }

    #[test]
    fn layout_vector_capacity_counts_toward_byte_budget() {
        let mut cache = LayoutCache::default();
        for n in 0..20 {
            let mut value = entry(&n.to_string());
            value
                .buffer
                .lines
                .reserve(MAX_BYTES / 4 / size_of::<cosmic_text::BufferLine>());
            cache.insert(value);
            assert!(cache.bytes <= MAX_BYTES);
        }
        assert!(cache.len() < 20);
        let mut oversized = entry("large");
        oversized
            .buffer
            .lines
            .reserve(MAX_BYTES / size_of::<cosmic_text::BufferLine>() + 1);
        cache.insert(oversized);
        assert!(cache.take("large", None).is_none());
        cache.clear();
        assert_eq!(cache.bytes, 0);
        assert_eq!(cache.len(), 0);
    }
}
