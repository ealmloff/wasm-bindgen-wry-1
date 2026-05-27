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
    /// Borrowed or owned closure handle for passing Rust closures to JavaScript.
    pub struct ScopedClosure<'a, T: ?Sized> {
        // careful: must be Box<T> not just T because unsized PhantomData
        // seems to have weird interaction with Pin<>
        pub(crate) _phantom: core::marker::PhantomData<(&'a (), crate::alloc::boxed::Box<T>)>,
        pub(crate) rust_callback: Option<crate::object_store::ObjectHandle>,
        pub(crate) drop_rust_callback_on_drop: bool,
        pub(crate) value: crate::JsValue,
    }

    /// Owned closure handle. This follows upstream wasm-bindgen's alias direction.
    pub type Closure<T> = ScopedClosure<'static, T>;

    use crate::__rt::WasmWord;
    pub use crate::WasmClosure;

    /// Drop hook exported for compatibility with wasm-bindgen-generated code.
    ///
    /// # Safety
    ///
    /// This is an ABI entry point called by generated glue code. The arguments
    /// must be the encoded closure metadata expected by that glue.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __wbindgen_destroy_closure(a: WasmWord, b: WasmWord) {
        let _ = (a, b);
    }
}

pub use closure::{Closure, ScopedClosure};

/// Runtime module for wasm-bindgen compatibility.
/// This module provides the wbg_cast function used for type casting.
pub mod __rt {
    use crate::{
        __wry_submit_js_function, JsValue, LazyJsFunction,
        encode::{BatchableResult, BinaryEncode, EncodeTypeDef},
    };

    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct WasmWord(pub u32);

    pub struct Ref<'b, T: ?Sized + 'b> {
        pub(crate) inner: core::cell::Ref<'b, T>,
    }

    impl<T: ?Sized> core::ops::Deref for Ref<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<T: ?Sized> core::borrow::Borrow<T> for Ref<'_, T> {
        fn borrow(&self) -> &T {
            self
        }
    }

    pub struct RefMut<'b, T: ?Sized + 'b> {
        pub(crate) inner: core::cell::RefMut<'b, T>,
    }

    impl<T: ?Sized> core::ops::Deref for RefMut<'_, T> {
        type Target = T;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl<T: ?Sized> core::ops::DerefMut for RefMut<'_, T> {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.inner
        }
    }

    impl<T: ?Sized> core::borrow::Borrow<T> for RefMut<'_, T> {
        fn borrow(&self) -> &T {
            self
        }
    }

    impl<T: ?Sized> core::borrow::BorrowMut<T> for RefMut<'_, T> {
        fn borrow_mut(&mut self) -> &mut T {
            self
        }
    }

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

macro_rules! to_js_value_number {
    ($ty:ty) => {
        impl From<$ty> for $crate::JsValue {
            fn from(n: $ty) -> $crate::JsValue {
                cast! {($ty => $crate::JsValue) n}
            }
        }
    };
}

macro_rules! to_js_value_bigint {
    ($ty:ty) => {
        impl From<$ty> for $crate::JsValue {
            fn from(arg: $ty) -> $crate::JsValue {
                cast! {($ty => $crate::JsValue) arg}
            }
        }
    };
}

macro_rules! to_js_value_word {
    ($ty:ty) => {
        impl From<$ty> for $crate::JsValue {
            fn from(n: $ty) -> Self {
                cast! {($ty => $crate::JsValue) n}
            }
        }
    };
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

    fn try_from(v: JsValue) -> Result<Self, JsValue> {
        <Self as convert::TryFromJsValue>::try_from_js_value(v)
    }
}

impl TryFrom<JsValue> for i64 {
    type Error = JsValue;

    fn try_from(v: JsValue) -> Result<Self, JsValue> {
        <Self as convert::TryFromJsValue>::try_from_js_value(v)
    }
}

impl TryFrom<JsValue> for f64 {
    type Error = JsValue;

    fn try_from(val: JsValue) -> Result<Self, Self::Error> {
        val.as_f64().ok_or(val)
    }
}

impl TryFrom<&JsValue> for f64 {
    type Error = JsValue;

    fn try_from(val: &JsValue) -> Result<Self, Self::Error> {
        val.as_f64().ok_or_else(|| val.clone())
    }
}

impl TryFrom<JsValue> for i128 {
    type Error = JsValue;

    fn try_from(v: JsValue) -> Result<Self, JsValue> {
        <Self as convert::TryFromJsValue>::try_from_js_value(v)
    }
}

impl TryFrom<JsValue> for u128 {
    type Error = JsValue;

    fn try_from(v: JsValue) -> Result<Self, JsValue> {
        <Self as convert::TryFromJsValue>::try_from_js_value(v)
    }
}

impl TryFrom<JsValue> for String {
    type Error = JsValue;

    fn try_from(value: JsValue) -> Result<Self, Self::Error> {
        value.as_string().ok_or(value)
    }
}

impl convert::TryFromJsValue for String {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        value.as_string()
    }
}

impl convert::TryFromJsValue for bool {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        value.as_bool()
    }
}

impl convert::TryFromJsValue for char {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        let s = value.as_string()?;
        if s.len() == 1 { s.chars().next() } else { None }
    }
}

impl convert::TryFromJsValue for () {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        if value.is_undefined() { Some(()) } else { None }
    }
}

impl<T: convert::TryFromJsValue> convert::TryFromJsValue for Option<T> {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        if value.is_undefined() {
            Some(None)
        } else {
            T::try_from_js_value_ref(value).map(Some)
        }
    }
}

impl<T: convert::TryFromJsValue> convert::TryFromJsValue for Vec<T> {
    fn try_from_js_value_ref(value: &JsValue) -> Option<Self> {
        if !value.is_array() {
            return None;
        }
        let length = crate::js_helpers::js_reflect_get(value, &JsValue::from_str("length"));
        let len = length.as_f64()? as u32;
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let element = crate::js_helpers::js_reflect_get(value, &JsValue::from_f64(i as f64));
            out.push(T::try_from_js_value(element).ok()?);
        }
        Some(out)
    }
}

fn js_number_is_integer_in_range(number: f64, min: f64, max: f64) -> bool {
    number.is_finite() && number.fract() == 0.0 && (min..=max).contains(&number)
}

macro_rules! try_from_js_value_signed_int {
    ($($ty:ty),* $(,)?) => {
        $(
            impl convert::TryFromJsValue for $ty {
                fn try_from_js_value_ref(val: &JsValue) -> Option<$ty> {
                    let number = val.as_f64()?;
                    if js_number_is_integer_in_range(number, <$ty>::MIN as f64, <$ty>::MAX as f64) {
                        Some(number as $ty)
                    } else {
                        None
                    }
                }
            }
        )*
    };
}

macro_rules! try_from_js_value_unsigned_int {
    ($($ty:ty),* $(,)?) => {
        $(
            impl convert::TryFromJsValue for $ty {
                fn try_from_js_value_ref(val: &JsValue) -> Option<$ty> {
                    let number = val.as_f64()?;
                    if js_number_is_integer_in_range(number, 0.0, <$ty>::MAX as f64) {
                        Some(number as $ty)
                    } else {
                        None
                    }
                }
            }
        )*
    };
}

try_from_js_value_signed_int!(i8, i16, i32);
try_from_js_value_unsigned_int!(u8, u16, u32);

impl convert::TryFromJsValue for f32 {
    fn try_from_js_value_ref(val: &JsValue) -> Option<f32> {
        val.as_f64().map(|n| n as f32)
    }
}

impl convert::TryFromJsValue for f64 {
    fn try_from_js_value_ref(val: &JsValue) -> Option<f64> {
        val.as_f64()
    }
}

impl convert::TryFromJsValue for i64 {
    fn try_from_js_value_ref(val: &JsValue) -> Option<i64> {
        crate::js_helpers::js_bigint_get_as_i64(val)
    }
}

impl convert::TryFromJsValue for u64 {
    fn try_from_js_value_ref(val: &JsValue) -> Option<u64> {
        crate::js_helpers::js_bigint_get_as_i64(val).map(|value| value as u64)
    }
}

impl convert::TryFromJsValue for i128 {
    fn try_from_js_value_ref(v: &JsValue) -> Option<i128> {
        crate::js_helpers::js_bigint_to_string(v)?.parse().ok()
    }
}

impl convert::TryFromJsValue for u128 {
    fn try_from_js_value_ref(v: &JsValue) -> Option<u128> {
        crate::js_helpers::js_bigint_to_string(v)?.parse().ok()
    }
}

impl convert::TryFromJsValue for isize {
    fn try_from_js_value_ref(val: &JsValue) -> Option<isize> {
        val.as_f64().map(|n| n as isize)
    }
}

impl convert::TryFromJsValue for usize {
    fn try_from_js_value_ref(val: &JsValue) -> Option<usize> {
        val.as_f64().map(|n| n as usize)
    }
}

to_js_value_number!(i8);
from_js_value!(i8);
to_js_value_number!(i16);
from_js_value!(i16);
to_js_value_number!(i32);
from_js_value!(i32);
to_js_value_bigint!(i64);
to_js_value_bigint!(i128);
to_js_value_number!(u8);
from_js_value!(u8);
to_js_value_number!(u16);
from_js_value!(u16);
to_js_value_number!(u32);
from_js_value!(u32);
to_js_value_bigint!(u64);
to_js_value_bigint!(u128);
to_js_value_number!(f32);
from_js_value!(f32);
to_js_value_number!(f64);
to_js_value_word!(usize);
from_js_value!(usize);
to_js_value_word!(isize);
from_js_value!(isize);
impl<'a> From<&'a str> for JsValue {
    fn from(s: &'a str) -> JsValue {
        cast! {(String => JsValue) s.to_string()}
    }
}
impl<'a> From<&'a String> for JsValue {
    fn from(s: &'a String) -> JsValue {
        cast! {(String => JsValue) s.clone()}
    }
}
impl<'a, T> From<&'a T> for JsValue
where
    T: JsCast,
{
    fn from(s: &'a T) -> JsValue {
        s.as_ref().clone()
    }
}
impl From<String> for JsValue {
    fn from(s: String) -> JsValue {
        cast! {(String => JsValue) s}
    }
}
to_js_value!(());
from_js_value!(());

impl<T: ?Sized> ScopedClosure<'static, T> {
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

impl<'a, T> ScopedClosure<'a, T>
where
    T: ?Sized + WasmClosure,
{
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

impl<T: ?Sized> Unpin for ScopedClosure<'_, T> {}

/// Marker for closures that are safe to invoke through panic-catching glue.
#[doc(hidden)]
pub trait MaybeUnwindSafe {}

impl<T: ?Sized> MaybeUnwindSafe for T {}

/// A trait for converting a Rust closure into the JS closure type `T`.
#[doc(hidden)]
pub trait IntoWasmClosure<T: ?Sized> {
    fn into_closure(self) -> Closure<T>
    where
        Self: Sized,
    {
        unreachable!("unsized closure objects must be converted from Box<Self>")
    }

    fn into_closure_box(self: Box<Self>) -> Closure<T>;
}

/// A trait for converting a shared borrowed Rust closure into the JS closure type `T`.
#[doc(hidden)]
pub trait IntoWasmClosureRef<T: ?Sized> {
    fn into_scoped_closure_ref<'a>(t: &'a Self) -> ScopedClosure<'a, T::Static>
    where
        T: WasmClosure;
}

/// A trait for converting a mutably borrowed Rust closure into the JS closure type `T`.
#[doc(hidden)]
pub trait IntoWasmClosureRefMut<T: ?Sized> {
    fn into_scoped_closure_ref_mut<'a>(t: &'a mut Self) -> ScopedClosure<'a, T::Static>
    where
        T: WasmClosure;
}

/// A trait for converting an `FnOnce(A...) -> R` into a `Closure<dyn FnMut(A...) -> R>`.
#[doc(hidden)]
pub trait WasmClosureFnOnce<T: ?Sized, A, R>: Sized + 'static {
    fn into_closure(self) -> Closure<T>;
}

/// A trait for converting an aborting `FnOnce(A...) -> R` into a closure.
#[doc(hidden)]
pub trait WasmClosureFnOnceAbort<T: ?Sized, A, R>: Sized + 'static {
    fn into_closure(self) -> Closure<T>;
}

impl<T: ?Sized> AsRef<JsValue> for ScopedClosure<'_, T> {
    fn as_ref(&self) -> &JsValue {
        &self.value
    }
}

impl<T> core::fmt::Debug for ScopedClosure<'_, T>
where
    T: ?Sized,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Closure")
            .field("value", &self.value)
            .finish()
    }
}

/// Upstream-compatible marker trait for closure signatures.
#[doc(hidden)]
pub trait WasmClosure {
    /// The `'static` version of this closure type.
    type Static: ?Sized;
    /// The mutable version of this closure type.
    type AsMut: ?Sized;
}

/// Internal trait for closure types that can be wrapped and passed to JavaScript.
#[doc(hidden)]
pub trait WryWasmClosure<M> {
    /// Create a Closure from a boxed closure.
    fn into_js_closure(boxed: Box<Self>) -> Closure<Self>;
}

impl<T> ScopedClosure<'static, T>
where
    T: ?Sized + WasmClosure,
{
    pub fn new<F>(t: F) -> Self
    where
        F: IntoWasmClosure<T> + MaybeUnwindSafe + 'static,
    {
        Self::own(t)
    }

    pub fn own<F>(t: F) -> Self
    where
        F: IntoWasmClosure<T> + MaybeUnwindSafe + 'static,
    {
        <F as IntoWasmClosure<T>>::into_closure(t)
    }

    pub fn own_aborting<F>(t: F) -> Self
    where
        F: IntoWasmClosure<T> + 'static,
    {
        <F as IntoWasmClosure<T>>::into_closure(t)
    }

    pub fn own_assert_unwind_safe<F>(t: F) -> Self
    where
        F: IntoWasmClosure<T> + 'static,
    {
        <F as IntoWasmClosure<T>>::into_closure(t)
    }

    pub fn wrap<F>(data: Box<F>) -> Self
    where
        F: IntoWasmClosure<T> + ?Sized + MaybeUnwindSafe,
    {
        <F as IntoWasmClosure<T>>::into_closure_box(data)
    }

    pub fn wrap_aborting<F>(data: Box<F>) -> Self
    where
        F: IntoWasmClosure<T> + ?Sized,
    {
        <F as IntoWasmClosure<T>>::into_closure_box(data)
    }

    pub fn wrap_assert_unwind_safe<F>(data: Box<F>) -> Self
    where
        F: IntoWasmClosure<T> + ?Sized,
    {
        <F as IntoWasmClosure<T>>::into_closure_box(data)
    }

    pub fn borrow<'a, F>(t: &'a F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRef<T> + MaybeUnwindSafe + ?Sized,
    {
        F::into_scoped_closure_ref(t)
    }

    pub fn borrow_aborting<'a, F>(t: &'a F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRef<T> + ?Sized,
    {
        F::into_scoped_closure_ref(t)
    }

    pub fn borrow_assert_unwind_safe<'a, F>(t: &'a F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRef<T> + ?Sized,
    {
        F::into_scoped_closure_ref(t)
    }

    pub fn borrow_mut<'a, F>(t: &'a mut F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRefMut<T> + MaybeUnwindSafe + ?Sized,
    {
        F::into_scoped_closure_ref_mut(t)
    }

    pub fn borrow_mut_aborting<'a, F>(t: &'a mut F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRefMut<T> + ?Sized,
    {
        F::into_scoped_closure_ref_mut(t)
    }

    pub fn borrow_mut_assert_unwind_safe<'a, F>(t: &'a mut F) -> ScopedClosure<'a, T::Static>
    where
        F: IntoWasmClosureRefMut<T> + ?Sized,
    {
        F::into_scoped_closure_ref_mut(t)
    }

    /// Create a `Closure` from a function that can only be called once.
    ///
    /// Since we have no way of enforcing that JS cannot attempt to call this
    /// `FnOnce` more than once, this produces a `Closure<dyn FnMut(A...) -> R>`
    /// that will panic if called more than once.
    pub fn once<F, A, R>(fn_once: F) -> Self
    where
        F: WasmClosureFnOnce<T, A, R> + MaybeUnwindSafe,
    {
        let mut closure = <F as WasmClosureFnOnce<T, A, R>>::into_closure(fn_once);
        closure.drop_rust_callback_on_drop = false;
        closure
    }

    pub fn once_aborting<F, A, R>(fn_once: F) -> Self
    where
        F: WasmClosureFnOnceAbort<T, A, R>,
    {
        let mut closure = <F as WasmClosureFnOnceAbort<T, A, R>>::into_closure(fn_once);
        closure.drop_rust_callback_on_drop = false;
        closure
    }

    pub fn once_assert_unwind_safe<F, A, R>(fn_once: F) -> Self
    where
        F: WasmClosureFnOnceAbort<T, A, R>,
    {
        Self::once_aborting(fn_once)
    }

    /// Forgets the closure, leaking it.
    pub fn forget(self) {
        core::mem::forget(self);
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
    pub fn once_into_js<F, A, R>(fn_once: F) -> JsValue
    where
        F: WasmClosureFnOnce<T, A, R> + MaybeUnwindSafe,
    {
        Self::once(fn_once).into_js_value()
    }
}

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
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

mod parent {
    use crate::{Ref, RefMut};

    /// Storage wrapper for the auto-injected parent field on extended Rust types.
    pub struct Parent<T> {
        inner: alloc::rc::Rc<core::cell::RefCell<T>>,
    }

    impl<T> Clone for Parent<T> {
        fn clone(&self) -> Self {
            Self {
                inner: alloc::rc::Rc::clone(&self.inner),
            }
        }
    }

    impl<T: core::fmt::Debug> core::fmt::Debug for Parent<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_tuple("Parent")
                .field(&*self.inner.borrow())
                .finish()
        }
    }

    impl<T> Parent<T> {
        pub fn new(value: T) -> Self {
            Self {
                inner: alloc::rc::Rc::new(core::cell::RefCell::new(value)),
            }
        }

        pub fn borrow(&self) -> Ref<'_, T> {
            Ref {
                inner: self.inner.borrow(),
            }
        }

        pub fn borrow_mut(&self) -> RefMut<'_, T> {
            RefMut {
                inner: self.inner.borrow_mut(),
            }
        }
    }

    impl<T> From<T> for Parent<T> {
        fn from(value: T) -> Self {
            Parent::new(value)
        }
    }
}

pub use crate::__rt::{Ref, RefMut};
pub use parent::Parent;

macro_rules! impl_js_value_wire {
    (for $ty:ty, field $field:ident) => {
        impl $crate::EncodeTypeDef for $ty {
            fn encode_type_def(buf: &mut $crate::alloc::vec::Vec<u8>) {
                <$crate::JsValue as $crate::EncodeTypeDef>::encode_type_def(buf);
            }
        }

        impl $crate::BinaryEncode for $ty {
            fn encode(self, encoder: &mut $crate::EncodedData) {
                <$crate::JsValue as $crate::BinaryEncode>::encode(self.$field, encoder);
            }
        }

        impl $crate::BinaryDecode for $ty {
            fn decode(
                decoder: &mut $crate::DecodedData,
            ) -> ::core::result::Result<Self, $crate::DecodeError> {
                <$crate::JsValue as $crate::BinaryDecode>::decode(decoder)
                    .map(::core::convert::Into::into)
            }
        }

        impl $crate::BatchableResult for $ty {
            fn try_placeholder(
                batch: &mut $crate::batch::Runtime,
            ) -> ::core::option::Option<Self> {
                ::core::option::Option::Some(
                    <$crate::JsValue as $crate::BatchableResult>::try_placeholder(batch)?.into(),
                )
            }
        }
    };
    (impl<$($generics:ident),*> for $ty:ty, field $field:ident) => {
        impl<$($generics),*> $crate::EncodeTypeDef for $ty {
            fn encode_type_def(buf: &mut $crate::alloc::vec::Vec<u8>) {
                <$crate::JsValue as $crate::EncodeTypeDef>::encode_type_def(buf);
            }
        }

        impl<$($generics),*> $crate::BinaryEncode for $ty {
            fn encode(self, encoder: &mut $crate::EncodedData) {
                <$crate::JsValue as $crate::BinaryEncode>::encode(self.$field, encoder);
            }
        }

        impl<$($generics),*> $crate::BinaryDecode for $ty {
            fn decode(
                decoder: &mut $crate::DecodedData,
            ) -> ::core::result::Result<Self, $crate::DecodeError> {
                <$crate::JsValue as $crate::BinaryDecode>::decode(decoder)
                    .map(::core::convert::Into::into)
            }
        }

        impl<$($generics),*> $crate::BatchableResult for $ty {
            fn try_placeholder(
                batch: &mut $crate::batch::Runtime,
            ) -> ::core::option::Option<Self> {
                ::core::option::Option::Some(
                    <$crate::JsValue as $crate::BatchableResult>::try_placeholder(batch)?.into(),
                )
            }
        }
    };
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
    pub fn new(s: &str) -> JsError {
        JsError {
            value: __wry_call_js_function!("(msg) => new Error(msg)", fn(&str) -> JsValue, (s)),
        }
    }
}

impl From<JsError> for JsValue {
    fn from(error: JsError) -> Self {
        error.value
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

#[cfg(feature = "std")]
impl std::error::Error for JsError {}

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

impl_js_value_wire!(for JsError, field value);

unsafe impl __rt::marker::ErasableGeneric for JsError {
    type Repr = JsValue;
}

impl IntoJsGeneric for JsError {
    type JsCanon = Self;

    fn to_js(self) -> Self {
        self
    }
}

impl convert::UpcastFrom<JsError> for JsError {}
impl convert::UpcastFrom<JsError> for JsValue {}

pub mod sys {
    use core::{fmt, marker::PhantomData, ops::Deref};

    use crate::{
        ErasableGeneric, IntoJsGeneric, JsCast, JsError, JsGeneric, JsValue, convert::UpcastFrom,
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

            impl_js_value_wire!(for $name, field obj);

            unsafe impl crate::__rt::marker::ErasableGeneric for $name {
                type Repr = JsValue;
            }

            impl IntoJsGeneric for $name {
                type JsCanon = Self;

                fn to_js(self) -> Self {
                    self
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
        pub fn wrap(val: T) -> Self {
            val.unchecked_into()
        }

        #[inline]
        pub fn from_option(opt: Option<T>) -> Self {
            match opt {
                Some(value) => Self::wrap(value),
                None => Self::new(),
            }
        }

        #[inline]
        pub fn is_empty(&self) -> bool {
            self.obj.is_null() || self.obj.is_undefined()
        }

        #[inline]
        pub fn as_option(&self) -> Option<T> {
            if self.is_empty() {
                None
            } else {
                Some(T::unchecked_from_js(self.obj.clone()))
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

        #[inline]
        pub fn unwrap(self) -> T {
            self.expect("called `JsOption::unwrap()` on an empty value")
        }

        #[inline]
        pub fn expect(self, msg: &str) -> T {
            match self.into_option() {
                Some(value) => value,
                None => panic!("{}", msg),
            }
        }

        #[inline]
        pub fn unwrap_or_default(self) -> T
        where
            T: Default,
        {
            self.into_option().unwrap_or_default()
        }

        #[inline]
        pub fn unwrap_or_else<F>(self, f: F) -> T
        where
            F: FnOnce() -> T,
        {
            self.into_option().unwrap_or_else(f)
        }
    }

    impl<T: JsGeneric> Default for JsOption<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: JsGeneric + fmt::Debug> fmt::Debug for JsOption<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}?(", core::any::type_name::<T>())?;
            match self.as_option() {
                Some(value) => write!(f, "{value:?}")?,
                None => f.write_str("null")?,
            }
            f.write_str(")")
        }
    }

    impl<T: JsGeneric + fmt::Display> fmt::Display for JsOption<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}?(", core::any::type_name::<T>())?;
            match self.as_option() {
                Some(value) => write!(f, "{value}")?,
                None => f.write_str("null")?,
            }
            f.write_str(")")
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

    impl_js_value_wire!(impl<T> for JsOption<T>, field obj);

    unsafe impl<T> crate::__rt::marker::ErasableGeneric for JsOption<T> {
        type Repr = JsValue;
    }

    impl<T: JsGeneric> IntoJsGeneric for JsOption<T> {
        type JsCanon = Self;

        fn to_js(self) -> Self {
            self
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

    macro_rules! promising_self {
        ($($ty:ty),* $(,)?) => {
            $(
                impl Promising for $ty {
                    type Resolution = $ty;
                }
            )*
        };
    }

    promising_self!(
        bool, char, f32, f64, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize,
        JsError
    );

    impl<T: Promising> Promising for Option<T> {
        type Resolution = Option<T::Resolution>;
    }

    impl<T: ErasableGeneric + Promising, E: ErasableGeneric> Promising for Result<T, E> {
        type Resolution = Result<T::Resolution, E>;
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

use crate::encode::CallbackKey;
use crate::function::RustCallback;
use crate::object_store::insert_object;

// Re-export function registry types
pub use crate::__rt::marker::ErasableGeneric;
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

/// Returns a handle to this Wasm instance's `WebAssembly.Instance`.
///
/// # Panics
/// This function always panics when running outside of WASM.
pub fn instance() -> JsValue {
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
    pub use crate::JsCast;
    pub use crate::JsError;
    pub use crate::JsValue;
    pub use crate::UnwrapThrowExt;
    pub use crate::WasmClosure;
    pub use crate::batch::batch;
    pub use crate::closure::{Closure, ScopedClosure};
    pub use crate::convert::Upcast;
    pub use crate::convert::{IntoJsGeneric, JsGeneric, UpcastFrom};
    pub use crate::encode::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef};
    pub use crate::function::JSFunction;
    pub use crate::lazy::JsThreadLocal;
    pub use crate::sys::{JsOption, Null, Promising, Undefined};
    pub use crate::wasm_bindgen;
}
