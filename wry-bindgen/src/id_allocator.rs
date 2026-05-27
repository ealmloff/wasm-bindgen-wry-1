//! ID allocation for JavaScript references and Rust-owned object handles.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::fmt::{Debug, Display};

use crate::value::{JSIDX_OFFSET, JSIDX_RESERVED};

trait SlabId: Copy + Ord + Debug + Display {
    fn next(self) -> Self;
}

impl SlabId for u64 {
    fn next(self) -> Self {
        self.checked_add(1).expect("u64 ID space exhausted")
    }
}

impl SlabId for u32 {
    fn next(self) -> Self {
        self.wrapping_add(1)
    }
}

/// A small ID-only slab.
///
/// IDs move through three states:
/// - unallocated,
/// - live, after `alloc` or `reserve_exact`,
/// - released, after `release` but before `recycle`.
///
/// Keeping `release` and `recycle` separate lets callers delay reuse until the
/// remote side has completed any required cleanup.
struct IdSlab<I> {
    live_ids: BTreeSet<I>,
    free_ids: BTreeSet<I>,
    next_id: I,
    reuse_released_ids: bool,
}

impl<I: SlabId> IdSlab<I> {
    fn new(first_id: I, reuse_released_ids: bool) -> Self {
        Self {
            live_ids: BTreeSet::new(),
            free_ids: BTreeSet::new(),
            next_id: first_id,
            reuse_released_ids,
        }
    }

    fn alloc(&mut self) -> I {
        if self.reuse_released_ids
            && let Some(id) = self.free_ids.iter().next().copied()
        {
            self.free_ids.remove(&id);
            self.mark_live(id);
            return id;
        }

        let id = self.next_id;
        self.next_id = self.next_id.next();
        self.mark_live(id);
        id
    }

    fn reserve_exact(&mut self, id: I) {
        self.free_ids.remove(&id);
        if id >= self.next_id {
            self.next_id = id.next();
        }
        self.mark_live(id);
    }

    fn release(&mut self, id: I) {
        assert!(
            self.live_ids.remove(&id),
            "Attempted to release ID {id}, but it is not live"
        );
    }

    fn recycle(&mut self, id: I) {
        assert!(
            !self.live_ids.contains(&id),
            "Attempted to recycle ID {id}, but it is still live"
        );

        if self.reuse_released_ids {
            self.free_ids.insert(id);
        }
    }

    fn recycle_if_released(&mut self, id: I) -> bool {
        if self.live_ids.contains(&id) {
            return false;
        }

        if self.reuse_released_ids {
            self.free_ids.insert(id);
        }
        true
    }

    #[cfg(test)]
    fn contains(&self, id: I) -> bool {
        self.live_ids.contains(&id)
    }

    #[cfg(test)]
    fn is_reusable(&self, id: I) -> bool {
        self.free_ids.contains(&id)
    }

    fn mark_live(&mut self, id: I) {
        assert!(self.live_ids.insert(id), "ID {id} is already live");
    }
}

#[derive(Clone)]
pub(crate) struct PendingInstallIds {
    pub(crate) request_id: u32,
    pub(crate) ids: Vec<u64>,
    pub(crate) drop_after_install: Vec<u64>,
}

struct HeapIds {
    slab: IdSlab<u64>,
    /// A stack of ongoing function encodings with the ids that need to be freed
    /// after each one is done.
    ids_to_free: Vec<Vec<u64>>,
    /// Heap IDs assigned to objects JS sent to Rust without encoding an ID.
    pending_install_ids: BTreeMap<u32, Vec<u64>>,
    /// Pending install IDs whose Rust owner was dropped before JS acknowledged
    /// installing them.
    pending_install_drops: BTreeMap<u32, Vec<u64>>,
    /// IDs reserved as placeholders for JS function return values.
    reserved_placeholder_ids: Vec<u64>,
}

impl HeapIds {
    fn new() -> Self {
        Self {
            slab: IdSlab::new(JSIDX_RESERVED, true),
            ids_to_free: Vec::new(),
            pending_install_ids: BTreeMap::new(),
            pending_install_drops: BTreeMap::new(),
            reserved_placeholder_ids: Vec::new(),
        }
    }

    fn next_heap_id(&mut self) -> u64 {
        self.slab.alloc()
    }

    fn observe_js_heap_id(&mut self, id: u64) {
        if id >= JSIDX_RESERVED {
            self.slab.reserve_exact(id);
        }
    }

    fn next_placeholder_id(&mut self) -> u64 {
        let id = self.next_heap_id();
        self.reserved_placeholder_ids.push(id);
        id
    }

    fn next_inbound_js_heap_id(&mut self, request_id: u32) -> u64 {
        self.next_inbound_js_heap_ids(request_id, 1)[0]
    }

    fn next_inbound_js_heap_ids(&mut self, request_id: u32, count: u32) -> Vec<u64> {
        let ids = self.pending_install_ids.entry(request_id).or_default();
        assert!(
            ids.is_empty(),
            "Attempted to allocate deferred heap-ref IDs twice for request {request_id}"
        );

        ids.reserve(count as usize);
        for _ in 0..count {
            ids.push(self.slab.alloc());
        }

        ids.clone()
    }

    fn release_heap_id(&mut self, id: u64) -> Option<u64> {
        if id < JSIDX_RESERVED {
            unreachable!("Attempted to release reserved JS heap ID {}", id);
        }

        self.slab.release(id);

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

    fn recycle_heap_id(&mut self, id: u64) {
        if id >= JSIDX_RESERVED {
            self.slab.recycle(id);
        }
    }

    fn recycle_heap_id_if_released(&mut self, id: u64) -> bool {
        id >= JSIDX_RESERVED && self.slab.recycle_if_released(id)
    }

    fn push_ids_to_free(&mut self) {
        self.ids_to_free.push(Vec::new());
    }

    fn pop_and_release_ids(&mut self) -> Vec<u64> {
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

    fn pending_install_id_batches(&self) -> Vec<PendingInstallIds> {
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

    fn ack_pending_install_ids(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.pending_install_ids.remove(&id);
            if let Some(dropped_ids) = self.pending_install_drops.remove(&id) {
                for dropped_id in dropped_ids {
                    self.recycle_heap_id(dropped_id);
                }
            }
        }
    }

    fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.reserved_placeholder_ids)
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

struct BorrowIds {
    /// Borrow stack pointer - uses indices 1-127, growing downward from
    /// JSIDX_OFFSET (128) to 1. Reset after each operation completes.
    stack_pointer: u64,
    /// Frame stack for nested operations - saves borrow stack pointers.
    frame_stack: Vec<u64>,
}

impl BorrowIds {
    fn new() -> Self {
        Self {
            stack_pointer: JSIDX_OFFSET,
            frame_stack: Vec::new(),
        }
    }

    fn next_borrow_id(&mut self) -> u64 {
        if self.stack_pointer <= 1 {
            panic!("Borrow stack overflow: too many borrowed references in a single operation");
        }
        self.stack_pointer -= 1;
        self.stack_pointer
    }

    fn push_frame(&mut self) {
        self.frame_stack.push(self.stack_pointer);
    }

    fn pop_frame(&mut self) {
        if let Some(saved_pointer) = self.frame_stack.pop() {
            self.stack_pointer = saved_pointer;
        } else {
            panic!("pop_borrow_frame called with empty frame stack");
        }
    }
}

struct ObjectHandles {
    slab: IdSlab<u32>,
}

impl ObjectHandles {
    fn new() -> Self {
        Self {
            slab: IdSlab::new(0, false),
        }
    }

    fn next_handle(&mut self) -> u32 {
        self.slab.alloc()
    }

    fn release_handle(&mut self, handle: u32) {
        self.slab.release(handle);
        self.slab.recycle(handle);
    }
}

/// Allocates IDs and handles used by the runtime.
pub(crate) struct IdAllocator {
    heap: HeapIds,
    borrows: BorrowIds,
    objects: ObjectHandles,
    /// Next Rust-originated IPC request ID.
    next_rust_request_id: u32,
}

impl IdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            heap: HeapIds::new(),
            borrows: BorrowIds::new(),
            objects: ObjectHandles::new(),
            next_rust_request_id: 1,
        }
    }

    /// Record a heap ID allocated by JS in a response so future Rust-side
    /// allocations cannot collide with it.
    pub(crate) fn observe_js_heap_id(&mut self, id: u64) {
        self.heap.observe_js_heap_id(id);
    }

    /// Get the next heap ID for a return value placeholder.
    pub(crate) fn next_placeholder_id(&mut self) -> u64 {
        self.heap.next_placeholder_id()
    }

    /// Allocate the Rust-side ID for a JS object sent without an encoded ID.
    pub(crate) fn next_inbound_js_heap_id(&mut self, request_id: u32) -> u64 {
        self.heap.next_inbound_js_heap_id(request_id)
    }

    pub(crate) fn next_inbound_js_heap_ids(&mut self, request_id: u32, count: u32) -> Vec<u64> {
        self.heap.next_inbound_js_heap_ids(request_id, count)
    }

    /// Get the next borrow ID from the borrow stack (indices 1-127).
    ///
    /// The borrow stack grows downward from JSIDX_OFFSET (128) toward 1.
    /// Panics if the borrow stack overflows.
    pub(crate) fn next_borrow_id(&mut self) -> u64 {
        self.borrows.next_borrow_id()
    }

    /// Push a borrow frame before a nested operation that may use borrowed refs.
    pub(crate) fn push_borrow_frame(&mut self) {
        self.borrows.push_frame();
    }

    /// Pop a borrow frame after a nested operation completes.
    pub(crate) fn pop_borrow_frame(&mut self) {
        self.borrows.pop_frame();
    }

    /// Track a heap ID as released and return it if JS should be notified now.
    pub(crate) fn release_heap_id(&mut self, id: u64) -> Option<u64> {
        self.heap.release_heap_id(id)
    }

    pub(crate) fn recycle_heap_id(&mut self, id: u64) {
        self.heap.recycle_heap_id(id);
    }

    pub(crate) fn recycle_heap_id_if_released(&mut self, id: u64) -> bool {
        self.heap.recycle_heap_id_if_released(id)
    }

    pub(crate) fn push_ids_to_free(&mut self) {
        self.heap.push_ids_to_free();
    }

    pub(crate) fn pop_and_release_ids(&mut self) -> Vec<u64> {
        self.heap.pop_and_release_ids()
    }

    /// Get unresolved ID batches JS should install for objects it sent to Rust.
    pub(crate) fn pending_install_id_batches(&self) -> Vec<PendingInstallIds> {
        self.heap.pending_install_id_batches()
    }

    /// Mark deferred JS heap-ref requests as installed by JS.
    pub(crate) fn ack_pending_install_ids(&mut self, ids: impl IntoIterator<Item = u32>) {
        self.heap.ack_pending_install_ids(ids);
    }

    /// Take IDs JS should reserve for pending Rust-to-JS return values.
    pub(crate) fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        self.heap.take_reserved_placeholder_ids()
    }

    /// Allocate an ID for a Rust-originated IPC Evaluate message.
    pub(crate) fn next_rust_request_id(&mut self) -> u32 {
        let id = self.next_rust_request_id;
        self.next_rust_request_id = self.next_rust_request_id.wrapping_add(1).max(1);
        id
    }

    /// Allocate a handle for a Rust-owned exported object.
    pub(crate) fn next_object_handle(&mut self) -> u32 {
        self.objects.next_handle()
    }

    pub(crate) fn release_object_handle(&mut self, handle: u32) {
        self.objects.release_handle(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slab_reuses_recycled_ids_in_order() {
        let mut slab = IdSlab::new(10_u64, true);
        let first = slab.alloc();
        let second = slab.alloc();
        assert_eq!(first, 10);
        assert_eq!(second, 11);

        slab.release(second);
        slab.recycle(second);
        slab.release(first);
        slab.recycle(first);

        assert_eq!(slab.alloc(), 10);
        assert_eq!(slab.alloc(), 11);
    }

    #[test]
    fn reserve_exact_removes_id_from_free_list() {
        let mut slab = IdSlab::new(10_u64, true);
        let id = slab.alloc();
        slab.release(id);
        slab.recycle(id);
        assert!(slab.is_reusable(id));

        slab.reserve_exact(id);
        assert!(slab.contains(id));
        assert!(!slab.is_reusable(id));
        assert_eq!(slab.alloc(), 11);
    }

    #[test]
    fn recycle_if_released_leaves_reobserved_id_live() {
        let mut slab = IdSlab::new(10_u64, true);
        let id = slab.alloc();
        slab.release(id);
        slab.reserve_exact(id);

        assert!(!slab.recycle_if_released(id));
        assert!(slab.contains(id));
        assert!(!slab.is_reusable(id));
    }

    #[test]
    fn heap_ids_start_at_js_reserved_and_reuse_after_recycle() {
        let mut heap = HeapIds::new();
        let id = heap.next_placeholder_id();
        assert_eq!(id, JSIDX_RESERVED);
        assert_eq!(heap.release_heap_id(id), Some(id));
        heap.recycle_heap_id(id);
        assert_eq!(heap.next_placeholder_id(), id);
    }

    #[test]
    fn pending_install_drop_recycles_only_after_ack() {
        let mut heap = HeapIds::new();
        let id = heap.next_inbound_js_heap_id(7);
        assert_eq!(heap.release_heap_id(id), None);

        let next = heap.next_placeholder_id();
        assert_ne!(next, id);

        heap.ack_pending_install_ids([7]);
        assert_eq!(heap.next_placeholder_id(), id);
    }

    #[test]
    fn inbound_heap_ids_are_allocated_as_complete_frames() {
        let mut heap = HeapIds::new();
        let ids = heap.next_inbound_js_heap_ids(7, 2);
        assert_eq!(ids, [JSIDX_RESERVED, JSIDX_RESERVED + 1]);

        let batches = heap.pending_install_id_batches();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].request_id, 7);
        assert_eq!(batches[0].ids, ids);
    }

    #[test]
    fn observe_js_heap_id_reserves_recycled_id() {
        let mut heap = HeapIds::new();
        let id = heap.next_placeholder_id();
        assert_eq!(heap.release_heap_id(id), Some(id));
        heap.recycle_heap_id(id);

        heap.observe_js_heap_id(id);
        assert_ne!(heap.next_placeholder_id(), id);
    }

    #[test]
    fn borrow_frames_restore_stack_pointer() {
        let mut borrows = BorrowIds::new();
        assert_eq!(borrows.next_borrow_id(), JSIDX_OFFSET - 1);
        borrows.push_frame();
        assert_eq!(borrows.next_borrow_id(), JSIDX_OFFSET - 2);
        assert_eq!(borrows.next_borrow_id(), JSIDX_OFFSET - 3);
        borrows.pop_frame();
        assert_eq!(borrows.next_borrow_id(), JSIDX_OFFSET - 2);
    }

    #[test]
    #[should_panic(expected = "Borrow stack overflow")]
    fn borrow_stack_panics_on_overflow() {
        let mut borrows = BorrowIds::new();
        for _ in 0..JSIDX_OFFSET {
            borrows.next_borrow_id();
        }
    }

    #[test]
    fn object_handles_are_not_reused_after_release() {
        let mut handles = ObjectHandles::new();
        let first = handles.next_handle();
        let second = handles.next_handle();
        handles.release_handle(first);
        assert_eq!(handles.next_handle(), second + 1);
    }
}
