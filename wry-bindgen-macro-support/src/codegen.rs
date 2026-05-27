//! Code generation for wasm_bindgen macro
//!
//! This module generates Rust code that uses the wry-bindgen runtime
//! and inventory-based function registration.

use std::{
    collections::{HashMap, HashSet},
    hash::{BuildHasher, RandomState},
    sync::atomic::AtomicU32,
};

use crate::ast::{
    ExportMethod, ExportMethodKind, ExportStruct, ImportFunction, ImportFunctionKind, ImportStatic,
    ImportType, Program, SelfType, StringEnum, StructField,
};
use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote, quote_spanned};

/// Generate clippy allow attributes for macro-generated code
fn clippy_allows() -> TokenStream {
    quote! {
        #[allow(clippy::unused_unit)]
        #[allow(clippy::too_many_arguments)]
        #[allow(clippy::type_complexity)]
        #[allow(clippy::should_implement_trait)]
        #[allow(clippy::await_holding_refcell_ref)]
    }
}

fn generate_member_type_helpers(
    arg_types: &[TokenStream],
    return_type: Option<TokenStream>,
    krate: &TokenStream,
    span: proc_macro2::Span,
) -> TokenStream {
    let return_body = match return_type {
        Some(ty) => quote_spanned! {span=>
            let mut ty = #krate::alloc::vec::Vec::new();
            <#ty as #krate::EncodeTypeDef>::encode_type_def(&mut ty);
            ::core::option::Option::Some(ty)
        },
        None => quote_spanned! {span=> ::core::option::Option::None },
    };

    quote_spanned! {span=>
        fn __wry_arg_types() -> #krate::alloc::vec::Vec<#krate::alloc::vec::Vec<u8>> {
            #krate::alloc::vec![
                #(
                {
                    let mut ty = #krate::alloc::vec::Vec::new();
                    <#arg_types as #krate::EncodeTypeDef>::encode_type_def(&mut ty);
                    ty
                }
                ),*
            ]
        }

        fn __wry_return_type() -> ::core::option::Option<#krate::alloc::vec::Vec<u8>> {
            #return_body
        }
    }
}

/// Generate code for the entire program
pub fn generate(program: &Program) -> syn::Result<TokenStream> {
    let mut tokens = TokenStream::new();
    let krate = &program.attrs.crate_path_tokens();

    // First generate the module for inline_js or module attribute if needed
    let mut prefix = String::new();

    // Determine the module content expression: either inline_js or include_str!(module_path)
    let module_content: Option<(proc_macro2::Span, TokenStream)> = if let Some((
        span,
        inline_js_module,
    )) = &program.attrs.inline_js
    {
        Some((*span, inline_js_module.to_token_stream()))
    } else if let Some((span, module_path)) = &program.attrs.module {
        // If path starts with '/', make it relative to CARGO_MANIFEST_DIR
        let include_expr = if module_path.starts_with('/') {
            quote_spanned! {*span=> include_str!(concat!(env!("CARGO_MANIFEST_DIR"), #module_path)) }
        } else {
            quote_spanned! {*span=> include_str!(#module_path) }
        };
        Some((*span, include_expr))
    } else {
        None
    };

    if let Some((span, content_expr)) = module_content {
        let unique_hash = {
            let s = RandomState::new();

            s.hash_one(content_expr.to_string())
        };
        let unique_ident = format_ident!("__WRY_BINDGEN_INLINE_JS_MODULE_HASH_{}", unique_hash);
        // Create a static and submit it to the inventory
        tokens.extend(quote_spanned! {span=>
            static #unique_ident: u64 = {
                static __WRY_BINDGEN_INLINE_JS_MODULE: #krate::InlineJsModule = #krate::InlineJsModule::new(
                    #content_expr
                );
                #krate::inventory::submit! {
                    __WRY_BINDGEN_INLINE_JS_MODULE
                }
                __WRY_BINDGEN_INLINE_JS_MODULE.const_hash()
            };
        });
        prefix = format!("module_{{{unique_ident}:x}}.");
    }

    // Collect type names being defined in this block
    let type_names: HashSet<String> = program
        .types
        .iter()
        .map(|t| t.rust_name.to_string())
        .collect();
    let type_generics: HashMap<String, syn::Generics> = program
        .types
        .iter()
        .map(|t| (t.rust_name.to_string(), t.generics.clone()))
        .collect();

    // Collect vendor_prefixes for each type
    let vendor_prefixes: std::collections::HashMap<String, Vec<String>> = program
        .types
        .iter()
        .map(|t| {
            (
                t.rust_name.to_string(),
                t.vendor_prefixes.iter().map(|i| i.to_string()).collect(),
            )
        })
        .collect();

    // Generate type definitions
    for ty in &program.types {
        tokens.extend(generate_type(ty, krate)?);
    }

    // Generate function definitions
    for func in &program.functions {
        tokens.extend(generate_function(
            func,
            &type_names,
            &type_generics,
            &vendor_prefixes,
            krate,
            &prefix,
        )?);
    }

    // Generate static definitions
    for st in &program.statics {
        tokens.extend(generate_static(st, krate, &prefix)?);
    }

    // Generate string enum definitions
    for string_enum in &program.string_enums {
        tokens.extend(generate_string_enum(string_enum, krate)?);
    }

    // Generate exported struct definitions
    for export_struct in &program.structs {
        tokens.extend(generate_export_struct(export_struct, krate)?);
    }

    // Generate exported method definitions
    for export_method in &program.exports {
        tokens.extend(generate_export_method(export_method, krate)?);
    }

    Ok(tokens)
}

/// Generate code for an imported type
fn generate_type(ty: &ImportType, krate: &TokenStream) -> syn::Result<TokenStream> {
    let vis = &ty.vis;
    let rust_name = &ty.rust_name;
    let generics = &ty.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let mut into_js_generics = add_static_bounds(generics);
    let (_, into_js_ty_generics, _) = into_js_generics.split_for_impl();
    let self_ty: syn::Type = syn::parse_quote!(#rust_name #into_js_ty_generics);
    into_js_generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(#self_ty: #krate::JsGeneric));
    let (into_js_impl_generics, into_js_ty_generics, into_js_where_clause) =
        into_js_generics.split_for_impl();
    let derives = &ty.derives;
    let span = rust_name.span();
    let storage_ty = if let Some(first_parent) = ty.extends.first() {
        first_parent.to_token_stream()
    } else {
        quote_spanned! {span=> #krate::JsValue }
    };
    let type_params: Vec<_> = generics.type_params().map(|param| &param.ident).collect();
    let generic_field = if type_params.is_empty() {
        quote! {}
    } else {
        quote_spanned! {span=>
            pub generics: ::core::marker::PhantomData<fn() -> (#(#type_params,)*)>,
        }
    };
    let generic_init = if type_params.is_empty() {
        quote! {}
    } else {
        quote_spanned! {span=>
            generics: ::core::marker::PhantomData,
        }
    };
    let from_jsvalue_obj = if ty.extends.is_empty() {
        quote_spanned! {span=> val }
    } else {
        quote_spanned! {span=> <#storage_ty as #krate::JsCast>::unchecked_from_js(val) }
    };

    // Generate the struct definition using JsValue from the configured crate
    // repr(transparent) ensures the same memory layout
    // Apply user-provided attributes (like #[derive(Debug, PartialEq, Eq)])
    // Use named struct with `obj` field to match wasm-bindgen's generated types
    let struct_def = quote_spanned! {span=>
        #(#derives)*
        #[repr(transparent)]
        #vis struct #rust_name #generics #where_clause {
            pub obj: #storage_ty,
            #generic_field
        }
    };

    // Generate AsRef<JsValue> implementation
    let as_ref_impl = quote_spanned! {span=>
        impl #impl_generics ::core::convert::AsRef<#krate::JsValue> for #rust_name #ty_generics #where_clause {
            fn as_ref(&self) -> &#krate::JsValue {
                ::core::convert::AsRef::as_ref(&self.obj)
            }
        }
    };

    // Generate From<Type> for JsValue and From<JsValue> for Type
    let into_jsvalue = quote_spanned! {span=>
        impl #impl_generics ::core::convert::From<#rust_name #ty_generics> for #krate::JsValue #where_clause {
            fn from(val: #rust_name #ty_generics) -> Self {
                ::core::convert::Into::into(val.obj)
            }
        }

        impl #impl_generics ::core::convert::From<#krate::JsValue> for #rust_name #ty_generics #where_clause {
            fn from(val: #krate::JsValue) -> Self {
                Self { obj: #from_jsvalue_obj, #generic_init }
            }
        }
    };

    // Generate Deref to the first parent or JsValue if no parents
    let deref_impls = {
        let deref_to = &storage_ty;
        quote_spanned! {span=>
            impl #impl_generics ::core::ops::Deref for #rust_name #ty_generics #where_clause {
                type Target = #deref_to;
                fn deref(&self) -> &#deref_to {
                    <Self as ::core::convert::AsRef<#deref_to>>::as_ref(self)
                }
            }
        }
    };

    // Generate From and AsRef impls for parent types
    let mut from_parents = TokenStream::new();
    from_parents.extend(quote_spanned! {span=>
        impl #impl_generics ::core::convert::AsRef<#rust_name #ty_generics> for #rust_name #ty_generics #where_clause {
            #[inline]
            fn as_ref(&self) -> &#rust_name #ty_generics {
                self
            }
        }
    });
    for (index, parent) in ty.extends.iter().enumerate() {
        let parent_from_owned = if index == 0 {
            quote_spanned! {span=> val.obj }
        } else {
            quote_spanned! {span=> <#parent as #krate::JsCast>::unchecked_from_js(::core::convert::Into::into(val.obj)) }
        };
        let parent_from_ref = if index == 0 {
            quote_spanned! {span=> ::core::clone::Clone::clone(&val.obj) }
        } else {
            quote_spanned! {span=> <#parent as #krate::JsCast>::unchecked_from_js(::core::convert::Into::into(val)) }
        };
        let parent_ref = if index == 0 {
            quote_spanned! {span=> &self.obj }
        } else {
            quote_spanned! {span=> <#parent as #krate::JsCast>::unchecked_from_js_ref(::core::convert::AsRef::<#krate::JsValue>::as_ref(self)) }
        };
        from_parents.extend(quote_spanned! {span=>
            impl #impl_generics ::core::convert::From<#rust_name #ty_generics> for #parent #where_clause {
                fn from(val: #rust_name #ty_generics) -> #parent {
                    #parent_from_owned
                }
            }

            impl #impl_generics ::core::convert::From<&#rust_name #ty_generics> for #parent #where_clause {
                fn from(val: &#rust_name #ty_generics) -> #parent {
                    #parent_from_ref
                }
            }

            impl #impl_generics ::core::convert::AsRef<#parent> for #rust_name #ty_generics #where_clause {
                #[inline]
                fn as_ref(&self) -> &#parent {
                    #parent_ref
                }
            }
        });
    }

    // Generate EncodeTypeDef implementation
    // All JS types use HeapRef since they're references to JS heap objects
    let encode_type_def_impl = quote_spanned! {span=>
        impl #impl_generics #krate::EncodeTypeDef for #rust_name #ty_generics #where_clause {
            fn encode_type_def(buf: &mut #krate::alloc::vec::Vec<u8>) {
                <#krate::JsValue as #krate::EncodeTypeDef>::encode_type_def(buf);
            }
        }
    };

    // Generate BinaryEncode implementation
    let binary_encode_impl = quote_spanned! {span=>
        impl #impl_generics #krate::BinaryEncode for #rust_name #ty_generics #where_clause {
            fn encode(self, encoder: &mut #krate::EncodedData) {
                self.obj.encode(encoder);
            }
        }
    };

    // Generate BinaryDecode implementation
    let binary_decode_impl = quote_spanned! {span=>
        impl #impl_generics #krate::BinaryDecode for #rust_name #ty_generics #where_clause {
            fn decode(decoder: &mut #krate::DecodedData) -> ::core::result::Result<Self, #krate::DecodeError> {
                ::core::result::Result::map(#krate::JsValue::decode(decoder), ::core::convert::Into::into)
            }
        }
    };

    // Generate BatchableResult implementation
    let batchable_impl = quote_spanned! {span=>
        impl #impl_generics #krate::BatchableResult for #rust_name #ty_generics #where_clause {
            fn try_placeholder(batch: &mut #krate::batch::Runtime) -> ::core::option::Option<Self> {
                ::core::option::Option::Some(::core::convert::Into::into(<#krate::JsValue as #krate::BatchableResult>::try_placeholder(batch)?))
            }
        }
    };

    // Generate JsCast implementation with actual instanceof check
    let js_name = &ty.js_name;

    // Generate JavaScript instanceof check code with vendor prefix fallback
    // Always generate a safe check that returns false if the class doesn't exist,
    // matching wasm-bindgen's try-catch behavior
    let instanceof_js_code = if ty.vendor_prefixes.is_empty() {
        // Simple case: check if class exists before instanceof
        format!("(a0) => typeof {js_name} !== 'undefined' && a0 instanceof {js_name}")
    } else {
        // Generate vendor-prefixed fallback:
        // (a0) => a0 instanceof (typeof Foo !== 'undefined' ? Foo : (typeof webkitFoo !== 'undefined' ? webkitFoo : ...))
        let mut class_expr = format!("(typeof {js_name} !== 'undefined' ? {js_name} : ");
        for (i, prefix) in ty.vendor_prefixes.iter().enumerate() {
            let prefixed = format!("{prefix}{js_name}");
            if i == ty.vendor_prefixes.len() - 1 {
                // Last prefix - use Object as final fallback (which will make instanceof return false for non-objects)
                class_expr.push_str(&format!(
                    "(typeof {prefixed} !== 'undefined' ? {prefixed} : Object)"
                ));
            } else {
                class_expr.push_str(&format!(
                    "(typeof {prefixed} !== 'undefined' ? {prefixed} : "
                ));
            }
        }
        // Close all the parentheses
        class_expr.push(')');
        format!("(a0) => a0 instanceof {class_expr}")
    };

    // Generate is_type_of implementation if provided
    let is_type_of_impl = ty.is_type_of.as_ref().map(|is_type_of| {
        quote_spanned! {span=>
            #[inline]
            fn is_type_of(__val: &#krate::JsValue) -> bool {
                let __is_type_of: fn(&#krate::JsValue) -> bool = #is_type_of;
                __is_type_of(__val)
            }
        }
    });

    let jscast_impl = quote_spanned! {span=>
        impl #impl_generics #krate::JsCast for #rust_name #ty_generics #where_clause {
            fn instanceof(__val: &#krate::JsValue) -> bool {
                #krate::__wry_call_js_function!(#instanceof_js_code, fn(&#krate::JsValue) -> bool, (__val))
            }

            #is_type_of_impl

            fn unchecked_from_js(val: #krate::JsValue) -> Self {
                ::core::convert::Into::into(val)
            }

            fn unchecked_from_js_ref(val: &#krate::JsValue) -> &Self {
                // SAFETY: #[repr(transparent)] guarantees same layout
                unsafe { &*(val as *const #krate::JsValue as *const Self) }
            }
        }
    };

    let generic_trait_impls = quote_spanned! {span=>
        unsafe impl #impl_generics #krate::__rt::marker::ErasableGeneric for #rust_name #ty_generics #where_clause {
            type Repr = #krate::JsValue;
        }

        impl #into_js_impl_generics #krate::IntoJsGeneric for #rust_name #into_js_ty_generics #into_js_where_clause {
            type JsCanon = Self;

            #[inline]
            fn to_js(self) -> Self::JsCanon {
                self
            }
        }

        impl #impl_generics #krate::convert::IntoWasmAbi for #rust_name #ty_generics #where_clause {
            type Abi = <#krate::JsValue as #krate::convert::IntoWasmAbi>::Abi;

            #[inline]
            fn into_abi(self) -> Self::Abi {
                <#krate::JsValue as #krate::convert::IntoWasmAbi>::into_abi(::core::convert::Into::into(self))
            }
        }

        impl #impl_generics #krate::convert::FromWasmAbi for #rust_name #ty_generics #where_clause {
            type Abi = <#krate::JsValue as #krate::convert::FromWasmAbi>::Abi;

            #[inline]
            unsafe fn from_abi(js: Self::Abi) -> Self {
                let value = unsafe { <#krate::JsValue as #krate::convert::FromWasmAbi>::from_abi(js) };
                <Self as #krate::JsCast>::unchecked_from_js(value)
            }
        }
    };

    let mut upcast_impls = TokenStream::new();
    if !ty.no_upcast {
        upcast_impls.extend(quote_spanned! {span=>
            impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #krate::JsValue #where_clause {}
            impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #krate::sys::JsOption<#krate::JsValue> #where_clause {}
        });

        let class_type_params: Vec<_> = generics.type_params().collect();
        if class_type_params.is_empty() {
            upcast_impls.extend(quote_spanned! {span=>
                impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #rust_name #ty_generics #where_clause {}
                impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #krate::sys::JsOption<#rust_name #ty_generics> #where_clause {}
            });
        } else {
            let mut target_generics = generics.clone();
            let target_param_names: Vec<_> = class_type_params
                .iter()
                .enumerate()
                .map(|(index, param)| {
                    let target_name = format_ident!("__WryUpcastTarget{}", index);
                    let bounds = &param.bounds;
                    if bounds.is_empty() {
                        target_generics.params.push(syn::parse_quote!(#target_name));
                    } else {
                        target_generics
                            .params
                            .push(syn::parse_quote!(#target_name: #bounds));
                    }
                    target_name
                })
                .collect();
            let mut target_where_clause =
                generics
                    .where_clause
                    .clone()
                    .unwrap_or_else(|| syn::WhereClause {
                        where_token: Default::default(),
                        predicates: Default::default(),
                    });
            for (param, target_name) in class_type_params.iter().zip(&target_param_names) {
                let param_name = &param.ident;
                target_where_clause.predicates.push(syn::parse_quote!(
                    #target_name: #krate::convert::UpcastFrom<#param_name>
                ));
            }
            let (target_impl_generics, _, _) = target_generics.split_for_impl();
            let mut target_args = Vec::new();
            let mut next_type_param = 0usize;
            for param in &generics.params {
                match param {
                    syn::GenericParam::Lifetime(param) => {
                        let lifetime = &param.lifetime;
                        target_args.push(quote! { #lifetime });
                    }
                    syn::GenericParam::Type(_) => {
                        let target_name = &target_param_names[next_type_param];
                        next_type_param += 1;
                        target_args.push(quote! { #target_name });
                    }
                    syn::GenericParam::Const(param) => {
                        let ident = &param.ident;
                        target_args.push(quote! { #ident });
                    }
                }
            }
            let target_ty_generics = if target_args.is_empty() {
                quote! {}
            } else {
                quote! { <#(#target_args),*> }
            };

            upcast_impls.extend(quote_spanned! {span=>
                impl #target_impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #rust_name #target_ty_generics #target_where_clause {}
                impl #target_impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #krate::sys::JsOption<#rust_name #target_ty_generics> #target_where_clause {}
            });
        }

        for parent in &ty.extends {
            upcast_impls.extend(quote_spanned! {span=>
                impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #parent #where_clause {}
                impl #impl_generics #krate::convert::UpcastFrom<#rust_name #ty_generics> for #krate::sys::JsOption<#parent> #where_clause {}
            });
        }
    }

    Ok(quote_spanned! {span=>
        #struct_def
        #as_ref_impl
        #into_jsvalue
        #deref_impls
        #from_parents
        #encode_type_def_impl
        #binary_encode_impl
        #binary_decode_impl
        #batchable_impl
        #jscast_impl
        #generic_trait_impls
        #upcast_impls
    })
}

/// Generate code for an imported function
fn generate_function(
    func: &ImportFunction,
    type_names: &HashSet<String>,
    type_generics: &HashMap<String, syn::Generics>,
    vendor_prefixes: &std::collections::HashMap<String, Vec<String>>,
    krate: &TokenStream,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let vis = &func.vis;
    let rust_name = &func.rust_name;
    let span = rust_name.span();
    let call_generics = add_js_call_bounds(func, krate, true);
    let (fn_generics, _, fn_where_clause) = call_generics.split_for_impl();

    // Generate argument lists
    let args = generate_args(func, krate)?;
    let fn_params = &args.fn_params;
    let fn_types = &args.fn_types;
    let call_values = &args.call_values;

    // Generate return type
    let ret_type = match &func.ret {
        Some(ty) => quote_spanned! {span=> #ty },
        None => quote_spanned! {span=> () },
    };

    // Handle async functions - generate code that uses JsFuture
    // For async functions with catch, skip the try-catch wrapper since JsFuture already returns Result
    if func.is_async {
        let js_code = generate_js_code(func, vendor_prefixes, prefix, true);
        let js_code_str = js_code.to_arrow_function();
        return generate_async_function(func, type_generics, krate, &js_code_str, &args);
    }

    // For non-async functions, generate a simple closure that returns a constant string
    let js_code = generate_js_code(func, vendor_prefixes, prefix, false);
    let js_code_str = js_code.to_arrow_function();

    // Generate the function body
    let func_body = quote_spanned! {span=>
        #krate::__wry_call_js_function!(#js_code_str, fn(#fn_types) -> #ret_type, (#call_values))
    };

    // Get the rust attributes to forward (like #[cfg(...)] and #[doc = "..."])
    let rust_attrs = func.fn_rust_attrs();
    let allows = clippy_allows();

    // Generate the full function based on kind
    match &func.kind {
        ImportFunctionKind::Normal => {
            // Check if this function has a single-element js_namespace that matches a type
            // defined in this extern block. If so, generate as a static method to avoid collisions.
            if let Some(ns) = &func.js_namespace
                && ns.len() == 1
                && type_names.contains(&ns[0])
            {
                let (impl_type, impl_generics, mut method_generics) =
                    class_impl_parts(func, &ns[0], type_generics);
                add_js_call_bounds_to_generics(&mut method_generics, func, krate, true);
                let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
                let (method_generics, _, method_where_clause) = method_generics.split_for_impl();
                return Ok(quote_spanned! {span=>
                    impl #impl_generics #impl_type #impl_where_clause {
                        #allows
                        #rust_attrs
                        #vis fn #rust_name #method_generics (#fn_params) -> #ret_type #method_where_clause {
                            #func_body
                        }
                    }
                });
            }
            Ok(quote_spanned! {span=>
                #allows
                #rust_attrs
                #vis fn #rust_name #fn_generics (#fn_params) -> #ret_type #fn_where_clause {
                    #func_body
                }
            })
        }
        ImportFunctionKind::Method { receiver }
        | ImportFunctionKind::Getter { receiver, .. }
        | ImportFunctionKind::Setter { receiver, .. }
        | ImportFunctionKind::IndexingGetter { receiver }
        | ImportFunctionKind::IndexingSetter { receiver }
        | ImportFunctionKind::IndexingDeleter { receiver } => {
            // Extract the type name from the receiver
            let receiver_type = receiver_impl_type(receiver)?;
            let (impl_generics, mut method_generics) =
                split_method_generics(&func.generics, receiver);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, true);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();

            // Build method signature with optional additional args
            let method_args = if fn_params.is_empty() {
                quote_spanned! {span=> &self }
            } else {
                quote_spanned! {span=> &self, #fn_params }
            };

            Ok(quote_spanned! {span=>
                impl #impl_generics #receiver_type #impl_where_clause {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name #method_generics (#method_args) -> #ret_type #method_where_clause {
                        #func_body
                    }
                }
            })
        }
        ImportFunctionKind::Constructor { class } => {
            let (impl_type, impl_generics, mut method_generics) =
                class_impl_parts(func, class, type_generics);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, true);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();
            // Use the actual return type (may be Result<T, JsValue> for catch constructors)
            Ok(quote_spanned! {span=>
                impl #impl_generics #impl_type #impl_where_clause {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name #method_generics (#fn_params) -> #ret_type #method_where_clause {
                        #func_body
                    }
                }
            })
        }
        ImportFunctionKind::StaticMethod { class } => {
            let (impl_type, impl_generics, mut method_generics) =
                class_impl_parts(func, class, type_generics);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, true);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();
            Ok(quote_spanned! {span=>
                impl #impl_generics #impl_type #impl_where_clause {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name #method_generics (#fn_params) -> #ret_type #method_where_clause {
                        #func_body
                    }
                }
            })
        }
    }
}

fn class_impl_parts(
    func: &ImportFunction,
    class: &str,
    type_generics: &HashMap<String, syn::Generics>,
) -> (syn::Type, syn::Generics, syn::Generics) {
    if type_generics
        .get(class)
        .is_some_and(|generics| !generics.params.is_empty())
        && let Some(class_type) = class_return_type(func, class)
    {
        let (impl_generics, method_generics) = split_method_generics(&func.generics, &class_type);
        return (class_type, impl_generics, method_generics);
    }

    let class_ident = format_ident!("{}", class);
    (
        syn::parse_quote!(#class_ident),
        syn::Generics::default(),
        func.generics.clone(),
    )
}

fn class_return_type(func: &ImportFunction, class: &str) -> Option<syn::Type> {
    let ret = func
        .ret
        .as_ref()
        .and_then(|ret| extract_result_ok_type(ret).or_else(|| Some(ret.clone())))?;
    let syn::Type::Path(path) = &ret else {
        return None;
    };
    if path.qself.is_some() {
        return None;
    }
    let segment = path.path.segments.last()?;
    if segment.ident != class {
        return None;
    }
    if !matches!(
        segment.arguments,
        syn::PathArguments::AngleBracketed(ref args) if !args.args.is_empty()
    ) {
        return None;
    }

    let known_type_params: HashSet<String> = func
        .generics
        .type_params()
        .map(|param| param.ident.to_string())
        .collect();
    let mut found = HashSet::new();
    if !collect_constraining_type_params(&ret, &known_type_params, &mut found) || found.is_empty() {
        return None;
    }

    Some(ret)
}

/// Generate code for an async imported function
/// Uses wasm_bindgen_futures::JsFuture to convert Promise to Future
fn generate_async_function(
    func: &ImportFunction,
    type_generics: &HashMap<String, syn::Generics>,
    krate: &TokenStream,
    js_code_str: &str,
    args: &GeneratedArgs,
) -> syn::Result<TokenStream> {
    let vis = &func.vis;
    let rust_name = &func.rust_name;
    let span = rust_name.span();
    let rust_attrs = &func.rust_attrs;
    let call_generics = add_js_call_bounds(func, krate, false);
    let (fn_generics, _, fn_where_clause) = call_generics.split_for_impl();

    let fn_params = &args.fn_params;
    let fn_types = &args.fn_types;
    let call_values = &args.call_values;

    // Generate the async function body
    // - Call JS function which returns a Promise (as JsValue)
    // - Cast to js_sys::Promise
    // - Wrap in JsFuture and await
    let async_body = quote_spanned! {span=>
        // Call the function, get Promise as JsValue
        let __promise_val = #krate::__wry_call_js_function!(#js_code_str, fn(#fn_types) -> #krate::JsValue, (#call_values));

        // Cast to js_sys::Promise and wrap in JsFuture
        let __promise: ::wasm_bindgen_futures::js_sys::Promise =
            #krate::JsCast::unchecked_from_js(__promise_val);
        ::wasm_bindgen_futures::JsFuture::from(__promise).await
    };

    // Generate return type handling
    // JsFuture::from(promise).await returns Result<JsValue, JsValue>
    // - For Result<T, E> return types: map Ok value, keep Err as JsValue
    // - For non-Result types: unwrap and cast
    let (ret_clause, ret_handling) = match &func.ret {
        Some(ty) => {
            // Check if return type is Result<T, E>
            if let Some(ok_type) = extract_result_ok_type(ty) {
                // Return type is Result<T, E> - map the Ok value
                if is_unit_type(&ok_type) {
                    // Result<(), E> - just map to ()
                    (
                        quote_spanned! {span=> -> #ty },
                        quote_spanned! {span=>
                            .map(|_| ())
                        },
                    )
                } else {
                    // Result<T, E> - cast the Ok value
                    (
                        quote_spanned! {span=> -> #ty },
                        quote_spanned! {span=>
                            .map(|v| <#ok_type as #krate::JsCast>::unchecked_from_js(v))
                        },
                    )
                }
            } else {
                // Non-Result type - unwrap and cast
                (
                    quote_spanned! {span=> -> #ty },
                    quote_spanned! {span=>
                        .map(|v| <#ty as #krate::JsCast>::unchecked_from_js(v))
                        .expect("async function failed")
                    },
                )
            }
        }
        None => (
            quote_spanned! {span=> },
            quote_spanned! {span=>
                .expect("async function failed");
            },
        ),
    };

    let allows = clippy_allows();

    // Generate the async function based on kind
    match &func.kind {
        ImportFunctionKind::Normal => Ok(quote_spanned! {span=>
            #allows
            #(#rust_attrs)*
            #vis async fn #rust_name #fn_generics (#fn_params) #ret_clause #fn_where_clause {
                #async_body #ret_handling
            }
        }),
        ImportFunctionKind::Method { receiver }
        | ImportFunctionKind::Getter { receiver, .. }
        | ImportFunctionKind::Setter { receiver, .. }
        | ImportFunctionKind::IndexingGetter { receiver }
        | ImportFunctionKind::IndexingSetter { receiver }
        | ImportFunctionKind::IndexingDeleter { receiver } => {
            // Extract the type name from the receiver
            let receiver_type = receiver_impl_type(receiver)?;
            let (impl_generics, mut method_generics) =
                split_method_generics(&func.generics, receiver);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, false);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();

            // Build method signature with optional additional args
            let method_args = if fn_params.is_empty() {
                quote_spanned! {span=> &self }
            } else {
                quote_spanned! {span=> &self, #fn_params }
            };

            Ok(quote_spanned! {span=>
                impl #impl_generics #receiver_type #impl_where_clause {
                    #allows
                    #(#rust_attrs)*
                    #vis async fn #rust_name #method_generics (#method_args) #ret_clause #method_where_clause {
                        #async_body #ret_handling
                    }
                }
            })
        }
        ImportFunctionKind::Constructor { class } => {
            let (impl_type, impl_generics, mut method_generics) =
                class_impl_parts(func, class, type_generics);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, false);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();
            Ok(quote_spanned! {span=>
                impl #impl_generics #impl_type #impl_where_clause {
                    #allows
                    #(#rust_attrs)*
                    #vis async fn #rust_name #method_generics (#fn_params) #ret_clause #method_where_clause {
                        #async_body #ret_handling
                    }
                }
            })
        }
        ImportFunctionKind::StaticMethod { class } => {
            let (impl_type, impl_generics, mut method_generics) =
                class_impl_parts(func, class, type_generics);
            add_js_call_bounds_to_generics(&mut method_generics, func, krate, false);
            let (impl_generics, _, impl_where_clause) = impl_generics.split_for_impl();
            let (method_generics, _, method_where_clause) = method_generics.split_for_impl();
            Ok(quote_spanned! {span=>
                impl #impl_generics #impl_type #impl_where_clause {
                    #allows
                    #(#rust_attrs)*
                    #vis async fn #rust_name #method_generics (#fn_params) #ret_clause #method_where_clause {
                        #async_body #ret_handling
                    }
                }
            })
        }
    }
}

/// Generate vendor-prefixed constructor fallback code
/// E.g., for class "MyApi" with prefixes ["webkit", "moz"], generates:
/// (typeof MyApi !== 'undefined' ? MyApi : (typeof webkitMyApi !== 'undefined' ? webkitMyApi : (typeof mozMyApi !== 'undefined' ? mozMyApi : undefined)))
fn generate_vendor_prefixed_constructor(class: &str, prefixes: &[String], prefix: &str) -> String {
    // Start with the base class name (no prefix)
    let mut result = format!("(typeof {prefix}{class} !== 'undefined' ? {prefix}{class} : ");

    // Add each vendor prefix
    for (i, vendor_prefix) in prefixes.iter().enumerate() {
        let prefixed_class = format!("{vendor_prefix}{class}");
        if i == prefixes.len() - 1 {
            // Last one - end with undefined if none found
            result.push_str(&format!(
                "(typeof {prefix}{prefixed_class} !== 'undefined' ? {prefix}{prefixed_class} : undefined)"
            ));
        } else {
            result.push_str(&format!(
                "(typeof {prefix}{prefixed_class} !== 'undefined' ? {prefix}{prefixed_class} : "
            ));
        }
    }

    // Close all the parentheses
    result.push(')');
    result
}

/// Generate JavaScript code for the function
fn generate_js_code(
    func: &ImportFunction,
    vendor_prefixes: &std::collections::HashMap<String, Vec<String>>,
    prefix: &str,
    skip_catch_wrapper: bool,
) -> JsCode {
    let js_name = &func.js_name;

    let prefix = if let Some(ns) = &func.js_namespace {
        if !ns.is_empty() {
            format!("{prefix}{}.", ns.join("."))
        } else {
            prefix.to_string()
        }
    } else {
        prefix.to_string()
    };

    let (params, body) = match &func.kind {
        ImportFunctionKind::Normal => {
            // Use a{index} naming to avoid conflicts with JS reserved words
            let args: Vec<_> = (0..func.arguments.len()).map(|i| format!("a{i}")).collect();
            let args_str = args.join(", ");
            let callee = if prefix.is_empty() {
                js_name.to_string()
            } else {
                let object = prefix.trim_end_matches('.');
                js_property_access(object, js_name)
            };
            (format!("({args_str})"), format!("{callee}({args_str})"))
        }
        ImportFunctionKind::Method { .. } => {
            // Use a{index} naming to avoid conflicts with JS reserved words
            let args: Vec<_> = (0..func.arguments.len()).map(|i| format!("a{i}")).collect();
            let args_str = args.join(", ");
            let method = js_property_access("obj", js_name);
            if args.is_empty() {
                ("(obj)".to_string(), format!("{method}()"))
            } else {
                (
                    format!("(obj, {args_str})"),
                    format!("{method}({args_str})"),
                )
            }
        }
        ImportFunctionKind::Getter { property, .. } => {
            ("(obj)".to_string(), js_property_access("obj", property))
        }
        ImportFunctionKind::Setter { property, .. } => (
            "(obj, value)".to_string(),
            format!("{} = value", js_property_access("obj", property)),
        ),
        ImportFunctionKind::IndexingGetter { .. } => {
            // obj[index] - takes one argument (the index)
            ("(obj, index)".to_string(), "obj[index]".to_string())
        }
        ImportFunctionKind::IndexingSetter { .. } => {
            // obj[index] = value - takes two arguments (index and value)
            (
                "(obj, index, value)".to_string(),
                "obj[index] = value".to_string(),
            )
        }
        ImportFunctionKind::IndexingDeleter { .. } => {
            // delete obj[index] - takes one argument (the index)
            ("(obj, index)".to_string(), "delete obj[index]".to_string())
        }
        ImportFunctionKind::Constructor { class } => {
            // Use a{index} naming to avoid conflicts with JS reserved words
            let args: Vec<_> = (0..func.arguments.len()).map(|i| format!("a{i}")).collect();
            let args_str = args.join(", ");

            // Check if this type has vendor prefixes
            let body = if let Some(prefixes) = vendor_prefixes.get(class) {
                if !prefixes.is_empty() {
                    // Generate vendor-prefixed fallback code
                    let constructor_expr =
                        generate_vendor_prefixed_constructor(class, prefixes, &prefix);
                    format!("new ({constructor_expr})({args_str})")
                } else {
                    format!("new {prefix}{class}({args_str})")
                }
            } else {
                format!("new {prefix}{class}({args_str})")
            };

            (format!("({args_str})"), body)
        }
        ImportFunctionKind::StaticMethod { class } => {
            // Use a{index} naming to avoid conflicts with JS reserved words
            let args: Vec<_> = (0..func.arguments.len()).map(|i| format!("a{i}")).collect();
            let args_str = args.join(", ");
            let class_object = format!("{prefix}{class}");
            let method = js_property_access(&class_object, js_name);
            (format!("({args_str})"), format!("{method}({args_str})"))
        }
    };

    // Wrap in try-catch if catch attribute is present
    // Skip for async functions since JsFuture already returns Result<JsValue, JsValue>
    let body = if func.catch && !skip_catch_wrapper {
        wrap_body_with_try_catch(&body)
    } else {
        body
    };

    JsCode { params, body }
}

fn js_property_access(object: &str, property: &str) -> String {
    format!("{object}[{}]", js_string_literal(property))
}

fn js_string_literal(value: &str) -> String {
    let mut literal = String::with_capacity(value.len() + 2);
    literal.push('"');
    for ch in value.chars() {
        match ch {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            '\u{08}' => literal.push_str("\\b"),
            '\u{0c}' => literal.push_str("\\f"),
            ch if ch < ' ' => {
                use core::fmt::Write;
                write!(&mut literal, "\\u{:04x}", ch as u32).unwrap();
            }
            ch => literal.push(ch),
        }
    }
    literal.push('"');
    literal
}

/// Wrap JavaScript body in try-catch block for error handling
fn wrap_body_with_try_catch(body: &str) -> String {
    // Wrap the body in try-catch and return Result-like object
    format!(
        "{{{{ try {{{{ return {{{{ ok: {body} }}}}; }}}} catch(e) {{{{ return {{{{ err: e }}}}; }}}} }}}}"
    )
}

/// JavaScript function code parts
struct JsCode {
    /// Function parameters (e.g., "(arg1, arg2)" or "(obj, arg1, arg2)")
    params: String,
    /// Function body (e.g., "obj.method(arg1, arg2)" or "new Class(arg1)")
    body: String,
}

impl JsCode {
    /// Convert to a complete JavaScript arrow function
    fn to_arrow_function(&self) -> String {
        format!("{} => {}", self.params, self.body)
    }
}

/// Generated argument information
struct GeneratedArgs {
    /// Function parameter declarations: `arg1: T1, arg2: T2`
    fn_params: TokenStream,
    /// Just the types for fn pointer: `T1, T2`
    fn_types: TokenStream,
    /// Values to pass to call: `&self.obj, arg1, arg2`
    call_values: TokenStream,
}

/// Generate argument lists
fn generate_args(func: &ImportFunction, krate: &TokenStream) -> syn::Result<GeneratedArgs> {
    let mut fn_params = Vec::new();
    let mut fn_types = Vec::new();
    let mut call_values = Vec::new();
    let span = func.rust_name.span();

    // For methods, add self as first call arg (but not as fn param since we use &self)
    match &func.kind {
        ImportFunctionKind::Method { .. }
        | ImportFunctionKind::Getter { .. }
        | ImportFunctionKind::Setter { .. }
        | ImportFunctionKind::IndexingGetter { .. }
        | ImportFunctionKind::IndexingSetter { .. }
        | ImportFunctionKind::IndexingDeleter { .. } => {
            fn_types.push(quote_spanned! {span=> &#krate::JsValue });
            call_values.push(quote_spanned! {span=> &self.obj });
        }
        _ => {}
    }

    // Add explicit arguments
    for arg in &func.arguments {
        let name = &arg.name;
        let ty = &arg.ty;
        fn_params.push(quote_spanned! {span=> #name: #ty });
        fn_types.push(quote_spanned! {span=> #ty });
        call_values.push(quote_spanned! {span=> #name });
    }

    let fn_params_tokens = if fn_params.is_empty() {
        quote_spanned! {span=>}
    } else {
        quote_spanned! {span=> #(#fn_params),* }
    };

    let fn_types_tokens = if fn_types.is_empty() {
        quote_spanned! {span=>}
    } else {
        quote_spanned! {span=> #(#fn_types),* }
    };

    let call_values_tokens = if call_values.is_empty() {
        quote_spanned! {span=>}
    } else {
        quote_spanned! {span=> #(#call_values),* }
    };

    Ok(GeneratedArgs {
        fn_params: fn_params_tokens,
        fn_types: fn_types_tokens,
        call_values: call_values_tokens,
    })
}

fn receiver_impl_type(ty: &syn::Type) -> syn::Result<syn::Type> {
    match ty {
        syn::Type::Reference(r) => receiver_impl_type(&r.elem),
        syn::Type::Path(_) => Ok(ty.clone()),
        _ => Err(syn::Error::new_spanned(ty, "unsupported receiver type")),
    }
}

fn add_static_bounds(generics: &syn::Generics) -> syn::Generics {
    let mut generics = generics.clone();
    for param in generics.type_params_mut() {
        param.bounds.push(syn::parse_quote!('static));
    }
    generics
}

fn add_js_call_bounds(
    func: &ImportFunction,
    krate: &TokenStream,
    include_ret: bool,
) -> syn::Generics {
    let mut generics = func.generics.clone();
    add_js_call_bounds_to_generics(&mut generics, func, krate, include_ret);
    generics
}

fn add_js_call_bounds_to_generics(
    generics: &mut syn::Generics,
    func: &ImportFunction,
    krate: &TokenStream,
    include_ret: bool,
) {
    let known_type_params: std::collections::HashSet<String> = func
        .generics
        .type_params()
        .map(|param| param.ident.to_string())
        .collect();

    for arg in &func.arguments {
        if type_uses_type_params(&arg.ty, &known_type_params) {
            push_arg_type_bounds(generics, &arg.ty, krate);
        }
    }

    if include_ret
        && let Some(ret) = &func.ret
        && type_uses_type_params(ret, &known_type_params)
    {
        push_type_bound(
            generics,
            ret,
            &[
                quote! { #krate::EncodeTypeDef },
                quote! { #krate::BatchableResult },
            ],
        );
    }
}

fn type_uses_type_params(ty: &syn::Type, known: &std::collections::HashSet<String>) -> bool {
    let mut found = std::collections::HashSet::new();
    collect_type_params(ty, known, &mut found);
    !found.is_empty()
}

fn push_arg_type_bounds(generics: &mut syn::Generics, ty: &syn::Type, krate: &TokenStream) {
    match ty {
        syn::Type::Reference(reference) => match &*reference.elem {
            syn::Type::Slice(slice) => {
                push_type_bound(
                    generics,
                    &slice.elem,
                    &[
                        quote! { #krate::EncodeTypeDef },
                        quote! { #krate::JsGeneric },
                    ],
                );
            }
            syn::Type::Path(path) if path_is_scoped_closure(path) => {}
            syn::Type::Path(path) if path.path.segments.len() == 1 => {
                let elem = &reference.elem;
                push_type_bound(generics, elem, &[quote! { #krate::EncodeTypeDef }]);
                push_reference_binary_encode_bound(generics, reference, krate);
            }
            _ => {
                push_type_bound(
                    generics,
                    ty,
                    &[
                        quote! { #krate::EncodeTypeDef },
                        quote! { #krate::BinaryEncode },
                    ],
                );
            }
        },
        _ => {
            push_type_bound(
                generics,
                ty,
                &[
                    quote! { #krate::EncodeTypeDef },
                    quote! { #krate::BinaryEncode },
                ],
            );
        }
    }
}

fn path_is_scoped_closure(path: &syn::TypePath) -> bool {
    path.path
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "ScopedClosure" || segment.ident == "Closure")
}

fn push_reference_binary_encode_bound(
    generics: &mut syn::Generics,
    reference: &syn::TypeReference,
    krate: &TokenStream,
) {
    let elem = &reference.elem;
    let predicate = if let Some(lifetime) = &reference.lifetime {
        let mutability = &reference.mutability;
        syn::parse_quote! {
            &#lifetime #mutability #elem: #krate::BinaryEncode
        }
    } else {
        let mutability = &reference.mutability;
        syn::parse_quote! {
            for<'__wry_bindgen> &'__wry_bindgen #mutability #elem: #krate::BinaryEncode
        }
    };
    generics.make_where_clause().predicates.push(predicate);
}

fn push_type_bound(generics: &mut syn::Generics, ty: &syn::Type, bounds: &[TokenStream]) {
    let bounds = quote! { #(#bounds)+* };
    let predicate = match ty {
        syn::Type::Reference(reference) if reference.lifetime.is_none() => {
            let mutability = &reference.mutability;
            let elem = &reference.elem;
            syn::parse_quote! {
                for<'__wry_bindgen> &'__wry_bindgen #mutability #elem: #bounds
            }
        }
        _ => syn::parse_quote! {
            #ty: #bounds
        },
    };
    generics.make_where_clause().predicates.push(predicate);
}

fn split_method_generics(
    generics: &syn::Generics,
    receiver: &syn::Type,
) -> (syn::Generics, syn::Generics) {
    let known_type_params: std::collections::HashSet<String> = generics
        .type_params()
        .map(|param| param.ident.to_string())
        .collect();
    let mut receiver_type_params = std::collections::HashSet::new();
    collect_type_params(receiver, &known_type_params, &mut receiver_type_params);
    let mut changed = true;
    while changed {
        changed = false;
        for param in generics.type_params() {
            if receiver_type_params.contains(&param.ident.to_string()) {
                let before = receiver_type_params.len();
                collect_type_params_from_bounds(
                    &param.bounds,
                    &known_type_params,
                    &mut receiver_type_params,
                );
                if let Some(default) = &param.default {
                    collect_type_params(default, &known_type_params, &mut receiver_type_params);
                }
                changed |= receiver_type_params.len() != before;
            }
        }
    }

    let mut impl_generics = generics.clone();
    impl_generics.params = generics
        .params
        .iter()
        .filter(|param| match param {
            syn::GenericParam::Type(param) => {
                receiver_type_params.contains(&param.ident.to_string())
            }
            syn::GenericParam::Lifetime(_) | syn::GenericParam::Const(_) => false,
        })
        .cloned()
        .collect();
    impl_generics.where_clause = None;

    let mut method_generics = generics.clone();
    method_generics.params = generics
        .params
        .iter()
        .filter(|param| match param {
            syn::GenericParam::Type(param) => {
                !receiver_type_params.contains(&param.ident.to_string())
            }
            syn::GenericParam::Lifetime(_) | syn::GenericParam::Const(_) => true,
        })
        .cloned()
        .collect();

    (impl_generics, method_generics)
}

fn collect_type_params(
    ty: &syn::Type,
    known: &std::collections::HashSet<String>,
    found: &mut std::collections::HashSet<String>,
) {
    match ty {
        syn::Type::Reference(reference) => collect_type_params(&reference.elem, known, found),
        syn::Type::Path(path) => {
            if let Some(qself) = &path.qself {
                collect_type_params(&qself.ty, known, found);
            }
            for segment in &path.path.segments {
                let ident = segment.ident.to_string();
                if known.contains(&ident) {
                    found.insert(ident);
                }
                if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                    for arg in &args.args {
                        match arg {
                            syn::GenericArgument::Type(ty) => collect_type_params(ty, known, found),
                            syn::GenericArgument::AssocType(assoc) => {
                                collect_type_params(&assoc.ty, known, found);
                            }
                            _ => {}
                        }
                    }
                } else if let syn::PathArguments::Parenthesized(args) = &segment.arguments {
                    for input in &args.inputs {
                        collect_type_params(input, known, found);
                    }
                    if let syn::ReturnType::Type(_, output) = &args.output {
                        collect_type_params(output, known, found);
                    }
                }
            }
        }
        syn::Type::TraitObject(trait_object) => {
            for bound in &trait_object.bounds {
                collect_type_params_from_bound(bound, known, found);
            }
        }
        syn::Type::BareFn(function) => {
            for input in &function.inputs {
                collect_type_params(&input.ty, known, found);
            }
            if let syn::ReturnType::Type(_, output) = &function.output {
                collect_type_params(output, known, found);
            }
        }
        syn::Type::Tuple(tuple) => {
            for elem in &tuple.elems {
                collect_type_params(elem, known, found);
            }
        }
        syn::Type::Paren(paren) => collect_type_params(&paren.elem, known, found),
        syn::Type::Group(group) => collect_type_params(&group.elem, known, found),
        syn::Type::Slice(slice) => collect_type_params(&slice.elem, known, found),
        syn::Type::Array(array) => collect_type_params(&array.elem, known, found),
        syn::Type::Ptr(ptr) => collect_type_params(&ptr.elem, known, found),
        _ => {}
    }
}

fn collect_constraining_type_params(
    ty: &syn::Type,
    known: &HashSet<String>,
    found: &mut HashSet<String>,
) -> bool {
    match ty {
        syn::Type::Reference(reference) => {
            collect_constraining_type_params(&reference.elem, known, found)
        }
        syn::Type::Path(path) => {
            if path.qself.is_some() {
                let mut qself_params = HashSet::new();
                collect_type_params(ty, known, &mut qself_params);
                return qself_params.is_empty();
            }

            if path.path.segments.len() == 1 {
                let segment = &path.path.segments[0];
                let ident = segment.ident.to_string();
                if known.contains(&ident) && segment.arguments.is_empty() {
                    found.insert(ident);
                    return true;
                }
            } else if path
                .path
                .segments
                .first()
                .is_some_and(|segment| known.contains(&segment.ident.to_string()))
            {
                return false;
            }

            for segment in &path.path.segments {
                match &segment.arguments {
                    syn::PathArguments::AngleBracketed(args) => {
                        for arg in &args.args {
                            match arg {
                                syn::GenericArgument::Type(ty) => {
                                    if !collect_constraining_type_params(ty, known, found) {
                                        return false;
                                    }
                                }
                                syn::GenericArgument::AssocType(assoc) => {
                                    let mut assoc_params = HashSet::new();
                                    collect_type_params(&assoc.ty, known, &mut assoc_params);
                                    if !assoc_params.is_empty() {
                                        return false;
                                    }
                                }
                                syn::GenericArgument::Constraint(constraint) => {
                                    let mut constraint_params = HashSet::new();
                                    collect_type_params_from_bounds(
                                        &constraint.bounds,
                                        known,
                                        &mut constraint_params,
                                    );
                                    if !constraint_params.is_empty() {
                                        return false;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    syn::PathArguments::Parenthesized(args) => {
                        let mut arg_params = HashSet::new();
                        for input in &args.inputs {
                            collect_type_params(input, known, &mut arg_params);
                        }
                        if let syn::ReturnType::Type(_, output) = &args.output {
                            collect_type_params(output, known, &mut arg_params);
                        }
                        if !arg_params.is_empty() {
                            return false;
                        }
                    }
                    syn::PathArguments::None => {}
                }
            }
            true
        }
        syn::Type::Tuple(tuple) => tuple
            .elems
            .iter()
            .all(|elem| collect_constraining_type_params(elem, known, found)),
        syn::Type::Paren(paren) => collect_constraining_type_params(&paren.elem, known, found),
        syn::Type::Group(group) => collect_constraining_type_params(&group.elem, known, found),
        syn::Type::Slice(slice) => collect_constraining_type_params(&slice.elem, known, found),
        syn::Type::Array(array) => collect_constraining_type_params(&array.elem, known, found),
        syn::Type::Ptr(ptr) => collect_constraining_type_params(&ptr.elem, known, found),
        syn::Type::TraitObject(_) | syn::Type::BareFn(_) => {
            let mut params = HashSet::new();
            collect_type_params(ty, known, &mut params);
            params.is_empty()
        }
        _ => true,
    }
}

fn collect_type_params_from_bounds(
    bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, syn::Token![+]>,
    known: &std::collections::HashSet<String>,
    found: &mut std::collections::HashSet<String>,
) {
    for bound in bounds {
        collect_type_params_from_bound(bound, known, found);
    }
}

fn collect_type_params_from_bound(
    bound: &syn::TypeParamBound,
    known: &std::collections::HashSet<String>,
    found: &mut std::collections::HashSet<String>,
) {
    if let syn::TypeParamBound::Trait(bound) = bound {
        for segment in &bound.path.segments {
            match &segment.arguments {
                syn::PathArguments::AngleBracketed(args) => {
                    for arg in &args.args {
                        match arg {
                            syn::GenericArgument::Type(ty) => {
                                collect_type_params(ty, known, found);
                            }
                            syn::GenericArgument::AssocType(assoc) => {
                                collect_type_params(&assoc.ty, known, found);
                            }
                            syn::GenericArgument::Constraint(constraint) => {
                                collect_type_params_from_bounds(&constraint.bounds, known, found);
                            }
                            _ => {}
                        }
                    }
                }
                syn::PathArguments::Parenthesized(args) => {
                    for input in &args.inputs {
                        collect_type_params(input, known, found);
                    }
                    if let syn::ReturnType::Type(_, output) = &args.output {
                        collect_type_params(output, known, found);
                    }
                }
                syn::PathArguments::None => {}
            }
        }
    }
}

/// Generate code for an imported static
fn generate_static(
    st: &ImportStatic,
    krate: &TokenStream,
    prefix: &str,
) -> syn::Result<TokenStream> {
    fn next_thread_local_id() -> u32 {
        static THREAD_LOCAL_ID: AtomicU32 = AtomicU32::new(0);
        THREAD_LOCAL_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    let vis = &st.vis;
    let rust_name = &st.rust_name;
    let ty = &st.ty;
    let span = rust_name.span();

    // Generate JavaScript code to access the static
    let js_code = generate_static_js_code(st, prefix);

    assert!(st.thread_local_v2);
    let id = next_thread_local_id();

    // Generate a lazily-initialized thread-local static
    // Type information is now passed at call time via JSFunction::call
    Ok(quote_spanned! {span=>
        #vis static #rust_name: #krate::JsThreadLocal<#ty> = {
            // This can't be named __init for compat with older rustc versions
            // https://github.com/rust-lang/rust/issues/147006
            fn __init_wbg() -> #ty {
                #krate::__wry_call_js_function!(#js_code, fn() -> #ty, ())
            }
            #krate::JsThreadLocal::new(__init_wbg, #id)
        };
    })
}

/// Generate JavaScript code to access a static value
fn generate_static_js_code(st: &ImportStatic, prefix: &str) -> String {
    let js_name = &st.js_name;

    // Build the prefix with namespace if present
    let full_prefix = if let Some(ref namespace) = st.js_namespace {
        if !namespace.is_empty() {
            format!("{prefix}{}.", namespace.join("."))
        } else {
            prefix.to_string()
        }
    } else {
        prefix.to_string()
    };

    format!("() => {full_prefix}{js_name}")
}

/// Generate code for a string enum
fn generate_string_enum(string_enum: &StringEnum, krate: &TokenStream) -> syn::Result<TokenStream> {
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

/// Generate code for an exported struct
fn generate_export_struct(s: &ExportStruct, krate: &TokenStream) -> syn::Result<TokenStream> {
    let vis = &s.vis;
    let rust_name = &s.rust_name;
    let js_name = &s.js_name;
    let rust_attrs = &s.rust_attrs;
    let span = rust_name.span();

    // Generate field definitions for the struct
    let field_defs: Vec<_> = s
        .fields
        .iter()
        .map(|f| {
            let field_vis = &f.vis;
            let field_name = &f.rust_name;
            let field_ty = &f.ty;
            quote_spanned! {span=> #field_vis #field_name: #field_ty }
        })
        .collect();

    // Generate the struct itself
    let struct_def = quote_spanned! {span=>
        #(#rust_attrs)*
        #vis struct #rust_name {
            #(#field_defs),*
        }
    };

    // Generate field getters and setters
    let mut field_impls = TokenStream::new();
    for field in &s.fields {
        field_impls.extend(generate_field_accessor(rust_name, field, krate)?);
    }

    // Generate drop function
    let drop_fn_name = format!("{js_name}::__drop");
    let drop_impl = quote_spanned! {span=>
        // Drop function for the struct
        const _: () = {
            #[allow(non_upper_case_globals)]
            static __DROP_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                #drop_fn_name,
                |decoder| {
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(
                        decoder
                    )?;
                    #krate::object_store::drop_object(handle);
                    Ok(#krate::EncodedData::new())
                }
            );

            #krate::inventory::submit! {
                __DROP_SPEC
            }
        };
    };

    // Generate inspectable methods if enabled
    let inspectable_impl = if s.is_inspectable {
        generate_inspectable(rust_name, &s.fields, js_name, krate)?
    } else {
        TokenStream::new()
    };

    // Generate From<StructName> for JsValue - inserts into object store and returns handle
    let into_jsvalue_impl = quote_spanned! {span=>
        impl ::core::convert::From<#rust_name> for #krate::JsValue {
            fn from(val: #rust_name) -> Self {
                let handle = #krate::object_store::insert_object(val);
                // Create a JS object wrapper with the handle
                #krate::object_store::create_js_wrapper::<#rust_name>(handle, #js_name)
            }
        }
    };

    // Generate EncodeTypeDef - exported structs use HeapRef encoding
    let encode_type_def_impl = quote_spanned! {span=>
        impl #krate::EncodeTypeDef for #rust_name {
            fn encode_type_def(buf: &mut #krate::alloc::vec::Vec<u8>) {
                buf.push(#krate::encode::TypeTag::HeapRef as u8);
            }
        }
    };

    // Generate BinaryEncode - encode struct by converting to JsValue
    let binary_encode_impl = quote_spanned! {span=>
        impl #krate::BinaryEncode for #rust_name {
            fn encode(self, encoder: &mut #krate::EncodedData) {
                // Convert to JsValue (which inserts into object store and creates wrapper)
                let js_value = #krate::JsValue::from(self);
                // Encode the JsValue
                js_value.encode(encoder);
            }
        }
    };

    // Generate BinaryDecode - decode JsValue, extract handle, remove from object store
    let binary_decode_impl = quote_spanned! {span=>
        impl #krate::BinaryDecode for #rust_name {
            fn decode(decoder: &mut #krate::DecodedData) -> ::core::result::Result<Self, #krate::DecodeError> {
                // Decode the JsValue
                let js = #krate::JsValue::decode(decoder)?;
                // Extract handle from JS wrapper
                let handle = #krate::extract_rust_handle(&js)
                    .ok_or_else(|| #krate::DecodeError::Custom(
                        #krate::alloc::string::String::from("expected Rust object wrapper")
                    ))?;
                // Remove from object store and return owned value
                Ok(#krate::object_store::remove_object::<#rust_name>(handle))
            }
        }
    };

    // Generate BatchableResult - exported structs need flush to get actual value
    let batchable_result_impl = quote_spanned! {span=>
        impl #krate::BatchableResult for #rust_name {}
    };

    Ok(quote_spanned! {span=>
        #struct_def
        #field_impls
        #drop_impl
        #inspectable_impl
        #into_jsvalue_impl
        #encode_type_def_impl
        #binary_encode_impl
        #binary_decode_impl
        #batchable_result_impl
    })
}

/// Generate getter and setter for a struct field
fn generate_field_accessor(
    struct_name: &syn::Ident,
    field: &StructField,
    krate: &TokenStream,
) -> syn::Result<TokenStream> {
    let field_name = &field.rust_name;
    let js_field_name = &field.js_name;
    let field_ty = &field.ty;
    let span = field_name.span();

    // Only generate accessors for public fields
    if !matches!(field.vis, syn::Visibility::Public(_)) {
        return Ok(TokenStream::new());
    }

    let struct_name_str = struct_name.to_string();
    let getter_name = format!("{struct_name_str}::{js_field_name}_get");
    let setter_name = format!("{struct_name_str}::{js_field_name}_set");

    // Generate getter
    let getter_body = if field.getter_with_clone {
        quote_spanned! {span=>
            #krate::object_store::with_object::<#struct_name, _>(handle, |obj| {
                let val = ::core::clone::Clone::clone(&obj.#field_name);
                let mut encoder = #krate::EncodedData::new();
                <#field_ty as #krate::BinaryEncode>::encode(val, &mut encoder);
                Ok(encoder)
            })
        }
    } else {
        quote_spanned! {span=>
            #krate::object_store::with_object::<#struct_name, _>(handle, |obj| {
                let val = obj.#field_name;
                let mut encoder = #krate::EncodedData::new();
                <#field_ty as #krate::BinaryEncode>::encode(val, &mut encoder);
                Ok(encoder)
            })
        }
    };

    let getter_impl = quote_spanned! {span=>
        const _: () = {
            #[allow(non_upper_case_globals)]
            static __GETTER_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                #getter_name,
                |decoder| {
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #getter_body
                }
            );

            #krate::inventory::submit! {
                __GETTER_SPEC
            }
        };
    };

    // Generate setter (unless readonly)
    let setter_impl = if !field.readonly {
        quote_spanned! {span=>
            const _: () = {
                #[allow(non_upper_case_globals)]
                static __SETTER_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                    #setter_name,
                    |decoder| {
                        let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                        let val = <#field_ty as #krate::BinaryDecode>::decode(decoder)?;
                        #krate::object_store::with_object_mut::<#struct_name, _>(handle, |obj| {
                            obj.#field_name = val;
                        });
                        Ok(#krate::EncodedData::new())
                    }
                );

                #krate::inventory::submit! {
                    __SETTER_SPEC
                }
            };
        }
    } else {
        TokenStream::new()
    };

    // Generate JsClassMemberSpec for the property getter
    let js_class_name = struct_name.to_string();
    let getter_type_helpers =
        generate_member_type_helpers(&[], Some(quote_spanned! {span=> #field_ty }), krate, span);
    let getter_member_spec = quote_spanned! {span=>
        const _: () = {
            #getter_type_helpers

            #[allow(non_upper_case_globals)]
            static __GETTER_MEMBER_SPEC: #krate::JsClassMemberSpec = #krate::JsClassMemberSpec::new(
                #js_class_name,
                #js_field_name,
                #getter_name,
                0,
                __wry_arg_types,
                __wry_return_type,
                #krate::JsClassMemberKind::Getter
            );

            #krate::inventory::submit! {
                __GETTER_MEMBER_SPEC
            }
        };
    };

    // Generate JsClassMemberSpec for the property setter (unless readonly)
    let setter_member_spec = if !field.readonly {
        let setter_arg_types = vec![quote_spanned! {span=> #field_ty }];
        let setter_type_helpers =
            generate_member_type_helpers(&setter_arg_types, None, krate, span);
        quote_spanned! {span=>
            const _: () = {
                #setter_type_helpers

                #[allow(non_upper_case_globals)]
                static __SETTER_MEMBER_SPEC: #krate::JsClassMemberSpec = #krate::JsClassMemberSpec::new(
                    #js_class_name,
                    #js_field_name,
                    #setter_name,
                    1,
                    __wry_arg_types,
                    __wry_return_type,
                    #krate::JsClassMemberKind::Setter
                );

                #krate::inventory::submit! {
                    __SETTER_MEMBER_SPEC
                }
            };
        }
    } else {
        TokenStream::new()
    };

    Ok(quote_spanned! {span=>
        #getter_impl
        #setter_impl
        #getter_member_spec
        #setter_member_spec
    })
}

/// Generate toJSON and toString methods for inspectable structs
fn generate_inspectable(
    struct_name: &syn::Ident,
    fields: &[StructField],
    js_name: &str,
    krate: &TokenStream,
) -> syn::Result<TokenStream> {
    let span = struct_name.span();
    let to_json_name = format!("{js_name}::toJSON");
    let to_string_name = format!("{js_name}::toString");

    // Build JSON object from fields
    let field_names: Vec<_> = fields
        .iter()
        .filter(|f| matches!(f.vis, syn::Visibility::Public(_)))
        .map(|f| &f.js_name)
        .collect();
    let field_idents: Vec<_> = fields
        .iter()
        .filter(|f| matches!(f.vis, syn::Visibility::Public(_)))
        .map(|f| &f.rust_name)
        .collect();

    let struct_name_str = struct_name.to_string();

    let js_name_str = js_name.to_string();
    let string_return_type = Some(quote_spanned! {span=> ::alloc::string::String });
    let to_json_type_helpers =
        generate_member_type_helpers(&[], string_return_type.clone(), krate, span);
    let to_string_type_helpers = generate_member_type_helpers(&[], string_return_type, krate, span);

    Ok(quote_spanned! {span=>
        const _: () = {
            #[allow(non_upper_case_globals)]
            static __TO_JSON_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                #to_json_name,
                |decoder| {
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #krate::object_store::with_object::<#struct_name, _>(handle, |obj| {
                        // Create a simple JSON-like representation
                        let mut json = ::alloc::string::String::from("{");
                        #(
                            json.push_str(&::alloc::format!("\"{}\":{:?},", #field_names, obj.#field_idents));
                        )*
                        if json.ends_with(',') {
                            json.pop();
                        }
                        json.push('}');
                        let mut encoder = #krate::EncodedData::new();
                        <::alloc::string::String as #krate::BinaryEncode>::encode(json, &mut encoder);
                        Ok(encoder)
                    })
                }
            );

            #krate::inventory::submit! {
                __TO_JSON_SPEC
            }
        };

        // JsClassMemberSpec for toJSON method
        const _: () = {
            #to_json_type_helpers

            #[allow(non_upper_case_globals)]
            static __TO_JSON_MEMBER_SPEC: #krate::JsClassMemberSpec = #krate::JsClassMemberSpec::new(
                #js_name_str,
                "toJSON",
                #to_json_name,
                0,
                __wry_arg_types,
                __wry_return_type,
                #krate::JsClassMemberKind::Method
            );

            #krate::inventory::submit! {
                __TO_JSON_MEMBER_SPEC
            }
        };

        const _: () = {
            #[allow(non_upper_case_globals)]
            static __TO_STRING_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                #to_string_name,
                |decoder| {
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #krate::object_store::with_object::<#struct_name, _>(handle, |obj| {
                        let s = ::alloc::format!("[object {}]", #struct_name_str);
                        let mut encoder = #krate::EncodedData::new();
                        <::alloc::string::String as #krate::BinaryEncode>::encode(s, &mut encoder);
                        Ok(encoder)
                    })
                }
            );

            #krate::inventory::submit! {
                __TO_STRING_SPEC
            }
        };

        // JsClassMemberSpec for toString method
        const _: () = {
            #to_string_type_helpers

            #[allow(non_upper_case_globals)]
            static __TO_STRING_MEMBER_SPEC: #krate::JsClassMemberSpec = #krate::JsClassMemberSpec::new(
                #js_name_str,
                "toString",
                #to_string_name,
                0,
                __wry_arg_types,
                __wry_return_type,
                #krate::JsClassMemberKind::Method
            );

            #krate::inventory::submit! {
                __TO_STRING_MEMBER_SPEC
            }
        };
    })
}

/// Generate code for an exported method
fn generate_export_method(method: &ExportMethod, krate: &TokenStream) -> syn::Result<TokenStream> {
    let class = &method.class;
    let rust_name = &method.rust_name;
    let js_name = &method.js_name;
    let span = rust_name.span();

    let class_str = class.to_string();
    let export_name = format!("{class_str}::{js_name}");

    // Generate argument decoding
    let arg_names: Vec<_> = method.arguments.iter().map(|a| &a.name).collect();
    let arg_types: Vec<_> = method.arguments.iter().map(|a| &a.ty).collect();

    let decode_args = quote_spanned! {span=>
        #(
            let #arg_names = <#arg_types as #krate::BinaryDecode>::decode(decoder)?;
        )*
    };

    // Generate the method call and return encoding based on kind
    let method_body = match &method.kind {
        ExportMethodKind::Constructor => {
            // Constructor: create new instance and store in object store
            quote_spanned! {span=>
                #decode_args
                let result = #class::#rust_name(#(#arg_names),*);
                let handle = #krate::object_store::insert_object(result);
                let mut encoder = #krate::EncodedData::new();
                <#krate::object_store::ObjectHandle as #krate::BinaryEncode>::encode(handle, &mut encoder);
                Ok(encoder)
            }
        }
        ExportMethodKind::Method { self_ty } => {
            // Instance method: get object from store, call method
            let call = match self_ty {
                SelfType::RefShared => {
                    quote_spanned! {span=>
                        #krate::object_store::with_object::<#class, _>(handle, |obj| {
                            obj.#rust_name(#(#arg_names),*)
                        })
                    }
                }
                SelfType::RefMutable => {
                    quote_spanned! {span=>
                        #krate::object_store::with_object_mut::<#class, _>(handle, |obj| {
                            obj.#rust_name(#(#arg_names),*)
                        })
                    }
                }
                SelfType::ByValue => {
                    // Consuming method: remove from store
                    quote_spanned! {span=>
                        {
                            let obj = #krate::object_store::remove_object::<#class>(handle);
                            obj.#rust_name(#(#arg_names),*)
                        }
                    }
                }
            };

            if method.ret.is_some() {
                let ret_ty = method.ret.as_ref().unwrap();
                quote_spanned! {span=>
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #decode_args
                    let result = #call;
                    let mut encoder = #krate::EncodedData::new();
                    <#ret_ty as #krate::BinaryEncode>::encode(result, &mut encoder);
                    Ok(encoder)
                }
            } else {
                quote_spanned! {span=>
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #decode_args
                    #call;
                    Ok(#krate::EncodedData::new())
                }
            }
        }
        ExportMethodKind::StaticMethod => {
            // Static method: just call directly
            if let Some(ret_ty) = &method.ret {
                quote_spanned! {span=>
                    #decode_args
                    let result = #class::#rust_name(#(#arg_names),*);
                    let mut encoder = #krate::EncodedData::new();
                    <#ret_ty as #krate::BinaryEncode>::encode(result, &mut encoder);
                    Ok(encoder)
                }
            } else {
                quote_spanned! {span=>
                    #decode_args
                    #class::#rust_name(#(#arg_names),*);
                    Ok(#krate::EncodedData::new())
                }
            }
        }
        ExportMethodKind::Getter { property: _ } => {
            // Property getter: call the getter method
            if let Some(ret_ty) = &method.ret {
                quote_spanned! {span=>
                    let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                    #krate::object_store::with_object::<#class, _>(handle, |obj| {
                        let result = obj.#rust_name();
                        let mut encoder = #krate::EncodedData::new();
                        <#ret_ty as #krate::BinaryEncode>::encode(result, &mut encoder);
                        Ok(encoder)
                    })
                }
            } else {
                return Err(syn::Error::new(span, "getter must have a return type"));
            }
        }
        ExportMethodKind::Setter { property: _ } => {
            // Property setter: call the setter method
            let arg_ty = method
                .arguments
                .first()
                .map(|a| &a.ty)
                .ok_or_else(|| syn::Error::new(span, "setter must have an argument"))?;
            let arg_name = method.arguments.first().map(|a| &a.name).unwrap();

            quote_spanned! {span=>
                let handle = <#krate::object_store::ObjectHandle as #krate::BinaryDecode>::decode(decoder)?;
                let #arg_name = <#arg_ty as #krate::BinaryDecode>::decode(decoder)?;
                #krate::object_store::with_object_mut::<#class, _>(handle, |obj| {
                    obj.#rust_name(#arg_name);
                });
                Ok(#krate::EncodedData::new())
            }
        }
    };

    // Generate the actual impl method
    let vis = &method.vis;
    let body = &method.body;
    let rust_attrs = method.fn_rust_attrs();
    let arg_names_idents: Vec<_> = method.arguments.iter().map(|a| &a.name).collect();
    let arg_types_refs: Vec<_> = method.arguments.iter().map(|a| &a.ty).collect();

    let fn_args: Vec<_> = arg_names_idents
        .iter()
        .zip(arg_types_refs.iter())
        .map(|(name, ty)| quote_spanned! {span=> #name: #ty })
        .collect();

    let ret_type = match &method.ret {
        Some(ty) => quote_spanned! {span=> -> #ty },
        None => quote_spanned! {span=> },
    };

    let allows = clippy_allows();
    let method_impl = match &method.kind {
        ExportMethodKind::Constructor | ExportMethodKind::StaticMethod => {
            // No self parameter
            quote_spanned! {span=>
                impl #class {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name(#(#fn_args),*) #ret_type #body
                }
            }
        }
        ExportMethodKind::Method { self_ty } => {
            let receiver = match self_ty {
                SelfType::RefShared => quote_spanned! {span=> &self },
                SelfType::RefMutable => quote_spanned! {span=> &mut self },
                SelfType::ByValue => quote_spanned! {span=> self },
            };
            let fn_args_with_self = if fn_args.is_empty() {
                quote_spanned! {span=> #receiver }
            } else {
                quote_spanned! {span=> #receiver, #(#fn_args),* }
            };
            quote_spanned! {span=>
                impl #class {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name(#fn_args_with_self) #ret_type #body
                }
            }
        }
        ExportMethodKind::Getter { .. } => {
            quote_spanned! {span=>
                impl #class {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name(&self) #ret_type #body
                }
            }
        }
        ExportMethodKind::Setter { .. } => {
            quote_spanned! {span=>
                impl #class {
                    #allows
                    #rust_attrs
                    #vis fn #rust_name(&mut self, #(#fn_args),*) #body
                }
            }
        }
    };

    // Generate JsClassMemberSpec for the method
    let arg_count = method.arguments.len();
    let member_arg_types: Vec<TokenStream> = method
        .arguments
        .iter()
        .map(|arg| {
            let ty = &arg.ty;
            quote_spanned! {span=> #ty }
        })
        .collect();
    let member_return_type = match &method.kind {
        ExportMethodKind::Constructor => {
            Some(quote_spanned! {span=> #krate::object_store::ObjectHandle })
        }
        ExportMethodKind::Setter { .. } => None,
        _ => method.ret.as_ref().map(|ty| quote_spanned! {span=> #ty }),
    };
    let member_type_helpers =
        generate_member_type_helpers(&member_arg_types, member_return_type, krate, span);
    let (member_name, member_kind) = match &method.kind {
        ExportMethodKind::Constructor => (
            js_name.clone(),
            quote! { #krate::JsClassMemberKind::Constructor },
        ),
        ExportMethodKind::Method { .. } => (
            js_name.clone(),
            quote! { #krate::JsClassMemberKind::Method },
        ),
        ExportMethodKind::StaticMethod => (
            js_name.clone(),
            quote! { #krate::JsClassMemberKind::StaticMethod },
        ),
        ExportMethodKind::Getter { property } => (
            property.clone(),
            quote! { #krate::JsClassMemberKind::Getter },
        ),
        ExportMethodKind::Setter { property } => (
            property.clone(),
            quote! { #krate::JsClassMemberKind::Setter },
        ),
    };

    let js_class_member_spec = quote_spanned! {span=>
        const _: () = {
            #member_type_helpers

            #[allow(non_upper_case_globals)]
            static __CLASS_MEMBER_SPEC: #krate::JsClassMemberSpec = #krate::JsClassMemberSpec::new(
                #class_str,
                #member_name,
                #export_name,
                #arg_count,
                __wry_arg_types,
                __wry_return_type,
                #member_kind
            );

            #krate::inventory::submit! {
                __CLASS_MEMBER_SPEC
            }
        };
    };

    Ok(quote_spanned! {span=>
        #method_impl

        const _: () = {
            #[allow(non_upper_case_globals)]
            static __EXPORT_SPEC: #krate::JsExportSpec = #krate::JsExportSpec::new(
                #export_name,
                |decoder| {
                    #method_body
                }
            );

            #krate::inventory::submit! {
                __EXPORT_SPEC
            }
        };

        #js_class_member_spec
    })
}

/// Extract the Ok type from a Result<T, E> type, or None if not a Result
fn extract_result_ok_type(ty: &syn::Type) -> Option<syn::Type> {
    if let syn::Type::Path(type_path) = ty {
        let segment = type_path.path.segments.last()?;
        if segment.ident != "Result" {
            return None;
        }
        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments
            && let Some(syn::GenericArgument::Type(ok_ty)) = args.args.first()
        {
            return Some(ok_ty.clone());
        }
    }
    None
}

/// Check if a type is the unit type ()
fn is_unit_type(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(tuple) if tuple.elems.is_empty())
}
