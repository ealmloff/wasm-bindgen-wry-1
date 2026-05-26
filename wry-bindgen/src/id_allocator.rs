//! ID allocation for JavaScript references and Rust-owned object handles.

use alloc::vec::Vec;

use crate::value::{JSIDX_OFFSET, JSIDX_RESERVED};

/// Allocates IDs and handles used by the runtime.
pub(crate) struct IdAllocator {
    /// Heap IDs already released by this runtime.
    free_ids: Vec<u64>,
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
    /// Count of IDs reserved as placeholders during the current batch.
    /// This is sent to JS so it can skip these IDs during nested callback allocations.
    reserved_placeholder_count: u32,
    /// Next handle to assign for Rust-owned exported objects.
    next_object_handle: u32,
}

impl IdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            free_ids: Vec::new(),
            // Start allocating heap IDs from JSIDX_RESERVED to match JS heap.
            max_id: JSIDX_RESERVED,
            ids_to_free: Vec::new(),
            // Borrow stack starts at JSIDX_OFFSET (128) and grows downward to 1.
            borrow_stack_pointer: JSIDX_OFFSET,
            borrow_frame_stack: Vec::new(),
            reserved_placeholder_count: 0,
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
        id
    }

    /// Get the next heap ID for a batched return value placeholder.
    pub(crate) fn next_placeholder_id(&mut self, is_batching: bool) -> u64 {
        let id = self.next_heap_id();
        if is_batching {
            self.reserved_placeholder_count += 1;
        }
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

        debug_assert!(
            !self.free_ids.contains(&id) && !self.ids_to_free.iter().any(|ids| ids.contains(&id)),
            "Double-free detected for heap ID {id}"
        );

        match self.ids_to_free.last_mut() {
            Some(ids) => {
                ids.push(id);
                None
            }
            None => {
                self.free_ids.push(id);
                Some(id)
            }
        }
    }

    pub(crate) fn push_ids_to_free(&mut self) {
        self.ids_to_free.push(Vec::new());
    }

    pub(crate) fn pop_and_release_ids(&mut self) -> Vec<u64> {
        let mut to_free = Vec::new();
        if let Some(ids) = self.ids_to_free.pop() {
            for id in ids {
                if let Some(freed_id) = self.release_heap_id(id) {
                    to_free.push(freed_id);
                }
            }
        }
        to_free
    }

    /// Take and reset the reserved placeholder count.
    pub(crate) fn take_reserved_placeholder_count(&mut self) -> u32 {
        core::mem::take(&mut self.reserved_placeholder_count)
    }

    /// Allocate a handle for a Rust-owned exported object.
    pub(crate) fn next_object_handle(&mut self) -> u32 {
        let handle = self.next_object_handle;
        self.next_object_handle = self.next_object_handle.wrapping_add(1);
        handle
    }
}
