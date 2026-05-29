use crate::ast::StringEnum;
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};

use super::common::clippy_allows;

pub(super) fn generate_string_enum(
    string_enum: &StringEnum,
    krate: &TokenStream,
) -> syn::Result<TokenStream> {
    let vis = &string_enum.vis;
    let enum_name = &string_enum.name;
    let variants = &string_enum.variants;
    let variant_values = &string_enum.variant_values;
    let rust_attrs = &string_enum.rust_attrs;
    let span = enum_name.span();

    let variant_count = variants.len();
    let variant_indices: Vec<u32> = (0..variant_count as u32).collect();

    let invalid_to_str_msg = format!(
        "Converting an invalid string enum ({enum_name}) back to a string is currently not supported"
    );

    // Generate variant paths for match arms (EnumName::VariantName)
    let variant_paths: Vec<TokenStream> = variants
        .iter()
        .map(|v| quote_spanned!(span=> #enum_name::#v))
        .collect();

    // Generate the enum definition with repr(u32)
    let enum_def = quote! {
        #(#rust_attrs)*
        #[non_exhaustive]
        #[repr(u32)]
        #vis enum #enum_name {
            #(#variants = #variant_indices,)*
            #[automatically_derived]
            #[doc(hidden)]
            __Invalid
        }
    };

    // Generate helper methods (from_str, to_str, from_js_value)
    let allows = clippy_allows();
    let impl_methods = quote! {
        #[automatically_derived]
        impl #enum_name {
            /// Convert a string to this enum variant.
            #allows
            pub fn from_str(s: &str) -> ::core::option::Option<#enum_name> {
                match s {
                    #(#variant_values => ::core::option::Option::Some(#variant_paths),)*
                    _ => ::core::option::Option::None,
                }
            }

            /// Convert this enum variant to its string representation.
            pub fn to_str(&self) -> &'static str {
                match self {
                    #(#variant_paths => #variant_values,)*
                    #enum_name::__Invalid => ::core::panic!(#invalid_to_str_msg),
                }
            }

            /// Convert a JsValue (if it's a string) to this enum variant.
            #allows
            #vis fn from_js_value(obj: &#krate::JsValue) -> ::core::option::Option<#enum_name> {
                ::core::option::Option::and_then(obj.as_string(), |s| Self::from_str(&s))
            }
        }
    };

    // Generate EncodeTypeDef implementation
    // String enums use StringEnum tag with embedded variant strings
    let variant_count_u8 = variant_count as u8;
    let encode_type_def_impl = quote! {
        impl #krate::EncodeTypeDef for #enum_name {
            fn encode_type_def(buf: &mut #krate::alloc::vec::Vec<u8>) {
                // Push StringEnum tag
                buf.push(#krate::encode::TypeTag::StringEnum as u8);
                // Push variant count
                buf.push(#variant_count_u8);
                // Push each variant string (length as u32 + bytes)
                #(
                    let s: &str = #variant_values;
                    let bytes = s.as_bytes();
                    let len = bytes.len() as u32;
                    buf.extend_from_slice(&len.to_le_bytes());
                    buf.extend_from_slice(bytes);
                )*
            }
        }
    };

    // Generate BinaryEncode implementation - encode as u32 discriminant
    let binary_encode_impl = quote! {
        impl #krate::BinaryEncode for #enum_name {
            fn encode(self, encoder: &mut #krate::EncodedData) {
                <u32 as #krate::BinaryEncode>::encode(self as u32, encoder);
            }
        }
    };

    // Generate BinaryDecode implementation - decode u32 to variant
    let binary_decode_impl = quote! {
        impl #krate::BinaryDecode for #enum_name {
            fn decode(decoder: &mut #krate::DecodedData) -> ::core::result::Result<Self, #krate::DecodeError> {
                let discriminant = <u32 as #krate::BinaryDecode>::decode(decoder)?;
                match discriminant {
                    #(#variant_indices => ::core::result::Result::Ok(#variant_paths),)*
                    _ => ::core::result::Result::Ok(#enum_name::__Invalid),
                }
            }
        }
    };

    // Generate BatchableResult implementation
    let batchable_impl = quote! {
        impl #krate::BatchableResult for #enum_name {}
    };

    // Generate From<EnumName> for JsValue
    let into_jsvalue_impl = quote! {
        #[automatically_derived]
        impl ::core::convert::From<#enum_name> for #krate::JsValue {
            fn from(val: #enum_name) -> Self {
                #krate::JsValue::from_str(val.to_str())
            }
        }
    };

    Ok(quote! {
        #enum_def
        #impl_methods
        #encode_type_def_impl
        #binary_encode_impl
        #binary_decode_impl
        #batchable_impl
        #into_jsvalue_impl
    })
}

// ============================================================================
// Export Code Generation (for Rust structs/impl blocks exposed to JavaScript)
// ============================================================================
