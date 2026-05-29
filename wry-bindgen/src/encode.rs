//! Core encoding and decoding traits for the binary protocol.
//!
//! This module provides traits for serializing and deserializing Rust types
//! to/from the binary IPC protocol.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::batch::{Runtime, with_runtime};
use crate::convert::RefFromBinaryDecode;
use crate::ipc::{DecodeError, DecodedData, EncodedData};
use crate::object_store::ObjectHandle;
use crate::value::JsValue;
use crate::{
    Closure, IntoWasmClosure, IntoWasmClosureRef, IntoWasmClosureRefMut, WasmClosureFnOnce,
    WasmClosureFnOnceAbort,
};

/// Trait for encoding Rust values into the binary protocol.
/// Each type specifies how to serialize itself.
pub trait BinaryEncode<P = ()> {
    fn encode(self, encoder: &mut EncodedData);
}

/// Trait for decoding values from the binary protocol.
/// Each type specifies how to deserialize itself.
pub trait BinaryDecode: Sized {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError>;
}

/// Trait for converting a closure into a Closure wrapper.
/// This trait is used instead of `From` to allow blanket implementations
/// for all closure types without conflicting with other `From` impls.
/// Output is a generic parameter (not associated type) to allow implementing
/// the trait multiple times for the same type with different outputs.
pub trait IntoClosure<M, Output> {
    fn into_closure(self) -> Output;
}

/// Trait for return types that can be used in batched JS calls.
/// Determines how the type behaves during batching.
pub trait BatchableResult: BinaryDecode {
    /// Returns Some(placeholder) for opaque types that can be batched,
    /// None for types that require flushing to get the actual value.
    ///
    /// For opaque types (JsValue, Closure), this reserves a heap ID and returns a placeholder.
    /// For trivial types like (), this returns the known value.
    /// For value types (primitives, String, Vec, etc.), returns None to trigger a flush.
    ///
    /// Default implementation returns None (requires flush).
    fn try_placeholder(_: &mut Runtime) -> Option<Self> {
        None
    }
}

/// Marker for cached type definition (type already sent, reference by ID).
/// Format: [TYPE_CACHED] [type_id: u32]
pub(crate) const TYPE_CACHED: u8 = 0xFF;

/// Marker for full type definition (first time sending this type signature).
/// Format: [TYPE_FULL] [type_id: u32] [param_count: u8] [param TypeDefs...] [return TypeDef]
pub(crate) const TYPE_FULL: u8 = 0xFE;

/// Type tags for the binary type definition protocol.
/// Used to encode type information that JavaScript can parse to create TypeClass instances.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeTag {
    // Primitive types
    Null = 0,
    Bool = 1,
    U8 = 2,
    U16 = 3,
    U32 = 4,
    U64 = 5,
    U128 = 6,
    I8 = 7,
    I16 = 8,
    I32 = 9,
    I64 = 10,
    I128 = 11,
    F32 = 12,
    F64 = 13,
    Usize = 14,
    Isize = 15,
    String = 16,
    HeapRef = 17,
    // Compound types
    /// Callback type: followed by param_count (u8), param TypeDefs..., return TypeDef
    Callback = 18,
    /// Option type: followed by inner TypeDef. Encodes as u8 flag (0=None, 1=Some) + value if Some
    Option = 19,
    /// Result type: followed by ok TypeDef and err TypeDef. Encodes as u8 flag (0=Err, 1=Ok) + value
    Result = 20,
    /// Array type: followed by element TypeDef. Encodes as u32 length + elements
    Array = 21,
    /// Borrowed reference: uses the borrow stack (indices 1-127) instead of the heap.
    /// Automatically cleaned up after each operation completes.
    BorrowedRef = 22,
    /// Clamped u8 array type: represents Uint8ClampedArray in JS.
    /// Element type is always u8. Encodes as u32 length + u8 elements.
    U8Clamped = 23,
    /// String enum type: encodes as u32 index, but type def includes variant strings.
    /// Format: [StringEnum tag] [variant_count: u8] [for each: string_len: u32, string_bytes...]
    /// Values encode as u32 discriminant. JS decodes using the lookup array.
    StringEnum = 24,
}

/// Trait for types that can encode their type definition into the binary protocol.
/// This is used to send type information to JavaScript for callback arguments.
pub trait EncodeTypeDef {
    /// Encode this type's definition into the buffer.
    /// For primitives, this is just the TypeTag byte.
    /// For callbacks, this includes param count, param types, and return type.
    fn encode_type_def(buf: &mut Vec<u8>);
}

// Unit type implementations

impl BatchableResult for () {
    fn try_placeholder(_: &mut Runtime) -> Option<Self> {
        Some(())
    }
}

impl EncodeTypeDef for () {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::Null as u8);
    }
}

impl BinaryEncode for () {
    fn encode(self, _encoder: &mut EncodedData) {
        // Unit type encodes as nothing
    }
}

impl BinaryDecode for () {
    fn decode(_decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(())
    }
}

impl EncodeTypeDef for bool {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::Bool as u8);
    }
}

impl BinaryEncode for bool {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u8(if self { 1 } else { 0 });
    }
}

impl BinaryDecode for bool {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u8()? != 0)
    }
}

impl EncodeTypeDef for char {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U32 as u8);
    }
}

impl BinaryEncode for char {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self as u32);
    }
}

impl BinaryDecode for char {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        char::from_u32(decoder.take_u32()?)
            .ok_or_else(|| DecodeError::Custom("invalid char scalar value".to_string()))
    }
}

impl EncodeTypeDef for u8 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U8 as u8);
    }
}

impl BinaryEncode for u8 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u8(self);
    }
}

impl BinaryDecode for u8 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        decoder.take_u8()
    }
}

impl EncodeTypeDef for u16 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U16 as u8);
    }
}

impl BinaryEncode for u16 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u16(self);
    }
}

impl BinaryDecode for u16 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        decoder.take_u16()
    }
}

impl EncodeTypeDef for u32 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U32 as u8);
    }
}

impl BinaryEncode for u32 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self);
    }
}

impl BinaryDecode for u32 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        decoder.take_u32()
    }
}

impl EncodeTypeDef for u64 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U64 as u8);
    }
}

impl BinaryEncode for u64 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self);
    }
}

impl BinaryDecode for u64 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        decoder.take_u64()
    }
}

impl EncodeTypeDef for u128 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U128 as u8);
    }
}

impl BinaryEncode for u128 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u128(self);
    }
}

impl BinaryDecode for u128 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        decoder.take_u128()
    }
}

impl EncodeTypeDef for i8 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::I8 as u8);
    }
}

impl BinaryEncode for i8 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u8(self as u8);
    }
}

impl BinaryDecode for i8 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u8()? as i8)
    }
}

impl EncodeTypeDef for i16 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::I16 as u8);
    }
}

impl BinaryEncode for i16 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u16(self as u16);
    }
}

impl BinaryDecode for i16 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u16()? as i16)
    }
}

impl EncodeTypeDef for i32 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::I32 as u8);
    }
}

impl BinaryEncode for i32 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self as u32);
    }
}

impl BinaryDecode for i32 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u32()? as i32)
    }
}

impl EncodeTypeDef for i64 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::I64 as u8);
    }
}

impl BinaryEncode for i64 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self as u64);
    }
}

impl BinaryDecode for i64 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u64()? as i64)
    }
}

impl EncodeTypeDef for i128 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::I128 as u8);
    }
}

impl BinaryEncode for i128 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u128(self as u128);
    }
}

impl BinaryDecode for i128 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u128()? as i128)
    }
}

impl EncodeTypeDef for f32 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::F32 as u8);
    }
}

impl BinaryEncode for f32 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.to_bits());
    }
}

impl BinaryDecode for f32 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(f32::from_bits(decoder.take_u32()?))
    }
}

impl EncodeTypeDef for f64 {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::F64 as u8);
    }
}

impl BinaryEncode for f64 {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self.to_bits());
    }
}

impl BinaryDecode for f64 {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(f64::from_bits(decoder.take_u64()?))
    }
}

// usize implementations (uses u64 for portability)

impl EncodeTypeDef for usize {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::Usize as u8);
    }
}

impl BinaryEncode for usize {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self as u64);
    }
}

impl BinaryDecode for usize {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u64()? as usize)
    }
}

// isize implementations (uses i64 for portability)

impl EncodeTypeDef for isize {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::Isize as u8);
    }
}

impl BinaryEncode for isize {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u64(self as u64);
    }
}

impl BinaryDecode for isize {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_u64()? as isize)
    }
}

// String/str implementations

impl EncodeTypeDef for str {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::String as u8);
    }
}

// Explicit impl for &str since str is not Sized and blanket impl doesn't apply
impl EncodeTypeDef for &str {
    fn encode_type_def(buf: &mut Vec<u8>) {
        <str as EncodeTypeDef>::encode_type_def(buf);
    }
}

// Blanket impl for &T references
impl<T: EncodeTypeDef> EncodeTypeDef for &T {
    fn encode_type_def(buf: &mut Vec<u8>) {
        T::encode_type_def(buf);
    }
}

impl BinaryEncode for &str {
    fn encode(self, encoder: &mut EncodedData) {
        encode_str(self, encoder);
    }
}

impl EncodeTypeDef for String {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::String as u8);
    }
}

impl BinaryEncode for String {
    fn encode(self, encoder: &mut EncodedData) {
        encode_str(&self, encoder);
    }
}

impl BinaryDecode for String {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        Ok(decoder.take_str()?.to_string())
    }
}

fn encode_str(value: &str, encoder: &mut EncodedData) {
    #[cfg(feature = "enable-interning")]
    if let Some(id) = crate::intern::unsafe_get_str(value) {
        encoder.push_u32(crate::ipc::CACHED_STRING_SENTINEL);
        encoder.push_u64(id);
        return;
    }

    encoder.push_str(value);
}

impl<T: EncodeTypeDef> EncodeTypeDef for Option<T> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Option encodes as: [Option tag] [inner type]
        // Actual values encode as: [u8 flag (0=None, 1=Some)] [value if Some]
        buf.push(TypeTag::Option as u8);
        T::encode_type_def(buf);
    }
}

impl<T: BinaryDecode> BinaryDecode for Option<T> {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        let has_value = decoder.take_u8()? != 0;
        if has_value {
            Ok(Some(T::decode(decoder)?))
        } else {
            Ok(None)
        }
    }
}

// Encoding for Option<T> where T is encodable
impl<T: BinaryEncode<P>, P> BinaryEncode<P> for Option<T> {
    fn encode(self, encoder: &mut EncodedData) {
        match self {
            Some(val) => {
                encoder.push_u8(1);
                val.encode(encoder);
            }
            None => {
                encoder.push_u8(0);
            }
        }
    }
}

impl<T: BinaryDecode> BatchableResult for Option<T> {}

impl<T: EncodeTypeDef, E: EncodeTypeDef> EncodeTypeDef for Result<T, E> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Result encodes as: [Result tag] [ok type] [err type]
        buf.push(TypeTag::Result as u8);
        T::encode_type_def(buf);
        E::encode_type_def(buf);
    }
}

impl<T: BinaryEncode, E: BinaryEncode> BinaryEncode for Result<T, E> {
    fn encode(self, encoder: &mut EncodedData) {
        match self {
            Ok(value) => {
                encoder.push_u8(1);
                value.encode(encoder);
            }
            Err(error) => {
                encoder.push_u8(0);
                error.encode(encoder);
            }
        }
    }
}

impl<T: BinaryDecode, E: BinaryDecode> BinaryDecode for Result<T, E> {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        let is_ok = decoder.take_u8()? != 0;
        if is_ok {
            Ok(Ok(T::decode(decoder)?))
        } else {
            Ok(Err(E::decode(decoder)?))
        }
    }
}

impl<T: BinaryDecode, E: BinaryDecode> BatchableResult for Result<T, E> {}

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

impl<T: EncodeTypeDef> EncodeTypeDef for Vec<T> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Array type tag followed by element type
        buf.push(TypeTag::Array as u8);
        T::encode_type_def(buf);
    }
}

impl<T: EncodeTypeDef> EncodeTypeDef for &[T] {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Array type tag followed by element type
        buf.push(TypeTag::Array as u8);
        T::encode_type_def(buf);
    }
}

impl<T: EncodeTypeDef> EncodeTypeDef for &mut [T] {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Array type tag followed by element type
        buf.push(TypeTag::Array as u8);
        T::encode_type_def(buf);
    }
}

impl<T: EncodeTypeDef> EncodeTypeDef for Box<[T]> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        // Array type tag followed by element type
        buf.push(TypeTag::Array as u8);
        T::encode_type_def(buf);
    }
}

impl<T: BinaryEncode> BinaryEncode for Box<[T]> {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.len() as u32);
        for val in self.into_vec() {
            val.encode(encoder);
        }
    }
}

impl<T: BinaryEncode> BinaryEncode for Vec<T> {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.len() as u32);
        for val in self {
            val.encode(encoder);
        }
    }
}

impl<T: BinaryDecode> BinaryDecode for Vec<T> {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        let len = decoder.take_u32()? as usize;
        let mut vec = Vec::with_capacity(len);
        for _ in 0..len {
            vec.push(T::decode(decoder)?);
        }
        Ok(vec)
    }
}

impl<T: BinaryDecode> BatchableResult for Vec<T> {}

macro_rules! impl_jsgeneric_slice_encode {
    ($($slice:ty),* $(,)?) => {
        $(
            impl<T> BinaryEncode for $slice
            where
                T: crate::convert::JsGeneric,
            {
                fn encode(self, encoder: &mut EncodedData) {
                    encoder.push_u32(self.len() as u32);
                    for val in self {
                        encoder.push_u64(val.as_ref().id());
                    }
                }
            }
        )*
    };
}

impl_jsgeneric_slice_encode!(&[T], &mut [T]);

// ============ Clamped<T> implementations ============

use crate::Clamped;

impl EncodeTypeDef for Clamped<Vec<u8>> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U8Clamped as u8);
    }
}

impl EncodeTypeDef for Clamped<&[u8]> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U8Clamped as u8);
    }
}

impl EncodeTypeDef for Clamped<&mut [u8]> {
    fn encode_type_def(buf: &mut Vec<u8>) {
        buf.push(TypeTag::U8Clamped as u8);
    }
}

impl BinaryEncode for Clamped<Vec<u8>> {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.0.len() as u32);
        for val in self.0 {
            encoder.push_u8(val);
        }
    }
}

impl BinaryEncode for Clamped<&[u8]> {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.0.len() as u32);
        for &val in self.0 {
            encoder.push_u8(val);
        }
    }
}

impl BinaryEncode for Clamped<&mut [u8]> {
    fn encode(self, encoder: &mut EncodedData) {
        encoder.push_u32(self.0.len() as u32);
        for &mut val in self.0 {
            encoder.push_u8(val);
        }
    }
}

impl BinaryDecode for Clamped<Vec<u8>> {
    fn decode(decoder: &mut DecodedData) -> Result<Self, DecodeError> {
        let len = decoder.take_u32()? as usize;
        let mut vec = Vec::with_capacity(len);
        for _ in 0..len {
            vec.push(decoder.take_u8()?);
        }
        Ok(Clamped(vec))
    }
}

impl BatchableResult for Clamped<Vec<u8>> {}

#[cfg(all(test, feature = "enable-interning"))]
mod tests {
    use super::*;

    #[test]
    fn interned_strings_encode_as_cached_heap_refs() {
        crate::intern::insert_for_test("cached", 0x0000_0001_0000_0002);

        let mut cached = EncodedData::new();
        "cached".encode(&mut cached);
        assert_eq!(
            cached.u32_buf,
            vec![crate::ipc::CACHED_STRING_SENTINEL, 2, 1]
        );
        assert!(cached.str_buf.is_empty());

        let mut inline = EncodedData::new();
        "uncached".encode(&mut inline);
        assert_eq!(inline.u32_buf, vec![8]);
        assert_eq!(inline.str_buf, b"uncached".to_vec());

        crate::intern::unintern("cached");
    }
}

#[cfg(test)]
mod decode_error_tests {
    use super::*;

    // A real Rust<->JS round-trip never produces a truncated or malformed buffer,
    // so the `take_*()?` failure branches in every `decode` are only reachable by
    // feeding a `Decoder` a short/garbage buffer directly. These cover those paths.

    #[test]
    fn primitive_decode_errors_on_empty_buffer() {
        macro_rules! assert_decode_err {
            ($($t:ty),* $(,)?) => {$({
                // header only, every value sub-buffer empty
                let bytes = EncodedData::new().to_bytes();
                let mut d = DecodedData::from_bytes(&bytes).unwrap();
                assert!(
                    <$t as BinaryDecode>::decode(&mut d).is_err(),
                    "expected decode error for {}", stringify!($t)
                );
            })*};
        }
        assert_decode_err!(
            u8, u16, u32, u64, u128, i8, i16, i32, i64, i128, f32, f64, usize, isize, bool, char,
            String,
        );
    }

    #[test]
    fn char_decode_rejects_invalid_scalar_value() {
        let mut enc = EncodedData::new();
        enc.push_u32(0x11_0000); // one past the maximum Unicode scalar value
        let bytes = enc.to_bytes();
        let mut d = DecodedData::from_bytes(&bytes).unwrap();
        assert!(matches!(char::decode(&mut d), Err(DecodeError::Custom(_))));
    }

    #[test]
    fn string_decode_errors_on_truncation_and_bad_utf8() {
        // length header claims 5 bytes, but the string buffer is empty
        let mut enc = EncodedData::new();
        enc.push_u32(5);
        let bytes = enc.to_bytes();
        let mut d = DecodedData::from_bytes(&bytes).unwrap();
        assert!(matches!(
            String::decode(&mut d),
            Err(DecodeError::StringBufferTooShort { .. })
        ));

        // length header says 2 bytes, body is invalid UTF-8
        let mut enc = EncodedData::new();
        enc.push_u32(2);
        enc.str_buf.extend_from_slice(&[0xff, 0xfe]);
        let bytes = enc.to_bytes();
        let mut d = DecodedData::from_bytes(&bytes).unwrap();
        assert!(matches!(
            String::decode(&mut d),
            Err(DecodeError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn clamped_decode_errors_when_buffer_truncated() {
        // length header claims 3 bytes, only 1 present
        let mut enc = EncodedData::new();
        enc.push_u32(3);
        enc.push_u8(1);
        let bytes = enc.to_bytes();
        let mut d = DecodedData::from_bytes(&bytes).unwrap();
        assert!(Clamped::<Vec<u8>>::decode(&mut d).is_err());
    }
}
