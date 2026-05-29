//! Wire-format helper macros for transparent JsValue wrappers.

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
