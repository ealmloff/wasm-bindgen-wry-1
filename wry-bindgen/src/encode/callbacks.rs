//! Callback key encoding and closure ABI implementations.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::convert::RefFromBinaryDecode;
use crate::ipc::{DecodeError, DecodedData, EncodedData};
use crate::object_store::ObjectHandle;
use crate::value::JsValue;
use crate::{
    Closure, IntoWasmClosure, IntoWasmClosureRef, IntoWasmClosureRefMut, WasmClosureFnOnce,
    WasmClosureFnOnceAbort,
};

use super::{BinaryDecode, BinaryEncode, EncodeTypeDef, IntoClosure, TypeTag};

/// Wrapper type that encodes a callback registration key with Callback type info.
/// This tells JS to create a RustFunction wrapper when decoding the value.
/// The type parameter F should be `dyn FnMut(...) -> R` to capture the callback signature.
#[derive(Clone, Copy)]
pub(crate) enum CallbackPolicy {
    RustOwned = 0,
    JsOwned = 1,
    JsOwnedOnce = 2,
}

pub struct CallbackKey<F: ?Sized>(ObjectHandle, CallbackPolicy, PhantomData<F>);

impl<F: ?Sized> CallbackKey<F> {
    /// Create a new CallbackKey from an ObjectHandle.
    pub(crate) fn new(handle: ObjectHandle) -> Self {
        Self::new_with_policy(handle, CallbackPolicy::JsOwned)
    }

    pub(crate) fn new_with_policy(handle: ObjectHandle, policy: CallbackPolicy) -> Self {
        CallbackKey(handle, policy, PhantomData)
    }
}

impl<F: ?Sized> BinaryEncode for CallbackKey<F> {
    fn encode(self, encoder: &mut EncodedData) {
        self.0.encode(encoder);
        (self.1 as u32).encode(encoder);
    }
}

// Blanket impl: All Closures encode as HeapRef since they're JS heap references
impl<T: ?Sized> EncodeTypeDef for crate::ScopedClosure<'_, T> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        JsValue::encode_type_def(buf);
    }
}

/// Helper macro to decode callback arguments and execute a body.
///
/// Usage: decode_args!(decoder; [type1, type2, ...] => body)
/// The body can use the type names as variables containing the decoded arguments.
macro_rules! decode_args {
    // Main entry: decode each arg and call body. A decode failure propagates as
    // `Err` from the enclosing callback closure (which returns
    // `Result<(), DecodeError>`) instead of panicking via `unwrap`.
    ($decoder:expr; [$first:ident, $($ty:ident,)*] => $body:expr) => {{
        #[allow(non_snake_case)]
        let $first = <$first as BinaryDecode>::decode($decoder)?;
        decode_args!($decoder; [$($ty,)*] => $body);
    }};
    // Nothing left to decode: run the body, then signal success to the closure.
    ($decoder:expr; [] => $body:expr) => {{
        $body;
        return Ok(());
    }};
}

/// Emit the body of an `EncodeTypeDef::encode_type_def` for a callback type.
///
/// Writes `[Callback tag] [arg count] [arg TypeDefs...] [return TypeDef]`. The
/// optional `borrow_first` flag (when present) pushes a `BorrowedRef` tag for the
/// first argument instead of its `EncodeTypeDef`, used by the borrowed-first-arg
/// closures.
macro_rules! callback_type_def_body {
    ($buf:expr; R = $R:ty; $($arg:ty),*) => {{
        $buf.push(TypeTag::Callback as u8);
        // Encode arg count
        let mut count: u8 = 0;
        $(
            let _ = PhantomData::<$arg>;
            count += 1;
        )*
        $buf.push(count);
        // Encode each argument type
        $(<$arg as EncodeTypeDef>::encode_type_def($buf);)*
        // Encode return type
        <$R as EncodeTypeDef>::encode_type_def($buf);
    }};
    // Borrowed-first-arg variant: the first argument is a borrowed ref encoded
    // as a `BorrowedRef` tag, the remaining `$rest` args use their `EncodeTypeDef`.
    ($buf:expr; R = $R:ty; borrow_first; $($rest:ty),*) => {{
        $buf.push(TypeTag::Callback as u8);
        // Encode arg count (starts at 1 for the borrowed first arg)
        let mut count: u8 = 1;
        $(
            let _ = PhantomData::<$rest>;
            count += 1;
        )*
        $buf.push(count);
        // Encode each argument type
        $buf.push(TypeTag::BorrowedRef as u8);
        $(<$rest as EncodeTypeDef>::encode_type_def($buf);)*
        // Encode return type
        <$R as EncodeTypeDef>::encode_type_def($buf);
    }};
}

macro_rules! impl_fnmut_stub {
    ($($arg:ident),*) => {
        // Implement EncodeTypeDef for fn(owned*) -> R
        impl<R, $($arg,)*> EncodeTypeDef for CallbackKey<fn($($arg),*) -> R>
            where
            $($arg: EncodeTypeDef + 'static, )*
            R: EncodeTypeDef + 'static,
        {
            #[allow(unused)]
            fn encode_type_def(buf: &mut Vec<u8>) {
                callback_type_def_body!(buf; R = R; $($arg),*);
            }
        }

        // Implement WasmClosure trait for dyn FnMut variants
        impl<R, $($arg,)*> crate::WryWasmClosure<fn($($arg),*) -> R> for dyn FnMut($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_js_closure(mut boxed: Box<Self>) -> crate::Closure<Self> {
                crate::Closure::wrap_encode_decode_mut::<fn($($arg),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        // Decode arguments and call the closure
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = boxed($($arg),*);
                            result.encode(encoder);
                        });
                    },
                )
            }
        }

        impl<R, $($arg,)*> crate::WasmClosure for dyn FnMut($($arg),*) -> R
            where
            $($arg: 'static, )*
            R: 'static,
        {
            type Static = dyn FnMut($($arg),*) -> R;
            type AsMut = dyn FnMut($($arg),*) -> R;
        }

        // Implement WasmClosure trait for dyn Fn variants (immutable closures)
        // These CAN be called reentrantly since Fn only needs &self
        impl<R, $($arg,)*> crate::WryWasmClosure<fn($($arg),*) -> R> for dyn Fn($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_js_closure(boxed: Box<Self>) -> crate::Closure<Self> {
                crate::Closure::wrap_encode_decode::<fn($($arg),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        // Decode arguments and call the closure
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = boxed($($arg),*);
                            result.encode(encoder);
                        });
                    }
                )
            }
        }

        impl<R, $($arg,)*> crate::WasmClosure for dyn Fn($($arg),*) -> R
            where
            $($arg: 'static, )*
            R: 'static,
        {
            type Static = dyn Fn($($arg),*) -> R;
            type AsMut = dyn FnMut($($arg),*) -> R;
        }

        // IntoClosure for F: FnMut -> Closure<dyn FnMut>
        impl<R, F, $($arg,)*> IntoClosure<fn($($arg),*) -> R, crate::Closure<dyn FnMut($($arg),*) -> R>> for F
            where F: FnMut($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_closure(mut self) -> crate::Closure<dyn FnMut($($arg),*) -> R> {
                crate::Closure::wrap_encode_decode_mut::<fn($($arg),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        // Decode arguments and call the closure
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = self($($arg),*);
                            result.encode(encoder);
                        });
                    },
                )
            }
        }

        // IntoClosure for F: Fn -> Closure<dyn Fn>
        impl<R, F, $($arg,)*> IntoClosure<fn($($arg),*) -> R, crate::Closure<dyn Fn($($arg),*) -> R>> for F
            where F: Fn($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_closure(self) -> crate::Closure<dyn Fn($($arg),*) -> R> {
                crate::Closure::wrap_encode_decode::<fn($($arg),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        // Decode arguments and call the closure
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = self($($arg),*);
                            result.encode(encoder);
                        });
                    },
                )
            }
        }

        impl<R, F, $($arg,)*> IntoWasmClosure<dyn FnMut($($arg),*) -> R> for F
            where F: FnMut($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure(self) -> crate::Closure<dyn FnMut($($arg),*) -> R> {
                <F as IntoClosure<fn($($arg),*) -> R, crate::Closure<dyn FnMut($($arg),*) -> R>>>::into_closure(self)
            }

            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn FnMut($($arg),*) -> R> {
                <F as IntoWasmClosure<dyn FnMut($($arg),*) -> R>>::into_closure(*self)
            }
        }

        impl<R, $($arg,)*> IntoWasmClosure<dyn FnMut($($arg),*) -> R> for dyn FnMut($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn FnMut($($arg),*) -> R> {
                <Self as crate::WryWasmClosure<fn($($arg),*) -> R>>::into_js_closure(self)
            }
        }

        impl<R, F, $($arg,)*> IntoWasmClosure<dyn Fn($($arg),*) -> R> for F
            where F: Fn($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure(self) -> crate::Closure<dyn Fn($($arg),*) -> R> {
                <F as IntoClosure<fn($($arg),*) -> R, crate::Closure<dyn Fn($($arg),*) -> R>>>::into_closure(self)
            }

            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn Fn($($arg),*) -> R> {
                <F as IntoWasmClosure<dyn Fn($($arg),*) -> R>>::into_closure(*self)
            }
        }

        impl<R, $($arg,)*> IntoWasmClosure<dyn Fn($($arg),*) -> R> for dyn Fn($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn Fn($($arg),*) -> R> {
                <Self as crate::WryWasmClosure<fn($($arg),*) -> R>>::into_js_closure(self)
            }
        }

        impl<R, F, $($arg,)*> IntoWasmClosureRef<dyn Fn($($arg),*) -> R> for F
            where F: Fn($($arg),*) -> R,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_scoped_closure_ref<'a>(t: &'a Self) -> crate::ScopedClosure<'a, <dyn Fn($($arg),*) -> R as crate::WasmClosure>::Static> {
                let t: &(dyn Fn($($arg),*) -> R) = t;
                let ptr = t as *const dyn Fn($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };
                let callback = crate::function::RustCallback::new_fn(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *const dyn Fn($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &dyn Fn($($arg),*) -> R = unsafe { &*ptr };
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = f($($arg),*);
                            result.encode(encoder);
                        });
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let value = crate::__rt::wbg_cast::<CallbackKey<fn($($arg),*) -> R>, crate::JsValue>(
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned),
                );
                crate::ScopedClosure {
                    _phantom: PhantomData,
                    callback: crate::closure::CallbackOwnership::Owned,
                    value,
                }
            }
        }

        impl<R, $($arg,)*> IntoWasmClosureRef<dyn Fn($($arg),*) -> R> for dyn Fn($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_scoped_closure_ref<'a>(t: &'a Self) -> crate::ScopedClosure<'a, <dyn Fn($($arg),*) -> R as crate::WasmClosure>::Static> {
                let ptr = t as *const dyn Fn($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };
                let callback = crate::function::RustCallback::new_fn(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *const dyn Fn($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &dyn Fn($($arg),*) -> R = unsafe { &*ptr };
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = f($($arg),*);
                            result.encode(encoder);
                        });
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let value = crate::__rt::wbg_cast::<CallbackKey<fn($($arg),*) -> R>, crate::JsValue>(
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned),
                );
                crate::ScopedClosure {
                    _phantom: PhantomData,
                    callback: crate::closure::CallbackOwnership::Owned,
                    value,
                }
            }
        }

        impl<R, F, $($arg,)*> IntoWasmClosureRefMut<dyn FnMut($($arg),*) -> R> for F
            where F: FnMut($($arg),*) -> R,
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_scoped_closure_ref_mut<'a>(t: &'a mut Self) -> crate::ScopedClosure<'a, <dyn FnMut($($arg),*) -> R as crate::WasmClosure>::Static> {
                let t: &mut dyn FnMut($($arg),*) -> R = t;
                let ptr = t as *mut dyn FnMut($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };
                let callback = crate::function::RustCallback::new_fn_mut(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *mut dyn FnMut($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &mut dyn FnMut($($arg),*) -> R = unsafe { &mut *ptr };
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = f($($arg),*);
                            result.encode(encoder);
                        });
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let value = crate::__rt::wbg_cast::<CallbackKey<fn($($arg),*) -> R>, crate::JsValue>(
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned),
                );
                crate::ScopedClosure {
                    _phantom: PhantomData,
                    callback: crate::closure::CallbackOwnership::Owned,
                    value,
                }
            }
        }

        impl<R, $($arg,)*> IntoWasmClosureRefMut<dyn FnMut($($arg),*) -> R> for dyn FnMut($($arg),*) -> R
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_scoped_closure_ref_mut<'a>(t: &'a mut Self) -> crate::ScopedClosure<'a, <dyn FnMut($($arg),*) -> R as crate::WasmClosure>::Static> {
                let ptr = t as *mut dyn FnMut($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };
                let callback = crate::function::RustCallback::new_fn_mut(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *mut dyn FnMut($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &mut dyn FnMut($($arg),*) -> R = unsafe { &mut *ptr };
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = f($($arg),*);
                            result.encode(encoder);
                        });
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let value = crate::__rt::wbg_cast::<CallbackKey<fn($($arg),*) -> R>, crate::JsValue>(
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned),
                );
                crate::ScopedClosure {
                    _phantom: PhantomData,
                    callback: crate::closure::CallbackOwnership::Owned,
                    value,
                }
            }
        }
    };
}

/// Emit a `BinaryEncode` impl for a closure-reference type.
///
/// The closure reference is decomposed into a raw fat pointer (data + vtable) to
/// erase its lifetime, registered as a `RustCallback`, and shipped to JS as a
/// `CallbackKey`. SAFETY across all variants: the closure reference must remain
/// valid for the duration of the JS call, which holds because `mark_needs_flush`
/// forces synchronous invocation before this function returns.
///
/// Variants differ only in the closure trait (`Fn`/`FnMut`), the pointer
/// mutability used to reconstruct it, and the `RustCallback` constructor.
macro_rules! impl_closure_ref_binary_encode {
    (
        impl ($($self_ty:tt)*) via *mut dyn FnMut, $ctor:ident;
        $($arg:ident),*
    ) => {
        impl<R, $($arg,)*> BinaryEncode for $($self_ty)*
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn encode(self, encoder: &mut EncodedData) {
                encoder.mark_needs_flush();

                let ptr = self as *mut dyn FnMut($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };

                let callback = crate::function::RustCallback::$ctor(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *mut dyn FnMut($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &mut dyn FnMut($($arg),*) -> R = unsafe { &mut *ptr };
                        $(let $arg = <$arg as BinaryDecode>::decode(decoder)?;)*
                        let result = f($($arg),*);
                        result.encode(encoder);
                        Ok(())
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let key: CallbackKey<fn($($arg),*) -> R> =
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned);
                key.encode(encoder);
                crate::batch::queue_rust_object_drop(handle);
            }
        }
    };
    (
        impl ($($self_ty:tt)*) via *const dyn Fn, $ctor:ident;
        $($arg:ident),*
    ) => {
        impl<R, $($arg,)*> BinaryEncode for $($self_ty)*
            where
            $($arg: BinaryDecode + EncodeTypeDef + 'static, )*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn encode(self, encoder: &mut EncodedData) {
                encoder.mark_needs_flush();

                let ptr = self as *const dyn Fn($($arg),*) -> R;
                let (data_ptr, vtable_ptr): (usize, usize) = unsafe { core::mem::transmute(ptr) };

                let callback = crate::function::RustCallback::$ctor(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let ptr: *const dyn Fn($($arg),*) -> R = unsafe {
                            core::mem::transmute((data_ptr, vtable_ptr))
                        };
                        let f: &dyn Fn($($arg),*) -> R = unsafe { &*ptr };
                        $(let $arg = <$arg as BinaryDecode>::decode(decoder)?;)*
                        let result = f($($arg),*);
                        result.encode(encoder);
                        Ok(())
                    },
                );
                let handle = crate::object_store::insert_object(callback);
                let key: CallbackKey<fn($($arg),*) -> R> =
                    CallbackKey::new_with_policy(handle, CallbackPolicy::RustOwned);
                key.encode(encoder);
                crate::batch::queue_rust_object_drop(handle);
            }
        }
    };
}

/// Macro to implement EncodeTypeDef and BinaryEncode for closure reference types.
/// These are used by js-sys bindings like `&mut dyn FnMut(JsValue, u32, Array) -> bool`.
/// Unlike the WasmClosure impls above, these use simple BinaryDecode arguments without markers.
macro_rules! impl_closure_ref_encode {
    ($($arg:ident),*) => {
        // Implement EncodeTypeDef for &mut dyn FnMut(...) -> R
        impl<R, $($arg,)*> EncodeTypeDef for &mut dyn FnMut($($arg),*) -> R
            where
            $($arg: EncodeTypeDef + 'static, )*
            R: EncodeTypeDef + 'static,
        {
            #[allow(unused)]
            fn encode_type_def(buf: &mut Vec<u8>) {
                callback_type_def_body!(buf; R = R; $($arg),*);
            }
        }

        // Implement BinaryEncode for &mut dyn FnMut(...) -> R
        impl_closure_ref_binary_encode!(
            impl (&mut dyn FnMut($($arg),*) -> R) via *mut dyn FnMut, new_fn_mut;
            $($arg),*
        );

        // Implement EncodeTypeDef for &dyn Fn(...) -> R
        impl<R, $($arg,)*> EncodeTypeDef for &dyn Fn($($arg),*) -> R
            where
            $($arg: EncodeTypeDef + 'static, )*
            R: EncodeTypeDef + 'static,
        {
            #[allow(unused)]
            fn encode_type_def(buf: &mut Vec<u8>) {
                callback_type_def_body!(buf; R = R; $($arg),*);
            }
        }

        // Implement BinaryEncode for &dyn Fn(...) -> R (supports reentrant calls)
        impl_closure_ref_binary_encode!(
            impl (&dyn Fn($($arg),*) -> R) via *const dyn Fn, new_fn;
            $($arg),*
        );

        // Implement EncodeTypeDef for &mut dyn Fn(...) -> R
        impl<R, $($arg,)*> EncodeTypeDef for &mut dyn Fn($($arg),*) -> R
            where
            $($arg: EncodeTypeDef + 'static, )*
            R: EncodeTypeDef + 'static,
        {
            #[allow(unused)]
            fn encode_type_def(buf: &mut Vec<u8>) {
                callback_type_def_body!(buf; R = R; $($arg),*);
            }
        }

        // Implement BinaryEncode for &mut dyn Fn(...) -> R (supports reentrant calls)
        // Uses *const because Fn only requires & to call.
        impl_closure_ref_binary_encode!(
            impl (&mut dyn Fn($($arg),*) -> R) via *const dyn Fn, new_fn;
            $($arg),*
        );
    };
}

impl_closure_ref_encode!();
impl_closure_ref_encode!(A1);
impl_closure_ref_encode!(A1, A2);
impl_closure_ref_encode!(A1, A2, A3);
impl_closure_ref_encode!(A1, A2, A3, A4);
impl_closure_ref_encode!(A1, A2, A3, A4, A5);
impl_closure_ref_encode!(A1, A2, A3, A4, A5, A6);
impl_closure_ref_encode!(A1, A2, A3, A4, A5, A6, A7);

impl_fnmut_stub!();
impl_fnmut_stub!(A1);
impl_fnmut_stub!(A1, A2);
impl_fnmut_stub!(A1, A2, A3);
impl_fnmut_stub!(A1, A2, A3, A4);
impl_fnmut_stub!(A1, A2, A3, A4, A5);
impl_fnmut_stub!(A1, A2, A3, A4, A5, A6);
impl_fnmut_stub!(A1, A2, A3, A4, A5, A6, A7);
impl_fnmut_stub!(A1, A2, A3, A4, A5, A6, A7, A8);

/// Marker type for closures that borrow the first argument.
pub struct BorrowedFirstArg;

/// Macro to implement WasmClosure and IntoClosure for closures that borrow the first argument.
/// This uses RefFromBinaryDecode for the first arg and BinaryDecode for the rest.
macro_rules! impl_fnmut_stub_ref {
    ($first:ident $(, $rest:ident)*) => {
        // Implement EncodeTypeDef for fn(borrowed, owned*) -> R
        #[allow(coherence_leak_check)]
        impl<R, $first, $($rest,)*> EncodeTypeDef for CallbackKey<fn(&$first, $($rest),*) -> R>
            where
            $first: EncodeTypeDef + 'static,
            $($rest: EncodeTypeDef + 'static, )*
            R: EncodeTypeDef + 'static,
        {
            #[allow(unused)]
            fn encode_type_def(buf: &mut Vec<u8>) {
                callback_type_def_body!(buf; R = R; borrow_first; $($rest),*);
            }
        }

        // WasmClosure for dyn FnMut(&First, ...) -> R
        impl<R, $first, $($rest,)*> crate::WryWasmClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R)> for dyn FnMut(&$first, $($rest),*) -> R
            where
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_js_closure(mut boxed: Box<Self>) -> crate::Closure<Self> {
                crate::Closure::wrap_encode_decode_mut::<fn(&$first, $($rest),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let anchor = <$first as RefFromBinaryDecode>::ref_decode(decoder)?;
                        $(let $rest = <$rest as BinaryDecode>::decode(decoder)?;)*
                        let result = boxed(&*anchor, $($rest),*);
                        result.encode(encoder);
                        Ok(())
                    },
                )
            }
        }

        // Trait objects like `dyn FnMut(&Event)` are commonly inferred as
        // higher-ranked over the borrowed argument lifetime.
        #[allow(coherence_leak_check)]
        impl<R, $first, $($rest,)*> crate::WasmClosure for dyn FnMut(&$first, $($rest),*) -> R
            where
            $first: 'static,
            $($rest: 'static,)*
            R: 'static,
        {
            type Static = dyn FnMut(&$first, $($rest),*) -> R;
            type AsMut = dyn FnMut(&$first, $($rest),*) -> R;
        }

        // WasmClosure for dyn Fn(&First, ...) -> R (supports reentrant calls)
        impl<R, $first, $($rest,)*> crate::WryWasmClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R)> for dyn Fn(&$first, $($rest),*) -> R
            where
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_js_closure(boxed: Box<Self>) -> crate::Closure<Self> {
                crate::Closure::wrap_encode_decode::<fn(&$first, $($rest),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let anchor = <$first as RefFromBinaryDecode>::ref_decode(decoder)?;
                        $(let $rest = <$rest as BinaryDecode>::decode(decoder)?;)*
                        let result = boxed(&*anchor, $($rest),*);
                        result.encode(encoder);
                        Ok(())
                    },
                )
            }
        }

        #[allow(coherence_leak_check)]
        impl<R, $first, $($rest,)*> crate::WasmClosure for dyn Fn(&$first, $($rest),*) -> R
            where
            $first: 'static,
            $($rest: 'static,)*
            R: 'static,
        {
            type Static = dyn Fn(&$first, $($rest),*) -> R;
            type AsMut = dyn FnMut(&$first, $($rest),*) -> R;
        }

        // IntoClosure for F: FnMut(&First, ...) -> R -> Closure<dyn FnMut(&First, ...) -> R>
        impl<R, F, $first, $($rest,)*> IntoClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R), crate::Closure<dyn FnMut(&$first, $($rest),*) -> R>> for F
            where F: FnMut(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_closure(mut self) -> crate::Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                crate::Closure::wrap_encode_decode_mut::<fn(&$first, $($rest),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let anchor = <$first as RefFromBinaryDecode>::ref_decode(decoder)?;
                        $(let $rest = <$rest as BinaryDecode>::decode(decoder)?;)*
                        let result = self(&*anchor, $($rest),*);
                        result.encode(encoder);
                        Ok(())
                    },
                )
            }
        }

        // IntoClosure for F: Fn(&First, ...) -> R -> Closure<dyn Fn(&First, ...) -> R>
        impl<R, F, $first, $($rest,)*> IntoClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R), crate::Closure<dyn Fn(&$first, $($rest),*) -> R>> for F
            where F: Fn(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused)]
            fn into_closure(self) -> crate::Closure<dyn Fn(&$first, $($rest),*) -> R> {
                crate::Closure::wrap_encode_decode::<fn(&$first, $($rest),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let anchor = <$first as RefFromBinaryDecode>::ref_decode(decoder)?;
                        $(let $rest = <$rest as BinaryDecode>::decode(decoder)?;)*
                        let result = self(&*anchor, $($rest),*);
                        result.encode(encoder);
                        Ok(())
                    },
                )
            }
        }

        #[allow(coherence_leak_check)]
        impl<R, F, $first, $($rest,)*> IntoWasmClosure<dyn FnMut(&$first, $($rest),*) -> R> for F
            where F: FnMut(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure(self) -> crate::Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                <F as IntoClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R), crate::Closure<dyn FnMut(&$first, $($rest),*) -> R>>>::into_closure(self)
            }

            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                <F as IntoWasmClosure<dyn FnMut(&$first, $($rest),*) -> R>>::into_closure(*self)
            }
        }

        #[allow(coherence_leak_check)]
        impl<R, $first, $($rest,)*> IntoWasmClosure<dyn FnMut(&$first, $($rest),*) -> R> for dyn FnMut(&$first, $($rest),*) -> R
            where
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                <Self as crate::WryWasmClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R)>>::into_js_closure(self)
            }
        }

        #[allow(coherence_leak_check)]
        impl<R, F, $first, $($rest,)*> IntoWasmClosure<dyn Fn(&$first, $($rest),*) -> R> for F
            where F: Fn(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure(self) -> crate::Closure<dyn Fn(&$first, $($rest),*) -> R> {
                <F as IntoClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R), crate::Closure<dyn Fn(&$first, $($rest),*) -> R>>>::into_closure(self)
            }

            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn Fn(&$first, $($rest),*) -> R> {
                <F as IntoWasmClosure<dyn Fn(&$first, $($rest),*) -> R>>::into_closure(*self)
            }
        }

        #[allow(coherence_leak_check)]
        impl<R, $first, $($rest,)*> IntoWasmClosure<dyn Fn(&$first, $($rest),*) -> R> for dyn Fn(&$first, $($rest),*) -> R
            where
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            fn into_closure_box(self: Box<Self>) -> crate::Closure<dyn Fn(&$first, $($rest),*) -> R> {
                <Self as crate::WryWasmClosure<(BorrowedFirstArg, fn(&$first, $($rest),*) -> R)>>::into_js_closure(self)
            }
        }
    };
}

impl_fnmut_stub_ref!(A1);
impl_fnmut_stub_ref!(A1, A2);
impl_fnmut_stub_ref!(A1, A2, A3);
impl_fnmut_stub_ref!(A1, A2, A3, A4);
impl_fnmut_stub_ref!(A1, A2, A3, A4, A5);
impl_fnmut_stub_ref!(A1, A2, A3, A4, A5, A6);
impl_fnmut_stub_ref!(A1, A2, A3, A4, A5, A6, A7);
impl_fnmut_stub_ref!(A1, A2, A3, A4, A5, A6, A7, A8);

/// Macro to implement WasmClosureFnOnce for FnOnce closures of various arities.
/// This wraps an FnOnce in an FnMut that panics if called more than once.
macro_rules! impl_fn_once {
    ($($arg:ident),*) => {
        impl<R, F, $($arg,)*> WasmClosureFnOnce<dyn FnMut($($arg),*) -> R, fn($($arg),*) -> R, R> for F
        where
            F: FnOnce($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused_variables)]
            fn into_closure(self) -> Closure<dyn FnMut($($arg),*) -> R> {
                // Use Option to allow taking the FnOnce
                let mut me = Some(self);
                // Register the callback using the same pattern as impl_fnmut_stub
                crate::Closure::wrap_once_encode_decode_mut::<fn($($arg),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let f = me.take().expect("FnOnce closure called more than once");
                        decode_args!(decoder; [$($arg,)*] => {
                            let result = f($($arg),*);
                            result.encode(encoder);
                        });
                    },
                )
            }
        }

        impl<R, F, $($arg,)*> WasmClosureFnOnceAbort<dyn FnMut($($arg),*) -> R, fn($($arg),*) -> R, R> for F
        where
            F: FnOnce($($arg),*) -> R + 'static,
            $($arg: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused_variables)]
            fn into_closure(self) -> Closure<dyn FnMut($($arg),*) -> R> {
                <F as WasmClosureFnOnce<dyn FnMut($($arg),*) -> R, fn($($arg),*) -> R, R>>::into_closure(self)
            }
        }
    };
}

impl_fn_once!();
impl_fn_once!(A1);
impl_fn_once!(A1, A2);
impl_fn_once!(A1, A2, A3);
impl_fn_once!(A1, A2, A3, A4);
impl_fn_once!(A1, A2, A3, A4, A5);
impl_fn_once!(A1, A2, A3, A4, A5, A6);
impl_fn_once!(A1, A2, A3, A4, A5, A6, A7);
impl_fn_once!(A1, A2, A3, A4, A5, A6, A7, A8);

/// Macro to implement WasmClosureFnOnce for FnOnce closures that borrow the first argument.
/// This uses RefFromBinaryDecode for the first arg and BinaryDecode for the rest.
macro_rules! impl_fn_once_ref {
    ($first:ident $(, $rest:ident)*) => {
        impl<R, F, $first, $($rest,)*> WasmClosureFnOnce<dyn FnMut(&$first, $($rest),*) -> R, (BorrowedFirstArg, fn(&$first, $($rest),*) -> R), R> for F
        where
            F: FnOnce(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused_variables)]
            fn into_closure(self) -> Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                let mut me = Some(self);
                crate::Closure::wrap_once_encode_decode_mut::<fn(&$first, $($rest),*) -> R>(
                    move |decoder: &mut DecodedData, encoder: &mut EncodedData| {
                        let f = me.take().expect("FnOnce closure called more than once");
                        let anchor = <$first as RefFromBinaryDecode>::ref_decode(decoder)?;
                        $(let $rest = <$rest as BinaryDecode>::decode(decoder)?;)*
                        let result = f(&*anchor, $($rest),*);
                        result.encode(encoder);
                        Ok(())
                    },
                )
            }
        }

        impl<R, F, $first, $($rest,)*> WasmClosureFnOnceAbort<dyn FnMut(&$first, $($rest),*) -> R, (BorrowedFirstArg, fn(&$first, $($rest),*) -> R), R> for F
        where
            F: FnOnce(&$first, $($rest),*) -> R + 'static,
            $first: RefFromBinaryDecode + EncodeTypeDef + 'static,
            $($rest: BinaryDecode + EncodeTypeDef + 'static,)*
            R: BinaryEncode + EncodeTypeDef + 'static,
        {
            #[allow(non_snake_case)]
            #[allow(unused_variables)]
            fn into_closure(self) -> Closure<dyn FnMut(&$first, $($rest),*) -> R> {
                <F as WasmClosureFnOnce<dyn FnMut(&$first, $($rest),*) -> R, (BorrowedFirstArg, fn(&$first, $($rest),*) -> R), R>>::into_closure(self)
            }
        }
    };
}

impl_fn_once_ref!(A1);
impl_fn_once_ref!(A1, A2);
impl_fn_once_ref!(A1, A2, A3);
impl_fn_once_ref!(A1, A2, A3, A4);
impl_fn_once_ref!(A1, A2, A3, A4, A5);
impl_fn_once_ref!(A1, A2, A3, A4, A5, A6);
impl_fn_once_ref!(A1, A2, A3, A4, A5, A6, A7);
impl_fn_once_ref!(A1, A2, A3, A4, A5, A6, A7, A8);

impl<F: ?Sized> BinaryDecode for crate::Closure<F> {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        // Decode the JsValue wrapping the closure
        let value = <crate::JsValue as BinaryDecode>::decode(decoder)?;
        Ok(Self {
            _phantom: PhantomData,
            callback: crate::closure::CallbackOwnership::None,
            value,
        })
    }
}

impl<F: ?Sized> BinaryEncode for crate::Closure<F> {
    fn encode(mut self, encoder: &mut EncodedData) {
        if self.callback.needs_flush() {
            encoder.mark_needs_flush();
        }
        // Hand the closure off to JS: ScopedClosure::drop must not dispose.
        // JsValue::drop still queues the heap-ref release.
        self.callback.detach();
        (&self.value).encode(encoder);
    }
}

impl<F: ?Sized> BinaryEncode for &crate::ScopedClosure<'_, F> {
    fn encode(self, encoder: &mut EncodedData) {
        if self.callback.needs_flush() {
            encoder.mark_needs_flush();
        }
        // Encode the JsValue
        (&self.value).encode(encoder);
    }
}
