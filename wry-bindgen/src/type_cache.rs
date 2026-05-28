//! Cache for Rust function type definitions sent to JavaScript.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

struct TypeCacheEntry {
    id: u32,
    acked_by_js: bool,
}

/// Assigns stable IDs to encoded type definitions for a runtime.
pub(crate) struct TypeCache {
    cache: BTreeMap<Vec<u8>, TypeCacheEntry>,
    types_by_id: BTreeMap<u32, Vec<u8>>,
    pending_type_ids_by_request: BTreeMap<u32, BTreeSet<u32>>,
    next_type_id: u32,
}

impl TypeCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: BTreeMap::new(),
            types_by_id: BTreeMap::new(),
            pending_type_ids_by_request: BTreeMap::new(),
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

    pub(crate) fn register_type_id_for_request(&mut self, request_id: u32, type_id: u32) {
        self.pending_type_ids_by_request
            .entry(request_id)
            .or_default()
            .insert(type_id);
    }

    pub(crate) fn ack_request(&mut self, request_id: u32) {
        let Some(type_ids) = self.pending_type_ids_by_request.remove(&request_id) else {
            return;
        };

        for type_id in type_ids {
            self.ack_type_id(type_id);
        }
    }

    pub(crate) fn move_pending_request(&mut self, from_request_id: u32, to_request_id: u32) {
        if from_request_id == to_request_id {
            return;
        }

        let Some(type_ids) = self.pending_type_ids_by_request.remove(&from_request_id) else {
            return;
        };

        self.pending_type_ids_by_request
            .entry(to_request_id)
            .or_default()
            .extend(type_ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_bytes(tag: u8) -> Vec<u8> {
        vec![tag]
    }

    #[test]
    fn type_id_is_cached_after_registered_request_is_acked() {
        let mut cache = TypeCache::new();
        let bytes = type_bytes(1);

        let (id, cached) = cache.get_or_create_type_id(bytes.clone());
        assert_eq!(id, 0);
        assert!(!cached);

        cache.register_type_id_for_request(7, id);
        assert_eq!(cache.get_or_create_type_id(bytes.clone()), (id, false));

        cache.ack_request(7);
        assert_eq!(cache.get_or_create_type_id(bytes), (id, true));
    }

    #[test]
    fn request_ack_only_caches_types_registered_to_that_request() {
        let mut cache = TypeCache::new();
        let first = type_bytes(1);
        let second = type_bytes(2);

        let (first_id, _) = cache.get_or_create_type_id(first.clone());
        let (second_id, _) = cache.get_or_create_type_id(second.clone());
        cache.register_type_id_for_request(7, first_id);
        cache.register_type_id_for_request(8, second_id);

        cache.ack_request(7);

        assert_eq!(cache.get_or_create_type_id(first), (first_id, true));
        assert_eq!(cache.get_or_create_type_id(second), (second_id, false));
    }

    #[test]
    fn one_acked_request_caches_type_sent_by_multiple_requests() {
        let mut cache = TypeCache::new();
        let bytes = type_bytes(1);
        let (id, _) = cache.get_or_create_type_id(bytes.clone());

        cache.register_type_id_for_request(7, id);
        cache.register_type_id_for_request(8, id);
        cache.ack_request(8);

        assert_eq!(cache.get_or_create_type_id(bytes), (id, true));
    }

    #[test]
    fn moving_pending_request_preserves_type_ack() {
        let mut cache = TypeCache::new();
        let bytes = type_bytes(1);
        let (id, _) = cache.get_or_create_type_id(bytes.clone());

        cache.register_type_id_for_request(8, id);
        cache.move_pending_request(8, 7);
        cache.ack_request(7);

        assert_eq!(cache.get_or_create_type_id(bytes), (id, true));
    }
}
