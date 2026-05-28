//! ID allocation for JavaScript references and Rust-owned object handles.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::value::{JSIDX_OFFSET, JSIDX_RESERVED};

/// A small ID-only slab.
///
/// IDs move through three states:
/// - unallocated,
/// - live, after `alloc` or `reserve_exact`,
/// - released, after `release` but before `recycle`.
///
/// Keeping `release` and `recycle` separate lets callers delay reuse until the
/// remote side has completed any required cleanup.
struct IdSlab {
    live_ids: BTreeSet<u64>,
    free_ids: BTreeSet<u64>,
    next_id: u64,
}

impl IdSlab {
    fn new(first_id: u64) -> Self {
        Self {
            live_ids: BTreeSet::new(),
            free_ids: BTreeSet::new(),
            next_id: first_id,
        }
    }

    fn alloc(&mut self) -> u64 {
        if let Some(id) = self.free_ids.iter().next().copied() {
            self.free_ids.remove(&id);
            self.mark_live(id);
            return id;
        }

        let id = self.next_id;
        self.next_id = next_heap_id_after(self.next_id);
        self.mark_live(id);
        id
    }

    fn reserve_exact(&mut self, id: u64) {
        self.free_ids.remove(&id);
        if id >= self.next_id {
            self.next_id = next_heap_id_after(id);
        }
        self.mark_live(id);
    }

    fn release(&mut self, id: u64) {
        assert!(
            self.live_ids.remove(&id),
            "Attempted to release ID {id}, but it is not live"
        );
    }

    fn recycle(&mut self, id: u64) {
        assert!(
            !self.live_ids.contains(&id),
            "Attempted to recycle ID {id}, but it is still live"
        );

        self.free_ids.insert(id);
    }

    fn recycle_if_released(&mut self, id: u64) -> bool {
        if self.live_ids.contains(&id) {
            return false;
        }

        self.free_ids.insert(id);
        true
    }

    #[cfg(test)]
    fn contains(&self, id: u64) -> bool {
        self.live_ids.contains(&id)
    }

    #[cfg(test)]
    fn is_reusable(&self, id: u64) -> bool {
        self.free_ids.contains(&id)
    }

    fn mark_live(&mut self, id: u64) {
        assert!(self.live_ids.insert(id), "ID {id} is already live");
    }
}

fn next_heap_id_after(id: u64) -> u64 {
    id.checked_add(1).expect("u64 ID space exhausted")
}

/// Heap IDs Rust allocated for values JS sent without encoding an ID. Sent
/// back to JS so it can install them into its heap at those slots.
pub(crate) type InstallIdBatch = Vec<u64>;

struct HeapIds {
    slab: IdSlab,
    /// IDs allocated while decoding the current inbound message, awaiting the
    /// next outbound's install prelude. Every outbound drains this via
    /// `take_pending_install_ids`, and the protocol decodes exactly one inbound
    /// between consecutive outbounds, so at each drain this holds exactly that
    /// one message's IDs — no per-message framing is needed.
    pending_install_ids: Vec<u64>,
    /// IDs reserved as placeholders for JS function return values.
    reserved_placeholder_ids: Vec<u64>,
}

impl HeapIds {
    fn new() -> Self {
        Self {
            slab: IdSlab::new(JSIDX_RESERVED),
            pending_install_ids: Vec::new(),
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

    fn next_inbound_js_heap_id(&mut self) -> u64 {
        let id = self.slab.alloc();
        self.pending_install_ids.push(id);
        id
    }

    fn release_heap_slot(&mut self, id: u64) {
        if id < JSIDX_RESERVED {
            unreachable!("Attempted to release reserved JS heap ID {}", id);
        }

        self.slab.release(id);
    }

    fn recycle_heap_id(&mut self, id: u64) {
        if id >= JSIDX_RESERVED {
            self.slab.recycle(id);
        }
    }

    fn recycle_heap_id_if_released(&mut self, id: u64) -> bool {
        id >= JSIDX_RESERVED && self.slab.recycle_if_released(id)
    }

    fn take_pending_install_ids(&mut self) -> InstallIdBatch {
        core::mem::take(&mut self.pending_install_ids)
    }

    fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        core::mem::take(&mut self.reserved_placeholder_ids)
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
    live_handles: BTreeSet<u32>,
    free_handles: BTreeSet<u32>,
    next_handle: u32,
}

impl ObjectHandles {
    fn new() -> Self {
        Self {
            live_handles: BTreeSet::new(),
            free_handles: BTreeSet::new(),
            next_handle: 1,
        }
    }

    fn next_handle(&mut self) -> u32 {
        if let Some(handle) = self.free_handles.iter().next().copied() {
            self.free_handles.remove(&handle);
            self.mark_live(handle);
            return handle;
        }

        let handle = self.next_handle;
        self.next_handle = next_object_handle_after(self.next_handle);
        self.mark_live(handle);
        handle
    }

    fn release_handle(&mut self, handle: u32) {
        assert!(
            self.live_handles.remove(&handle),
            "Attempted to release object handle {handle}, but it is not live"
        );
        self.free_handles.insert(handle);
    }

    fn mark_live(&mut self, handle: u32) {
        assert!(
            self.live_handles.insert(handle),
            "Object handle {handle} is already live"
        );
    }
}

fn next_object_handle_after(handle: u32) -> u32 {
    handle
        .checked_add(1)
        .expect("u32 object handle space exhausted")
}

/// Allocates IDs and handles used by the runtime.
pub(crate) struct IdAllocator {
    heap: HeapIds,
    borrows: BorrowIds,
    objects: ObjectHandles,
}

impl IdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            heap: HeapIds::new(),
            borrows: BorrowIds::new(),
            objects: ObjectHandles::new(),
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

    pub(crate) fn next_inbound_js_heap_id(&mut self) -> u64 {
        self.heap.next_inbound_js_heap_id()
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

    /// Release a heap ID's slab slot. Whether JS should be notified now is
    /// decided by the runtime's operation-free batching, not here.
    pub(crate) fn release_heap_slot(&mut self, id: u64) {
        self.heap.release_heap_slot(id);
    }

    pub(crate) fn recycle_heap_id(&mut self, id: u64) {
        self.heap.recycle_heap_id(id);
    }

    pub(crate) fn recycle_heap_id_if_released(&mut self, id: u64) -> bool {
        self.heap.recycle_heap_id_if_released(id)
    }

    /// Take the IDs JS should install for objects it sent to Rust. Empty when
    /// the last inbound message carried no heap refs.
    pub(crate) fn take_pending_install_ids(&mut self) -> InstallIdBatch {
        self.heap.take_pending_install_ids()
    }

    /// Take IDs JS should reserve for pending Rust-to-JS return values.
    pub(crate) fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        self.heap.take_reserved_placeholder_ids()
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
        let mut slab = IdSlab::new(10_u64);
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
        let mut slab = IdSlab::new(10_u64);
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
        let mut slab = IdSlab::new(10_u64);
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
        heap.release_heap_slot(id);
        heap.recycle_heap_id(id);
        assert_eq!(heap.next_placeholder_id(), id);
    }

    #[test]
    fn inbound_drop_uses_normal_release_path() {
        let mut heap = HeapIds::new();
        let id = heap.next_inbound_js_heap_id();
        heap.release_heap_slot(id);

        let next = heap.next_placeholder_id();
        assert_ne!(next, id);

        let batch = heap.take_pending_install_ids();
        assert_eq!(batch, vec![id]);

        heap.recycle_heap_id(id);
        assert_eq!(heap.next_placeholder_id(), id);
    }

    #[test]
    fn inbound_heap_ids_collapse_into_one_install_batch() {
        let mut heap = HeapIds::new();
        let ids = [
            heap.next_inbound_js_heap_id(),
            heap.next_inbound_js_heap_id(),
        ];
        assert_eq!(ids, [JSIDX_RESERVED, JSIDX_RESERVED + 1]);

        let batch = heap.take_pending_install_ids();
        assert_eq!(batch, ids);
        assert!(heap.take_pending_install_ids().is_empty());
    }

    #[test]
    fn taking_with_no_inbound_ids_is_empty() {
        let mut heap = HeapIds::new();
        assert!(heap.take_pending_install_ids().is_empty());
    }

    #[test]
    fn each_take_drains_only_ids_since_the_last_take() {
        let mut heap = HeapIds::new();
        let first = heap.next_inbound_js_heap_id();
        assert_eq!(heap.take_pending_install_ids(), vec![first]);

        let second = heap.next_inbound_js_heap_id();
        assert_eq!(heap.take_pending_install_ids(), vec![second]);
    }

    #[test]
    fn observe_js_heap_id_reserves_recycled_id() {
        let mut heap = HeapIds::new();
        let id = heap.next_placeholder_id();
        heap.release_heap_slot(id);
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
    fn object_handles_reuse_released_handles() {
        let mut handles = ObjectHandles::new();
        let first = handles.next_handle();
        let second = handles.next_handle();
        assert_eq!(first, 1);
        handles.release_handle(first);
        assert_eq!(handles.next_handle(), first);
        assert_eq!(second, first + 1);
    }

    #[test]
    #[should_panic(expected = "u32 object handle space exhausted")]
    fn object_handles_panic_on_overflow() {
        let mut handles = ObjectHandles {
            live_handles: BTreeSet::new(),
            free_handles: BTreeSet::new(),
            next_handle: u32::MAX,
        };

        handles.next_handle();
    }
}
