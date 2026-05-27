//! Cache for Rust function type definitions sent to JavaScript.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

struct TypeCacheEntry {
    id: u32,
    acked_by_js: bool,
}

/// Assigns stable IDs to encoded type definitions for a runtime.
pub(crate) struct TypeCache {
    cache: BTreeMap<Vec<u8>, TypeCacheEntry>,
    types_by_id: BTreeMap<u32, Vec<u8>>,
    next_type_id: u32,
}

impl TypeCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: BTreeMap::new(),
            types_by_id: BTreeMap::new(),
            next_type_id: 0,
        }
    }

    /// Get or create a type ID for the given type definition bytes.
    ///
    /// Returns `(type_id, can_use_cached)`, where `can_use_cached` is true only
    /// after JS has acknowledged parsing a full definition for the ID.
    pub(crate) fn get_or_create_type_id(&mut self, type_bytes: Vec<u8>) -> (u32, bool) {
        if let Some(entry) = self.cache.get(&type_bytes) {
            (entry.id, entry.acked_by_js)
        } else {
            let id = self.next_type_id;
            self.next_type_id += 1;
            self.types_by_id.insert(id, type_bytes.clone());
            self.cache.insert(
                type_bytes,
                TypeCacheEntry {
                    id,
                    acked_by_js: false,
                },
            );
            (id, false)
        }
    }

    pub(crate) fn ack_type_id(&mut self, id: u32) {
        if let Some(type_bytes) = self.types_by_id.get(&id) {
            if let Some(entry) = self.cache.get_mut(type_bytes) {
                entry.acked_by_js = true;
            }
        }
    }
}
