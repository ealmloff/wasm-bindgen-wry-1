//! Container, result, and slice binary protocol implementations.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::ipc::{DecodeError, DecodedData, EncodedData};

use super::{BatchableResult, BinaryDecode, BinaryEncode, EncodeTypeDef, TypeTag};

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
