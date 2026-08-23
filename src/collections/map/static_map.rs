use crate::collections::traits::Map;

/// A zero-heap, fixed-capacity key-value map backed by an inline array.
/// Completely safe to use in interrupt handlers, panic hooks, and pre-heap early boot.
pub struct StaticMap<K, V, const CAPACITY: usize> {
    entries: [Option<(K, V)>; CAPACITY],
    len: usize,
}

impl<K, V, const CAPACITY: usize> StaticMap<K, V, CAPACITY> {
    pub const fn new() -> Self {
        Self {
            entries: [const { None }; CAPACITY],
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        CAPACITY
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries
            .iter()
            .filter_map(|entry| entry.as_ref().map(|(k, v)| (k, v)))
    }
}

impl<K, V, const CAPACITY: usize> Default for StaticMap<K, V, CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, const CAPACITY: usize> Map<K, V> for StaticMap<K, V, CAPACITY>
where
    K: PartialEq,
{
    fn insert(&mut self, key: K, value: V) -> Option<V> {
        // Check if key already exists
        for (existing_key, existing_val) in self.entries.iter_mut().flatten() {
            if *existing_key == key {
                return Some(core::mem::replace(existing_val, value));
            }
        }

        // Find empty slot
        for entry in self.entries.iter_mut() {
            if entry.is_none() {
                *entry = Some((key, value));
                self.len += 1;
                return None;
            }
        }

        // Capacity exceeded
        None
    }

    fn get(&self, key: &K) -> Option<&V> {
        for (existing_key, existing_val) in self.entries.iter().flatten() {
            if existing_key == key {
                return Some(existing_val);
            }
        }
        None
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        for (existing_key, existing_val) in self.entries.iter_mut().flatten() {
            if existing_key == key {
                return Some(existing_val);
            }
        }
        None
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        for entry in self.entries.iter_mut() {
            if let Some((existing_key, _)) = entry
                && existing_key == key
            {
                let old = entry.take();
                self.len -= 1;
                return old.map(|(_, v)| v);
            }
        }
        None
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = None;
        }
        self.len = 0;
    }
}
