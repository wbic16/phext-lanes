//! Serialized responses, kept until the phext changes.
//!
//! An entry is valid while the store's write version and the hub's fabric
//! list are unchanged, and for at most `ttl`. The time limit is there
//! because the feed ranks by recency: with no new posts, the order still
//! moves as threads age.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::body::Bytes;

/// Store version and hub generation the body was built from.
pub type Version = (u64, u64);

struct Entry {
    version: Version,
    at: Instant,
    body: Bytes,
}

pub struct Cache {
    entries: Mutex<HashMap<String, Entry>>,
    ttl: Duration,
    capacity: usize,
}

impl Cache {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Cache {
            entries: Mutex::new(HashMap::new()),
            ttl,
            capacity: capacity.max(1),
        }
    }

    pub fn get(&self, key: &str, version: Version) -> Option<Bytes> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(key)?;
        if entry.version == version && entry.at.elapsed() < self.ttl {
            Some(entry.body.clone())
        } else {
            None
        }
    }

    pub fn put(&self, key: String, version: Version, body: Bytes) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            let oldest = entries
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                entries.remove(&oldest);
            }
        }
        entries.insert(
            key,
            Entry {
                version,
                at: Instant::now(),
                body,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_version_or_an_old_entry_misses() {
        let cache = Cache::new(Duration::from_secs(60), 8);
        cache.put("state".into(), (1, 0), Bytes::from_static(b"one"));
        assert_eq!(cache.get("state", (1, 0)).unwrap(), "one");
        assert!(cache.get("state", (2, 0)).is_none(), "the phext changed");
        assert!(cache.get("state", (1, 1)).is_none(), "a fabric was added");
        assert!(cache.get("other", (1, 0)).is_none());

        let stale = Cache::new(Duration::ZERO, 8);
        stale.put("state".into(), (1, 0), Bytes::from_static(b"one"));
        assert!(stale.get("state", (1, 0)).is_none(), "past the time limit");
    }

    #[test]
    fn the_oldest_entry_makes_room() {
        let cache = Cache::new(Duration::from_secs(60), 2);
        cache.put("a".into(), (1, 0), Bytes::from_static(b"a"));
        std::thread::sleep(Duration::from_millis(2));
        cache.put("b".into(), (1, 0), Bytes::from_static(b"b"));
        cache.put("c".into(), (1, 0), Bytes::from_static(b"c"));
        assert_eq!(cache.len(), 2);
        assert!(cache.get("a", (1, 0)).is_none());
        assert!(cache.get("c", (1, 0)).is_some());
    }
}
