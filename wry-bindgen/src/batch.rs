//! Batching system for grouping multiple JS operations into single messages.
//!
//! This module provides the batching infrastructure that allows multiple
//! JS operations to be grouped together for efficient execution.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::any::Any;
use core::cell::{Ref, RefCell, RefMut};
use std::boxed::Box;

use crate::encode::{BatchableResult, BinaryDecode};
use crate::id_allocator::{IdAllocator, PendingInstallIds};
use crate::ipc::DecodedData;
use crate::ipc::{EncodedData, IPCMessage, MessageType};
use crate::lazy::ThreadLocalKey;
use crate::object_store::ObjectHandle;
use crate::runtime::WryIPC;
use crate::type_cache::TypeCache;
use crate::value::JSIDX_RESERVED;

/// State for batching operations and object storage.
/// Every evaluation is a batch - it may just have one operation.
///
/// Also stores exported Rust structs and callback functions.
pub struct Runtime {
    /// The encoder accumulating batched operations
    encoder: EncodedData,
    /// Allocator for heap and borrow-stack IDs that mirror the JS runtime.
    id_allocator: IdAllocator,
    /// Whether we're inside a batch() call
    is_batching: bool,
    /// Type cache for avoiding resending type definitions to JS.
    type_cache: TypeCache,
    /// Stack of JS-originated IPC requests currently executing Rust code.
    inbound_js_request_stack: Vec<u32>,
    /// Exported Rust structs stored by handle
    objects: BTreeMap<u32, Box<dyn Any>>,
    /// Recently removed object handles for diagnostics.
    removed_objects: BTreeMap<u32, &'static str>,
    /// Rust-owned object handles to drop after the current encoded JS call
    /// has finished executing.
    objects_to_free: Vec<Vec<u32>>,
    /// The ipc layer used to communicate with the JS runtime
    ipc: WryIPC,
    /// The id of the webview this is associated with
    webview_id: u64,
    /// Thread locals associated with the runtime
    thread_locals: BTreeMap<ThreadLocalKey<'static>, Box<dyn Any>>,
}

impl Runtime {
    pub(crate) fn new(ipc: WryIPC, webview_id: u64) -> Self {
        let mut id_allocator = IdAllocator::new();
        let encoder = Self::new_encoder_for_evaluate(&mut id_allocator, 0);
        Self {
            encoder,
            id_allocator,
            is_batching: false,
            type_cache: TypeCache::new(),
            inbound_js_request_stack: Vec::new(),
            // Object store starts empty
            objects: BTreeMap::new(),
            removed_objects: BTreeMap::new(),
            objects_to_free: Vec::new(),
            ipc,
            webview_id,
            thread_locals: BTreeMap::new(),
        }
    }

    fn new_encoder_for_evaluate(
        id_allocator: &mut IdAllocator,
        response_channel_id: u32,
    ) -> EncodedData {
        let mut encoder = EncodedData::new();
        encoder.push_u8(MessageType::Evaluate as u8);
        encoder.push_u32(id_allocator.next_rust_request_id());
        encoder.push_u32(response_channel_id);
        encoder
    }

    /// Record a JS-allocated heap ID from a response.
    pub fn observe_js_heap_id(&mut self, id: u64) {
        self.id_allocator.observe_js_heap_id(id);
    }

    /// Get the next heap ID for a return value placeholder.
    pub fn get_next_placeholder_id(&mut self) -> u64 {
        self.id_allocator.next_placeholder_id()
    }

    /// Allocate an ID for a JS object sent to Rust without an encoded ID.
    pub fn get_next_inbound_js_heap_id(&mut self, request_id: u32) -> u64 {
        self.id_allocator.next_inbound_js_heap_id(request_id)
    }

    pub(crate) fn push_inbound_js_request(&mut self, request_id: u32) {
        self.inbound_js_request_stack.push(request_id);
    }

    pub(crate) fn pop_inbound_js_request(&mut self, request_id: u32) {
        let popped = self
            .inbound_js_request_stack
            .pop()
            .expect("pop_inbound_js_request called with empty stack");
        assert_eq!(
            popped, request_id,
            "inbound JS request stack was popped out of order"
        );
    }

    pub(crate) fn current_response_channel_id(&self) -> u32 {
        self.inbound_js_request_stack.last().copied().unwrap_or(0)
    }

    /// Get the next borrow ID from the borrow stack (indices 1-127).
    /// The borrow stack grows downward from JSIDX_OFFSET (128) toward 1.
    /// Panics if the borrow stack overflows (more than 127 borrowed refs in one operation).
    pub fn get_next_borrow_id(&mut self) -> u64 {
        self.id_allocator.next_borrow_id()
    }

    /// Push a borrow frame before a nested operation that may use borrowed refs.
    /// This saves the current borrow stack pointer so we can restore it later.
    pub fn push_borrow_frame(&mut self) {
        self.id_allocator.push_borrow_frame();
    }

    /// Pop a borrow frame after a nested operation completes.
    /// This restores the borrow stack pointer to where it was before the nested operation.
    pub fn pop_borrow_frame(&mut self) {
        self.id_allocator.pop_borrow_frame();
    }

    /// Track a heap ID as released and queue it for JS drop when appropriate.
    pub fn release_heap_id(&mut self, id: u64) -> Option<u64> {
        self.id_allocator.release_heap_id(id)
    }

    pub fn recycle_heap_id(&mut self, id: u64) {
        self.id_allocator.recycle_heap_id(id);
    }

    /// Take the message data and reset the batch for reuse.
    /// Includes ID installation and placeholder reservation metadata at the start of the message.
    pub(crate) fn take_message(&mut self) -> IPCMessage {
        let install_ids = self.pending_install_id_batches();
        let reserved_ids = self.take_reserved_placeholder_ids();
        let mut encoder = self.take_encoder();
        set_response_channel_id(&mut encoder, self.current_response_channel_id());
        prepend_rust_to_js_evaluate_prelude(&mut encoder, &install_ids, &reserved_ids);
        IPCMessage::new(encoder.to_bytes())
    }

    /// Add Rust-to-JS response metadata and turn the encoder into a response message.
    pub(crate) fn finish_respond_message(&mut self, mut encoder: EncodedData) -> IPCMessage {
        let install_ids = self.pending_install_id_batches();
        set_response_channel_id(&mut encoder, self.current_response_channel_id());
        prepend_rust_to_js_respond_prelude(&mut encoder, &install_ids);
        IPCMessage::new(encoder.to_bytes())
    }

    pub(crate) fn is_empty(&self) -> bool {
        // 12 bytes for offsets, 1 byte for message type, and two u32 header
        // words.
        self.encoder.byte_len() <= 21
    }

    pub(crate) fn push_ids_to_free(&mut self) {
        self.id_allocator.push_ids_to_free();
        self.objects_to_free.push(Vec::new());
    }

    pub(crate) fn pop_and_release_ids(&mut self) -> Vec<u64> {
        self.id_allocator.pop_and_release_ids()
    }

    pub(crate) fn release_object_handle(
        &mut self,
        handle: ObjectHandle,
        reason: &'static str,
    ) -> Option<Box<dyn Any>> {
        match self.objects_to_free.last_mut() {
            Some(handles) => {
                handles.push(handle.raw());
                None
            }
            None => self.remove_object_untyped_with_reason(handle.raw(), reason),
        }
    }

    pub(crate) fn pop_and_release_objects(&mut self) -> Vec<u32> {
        let handles = self
            .objects_to_free
            .pop()
            .expect("pop_and_release_objects called with empty frame stack");

        if let Some(parent) = self.objects_to_free.last_mut() {
            parent.extend(handles);
            Vec::new()
        } else {
            handles
        }
    }

    pub(crate) fn set_batching(&mut self, batching: bool) {
        self.is_batching = batching;
    }

    pub(crate) fn is_batching(&self) -> bool {
        self.is_batching
    }

    /// Get unresolved ID batches JS should install for objects it sent to Rust.
    pub(crate) fn pending_install_id_batches(&self) -> Vec<PendingInstallIds> {
        self.id_allocator.pending_install_id_batches()
    }

    /// Mark deferred JS heap-ref requests as installed by JS.
    pub(crate) fn ack_pending_install_ids(&mut self, ids: impl IntoIterator<Item = u32>) {
        self.id_allocator.ack_pending_install_ids(ids);
    }

    /// Take IDs JS should reserve for pending Rust-to-JS return values.
    pub(crate) fn take_reserved_placeholder_ids(&mut self) -> Vec<u64> {
        self.id_allocator.take_reserved_placeholder_ids()
    }

    pub(crate) fn take_encoder(&mut self) -> EncodedData {
        if self.is_empty() {
            let response_channel_id = self.current_response_channel_id();
            self.encoder =
                Self::new_encoder_for_evaluate(&mut self.id_allocator, response_channel_id);
        }
        let response_channel_id = self.current_response_channel_id();
        let next = Self::new_encoder_for_evaluate(&mut self.id_allocator, response_channel_id);
        core::mem::replace(&mut self.encoder, next)
    }

    pub(crate) fn extend_encoder(&mut self, other: &EncodedData) {
        // Manually extend to avoid adding an extra message type byte or message
        // header.
        self.encoder.u8_buf.extend_from_slice(&other.u8_buf[1..]);
        self.encoder.u32_buf.extend_from_slice(&other.u32_buf[2..]);
        self.encoder.u16_buf.extend_from_slice(&other.u16_buf);
        self.encoder.str_buf.extend_from_slice(&other.str_buf);
    }

    /// Get or create a type ID for the given type definition bytes.
    /// Returns (type_id, is_cached) where is_cached is true if the type was already in the cache.
    pub(crate) fn get_or_create_type_id(&mut self, type_bytes: Vec<u8>) -> (u32, bool) {
        self.type_cache.get_or_create_type_id(type_bytes)
    }

    /// Mark type IDs as available in JS after JS acknowledges parsing them.
    pub(crate) fn ack_type_ids(&mut self, ids: impl IntoIterator<Item = u32>) {
        for id in ids {
            self.type_cache.ack_type_id(id);
        }
    }

    /// Insert an exported object and return its handle.
    pub(crate) fn insert_object<T: 'static>(&mut self, obj: T) -> u32 {
        let handle = self.id_allocator.next_object_handle();
        self.objects.insert(handle, Box::new(RefCell::new(obj)));
        handle
    }

    /// Get a thread-local variable.
    pub(crate) fn take_thread_local<T: 'static>(&mut self, key: ThreadLocalKey<'static>) -> T {
        *self
            .thread_locals
            .remove(&key)
            .expect("thread local not found")
            .downcast::<T>()
            .expect("type mismatch")
    }

    /// Insert a thread-local variable.
    pub(crate) fn insert_thread_local<T: 'static>(
        &mut self,
        key: ThreadLocalKey<'static>,
        value: T,
    ) {
        self.thread_locals.insert(key, Box::new(value));
    }

    /// Check if a thread-local variable exists.
    pub(crate) fn has_thread_local(&self, key: ThreadLocalKey<'static>) -> bool {
        self.thread_locals.contains_key(&key)
    }

    /// Get a reference to an exported object.
    pub(crate) fn get_object<T: 'static>(&self, handle: u32) -> Ref<'_, T> {
        let boxed =
            self.objects
                .get(&handle)
                .unwrap_or_else(|| match self.removed_objects.get(&handle) {
                    Some(reason) => panic!("invalid handle {handle} (removed by {reason})"),
                    None => panic!("invalid handle {handle}"),
                });
        let cell = boxed.downcast_ref::<RefCell<T>>().expect("type mismatch");
        cell.borrow()
    }

    /// Get a mutable reference to an exported object.
    pub(crate) fn get_object_mut<T: 'static>(&self, handle: u32) -> RefMut<'_, T> {
        let boxed = self.objects.get(&handle).expect("invalid handle");
        let cell = boxed.downcast_ref::<RefCell<T>>().expect("type mismatch");
        cell.borrow_mut()
    }

    /// Remove an exported object and return it.
    pub(crate) fn remove_object<T: 'static>(&mut self, handle: u32) -> T {
        let boxed = self.objects.remove(&handle).expect("invalid handle");
        self.removed_objects.insert(handle, "typed remove_object");
        let cell = boxed.downcast::<RefCell<T>>().expect("type mismatch");
        cell.into_inner()
    }

    pub(crate) fn remove_object_untyped_with_reason(
        &mut self,
        handle: u32,
        reason: &'static str,
    ) -> Option<Box<dyn Any>> {
        let object = self.objects.remove(&handle);
        if object.is_some() {
            self.removed_objects.insert(handle, reason);
        }
        object
    }

    /// Get a reference to the IPC layer.
    pub(crate) fn ipc(&self) -> &WryIPC {
        &self.ipc
    }

    /// Get the webview ID associated with this runtime.
    pub(crate) fn webview_id(&self) -> u64 {
        self.webview_id
    }
}

fn push_id_list(buf: &mut Vec<u32>, ids: &[u64]) {
    buf.push(ids.len() as u32);
    for &id in ids {
        buf.push((id & 0xFFFF_FFFF) as u32);
        buf.push((id >> 32) as u32);
    }
}

fn set_response_channel_id(encoder: &mut EncodedData, id: u32) {
    encoder.u32_buf[1] = id;
}

fn push_install_batches(buf: &mut Vec<u32>, batches: &[PendingInstallIds]) {
    buf.push(batches.len() as u32);
    for batch in batches {
        buf.push(batch.request_id);
        push_id_list(buf, &batch.ids);
        push_id_list(buf, &batch.drop_after_install);
    }
}

fn prepend_rust_to_js_respond_prelude(
    encoder: &mut EncodedData,
    install_ids: &[PendingInstallIds],
) {
    let mut prelude = Vec::new();
    push_install_batches(&mut prelude, install_ids);
    encoder.insert_u32s(2, &prelude);
}

fn prepend_rust_to_js_evaluate_prelude(
    encoder: &mut EncodedData,
    install_ids: &[PendingInstallIds],
    reserved_ids: &[u64],
) {
    let mut prelude = Vec::new();
    push_install_batches(&mut prelude, install_ids);
    push_id_list(&mut prelude, reserved_ids);
    encoder.insert_u32s(2, &prelude);
}

thread_local! {
    /// Thread-local runtime state - always exists, reset after each flush
    pub(crate) static RUNTIME: RefCell<Vec<Runtime>> = const { RefCell::new(Vec::new()) };
}

fn push_runtime(runtime: Runtime) {
    RUNTIME.with(|state| {
        state.borrow_mut().push(runtime);
    });
}

fn pop_runtime() -> Runtime {
    RUNTIME.with(|state| {
        state
            .borrow_mut()
            .pop()
            .expect("No runtime available to pop")
    })
}

pub(crate) fn in_runtime<O>(runtime: Runtime, run: impl FnOnce() -> O) -> (Runtime, O) {
    push_runtime(runtime);
    let out = run();
    let runtime = pop_runtime();
    (runtime, out)
}

pub(crate) fn with_runtime<R>(f: impl FnOnce(&mut Runtime) -> R) -> R {
    RUNTIME.with(|state| {
        let mut state = state.borrow_mut();
        f(state.last_mut().expect("No runtime available"))
    })
}

/// Check if we're currently inside a batch() call
pub fn is_batching() -> bool {
    with_runtime(|state| state.is_batching())
}

/// Queue a JS drop operation for a heap ID.
/// This is called when a JsValue is dropped.
pub(crate) fn queue_js_drop(id: u64) {
    debug_assert!(
        id >= JSIDX_RESERVED,
        "Attempted to drop reserved JS heap ID {id}"
    );

    let runtime_already_dropped = match RUNTIME.try_with(|state| {
        state
            .try_borrow()
            .map(|runtime_stack| runtime_stack.is_empty())
    }) {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => return,
        Err(_) => return,
    };
    // If the runtime has already been dropped, we don't need to drop the JS reference
    if runtime_already_dropped {
        return;
    }

    let id = match RUNTIME.try_with(|state| {
        state.try_borrow_mut().ok().and_then(|mut runtime_stack| {
            runtime_stack
                .last_mut()
                .map(|runtime| runtime.release_heap_id(id))
        })
    }) {
        Ok(Some(id)) => id,
        Ok(None) | Err(_) => return,
    };
    if let Some(id) = id {
        crate::js_helpers::js_drop_heap_ref(id);
        let _ = RUNTIME.try_with(|state| {
            let Ok(mut runtime_stack) = state.try_borrow_mut() else {
                return;
            };
            if let Some(runtime) = runtime_stack.last_mut() {
                runtime.recycle_heap_id(id);
            }
        });
    }
}

/// Queue a JS drop for a RustFunction heap ID, disposing the exact JS callable
/// removed from the heap before the ID can be reused.
pub(crate) fn queue_js_dispose_and_drop_rust_function(id: u64) {
    debug_assert!(
        id >= JSIDX_RESERVED,
        "Attempted to drop reserved JS heap ID {id}"
    );

    let runtime_already_dropped = match RUNTIME.try_with(|state| {
        state
            .try_borrow()
            .map(|runtime_stack| runtime_stack.is_empty())
    }) {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => return,
        Err(_) => return,
    };
    if runtime_already_dropped {
        return;
    }

    let id = match RUNTIME.try_with(|state| {
        state.try_borrow_mut().ok().and_then(|mut runtime_stack| {
            runtime_stack
                .last_mut()
                .map(|runtime| runtime.release_heap_id(id))
        })
    }) {
        Ok(Some(id)) => id,
        Ok(None) | Err(_) => return,
    };
    if let Some(id) = id {
        crate::js_helpers::js_dispose_and_drop_rust_function(id);
        let _ = RUNTIME.try_with(|state| {
            let Ok(mut runtime_stack) = state.try_borrow_mut() else {
                return;
            };
            if let Some(runtime) = runtime_stack.last_mut() {
                runtime.recycle_heap_id(id);
            }
        });
    }
}

/// Drop a Rust-owned object now, or after the current encoded JS operation
/// finishes if that object is being passed to JS.
pub(crate) fn queue_rust_object_drop_with_reason(handle: ObjectHandle, reason: &'static str) {
    let object = RUNTIME
        .try_with(|state| {
            state.try_borrow_mut().ok().and_then(|mut runtime_stack| {
                runtime_stack
                    .last_mut()
                    .and_then(|runtime| runtime.release_object_handle(handle, reason))
            })
        })
        .unwrap_or_default();
    drop(object);
}

/// Add an operation to the current batch.
pub(crate) fn add_operation(
    encoder: &mut EncodedData,
    fn_id: u32,
    add_args: impl FnOnce(&mut EncodedData),
) {
    encoder.push_u32(fn_id);
    add_args(encoder);
}

/// Core function for executing JavaScript calls.
///
/// For each call:
/// 1. Encode the current evaluate message into the current batch
/// 2. If the return value is needed immediately, flush the batch and return the result
/// 3. Otherwise get the pending result from BatchableResult
pub(crate) fn run_js_sync<R: BatchableResult>(
    fn_id: u32,
    add_args: impl FnOnce(&mut EncodedData),
) -> R {
    // Step 1: Encode the operation into the batch and get placeholder for non-flush types
    // We take the current encoder out of the thread-local state to avoid borrowing issues
    // and then put it back after adding the operation. Drops or other calls may happen while
    // we are encoding, but they should be queued after this operation.
    let mut batch = with_runtime(|state| {
        // Push a new operation into the batch
        state.push_ids_to_free();
        state.take_encoder()
    });
    add_operation(&mut batch, fn_id, add_args);

    // Check if any encoded argument requires immediate flush (e.g., stack-allocated callbacks)
    let needs_flush = batch.needs_flush;

    with_runtime(|state| {
        let encoded_during_op = core::mem::replace(&mut state.encoder, batch);
        state.extend_encoder(&encoded_during_op);
    });

    // Reserve placeholders before any flush so JS receives exact IDs to fill.
    let mut placeholder = with_runtime(|state| R::try_placeholder(state));

    // Must flush if: not batching, or if the operation requires immediate execution
    // (e.g., stack-allocated callbacks that must be invoked before returning)
    let result = if !is_batching() || needs_flush {
        flush_and_then(move |mut data| {
            let response = placeholder
                .take()
                .unwrap_or_else(|| R::decode(&mut data).expect("Failed to decode return value"));
            assert!(
                data.is_empty(),
                "Extra data remaining after decoding response"
            );
            response
        })
    } else {
        placeholder.unwrap_or_else(|| flush_and_return::<R>())
    };

    // After running, free any queued IDs for this operation
    let ids = with_runtime(|state| state.pop_and_release_ids());
    for id in ids {
        crate::js_helpers::js_drop_heap_ref(id);
        with_runtime(|state| state.recycle_heap_id(id));
    }

    let objects = with_runtime(|state| state.pop_and_release_objects());
    for handle in objects {
        let object = with_runtime(|state| {
            state.remove_object_untyped_with_reason(handle, "queued rust object drop")
        });
        drop(object);
    }

    result
}

/// Flush the current batch and return the decoded result.
pub(crate) fn flush_and_return<R: BinaryDecode>() -> R {
    flush_and_then(|mut data| {
        let response = R::decode(&mut data).expect("Failed to decode return value");
        assert!(
            data.is_empty(),
            "Extra data remaining after decoding response"
        );
        response
    })
}

pub(crate) fn flush_and_then<R>(mut then: impl for<'a> FnMut(DecodedData<'a>) -> R) -> R {
    use crate::runtime::WryBindgenEvent;

    let batch_msg = with_runtime(|state| state.take_message());
    let request_id = batch_msg
        .header()
        .expect("Failed to decode batch message header")
        .request_id;

    // Send and wait for result
    with_runtime(|runtime| {
        (runtime.ipc().proxy)(WryBindgenEvent::ipc(runtime.webview_id(), batch_msg))
    });
    loop {
        if let Some(result) = crate::runtime::progress_js_with(request_id, &mut then) {
            return result;
        }
    }
}

/// Execute operations inside a batch. Operations that return opaque types (like JsValue)
/// will be batched and executed together. Operations that return non-opaque types will
/// flush the batch to get the actual result.
pub fn batch<R, F: FnOnce() -> R>(f: F) -> R {
    let currently_batching = is_batching();
    // Start batching
    with_runtime(|state| state.set_batching(true));

    // Execute the closure
    let result = f();

    if !currently_batching {
        // Flush any remaining batched operations
        force_flush();
    }

    // End batching
    with_runtime(|state| state.set_batching(currently_batching));

    result
}

/// Like `batch`, but async.
pub fn batch_async<'a, R, F: core::future::Future<Output = R> + 'a>(
    f: F,
) -> impl core::future::Future<Output = R> + 'a {
    let mut f = Box::pin(f);
    std::future::poll_fn(move |ctx| batch(|| f.as_mut().poll(ctx)))
}

pub fn force_flush() {
    let has_pending = with_runtime(|state| !state.is_empty());
    if has_pending {
        flush_and_return::<()>();
    }
}
