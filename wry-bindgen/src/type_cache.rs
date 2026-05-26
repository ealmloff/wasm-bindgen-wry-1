//! Cache for Rust function type definitions sent to JavaScript.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Assigns stable IDs to encoded type definitions for a runtime.
pub(crate) struct TypeCache {
    cache: BTreeMap<Vec<u8>, u32>,
    next_type_id: u32,
}

impl TypeCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: BTreeMap::new(),
            next_type_id: 0,
        }
    }

    /// Get or create a type ID for the given type definition bytes.
    ///
    /// Returns `(type_id, is_cached)`, where `is_cached` is true if the type was
    /// already in the cache.
    pub(crate) fn get_or_create_type_id(&mut self, type_bytes: Vec<u8>) -> (u32, bool) {
        if let Some(&id) = self.cache.get(&type_bytes) {
            (id, true)
        } else {
            let id = self.next_type_id;
            self.next_type_id += 1;
            self.cache.insert(type_bytes, id);
            (id, false)
        }
    }
}
