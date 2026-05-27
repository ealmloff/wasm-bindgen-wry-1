//! ID allocation for JavaScript references and Rust-owned object handles.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::value::{JSIDX_OFFSET, JSIDX_RESERVED};

#[derive(Clone)]
pub(crate) struct PendingInstallIds {
    pub(crate) request_id: u32,
    pub(crate) ids: Vec<u64>,
    pub(crate) drop_after_install: Vec<u64>,
}

/// Allocates IDs and handles used by the runtime.
pub(crate) struct IdAllocator {
    /// Heap IDs currently owned by Rust handles.
    live_heap_ids: BTreeSet<u64>,
    /// Next heap ID to allocate.
    max_id: u64,
    /// A stack of ongoing function encodings with the ids that need to be freed
    /// after each one is done.
    ids_to_free: Vec<Vec<u64>>,
    /// Borrow stack pointer - uses indices 1-127, growing downward from
    /// JSIDX_OFFSET (128) to 1. Reset after each operation completes.
    borrow_stack_pointer: u64,
    /// Frame stack for nested operations - saves borrow stack pointers.
    borrow_frame_stack: Vec<u64>,
    /// Heap IDs assigned to objects JS sent to Rust without encoding an ID.
    pending_install_ids: BTreeMap<u32, Vec<u64>>,
    /// Pending install IDs whose Rust owner was dropped before JS acknowledged
    /// installing them.
    pending_install_drops: BTreeMap<u32, Vec<u64>>,
    /// IDs reserved as placeholders for JS function return values.
    reserved_placeholder_ids: Vec<u64>,
    /// Next Rust-originated IPC request ID.
    next_rust_request_id: u32,
    /// Next handle to assign for Rust-owned exported objects.
    next_object_handle: u32,
}

impl IdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            live_heap_ids: BTreeSet::new(),
            // Start allocating heap IDs from JSIDX_RESERVED to match JS heap.
            max_id: JSIDX_RESERVED,
            ids_to_free: Vec::new(),
            // Borrow stack starts at JSIDX_OFFSET (128) and grows downward to 1.
            borrow_stack_pointer: JSIDX_OFFSET,
            borrow_frame_stack: Vec::new(),
            pending_install_ids: BTreeMap::new(),
            pending_install_drops: BTreeMap::new(),
            reserved_placeholder_ids: Vec::new(),
            next_rust_request_id: 1,
            next_object_handle: 0,
        }
    }

    /// Get the next heap ID for placeholder allocation.
    ///
    /// This intentionally advances monotonically to stay in sync with JS heap
    /// allocation.
    pub(crate) fn next_heap_id(&mut self) -> u64 {
        let id = self.max_id;
        self.max_id += 1;
        self.mark_heap_id_live(id);
        id
    }

    /// Record a heap ID allocated by JS in a response so future Rust-side
    /// allocations cannot collide with it.
    pub(crate) fn observe_js_heap_id(&mut self, id: u64) {
        if id >= JSIDX_RESERVED {
            self.max_id = self.max_id.max(id + 1);
            self.mark_heap_id_live(id);
        }
    }

    /// Get the next heap ID for a return value placeholder.
    pub(crate) fn next_placeholder_id(&mut self) -> u64 {
        let id = self.next_heap_id();
        self.reserved_placeholder_ids.push(id);
        id
    }

    /// Allocate the Rust-side ID for a JS object sent without an encoded ID.
    pub(crate) fn next_inbound_js_heap_id(&mut self, request_id: u32) -> u64 {
        let id = self.next_heap_id();
        self.pending_install_ids
            .entry(request_id)
            .or_default()
            .push(id);
        id
    }

    /// Get the next borrow ID from the borrow stack (indices 1-127).
    ///
    /// The borrow stack grows downward from JSIDX_OFFSET (128) toward 1.
    /// Panics if the borrow stack overflows.
    pub(crate) fn next_borrow_id(&mut self) -> u64 {
        if self.borrow_stack_pointer <= 1 {
            panic!("Borrow stack overflow: too many borrowed references in a single operation");
        }
        self.borrow_stack_pointer -= 1;
        self.borrow_stack_pointer
    }

    /// Push a borrow frame before a nested operation that may use borrowed refs.
    pub(crate) fn push_borrow_frame(&mut self) {
        self.borrow_frame_stack.push(self.borrow_stack_pointer);
    }

    /// Pop a borrow frame after a nested operation completes.
    pub(crate) fn pop_borrow_frame(&mut self) {
        if let Some(saved_pointer) = self.borrow_frame_stack.pop() {
            self.borrow_stack_pointer = saved_pointer;
        } else {
            panic!("pop_borrow_frame called with empty frame stack");
        }
    }

    /// Track a heap ID as released and return it if JS should be notified now.
    pub(crate) fn release_heap_id(&mut self, id: u64) -> Option<u64> {
        if id < JSIDX_RESERVED {
            unreachable!("Attempted to release reserved JS heap ID {}", id);
        }

        assert!(
            self.live_heap_ids.remove(&id),
            "Attempted to release heap ID {id}, but it is not live on the Rust side"
        );

        if self.mark_pending_install_dropped(id) {
            return None;
        }

        match self.ids_to_free.last_mut() {
            Some(ids) => {
                ids.push(id);
                None
            }
            None => Some(id),
        }
    }

    pub(crate) fn recycle_heap_id(&mut self, _id: u64) {
        // Heap ID reuse needs an explicit JS-side acknowledgement that no
        // delayed installer or callback can still materialize the old slot.
    }

    pub(crate) fn push_ids_to_free(&mut self) {
        self.ids_to_free.push(Vec::new());
    }

    pub(crate) fn pop_and_release_ids(&mut self) -> Vec<u64> {
        let ids = self
            .ids_to_free
            .pop()
            .expect("pop_and_release_ids called with empty frame stack");

        if let Some(parent) = self.ids_to_free.last_mut() {
            parent.extend(ids);
            Vec::new()
        } else {
            ids
        }
    }

    /// Get unresolved ID batches JS should install for objects it sent to Rust.
    pub(crate) fn pending_install_id_batches(&self) -> Vec<PendingInstallIds> {
        self.pending_install_ids
            .iter()
            .map(|(&request_id, ids)| PendingInstallIds {
                request_id,
                ids: ids.clone(),
                drop_after_install: self
                    .pending_install_drops
                    .get(&request_id)
                    .cloned()
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// Mark deferred JS heap-ref requests as installed by JS.
    pub(crate) fn ack_pending_install_ids(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.pending_install_ids.remove(&id);
            self.pending_install_drops.remove(&id);
        }
    }

    /// Take IDs JS should reserve for pending Rust-to-JS return values.
    pub(crate) fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.reserved_placeholder_ids)
    }

    /// Allocate an ID for a Rust-originated IPC Evaluate message.
    pub(crate) fn next_rust_request_id(&mut self) -> u32 {
        let id = self.next_rust_request_id;
        self.next_rust_request_id = self.next_rust_request_id.wrapping_add(1).max(1);
        id
    }

    /// Allocate a handle for a Rust-owned exported object.
    pub(crate) fn next_object_handle(&mut self) -> u32 {
        let handle = self.next_object_handle;
        self.next_object_handle = self.next_object_handle.wrapping_add(1);
        handle
    }

    fn mark_heap_id_live(&mut self, id: u64) {
        assert!(
            self.live_heap_ids.insert(id),
            "Heap ID {id} is already live on the Rust side"
        );
    }

    fn mark_pending_install_dropped(&mut self, id: u64) -> bool {
        for (&request_id, ids) in &self.pending_install_ids {
            if ids.contains(&id) {
                self.pending_install_drops
                    .entry(request_id)
                    .or_default()
                    .push(id);
                return true;
            }
        }

        false
    }
}
