//! Core kernel collection traits.

/// A common interface for associative key-value mapping data structures in the kernel.
pub trait Map<K, V> {
    /// Inserts a key-value pair into the map.
    /// If the map did have this key present, the previous value is returned.
    fn insert(&mut self, key: K, value: V) -> Option<V>;

    /// Returns a reference to the value corresponding to the key.
    fn get(&self, key: &K) -> Option<&V>;

    /// Returns a mutable reference to the value corresponding to the key.
    fn get_mut(&mut self, key: &K) -> Option<&mut V>;

    /// Removes a key from the map, returning the value at the key if the key was previously in the map.
    fn remove(&mut self, key: &K) -> Option<V>;

    /// Returns true if the map contains a value for the specified key.
    fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Returns the number of elements in the map.
    fn len(&self) -> usize;

    /// Returns true if the map contains no elements.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears the map, removing all key-value pairs.
    fn clear(&mut self);
}

/// A common interface for set data structures in the kernel.
pub trait Set<T> {
    /// Adds a value to the set.
    /// Returns whether the value was newly inserted.
    fn insert(&mut self, value: T) -> bool;

    /// Returns true if the set contains a value.
    fn contains(&self, value: &T) -> bool;

    /// Removes a value from the set. Returns whether the value was present.
    fn remove(&mut self, value: &T) -> bool;

    /// Returns the number of elements in the set.
    fn len(&self) -> usize;

    /// Returns true if the set contains no elements.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears the set, removing all elements.
    fn clear(&mut self);
}
