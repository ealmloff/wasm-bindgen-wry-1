//! Primitive and string binary protocol implementations.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::batch::Runtime;
use crate::ipc::{DecodeError, DecodedData, EncodedData};

use super::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef, TypeTag};

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
