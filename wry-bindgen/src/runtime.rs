//! Runtime setup and event loop management.
//!
//! This module handles the connection between the Rust runtime and the
//! JavaScript environment via winit's event loop.

use core::pin::Pin;
use std::sync::Arc;

use alloc::boxed::Box;
use async_channel::{Receiver, Sender};
use futures_util::{FutureExt, StreamExt};
use spin::RwLock;

use crate::BinaryDecode;
use crate::batch::with_runtime;
use crate::function::{CALL_EXPORT_FN_ID, DROP_NATIVE_REF_FN_ID, RustCallback};
use crate::ipc::MessageType;
use crate::ipc::{DecodedData, DecodedVariant, InboundIPCMessage, OutboundIPCMessage};
use crate::object_store::ObjectHandle;

/// Application-level events that can be sent through the event loop.
///
/// This enum wraps both IPC messages from JavaScript and control messages
/// from the application (like shutdown requests).
#[derive(Debug, Clone)]
pub struct WryBindgenEvent {
    id: u64,
    event: AppEventVariant,
}

impl WryBindgenEvent {
    /// Get the id of the event
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Create a new IPC event.
    pub(crate) fn ipc(id: u64, msg: OutboundIPCMessage) -> Self {
        Self {
            id,
            event: AppEventVariant::Ipc(msg),
        }
    }

    /// Create a new webview loaded event.
    pub(crate) fn webview_loaded(id: u64) -> Self {
        Self {
            id,
            event: AppEventVariant::WebviewLoaded,
        }
    }

    /// Consume the event and return the inner variant.
    pub(crate) fn into_variant(self) -> AppEventVariant {
        self.event
    }
}

#[derive(Debug, Clone)]
pub(crate) enum AppEventVariant {
    /// An IPC message from JavaScript
    Ipc(OutboundIPCMessage),
    /// The webview has finished loading
    WebviewLoaded,
}

#[derive(Clone)]
pub(crate) struct IPCSenders {
    eval_sender: Sender<InboundIPCMessage>,
    respond_sender: futures_channel::mpsc::UnboundedSender<InboundIPCMessage>,
}

impl IPCSenders {
    pub(crate) fn start_send(&self, msg: InboundIPCMessage) -> bool {
        match msg.message.ty().unwrap() {
            MessageType::Evaluate => self.eval_sender.try_send(msg).is_ok(),
            MessageType::Respond => self.respond_sender.unbounded_send(msg).is_ok(),
        }
    }
}

struct IPCReceivers {
    eval_receiver: Pin<Box<Receiver<InboundIPCMessage>>>,
    respond_receiver: futures_channel::mpsc::UnboundedReceiver<InboundIPCMessage>,
}

impl IPCReceivers {
    pub fn recv_blocking(&mut self) -> Option<InboundIPCMessage> {
        pollster::block_on(async {
            let Self {
                eval_receiver,
                respond_receiver,
            } = self;
            futures_util::select_biased! {
                // We need to always poll the respond receiver first. If the response is ready, quit immediately
                // before running any more callbacks
                respond_msg = respond_receiver.next().fuse() => {
                    respond_msg
                },
                eval_msg = eval_receiver.next().fuse() => {
                    eval_msg
                },
            }
        })
    }
}

/// The runtime environment for communicating with JavaScript.
///
/// This struct holds the event loop proxy for sending messages to the
/// WebView and manages queued Rust calls.
pub(crate) struct WryIPC {
    pub(crate) proxy: Arc<dyn Fn(WryBindgenEvent) + Send + Sync>,
    receivers: RwLock<IPCReceivers>,
}

impl WryIPC {
    /// Create a new runtime with the given event loop proxy.
    pub(crate) fn new(proxy: Arc<dyn Fn(WryBindgenEvent) + Send + Sync>) -> (Self, IPCSenders) {
        let (eval_sender, eval_receiver) = async_channel::unbounded();
        let (respond_sender, respond_receiver) = futures_channel::mpsc::unbounded();
        let senders = IPCSenders {
            eval_sender,
            respond_sender,
        };
        let receivers = RwLock::new(IPCReceivers {
            eval_receiver: Box::pin(eval_receiver),
            respond_receiver,
        });
        let ipc = Self { proxy, receivers };
        (ipc, senders)
    }

    /// Send a response back to JavaScript.
    pub(crate) fn js_response(&self, id: u64, responder: OutboundIPCMessage) {
        (self.proxy)(WryBindgenEvent::ipc(id, responder));
    }
}

pub(crate) fn progress_js_with<O>(
    mut with_respond: impl for<'a> FnMut(DecodedData<'a>) -> O,
) -> Option<O> {
    let response = with_runtime(|runtime| runtime.ipc().receivers.write().recv_blocking())?;
    dispatch_inbound_message(&response, &mut with_respond)
}

pub async fn handle_callbacks() {
    let receiver = with_runtime(|runtime| runtime.ipc().receivers.read().eval_receiver.clone());

    while let Ok(response) = receiver.recv().await {
        dispatch_inbound_message(&response, &mut |_| unreachable!());
    }
}

fn dispatch_inbound_message<O>(
    response: &InboundIPCMessage,
    with_respond: &mut impl for<'a> FnMut(DecodedData<'a>) -> O,
) -> Option<O> {
    let decoder = response
        .message
        .decoded()
        .expect("Failed to decode response");
    match decoder {
        DecodedVariant::Respond { data } => {
            with_runtime(|runtime| {
                // JS has now consumed the Rust→JS Evaluate this Respond
                // closes, so types it carried can be sent as `TYPE_CACHED`
                // from here on.
                runtime.pop_and_ack_type_cache_frame();
            });
            let result = with_respond(data);
            Some(result)
        }
        DecodedVariant::Evaluate { data } => {
            handle_inbound_evaluate(data);
            None
        }
    }
}

fn handle_inbound_evaluate(mut data: DecodedData<'_>) {
    // Mark that we are inside a callback so any Evaluate this callback emits is
    // routed back through the parked JS XHR instead of a fresh top-level
    // `evaluate_script`. The guard restores the depth even if the callback
    // panics.
    let _eval = InboundEvaluateGuard::new();
    handle_rust_callback(&mut data);
}

/// Handle a Rust callback invocation from JavaScript.
fn handle_rust_callback(data: &mut DecodedData) {
    let fn_id = data.take_u32().expect("Failed to read fn_id");
    let response = match fn_id {
        // Call a registered Rust callback
        0 => {
            let key = data.take_u32().unwrap();

            // Clone the Rc while briefly borrowing the batch state, then release the borrow.
            // This allows nested callbacks to access the object store during our callback execution.
            let callback = with_runtime(|state| {
                let rust_callback = state.get_object::<RustCallback>(key);

                rust_callback.clone_rc()
            });

            // Push a borrow frame before calling the callback - nested calls
            // won't clear our borrowed refs. The guard pops the frame even if
            // the callback panics.
            let _frame = BorrowFrameGuard::new();

            let mut encoder = respond_encoder();
            // Call through the cloned Rc (uniform Fn interface)
            (callback)(data, &mut encoder);

            finish_respond_message(encoder)
        }
        // Drop a native Rust object when JS GC'd the wrapper
        DROP_NATIVE_REF_FN_ID => {
            let key = ObjectHandle::decode(data).expect("Failed to decode object handle");

            // The Rust owner may have dropped this closure before JS GC runs.
            crate::object_store::drop_object(key);

            finish_respond_message(respond_encoder())
        }
        // Call an exported Rust struct method
        CALL_EXPORT_FN_ID => {
            // Read the export name
            let export_name: alloc::string::String =
                crate::encode::BinaryDecode::decode(data).expect("Failed to decode export name");

            // Find the export handler
            let export = crate::inventory::iter::<crate::JsExportSpec>()
                .find(|e| e.name == export_name)
                .unwrap_or_else(|| panic!("Unknown export: {export_name}"));

            // Call the handler
            let result = (export.handler)(data);

            assert!(data.is_empty(), "Extra data remaining after export call");

            // Send response
            match result {
                Ok(encoded) => new_respond_message(|encoder| {
                    encoder.extend(&encoded);
                }),
                Err(err) => {
                    panic!("Export call failed: {err}");
                }
            }
        }
        _ => panic!("Unknown Rust callback function ID: {fn_id}"),
    };
    with_runtime(|runtime| runtime.ipc().js_response(runtime.webview_id(), response));
}

/// Scopes a borrow frame for the duration of a callback. The frame is pushed on
/// construction and popped on drop, so it survives a panicking callback.
struct BorrowFrameGuard;

impl BorrowFrameGuard {
    fn new() -> Self {
        with_runtime(|state| state.push_borrow_frame());
        Self
    }
}

impl Drop for BorrowFrameGuard {
    fn drop(&mut self) {
        with_runtime(|state| state.pop_borrow_frame());
    }
}

/// Scopes the inbound-evaluate depth for the duration of a JS→Rust callback. The
/// depth is incremented on construction and decremented on drop.
struct InboundEvaluateGuard;

impl InboundEvaluateGuard {
    fn new() -> Self {
        with_runtime(|state| state.enter_inbound_evaluate());
        Self
    }
}

impl Drop for InboundEvaluateGuard {
    fn drop(&mut self) {
        with_runtime(|state| state.leave_inbound_evaluate());
    }
}

fn respond_encoder() -> crate::ipc::EncodedData {
    let mut encoder = crate::ipc::EncodedData::new();
    encoder.push_u8(MessageType::Respond as u8);
    encoder
}

fn finish_respond_message(encoder: crate::ipc::EncodedData) -> OutboundIPCMessage {
    with_runtime(|runtime| runtime.finish_respond_message(encoder))
}

fn new_respond_message(push_data: impl FnOnce(&mut crate::ipc::EncodedData)) -> OutboundIPCMessage {
    let mut encoder = respond_encoder();
    push_data(&mut encoder);
    finish_respond_message(encoder)
}
