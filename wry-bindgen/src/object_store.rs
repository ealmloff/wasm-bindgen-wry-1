//! Object store for exported Rust structs and callback functions.
//!
//! This module provides the runtime infrastructure for storing Rust objects
//! that are exported to JavaScript. Objects are stored by handle (u32) and
//! can be retrieved, borrowed, and dropped. It also stores callback functions
//! that can be called from JavaScript.

use crate::batch::with_runtime;
use crate::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef};

/// Handle to an exported object in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectHandle(u32);

impl ObjectHandle {
    pub(crate) fn raw(self) -> u32 {
        self.0
    }
}

impl BinaryDecode for ObjectHandle {
    fn decode(decoder: &mut crate::DecodedData) -> Result<Self, crate::DecodeError> {
        let raw = u32::decode(decoder)?;
        Ok(ObjectHandle(raw))
    }
}

impl BinaryEncode for ObjectHandle {
    fn encode(self, encoder: &mut crate::EncodedData) {
        self.0.encode(encoder);
    }
}

impl EncodeTypeDef for ObjectHandle {
    fn encode_type_def(buf: &mut std::vec::Vec<u8>) {
        u32::encode_type_def(buf);
    }
}

impl BatchableResult for ObjectHandle {}

struct CheckedOutObject<T: 'static> {
    handle: ObjectHandle,
    value: Option<T>,
}

impl<T: 'static> CheckedOutObject<T> {
    fn new(handle: ObjectHandle) -> Self {
        let value = with_runtime(|state| state.take_object(handle.0));
        Self {
            handle,
            value: Some(value),
        }
    }

    fn get(&self) -> &T {
        self.value.as_ref().expect("checked-out object missing")
    }

    fn get_mut(&mut self) -> &mut T {
        self.value.as_mut().expect("checked-out object missing")
    }
}

impl<T: 'static> Drop for CheckedOutObject<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            with_runtime(|state| state.reinsert_object(self.handle.0, value));
        }
    }
}

pub fn with_object<T: 'static, R>(handle: ObjectHandle, f: impl FnOnce(&T) -> R) -> R {
    // Run user code after releasing the runtime borrow; destructors may queue
    // JS cleanup through the same runtime.
    let obj = CheckedOutObject::new(handle);
    f(obj.get())
}

pub fn with_object_mut<T: 'static, R>(handle: ObjectHandle, f: impl FnOnce(&mut T) -> R) -> R {
    // Run user code after releasing the runtime borrow; destructors may queue
    // JS cleanup through the same runtime.
    let mut obj = CheckedOutObject::new(handle);
    f(obj.get_mut())
}

pub fn insert_object<T: 'static>(obj: T) -> ObjectHandle {
    with_runtime(|state| ObjectHandle(state.insert_object(obj)))
}

pub fn remove_object<T: 'static>(handle: ObjectHandle) -> T {
    with_runtime(|state| state.remove_object(handle.0))
}

pub fn drop_object(handle: ObjectHandle) -> bool {
    let object = with_runtime(|state| state.remove_object_untyped(handle.0));
    let dropped = object.is_some();
    drop(object);
    dropped
}

/// Create a JavaScript wrapper object for an exported Rust struct.
/// The wrapper is a JS object with methods that call back into Rust via the export specs.
pub fn create_js_wrapper<T: 'static>(handle: ObjectHandle, class_name: &str) -> crate::JsValue {
    // Call into JavaScript to create the wrapper object
    // The JS side will create an object with the appropriate methods
    crate::js_helpers::create_rust_object_wrapper(handle.0, class_name)
}
