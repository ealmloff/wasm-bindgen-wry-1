//! `Clamped<T>` binary protocol implementations.

use alloc::vec::Vec;

use crate::Clamped;
use crate::ipc::{DecodeError, DecodedData, EncodedData};

use super::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef, TypeTag};

// ============ Clamped<T> implementations ============

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
