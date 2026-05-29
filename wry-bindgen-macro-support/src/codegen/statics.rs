use std::sync::atomic::AtomicU32;

use crate::ast::ImportStatic;
use proc_macro2::TokenStream;
use quote::quote_spanned;

use super::js::namespace_prefix;

pub(super) fn generate_static(
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
    let full_prefix = namespace_prefix(prefix, st.js_namespace.as_deref());

    format!("() => {full_prefix}{js_name}")
}
