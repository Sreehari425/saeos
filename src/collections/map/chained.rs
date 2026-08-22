use alloc::boxed::Box;
use alloc::vec::Vec;
use core::hash::{BuildHasher, Hash};

use crate::collections::hasher::BuildFnvHasher;
use crate::collections::traits::Map;

const DEFAULT_NUM_BUCKETS: usize = 32;

struct Node<K, V> {
    key: K,
    value: V,
    next: Option<Box<Node<K, V>>>,
}

/// A Chained Hash Table with fixed/extensible buckets and singly-linked list collision chains.
/// Modeled after the Linux kernel's `include/linux/hashtable.h` (`hlist_head`).
pub struct ChainedMap<K, V, S = BuildFnvHasher> {
    buckets: Vec<Option<Box<Node<K, V>>>>,
    len: usize,
    hasher_builder: S,
}

impl<K, V> ChainedMap<K, V, BuildFnvHasher> {
    pub fn new() -> Self {
        Self::with_hasher(BuildFnvHasher::new())
    }

    pub fn with_buckets(num_buckets: usize) -> Self {
        Self::with_buckets_and_hasher(num_buckets, BuildFnvHasher::new())
    }
}

impl<K, V, S> ChainedMap<K, V, S> {
    pub fn with_hasher(hasher_builder: S) -> Self {
        Self::with_buckets_and_hasher(DEFAULT_NUM_BUCKETS, hasher_builder)
    }

    pub fn with_buckets_and_hasher(num_buckets: usize, hasher_builder: S) -> Self {
        let cap = num_buckets.max(4).next_power_of_two();
        let mut buckets = Vec::with_capacity(cap);
        for _ in 0..cap {
            buckets.push(None);
        }

        Self {
            buckets,
            len: 0,
            hasher_builder,
        }
    }
}

impl<K, V> Default for ChainedMap<K, V, BuildFnvHasher> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S> ChainedMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn hash_key(&self, key: &K) -> usize {
        (self.hasher_builder.hash_one(key) as usize) & (self.buckets.len() - 1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.buckets.iter().flat_map(|bucket| {
            let mut items = Vec::new();
            let mut curr = bucket.as_deref();
            while let Some(node) = curr {
                items.push((&node.key, &node.value));
                curr = node.next.as_deref();
            }
            items
        })
    }
}

impl<K, V, S> Map<K, V> for ChainedMap<K, V, S>
where
    K: Eq + Hash,
    S: BuildHasher,
{
    fn insert(&mut self, key: K, value: V) -> Option<V> {
        let bucket_idx = self.hash_key(&key);
        let mut curr = &mut self.buckets[bucket_idx];

        while let Some(node) = curr {
            if node.key == key {
                return Some(core::mem::replace(&mut node.value, value));
            }
            curr = &mut node.next;
        }

        let old_head = self.buckets[bucket_idx].take();
        self.buckets[bucket_idx] = Some(Box::new(Node {
            key,
            value,
            next: old_head,
        }));
        self.len += 1;
        None
    }

    fn get(&self, key: &K) -> Option<&V> {
        let bucket_idx = self.hash_key(key);
        let mut curr = self.buckets[bucket_idx].as_deref();

        while let Some(node) = curr {
            if &node.key == key {
                return Some(&node.value);
            }
            curr = node.next.as_deref();
        }

        None
    }

    fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        let bucket_idx = self.hash_key(key);
        let mut curr = self.buckets[bucket_idx].as_deref_mut();

        while let Some(node) = curr {
            if &node.key == key {
                return Some(&mut node.value);
            }
            curr = node.next.as_deref_mut();
        }

        None
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        let bucket_idx = self.hash_key(key);
        let mut curr = &mut self.buckets[bucket_idx];

        while curr.is_some() {
            if curr.as_ref().unwrap().key == *key {
                let node = curr.take().unwrap();
                *curr = node.next;
                self.len -= 1;
                return Some(node.value);
            }
            curr = &mut curr.as_mut().unwrap().next;
        }

        None
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        for bucket in self.buckets.iter_mut() {
            *bucket = None;
        }
        self.len = 0;
    }
}
