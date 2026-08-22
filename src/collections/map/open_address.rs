use alloc::vec::Vec;
use core::hash::{BuildHasher, Hash};

use crate::collections::hasher::BuildFnvHasher;
use crate::collections::traits::Map;

const INITIAL_CAPACITY: usize = 16;
const MAX_LOAD_FACTOR_PERCENT: usize = 70;

#[derive(Clone)]
enum Bucket<K, V> {
    Empty,
    Occupied(K, V),
    Tombstone,
}

pub struct OpenAddressMap<K, V, S = BuildFnvHasher> {
    buckets: Vec<Bucket<K, V>>,
    len: usize,
    occupied_count: usize, // Includes Tombstones
    hasher_builder: S,
}

impl<K, V> OpenAddressMap<K, V, BuildFnvHasher> {
    pub fn new() -> Self {
        Self::with_hasher(BuildFnvHasher::new())
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_and_hasher(capacity, BuildFnvHasher::new())
    }
}

impl<K, V, S> OpenAddressMap<K, V, S> {
    pub fn with_hasher(hasher_builder: S) -> Self {
        Self::with_capacity_and_hasher(INITIAL_CAPACITY, hasher_builder)
    }

    pub fn with_capacity_and_hasher(capacity: usize, hasher_builder: S) -> Self {
        let cap = capacity.max(4).next_power_of_two();
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(Bucket::Empty);
        }

        Self {
            buckets,
            len: 0,
            occupied_count: 0,
            hasher_builder,
        }
    }
}

impl<K, V> Default for OpenAddressMap<K, V, BuildFnvHasher> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S> OpenAddressMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn hash_key<Q: ?Sized + Hash>(&self, key: &Q) -> usize {
        self.hasher_builder.hash_one(key) as usize
    }

    fn resize(&mut self) {
        let new_capacity = (self.buckets.len() * 2).max(INITIAL_CAPACITY);
        let mut new_buckets = Vec::with_capacity(new_capacity);
        for _ in 0..new_capacity {
            new_buckets.push(Bucket::Empty);
        }

        let old_buckets = core::mem::replace(&mut self.buckets, new_buckets);
        self.occupied_count = 0;

        for bucket in old_buckets {
            if let Bucket::Occupied(key, value) = bucket {
                let mask = self.buckets.len() - 1;
                let mut idx = self.hash_key(&key) & mask;

                while let Bucket::Occupied(..) = &self.buckets[idx] {
                    idx = (idx + 1) & mask;
                }

                self.buckets[idx] = Bucket::Occupied(key, value);
                self.occupied_count += 1;
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.buckets.iter().filter_map(|bucket| match bucket {
            Bucket::Occupied(k, v) => Some((k, v)),
            _ => None,
        })
    }
}

impl<K, V, S> Map<K, V> for OpenAddressMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn insert(&mut self, key: K, value: V) -> Option<V> {
        if (self.occupied_count + 1) * 100 / self.buckets.len() >= MAX_LOAD_FACTOR_PERCENT {
            self.resize();
        }

        let mask = self.buckets.len() - 1;
        let mut idx = self.hash_key(&key) & mask;
        let mut first_tombstone = None;

        loop {
            match &mut self.buckets[idx] {
                Bucket::Empty => {
                    let target_idx = first_tombstone.unwrap_or(idx);
                    if first_tombstone.is_none() {
                        self.occupied_count += 1;
                    }
                    self.buckets[target_idx] = Bucket::Occupied(key, value);
                    self.len += 1;
                    return None;
                }
                Bucket::Tombstone => {
                    if first_tombstone.is_none() {
                        first_tombstone = Some(idx);
                    }
                }
                Bucket::Occupied(existing_key, existing_val) => {
                    if existing_key == &key {
                        return Some(core::mem::replace(existing_val, value));
                    }
                }
            }
            idx = (idx + 1) & mask;
        }
    }

    fn get(&self, key: &K) -> Option<&V> {
        let mask = self.buckets.len() - 1;
        let mut idx = self.hash_key(key) & mask;

        loop {
            match &self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Occupied(k, v) => {
                    if k == key {
                        return Some(v);
                    }
                }
                Bucket::Tombstone => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let mask = self.buckets.len() - 1;
        let mut idx = self.hash_key(key) & mask;

        loop {
            match &mut self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Occupied(k, v) => {
                    if k == key {
                        return Some(v);
                    }
                }
                Bucket::Tombstone => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        let mask = self.buckets.len() - 1;
        let mut idx = self.hash_key(key) & mask;

        loop {
            match &mut self.buckets[idx] {
                Bucket::Empty => return None,
                Bucket::Occupied(k, _) => {
                    if k == key
                        && let Bucket::Occupied(_, v) =
                            core::mem::replace(&mut self.buckets[idx], Bucket::Tombstone)
                    {
                        self.len -= 1;
                        return Some(v);
                    }
                }
                Bucket::Tombstone => {}
            }
            idx = (idx + 1) & mask;
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        for bucket in self.buckets.iter_mut() {
            *bucket = Bucket::Empty;
        }
        self.len = 0;
        self.occupied_count = 0;
    }
}
