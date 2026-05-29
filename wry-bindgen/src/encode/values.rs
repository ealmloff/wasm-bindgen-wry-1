//! JsValue, Closure, and reference encoding implementations.

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::Closure;
use crate::batch::{Runtime, with_runtime};
use crate::ipc::{DecodeError, DecodedData, EncodedData};
use crate::value::JsValue;

use super::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef, TypeTag};

impl EncodeTypeDef for JsValue {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::HeapRef as u8);
    }
}

impl BinaryEncode for JsValue {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self.id());
    }
}

impl BinaryDecode for JsValue {
    fn decode(_decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        // JS always sends heap references without inline IDs: Rust allocates them
        // into the current inbound batch and ships the IDs back in the next
        // outbound message's install-batch list.
        let id = with_runtime(|runtime| runtime.get_next_inbound_js_heap_id());
        Ok(JsValue::from_id(id))
    }
}

impl BatchableResult for JsValue {
    fn try_placeholder(batch: &mut Runtime) -> Option<Self> {
        // Use get_next_placeholder_id() to track reserved slots for JS
        Some(JsValue::from_id(batch.get_next_placeholder_id()))
    }
}

impl<F: ?Sized> BatchableResult for Closure<F> {
    fn try_placeholder(batch: &mut Runtime) -> Option<Self> {
        Some(Closure {
            _phantom: PhantomData,
            callback: crate::closure::CallbackOwnership::None,
            value: JsValue::try_placeholder(batch)?,
        })
    }
}

/// Implement BatchableResult for value types that always need a flush to get the result.
macro_rules! impl_value_type {
    ($($ty:ty),*) => {
        $(impl BatchableResult for $ty {})*
    };
}

impl_value_type!(
    bool, char, u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, isize, usize, f32, f64, String
);

/// Marker trait for types that can be cheaply cloned for encoding.
macro_rules! ref_encode_via_clone {
    ($($ty:ty),* $(,)?) => {
        $(
            impl BinaryEncode for &$ty {
                fn encode(self, encoder: &mut EncodedData) {
                    self.clone().encode(encoder);
                }
            }
        )*
    };
}

ref_encode_via_clone!(
    bool, char, u8, u16, u32, u64, i8, i16, i32, i64, f32, f64, usize, isize, String,
);

macro_rules! slice_encode_via_copy {
    ($($ty:ty),* $(,)?) => {
        $(
            impl BinaryEncode for &[$ty] {
                fn encode(self, encoder: &mut EncodedData) {
                    encoder.push_u32(self.len() as u32);
                    for val in self {
                        (*val).encode(encoder);
                    }
                }
            }

            impl BinaryEncode for &mut [$ty] {
                fn encode(self, encoder: &mut EncodedData) {
                    encoder.push_u32(self.len() as u32);
                    for val in self {
                        (*val).encode(encoder);
                    }
                }
            }
        )*
    };
}

slice_encode_via_copy!(
    bool, char, u8, u16, u32, u64, i8, i16, i32, i64, f32, f64, usize, isize
);

impl<T: crate::convert::JsGeneric> BinaryEncode for &T {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self.as_ref().id());
    }
}
