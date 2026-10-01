//! Values computed once per key, also when several threads ask for the same key at the same
//! time: the first computes, the others wait for its value instead of computing it again.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

pub(crate) struct OnceMap<K, V>(Mutex<HashMap<K, Arc<OnceLock<V>>>>);

impl<K, V> Default for OnceMap<K, V> {
    fn default() -> OnceMap<K, V> {
        OnceMap(Mutex::new(HashMap::new()))
    }
}

impl<K: Hash + Eq, V: Clone> OnceMap<K, V> {
    /// The value of `key`, computed by `init` on first use.
    pub(crate) fn get_or_init(&self, key: K, init: impl FnOnce() -> V) -> V {
        self.get_or_init_bounded(key, usize::MAX, init)
    }

    /// [`Self::get_or_init`] keeping at most `max` keys (all are dropped when a new key
    /// would exceed it).
    pub(crate) fn get_or_init_bounded(&self, key: K, max: usize, init: impl FnOnce() -> V) -> V {
        let cell = {
            let mut cells = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            if !cells.contains_key(&key) && cells.len() >= max {
                cells.clear();
            }
            cells.entry(key).or_default().clone()
        };
        cell.get_or_init(init).clone()
    }

    /// The cell of `key`; an absent key gets one only when `create` is true.
    pub(crate) fn cell(&self, key: K, create: bool) -> Option<Arc<OnceLock<V>>> {
        let mut cells = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match cells.get(&key) {
            Some(cell) => Some(cell.clone()),
            None if create => Some(cells.entry(key).or_default().clone()),
            None => None,
        }
    }
}
