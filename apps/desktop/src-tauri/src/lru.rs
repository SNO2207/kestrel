//! A cache of the values used most recently, up to a total size, for the icon and thumbnail
//! services, which are asked for more distinct images over a session than memory should hold.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

struct Entry<V> {
    value: V,
    cost: usize,
    /// When it was last used, on the cache's own clock.
    stamp: u64,
}

/// Keeps values up to a total cost, such as their size in bytes, and lets the least recently
/// used go first when a new one would take it over that.
pub struct Lru<K, V> {
    cap: usize,
    used: usize,
    clock: u64,
    entries: HashMap<K, Entry<V>>,
    /// Keys by when they were last used, oldest first.
    by_age: BTreeMap<u64, K>,
}

impl<K: Clone + Eq + Hash, V: Clone> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Self { cap, used: 0, clock: 0, entries: HashMap::new(), by_age: BTreeMap::new() }
    }

    /// The value kept for `key`, which becomes the most recently used.
    pub fn get(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.get_mut(key)?;
        self.by_age.remove(&entry.stamp);
        self.clock += 1;
        entry.stamp = self.clock;
        self.by_age.insert(self.clock, key.clone());
        Some(entry.value.clone())
    }

    /// Keeps `value` for `key` in place of any earlier one, then lets the least recently used
    /// values go until the total cost is within the cap.
    pub fn put(&mut self, key: K, value: V, cost: usize) {
        self.remove(&key);
        self.clock += 1;
        self.used += cost;
        self.by_age.insert(self.clock, key.clone());
        self.entries.insert(key, Entry { value, cost, stamp: self.clock });
        while self.used > self.cap
            && let Some((_, oldest)) = self.by_age.pop_first()
        {
            if let Some(entry) = self.entries.remove(&oldest) {
                self.used -= entry.cost;
            }
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.used -= entry.cost;
        self.by_age.remove(&entry.stamp);
        Some(entry.value)
    }

    /// The total cost of what is kept.
    #[cfg(test)]
    pub fn used(&self) -> usize {
        self.used
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_most_recently_used_within_its_cap() {
        let mut cache = Lru::new(300);
        cache.put(1, "a", 100);
        cache.put(2, "b", 100);
        cache.put(3, "c", 100);
        assert_eq!(cache.get(&1), Some("a"), "using a value makes it recent");
        cache.put(4, "d", 100);
        assert_eq!(cache.get(&2), None, "the least recently used goes first");
        assert_eq!((cache.len(), cache.used()), (3, 300));

        // Replacing a value does not count it twice.
        cache.put(4, "e", 50);
        assert_eq!((cache.get(&4), cache.used()), (Some("e"), 250));
        assert_eq!(cache.remove(&4), Some("e"));
        assert_eq!((cache.len(), cache.used()), (2, 200));

        // One value larger than the cap makes room for itself, and then goes too.
        cache.put(5, "huge", 1000);
        assert_eq!((cache.len(), cache.used()), (0, 0));
    }
}
