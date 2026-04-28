use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct CacheEntry<V> {
    value: V,
    inserted_at: Instant,
}

#[derive(Debug)]
pub struct TtlCache<V> {
    ttl: Duration,
    capacity: usize,
    entries: HashMap<String, CacheEntry<V>>,
    order: VecDeque<String>,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<V> {
        self.purge_expired();
        self.entries.get(key).map(|entry| entry.value.clone())
    }

    pub fn insert(&mut self, key: String, value: V) {
        self.purge_expired();

        if self.entries.contains_key(&key) {
            self.entries.remove(&key);
            self.order.retain(|k| k != &key);
        }

        self.entries.insert(
            key.clone(),
            CacheEntry {
                value,
                inserted_at: Instant::now(),
            },
        );
        self.order.push_back(key);
        self.evict_oldest();
    }

    fn purge_expired(&mut self) {
        let now = Instant::now();
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                (now.saturating_duration_since(entry.inserted_at) > self.ttl).then_some(key.clone())
            })
            .collect();

        for key in expired {
            self.entries.remove(&key);
            self.order.retain(|k| k != &key);
        }
    }

    fn evict_oldest(&mut self) {
        while self.entries.len() > self.capacity {
            let Some(oldest_key) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_cache_evicts_oldest_when_capacity_reached() {
        let mut cache = TtlCache::new(Duration::from_secs(300), 2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        cache.insert("c".into(), 3);

        assert_eq!(cache.get("a"), None);
        assert_eq!(cache.get("b"), Some(2));
        assert_eq!(cache.get("c"), Some(3));
    }

    #[test]
    fn ttl_cache_expires_entries() {
        let mut cache = TtlCache::new(Duration::from_millis(5), 10);
        cache.insert("a".into(), 1);

        std::thread::sleep(Duration::from_millis(10));

        assert_eq!(cache.get("a"), None);
    }
}
