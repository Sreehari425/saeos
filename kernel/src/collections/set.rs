use core::hash::{BuildHasher, Hash};

use crate::collections::hasher::BuildFnvHasher;
use crate::collections::map::OpenAddressMap;
use crate::collections::traits::{Map, Set};

pub struct HashSet<T, S = BuildFnvHasher> {
    map: OpenAddressMap<T, (), S>,
}

impl<T> HashSet<T, BuildFnvHasher> {
    pub fn new() -> Self {
        Self {
            map: OpenAddressMap::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: OpenAddressMap::with_capacity(capacity),
        }
    }
}

impl<T, S> HashSet<T, S> {
    pub fn with_hasher(hasher_builder: S) -> Self {
        Self {
            map: OpenAddressMap::with_hasher(hasher_builder),
        }
    }
}

impl<T> Default for HashSet<T, BuildFnvHasher> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, S> Set<T> for HashSet<T, S>
where
    T: Eq + Hash,
    S: BuildHasher,
{
    fn insert(&mut self, value: T) -> bool {
        self.map.insert(value, ()).is_none()
    }

    fn contains(&self, value: &T) -> bool {
        self.map.contains_key(value)
    }

    fn remove(&mut self, value: &T) -> bool {
        self.map.remove(value).is_some()
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn clear(&mut self) {
        self.map.clear();
    }
}

impl<T, S> HashSet<T, S>
where
    T: Eq + Hash,
    S: BuildHasher,
{
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.iter().map(|(k, _)| k)
    }
}
