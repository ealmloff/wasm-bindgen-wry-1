//! wry-bindgen - Runtime support for wasm-bindgen-style bindings over Wry's WebView
//!
//! This crate provides the runtime types and traits needed for the `#[wasm_bindgen]`
//! attribute macro to generate code that works with Wry's IPC protocol.
//!
//! # Architecture
//!
//! The crate is organized into several modules:
//!
//! - [`encode`] - Core encoding/decoding traits for Rust types
//! - [`function`] - JSFunction type for calling JavaScript functions
//! - [`mod@batch`] - Batching system for grouping multiple JS operations
//! - [`runtime`] - Event loop and runtime management

#![no_std]

pub extern crate alloc;
#[macro_use]
extern crate std;

pub mod batch;
mod cast;
pub mod convert;
pub mod encode;
pub mod function;
mod function_registry;
mod id_allocator;
mod intern;
pub(crate) mod ipc;
mod js_helpers;
mod lazy;
#[doc(hidden)]
pub mod object_store;
pub mod runtime;
mod type_cache;
mod value;
pub mod wry;

pub use intern::*;

/// Re-export of the Closure type for wasm-bindgen API compatibility.
/// Allows `use wasm_bindgen::closure::Closure;`
pub mod closure {
    pub use crate::Closure;
    pub use crate::ScopedClosure;
    pub use crate::WasmClosure;
}

/// Runtime module for wasm-bindgen compatibility.
/// This module provides the wbg_cast function used for type casting.
pub mod __rt {
    use crate::{
        __wry_submit_js_function, JsValue, LazyJsFunction,
        encode::{BatchableResult, BinaryEncode, EncodeTypeDef},
    };

    pub mod marker {
        /// Marker for types whose generic parameters erase to one stable runtime representation.
        ///
        /// # Safety
        /// Implementors must have the same runtime representation as `Repr`.
        pub unsafe trait ErasableGeneric {
            type Repr: 'static;
        }

        unsafe impl<T: ErasableGeneric> ErasableGeneric for &T {
            type Repr = &'static T::Repr;
        }

        unsafe impl<T: ErasableGeneric> ErasableGeneric for &mut T {
            type Repr = &'static mut T::Repr;
        }
    }

    /// Cast between types via the binary protocol.
    ///
    /// This is the wry-bindgen equivalent of wasm-bindgen's wbg_cast.
    /// It encodes `value` using From's BinaryEncode, sends to JS as identity,
    /// and decodes the result using To's BinaryDecode.
    #[inline]
    pub fn wbg_cast<From, To>(value: From) -> To
    where
        From: BinaryEncode + EncodeTypeDef,
        To: BatchableResult + EncodeTypeDef,
    {
        let func: LazyJsFunction<fn(From) -> To> = __wry_submit_js_function!("(a0) => a0");
        func.call(value)
    }

    /// Convert a panic value into a JsValue error.
    ///
    /// This is used by wasm-bindgen-futures to convert Rust panics into JS errors.
    #[cfg(feature = "std")]
    pub fn panic_to_panic_error(val: std::boxed::Box<dyn std::any::Any + Send>) -> JsValue {
        let maybe_panic_msg: Option<&str> = if let Some(s) = val.downcast_ref::<&str>() {
            Some(s)
        } else if let Some(s) = val.downcast_ref::<std::string::String>() {
            Some(s)
        } else {
            None
        };
        // Create an Error object with the panic message
        JsValue::from_str(maybe_panic_msg.unwrap_or("Rust panic"))
    }
}

macro_rules! cast {
    (($from:ty => $to:ty) $val:expr) => {{ $crate::__rt::wbg_cast::<$from, $to>($val) }};
}

macro_rules! to_js_value {
    ($ty:ty) => {
        impl From<$ty> for $crate::JsValue {
            fn from(val: $ty) -> Self {
                cast! {($ty => $crate::JsValue) val}
            }
        }
    };
}

macro_rules! from_js_value {
    ($ty:ty) => {
        impl From<$crate::JsValue> for $ty {
            fn from(val: $crate::JsValue) -> Self {
                cast! {($crate::JsValue => $ty) val}
            }
        }
    };
}

impl TryFrom<JsValue> for u64 {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        eprintln!("TryFrom<JsValue> for u64 is likely wrong");
        #[wasm_bindgen(crate = crate, inline_js = "export function BigIntAsU64(val) {
            if (typeof val !== 'bigint') {
                throw new Error('Value is not a BigInt');
            }
            return Number(val);
        }")]
        extern "C" {
            #[wasm_bindgen(js_name = "BigIntAsU64")]
            fn big_int_as_u64(val: &JsValue) -> Result<u64, JsValue>;
        }

        big_int_as_u64(&value)
    }
}

impl TryFrom<JsValue> for i64 {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        eprintln!("TryFrom<JsValue> for u64 is likely wrong");
        #[wasm_bindgen(crate = crate, inline_js = "export function BigIntAsU64(val) {
            if (typeof val !== 'bigint') {
                throw new Error('Value is not a BigInt');
            }
            return Number(val);
        }")]
        extern "C" {
            #[wasm_bindgen(js_name = "BigIntAsU64")]
            fn big_int_as_i64(val: &JsValue) -> Result<i64, JsValue>;
        }

        big_int_as_i64(&value)
    }
}

impl TryFrom<JsValue> for f64 {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        value.as_f64().ok_or(value)
    }
}

impl TryFrom<&JsValue> for f64 {
    type Error = JsValue;

    fn try_from(value: &JsValue) -> Result<Self, Self::Error> {
        value.as_f64().ok_or_else(|| value.clone())
    }
}

impl TryFrom<JsValue> for i128 {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        #[wasm_bindgen(crate = crate, inline_js = "export function BigIntAsI128(val) {
            if (typeof val !== 'bigint') {
                throw new Error('Value is not a BigInt');
            }
            return Number(val);
        }")]
        extern "C" {
            #[wasm_bindgen(js_name = "BigIntAsI128")]
            fn big_int_as_i128(val: &JsValue) -> Result<i128, JsValue>;
        }

        big_int_as_i128(&value)
    }
}

impl TryFrom<JsValue> for u128 {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        #[wasm_bindgen(crate = crate, inline_js = "export function BigIntAsU128(val) {
            if (typeof val !== 'bigint') {
                throw new Error('Value is not a BigInt');
            }
            if (val < 0n) {
                throw new Error('Value is negative');
            }
            return Number(val);
        }")]
        extern "C" {
            #[wasm_bindgen(js_name = "BigIntAsU128")]
            fn big_int_as_u128(val: &JsValue) -> Result<u128, JsValue>;
        }

        big_int_as_u128(&value)
    }
}

impl TryFrom<JsValue> for String {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        value.as_string().ok_or(value)
    }
}

to_js_value!(i8);
from_js_value!(i8);
to_js_value!(i16);
from_js_value!(i16);
to_js_value!(i32);
from_js_value!(i32);
to_js_value!(i64);
to_js_value!(i128);
to_js_value!(u8);
from_js_value!(u8);
to_js_value!(u16);
from_js_value!(u16);
to_js_value!(u32);
from_js_value!(u32);
to_js_value!(u64);
to_js_value!(u128);
to_js_value!(f32);
from_js_value!(f32);
to_js_value!(f64);
to_js_value!(usize);
from_js_value!(usize);
to_js_value!(isize);
from_js_value!(isize);
impl From<&str> for JsValue {
    fn from(val: &str) -> Self {
        cast! {(String => JsValue) val.to_string()}
    }
}
impl From<&String> for JsValue {
    fn from(val: &String) -> Self {
        cast! {(String => JsValue) val.clone()}
    }
}
to_js_value!(String);
to_js_value!(());
from_js_value!(());

/// Borrowed or owned closure handle for passing Rust closures to JavaScript.
pub struct ScopedClosure<'a, T: ?Sized> {
    // careful: must be Box<T> not just T because unsized PhantomData
    // seems to have weird interaction with Pin<>
    _phantom: core::marker::PhantomData<(&'a (), Box<T>)>,
    rust_callback: Option<object_store::ObjectHandle>,
    drop_rust_callback_on_drop: bool,
    pub(crate) value: JsValue,
}

/// Owned closure handle. This follows upstream wasm-bindgen's alias direction.
pub type Closure<T> = ScopedClosure<'static, T>;

impl<T: ?Sized> ScopedClosure<'static, T> {
    pub fn new<M, F: IntoClosure<M, Self>>(f: F) -> Self {
        f.into_closure()
    }

    /// Create a `Closure` from a function that can only be called once.
    ///
    /// Since we have no way of enforcing that JS cannot attempt to call this
    /// `FnOnce` more than once, this produces a `Closure<dyn FnMut(A...) -> R>`
    /// that will panic if called more than once.
    pub fn once<F, M>(fn_once: F) -> Closure<T>
    where
        F: WasmClosureFnOnce<T, M>,
    {
        let mut closure = fn_once.into_closure();
        closure.drop_rust_callback_on_drop = false;
        closure
    }

    /// Wrap a raw closure. Only for use by generated code.
    pub(crate) fn wrap_encode_decode<FnPtr>(
        encode_decode: impl Fn(&mut DecodedData, &mut EncodedData) + 'static,
    ) -> Self
    where
        CallbackKey<FnPtr>: BinaryEncode + EncodeTypeDef,
    {
        let key = insert_object(RustCallback::new_fn(encode_decode));
        let value =
            crate::__rt::wbg_cast::<CallbackKey<FnPtr>, crate::JsValue>(CallbackKey::new(key));
        Self {
            _phantom: core::marker::PhantomData,
            rust_callback: Some(key),
            drop_rust_callback_on_drop: true,
            value,
        }
    }

    /// Wrap a raw closure. Only for use by generated code.
    pub(crate) fn wrap_encode_decode_mut<FnPtr>(
        encode_decode: impl FnMut(&mut DecodedData, &mut EncodedData) + 'static,
    ) -> Self
    where
        CallbackKey<FnPtr>: BinaryEncode + EncodeTypeDef,
    {
        let key = insert_object(RustCallback::new_fn_mut(encode_decode));
        let value =
            crate::__rt::wbg_cast::<CallbackKey<FnPtr>, crate::JsValue>(CallbackKey::new(key));
        Self {
            _phantom: core::marker::PhantomData,
            rust_callback: Some(key),
            drop_rust_callback_on_drop: true,
            value,
        }
    }

    /// Wrap a raw one-shot closure. Only for use by generated code.
    pub(crate) fn wrap_once_encode_decode_mut<FnPtr>(
        mut encode_decode: impl FnMut(&mut DecodedData, &mut EncodedData) + 'static,
    ) -> Self
    where
        CallbackKey<FnPtr>: BinaryEncode + EncodeTypeDef,
    {
        let handle_cell = alloc::rc::Rc::new(core::cell::Cell::new(None));
        let handle_for_callback = handle_cell.clone();
        let key = insert_object(RustCallback::new_fn_mut(move |decoder, encoder| {
            encode_decode(decoder, encoder);
            if let Some(handle) = handle_for_callback.take() {
                crate::batch::queue_rust_object_drop_with_reason(
                    handle,
                    "once callback after call",
                );
            }
        }));
        handle_cell.set(Some(key));
        let value = crate::__rt::wbg_cast::<CallbackKey<FnPtr>, crate::JsValue>(
            CallbackKey::new_with_policy(key, crate::encode::CallbackPolicy::JsOwnedOnce),
        );
        Self {
            _phantom: core::marker::PhantomData,
            rust_callback: Some(key),
            drop_rust_callback_on_drop: false,
            value,
        }
    }
}

impl<'a, T: ?Sized> ScopedClosure<'a, T> {
    /// Forgets the closure, leaking it.
    pub fn forget(self) {
        core::mem::forget(self);
    }

    /// Returns the JavaScript function value for this closure.
    pub fn as_js_value(&self) -> &JsValue {
        &self.value
    }
}

impl<T: ?Sized> Drop for ScopedClosure<'_, T> {
    fn drop(&mut self) {
        if self.drop_rust_callback_on_drop && self.rust_callback.take().is_some() {
            crate::batch::queue_js_dispose_and_drop_rust_function(self.value.id());
            self.value.idx = crate::value::JSIDX_UNDEFINED;
        }
    }
}

/// A trait for converting an `FnOnce(A...) -> R` into a `Closure<dyn FnMut(A...) -> R>`.
#[doc(hidden)]
pub trait WasmClosureFnOnce<T: ?Sized, M>: Sized + 'static {
    fn into_closure(self) -> Closure<T>;
}

impl<T: ?Sized> AsRef<JsValue> for ScopedClosure<'_, T> {
    fn as_ref(&self) -> &JsValue {
        &self.value
    }
}

impl<T: ?Sized> core::fmt::Debug for ScopedClosure<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Closure")
            .field("value", &self.value)
            .finish()
    }
}

/// Upstream-compatible marker trait for closure signatures.
pub trait WasmClosure {
    /// The `'static` version of this closure type.
    type Static: ?Sized;
    /// The mutable version of this closure type.
    type AsMut: ?Sized;
}

/// Internal trait for closure types that can be wrapped and passed to JavaScript.
pub trait WryWasmClosure<M> {
    /// Create a Closure from a boxed closure.
    fn into_js_closure(boxed: Box<Self>) -> Closure<Self>;
}

impl<T: ?Sized> ScopedClosure<'static, T> {
    /// Wrap a boxed closure to create a `Closure`.
    ///
    /// This is the classic wasm-bindgen API for creating closures from boxed trait objects.
    pub fn wrap<M>(data: Box<T>) -> Closure<T>
    where
        T: WryWasmClosure<M>,
    {
        T::into_js_closure(data)
    }

    /// Converts the `Closure` into a `JsValue`.
    pub fn into_js_value(self) -> JsValue {
        let value = core::mem::ManuallyDrop::new(self);
        // Clone the value to get ownership without triggering drop
        value.value.clone()
    }

    /// Create a `Closure` from a function that can only be called once,
    /// and return the underlying `JsValue` directly.
    ///
    /// This is a convenience method that combines `once` and `into_js_value`.
    pub fn once_into_js<F, M>(fn_once: F) -> JsValue
    where
        F: WasmClosureFnOnce<T, M>,
    {
        Closure::once(fn_once).into_js_value()
    }
}

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use core::ops::{Deref, DerefMut};
// Re-export core types
pub use cast::JsCast;
pub use lazy::JsThreadLocal;
pub use value::JsValue;

unsafe impl __rt::marker::ErasableGeneric for JsValue {
    type Repr = JsValue;
}

/// A wrapper type around slices and vectors for binding the `Uint8ClampedArray` in JS.
///
/// Supported inner types:
/// * `Clamped<&[u8]>`
/// * `Clamped<&mut [u8]>`
/// * `Clamped<Vec<u8>>`
#[derive(Copy, Clone, PartialEq, Debug, Eq)]
pub struct Clamped<T>(pub T);

impl<T> Deref for Clamped<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for Clamped<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// A JavaScript Error object.
///
/// This type is used to create JavaScript Error objects that can be thrown or returned.
#[derive(Debug)]
#[repr(transparent)]
pub struct JsError {
    value: JsValue,
}

impl JsError {
    /// Create a new JavaScript Error with the given message.
    pub fn new(message: &str) -> Self {
        JsError {
            value: __wry_call_js_function!(
                "(msg) => new Error(msg)",
                fn(&str) -> JsValue,
                (message)
            ),
        }
    }
}

impl From<JsError> for JsValue {
    fn from(e: JsError) -> Self {
        e.value
    }
}

impl From<JsValue> for JsError {
    fn from(value: JsValue) -> Self {
        JsError { value }
    }
}

impl<T> From<Option<T>> for JsValue
where
    T: Into<JsValue>,
{
    fn from(s: Option<T>) -> JsValue {
        match s {
            Some(s) => s.into(),
            None => JsValue::undefined(),
        }
    }
}

impl AsRef<JsValue> for JsError {
    fn as_ref(&self) -> &JsValue {
        &self.value
    }
}

impl core::fmt::Display for JsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "JsError")
    }
}

impl core::error::Error for JsError {}

impl JsCast for JsError {
    fn instanceof(val: &JsValue) -> bool {
        crate::js_helpers::js_is_error(val)
    }

    fn unchecked_from_js(val: JsValue) -> Self {
        JsError { value: val }
    }

    fn unchecked_from_js_ref(val: &JsValue) -> &Self {
        // SAFETY: #[repr(transparent)] guarantees same layout
        unsafe { &*(val as *const JsValue as *const JsError) }
    }
}

impl EncodeTypeDef for JsError {
    fn encode_type_def(buf: &mut alloc::vec::Vec<u8>) {
        JsValue::encode_type_def(buf);
    }
}

impl BinaryEncode for JsError {
    fn encode(self, encoder: &mut EncodedData) {
        self.value.encode(encoder);
    }
}

impl BinaryDecode for JsError {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        JsValue::decode(decoder).map(Into::into)
    }

    fn decode_inbound(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        <JsValue as BinaryDecode>::decode_inbound(decoder).map(Into::into)
    }
}

impl BatchableResult for JsError {
    fn try_placeholder(batch: &mut batch::Runtime) -> Option<Self> {
        Some(<JsValue as BatchableResult>::try_placeholder(batch)?.into())
    }
}

unsafe impl __rt::marker::ErasableGeneric for JsError {
    type Repr = JsValue;
}

impl IntoJsGeneric for JsError {
    type JsCanon = Self;

    fn to_js(self) -> Self {
        self
    }
}

impl convert::IntoWasmAbi for JsError {
    type Abi = <JsValue as convert::IntoWasmAbi>::Abi;

    fn into_abi(self) -> Self::Abi {
        self.value.into_abi()
    }
}

impl convert::FromWasmAbi for JsError {
    type Abi = <JsValue as convert::FromWasmAbi>::Abi;

    unsafe fn from_abi(js: Self::Abi) -> Self {
        unsafe { <JsValue as convert::FromWasmAbi>::from_abi(js) }.into()
    }
}

impl convert::UpcastFrom<JsError> for JsError {}
impl convert::UpcastFrom<JsError> for JsValue {}

pub mod sys {
    use core::{fmt, marker::PhantomData, ops::Deref};

    use crate::{
        BinaryDecode, BinaryEncode, DecodeError, DecodedData, EncodeTypeDef, EncodedData,
        IntoJsGeneric, JsCast, JsGeneric, JsValue,
        batch::Runtime,
        convert::{FromWasmAbi, IntoWasmAbi, UpcastFrom},
    };

    /// Marker trait for values that are either a resolution value or a promise-like value.
    pub trait Promising {
        type Resolution;
    }

    macro_rules! js_primitive {
        ($name:ident, $const_name:ident, $value:expr, $display:literal, $check:expr) => {
            #[derive(Clone, PartialEq)]
            #[repr(transparent)]
            pub struct $name {
                pub obj: JsValue,
            }

            impl $name {
                pub const $const_name: $name = Self { obj: $value };
            }

            impl Eq for $name {}

            impl Default for $name {
                fn default() -> Self {
                    Self::$const_name
                }
            }

            impl fmt::Debug for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str($display)
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str($display)
                }
            }

            impl AsRef<JsValue> for $name {
                fn as_ref(&self) -> &JsValue {
                    &self.obj
                }
            }

            impl From<$name> for JsValue {
                fn from(value: $name) -> JsValue {
                    value.obj
                }
            }

            impl From<&$name> for JsValue {
                fn from(value: &$name) -> JsValue {
                    value.obj.clone()
                }
            }

            impl From<JsValue> for $name {
                fn from(obj: JsValue) -> Self {
                    Self { obj }
                }
            }

            impl JsCast for $name {
                fn instanceof(value: &JsValue) -> bool {
                    $check(value)
                }

                fn is_type_of(value: &JsValue) -> bool {
                    $check(value)
                }

                fn unchecked_from_js(value: JsValue) -> Self {
                    Self { obj: value }
                }

                fn unchecked_from_js_ref(value: &JsValue) -> &Self {
                    unsafe { &*(value as *const JsValue as *const Self) }
                }
            }

            impl EncodeTypeDef for $name {
                fn encode_type_def(buf: &mut alloc::vec::Vec<u8>) {
                    JsValue::encode_type_def(buf);
                }
            }

            impl BinaryEncode for $name {
                fn encode(self, encoder: &mut EncodedData) {
                    self.obj.encode(encoder);
                }
            }

            impl BinaryDecode for $name {
                fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
                    JsValue::decode(decoder).map(Into::into)
                }

                fn decode_inbound(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
                    <JsValue as BinaryDecode>::decode_inbound(decoder).map(Into::into)
                }
            }

            impl crate::BatchableResult for $name {
                fn try_placeholder(batch: &mut Runtime) -> Option<Self> {
                    Some(<JsValue as crate::BatchableResult>::try_placeholder(batch)?.into())
                }
            }

            unsafe impl crate::__rt::marker::ErasableGeneric for $name {
                type Repr = JsValue;
            }

            impl IntoJsGeneric for $name {
                type JsCanon = Self;

                fn to_js(self) -> Self {
                    self
                }
            }

            impl IntoWasmAbi for $name {
                type Abi = <JsValue as IntoWasmAbi>::Abi;

                fn into_abi(self) -> Self::Abi {
                    self.obj.into_abi()
                }
            }

            impl FromWasmAbi for $name {
                type Abi = <JsValue as FromWasmAbi>::Abi;

                unsafe fn from_abi(js: Self::Abi) -> Self {
                    unsafe { <JsValue as FromWasmAbi>::from_abi(js) }.into()
                }
            }

            impl UpcastFrom<$name> for $name {}
            impl UpcastFrom<$name> for JsValue {}
        };
    }

    js_primitive!(
        Undefined,
        UNDEFINED,
        JsValue::UNDEFINED,
        "undefined",
        JsValue::is_undefined
    );
    js_primitive!(Null, NULL, JsValue::NULL, "null", JsValue::is_null);

    impl UpcastFrom<()> for Undefined {}
    impl UpcastFrom<Undefined> for () {}
    impl UpcastFrom<()> for JsValue {}
    impl UpcastFrom<()> for () {}

    #[derive(Clone, PartialEq)]
    #[repr(transparent)]
    pub struct JsOption<T = JsValue> {
        pub obj: JsValue,
        pub generics: PhantomData<fn() -> T>,
    }

    impl<T: JsGeneric> JsOption<T> {
        #[inline]
        pub fn new() -> Self {
            Undefined::UNDEFINED.unchecked_into()
        }

        #[inline]
        pub fn wrap(value: T) -> Self {
            value.unchecked_into()
        }

        #[inline]
        pub fn from_option(value: Option<T>) -> Self {
            match value {
                Some(value) => Self::wrap(value),
                None => Self::new(),
            }
        }

        #[inline]
        pub fn is_empty(&self) -> bool {
            self.obj.is_null() || self.obj.is_undefined()
        }

        #[inline]
        pub fn as_option(&self) -> Option<T>
        where
            T: Clone,
        {
            if self.is_empty() {
                None
            } else {
                Some(self.deref().clone().unchecked_into())
            }
        }

        #[inline]
        pub fn into_option(self) -> Option<T> {
            if self.is_empty() {
                None
            } else {
                Some(self.unchecked_into())
            }
        }
    }

    impl<T: JsGeneric> Default for JsOption<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T> AsRef<JsValue> for JsOption<T> {
        fn as_ref(&self) -> &JsValue {
            &self.obj
        }
    }

    impl<T: JsGeneric> Deref for JsOption<T> {
        type Target = T;

        fn deref(&self) -> &T {
            T::unchecked_from_js_ref(&self.obj)
        }
    }

    impl<T> From<JsOption<T>> for JsValue {
        fn from(value: JsOption<T>) -> JsValue {
            value.obj
        }
    }

    impl<T> From<&JsOption<T>> for JsValue {
        fn from(value: &JsOption<T>) -> JsValue {
            value.obj.clone()
        }
    }

    impl<T> From<JsValue> for JsOption<T> {
        fn from(obj: JsValue) -> Self {
            Self {
                obj,
                generics: PhantomData,
            }
        }
    }

    impl<T: JsGeneric> JsCast for JsOption<T> {
        fn instanceof(value: &JsValue) -> bool {
            T::is_type_of(value) || value.is_null() || value.is_undefined()
        }

        fn unchecked_from_js(value: JsValue) -> Self {
            value.into()
        }

        fn unchecked_from_js_ref(value: &JsValue) -> &Self {
            unsafe { &*(value as *const JsValue as *const Self) }
        }
    }

    impl<T> EncodeTypeDef for JsOption<T> {
        fn encode_type_def(buf: &mut alloc::vec::Vec<u8>) {
            JsValue::encode_type_def(buf);
        }
    }

    impl<T> BinaryEncode for JsOption<T> {
        fn encode(self, encoder: &mut EncodedData) {
            self.obj.encode(encoder);
        }
    }

    impl<T> BinaryDecode for JsOption<T> {
        fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
            JsValue::decode(decoder).map(Into::into)
        }

        fn decode_inbound(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
            <JsValue as BinaryDecode>::decode_inbound(decoder).map(Into::into)
        }
    }

    impl<T> crate::BatchableResult for JsOption<T> {
        fn try_placeholder(batch: &mut Runtime) -> Option<Self> {
            Some(<JsValue as crate::BatchableResult>::try_placeholder(batch)?.into())
        }
    }

    unsafe impl<T> crate::__rt::marker::ErasableGeneric for JsOption<T> {
        type Repr = JsValue;
    }

    impl<T: JsGeneric> IntoJsGeneric for JsOption<T> {
        type JsCanon = Self;

        fn to_js(self) -> Self {
            self
        }
    }

    impl<T> IntoWasmAbi for JsOption<T> {
        type Abi = <JsValue as IntoWasmAbi>::Abi;

        fn into_abi(self) -> Self::Abi {
            self.obj.into_abi()
        }
    }

    impl<T> FromWasmAbi for JsOption<T> {
        type Abi = <JsValue as FromWasmAbi>::Abi;

        unsafe fn from_abi(js: Self::Abi) -> Self {
            unsafe { <JsValue as FromWasmAbi>::from_abi(js) }.into()
        }
    }

    impl UpcastFrom<JsValue> for JsOption<JsValue> {}
    impl<T> UpcastFrom<Undefined> for JsOption<T> {}
    impl<T> UpcastFrom<Null> for JsOption<T> {}
    impl<T> UpcastFrom<()> for JsOption<T> {}
    impl<T> UpcastFrom<JsOption<T>> for JsValue {}
    impl<T, U> UpcastFrom<JsOption<U>> for JsOption<T> where T: UpcastFrom<U> {}

    impl Promising for JsValue {
        type Resolution = JsValue;
    }

    impl Promising for () {
        type Resolution = Undefined;
    }

    impl<T: Promising> Promising for Option<T> {
        type Resolution = Option<T::Resolution>;
    }
}

// Re-export commonly used items
pub use batch::batch;
pub use convert::{IntoJsGeneric, JsGeneric};
pub use encode::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef};
pub use function::JSFunction;
pub use ipc::{DecodeError, DecodedData, EncodedData};
pub use sys::{JsOption, Null, Promising, Undefined};

// Re-export the macros
pub use wry_bindgen_macro::link_to;
pub use wry_bindgen_macro::wasm_bindgen;

// Re-export inventory for macro use
pub use inventory;

use crate::encode::{CallbackKey, IntoClosure};
use crate::function::RustCallback;
use crate::object_store::insert_object;

// Re-export function registry types
pub use __rt::marker::ErasableGeneric;
pub use function_registry::{
    InlineJsModule, JsClassMemberKind, JsClassMemberSpec, JsExportSpec, JsFunctionSpec,
    LazyJsFunction,
};

/// Macro to register and call a JavaScript function.
///
/// This macro encapsulates the common pattern of:
/// 1. Creating a static JsFunctionSpec
/// 2. Submitting it to inventory
/// 3. Creating a LazyJsFunction with the given signature
/// 4. Calling the function with the provided arguments
///
/// # Usage
/// ```ignore
/// __wry_call_js_function!("(a, b) => a + b", fn(i32, i32) -> i32, (x, y))
/// ```
#[macro_export]
#[doc(hidden)]
macro_rules! __wry_call_js_function {
    ($js_code:expr, $fn_type:ty, ($($args:expr),*)) => {{
        let __func: $crate::LazyJsFunction<$fn_type> = $crate::__wry_submit_js_function!($js_code);

        __func.call($($args),*)
    }};
}

/// Macro to register and call a JavaScript function.
///
/// This macro encapsulates the common pattern of:
/// 1. Creating a static JsFunctionSpec
/// 2. Submitting it to inventory
/// 3. Creating a LazyJsFunction with the given signature
///
/// # Usage
/// ```ignore
/// __wry_submit_js_function!("(a, b) => a + b")
/// ```
#[macro_export]
#[doc(hidden)]
macro_rules! __wry_submit_js_function {
    ($js_code:expr) => {{
        static __SPEC: $crate::JsFunctionSpec =
            $crate::JsFunctionSpec::new(|| $crate::alloc::format!($js_code));

        $crate::inventory::submit! {
            __SPEC
        }

        __SPEC.resolve_as()
    }};
}

/// Extension trait for Option to unwrap or throw a JS error.
/// This is API-compatible with wasm-bindgen's UnwrapThrowExt.
pub trait UnwrapThrowExt<T>: Sized {
    /// Unwrap the value or panic with a message.
    fn unwrap_throw(self) -> T;

    /// Unwrap the value or panic with a custom message.
    fn expect_throw(self, message: &str) -> T;
}

impl<T> UnwrapThrowExt<T> for Option<T> {
    fn unwrap_throw(self) -> T {
        self.expect("called `Option::unwrap_throw()` on a `None` value")
    }

    fn expect_throw(self, message: &str) -> T {
        self.expect(message)
    }
}

impl<T, E> UnwrapThrowExt<T> for Result<T, E>
where
    E: core::fmt::Debug,
{
    fn unwrap_throw(self) -> T {
        self.expect("called `Result::unwrap_throw()` on an `Err` value")
    }

    fn expect_throw(self, message: &str) -> T {
        self.expect(message)
    }
}

#[cold]
#[inline(never)]
pub fn throw_val(s: JsValue) -> ! {
    panic!("{s:?}");
}

/// Throw a JS exception with the given message.
///
/// # Panics
/// This function always panics when running outside of WASM.
#[cold]
#[inline(never)]
pub fn throw_str(s: &str) -> ! {
    panic!("cannot throw JS exception when running outside of wasm: {s}");
}

/// Returns the number of live externref objects.
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn externref_heap_live_count() -> u32 {
    panic!("cannot introspect wasm memory when running outside of wasm")
}

/// Returns a handle to this Wasm instance's `WebAssembly.Module`.
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn module() -> JsValue {
    panic!("cannot introspect wasm memory when running outside of wasm")
}

/// Returns a handle to this Wasm instance's `WebAssembly.Instance.prototype.exports`.
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn exports() -> JsValue {
    panic!("cannot introspect wasm memory when running outside of wasm")
}

/// Returns a handle to this Wasm instance's `WebAssembly.Memory`.
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn memory() -> JsValue {
    panic!("cannot introspect wasm memory when running outside of wasm")
}

/// Returns a handle to this Wasm instance's `WebAssembly.Table` (indirect function table).
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn function_table() -> JsValue {
    panic!("cannot introspect wasm memory when running outside of wasm")
}

// Re-export extract_rust_handle from js_helpers
pub use js_helpers::js_extract_rust_handle as extract_rust_handle;

/// Prelude module for common imports
pub mod prelude {
    pub use crate::Clamped;
    pub use crate::Closure;
    pub use crate::JsError;
    pub use crate::UnwrapThrowExt;
    pub use crate::WasmClosure;
    pub use crate::batch::batch;
    pub use crate::cast::JsCast;
    pub use crate::convert::{IntoJsGeneric, JsGeneric, Upcast, UpcastFrom};
    pub use crate::encode::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef};
    pub use crate::function::JSFunction;
    pub use crate::lazy::JsThreadLocal;
    pub use crate::sys::{JsOption, Null, Promising, Undefined};
    pub use crate::value::JsValue;
    pub use crate::wasm_bindgen;
}
