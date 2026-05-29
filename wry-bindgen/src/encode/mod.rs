//! Core encoding and decoding traits for the binary protocol.
//!
//! This module provides traits for serializing and deserializing Rust types
//! to/from the binary IPC protocol.

use alloc::vec::Vec;

use crate::batch::Runtime;
use crate::ipc::{DecodeError, DecodedData, EncodedData};

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

mod callbacks;
mod clamped;
mod containers;
mod primitives;
#[cfg(test)]
mod tests;
mod values;

pub use callbacks::CallbackKey;
pub(crate) use callbacks::CallbackPolicy;
