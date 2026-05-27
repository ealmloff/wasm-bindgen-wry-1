#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$script_dir"
wasm_bindgen_submodule="$repo_root/wasm-bindgen"
patch_base_file="$repo_root/patches/wasm-bindgen/BASE"

public_api_args=(
    public-api
    --omit
    blanket-impls,auto-trait-impls,auto-derived-impls
)
tmp_dir=""

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

cleanup() {
    if [[ -n "${tmp_dir:-}" ]]; then
        rm -rf "$tmp_dir"
    fi
}

run_public_api() {
    local output="$1"
    local log="$2"
    shift 2

    if ! cargo "${public_api_args[@]}" "$@" >"$output" 2>"$log"; then
        cat "$log" >&2
        return 1
    fi
}

resolve_wasm_bindgen_ref() {
    if [[ $# -gt 1 ]]; then
        die "usage: $0 [wasm-bindgen-ref]"
    fi

    if [[ $# -eq 1 ]]; then
        printf '%s\n' "$1"
    elif [[ -f "$patch_base_file" ]]; then
        head -n 1 "$patch_base_file"
    else
        printf 'HEAD\n'
    fi
}

prune_upstream_workspace() {
    local manifest="$1"

    # Keep the upstream package dependencies intact, but avoid resolving the
    # full wasm-bindgen workspace. Some examples have git dependencies that are
    # irrelevant for this API comparison.
    perl -0pi -e 's/members = \[\n.*?\n\]/members = [\n  "crates\/macro",\n  "crates\/macro-support",\n  "crates\/shared",\n]/s' "$manifest"
}

filter_cosmetic() {
    awk '
        /<&'\''static wasm_bindgen::JsValue as/ { next }
        /^pub type.*Prim[[:digit:]]/ { next }
        /convert::/ { next }
        /WasmRet/ { next }
        /WasmSlice/ { next }
        /WasmAbi/ { next }
        /WasmPrimitive/ { next }
        /WasmClosure/ { next }
        /IntoWasmClosure/ { next }
        /into_abi/ { next }
        /from_abi/ { next }
        /::Abi/ { next }
        /::Anchor/ { next }
        /JsStatic/ { next }
        /impl core::ops.*for &wasm_bindgen::JsValue$/ { next }
        /impl<'\''a> core::cmp::PartialEq<&'\''a/ { next }
        /\?core::marker::Sized/ { next }
        /^pub struct wasm_bindgen::JsError$/ { next }
        /^pub struct wasm_bindgen::prelude::JsError$/ { next }
        { print }
    '
}

normalize_api() {
    perl -pe '
        s/\bwry_bindgen\b/wasm_bindgen/g;
        s/\b(?:wasm_bindgen|crate)::__rt::marker::ErasableGeneric\b/ErasableGeneric/g;
        s/\b(?:wasm_bindgen|crate)::__rt::(RefMut|Ref|WasmWord)\b/$1/g;
        s/^pub unsafe trait wasm_bindgen::ErasableGeneric$/pub use wasm_bindgen::ErasableGeneric/;
    '
}

generate_upstream_api() {
    local upstream_ref="$1"
    local checkout_dir="$2"
    local output="$3"

    mkdir -p "$checkout_dir"

    git -C "$wasm_bindgen_submodule" rev-parse --verify --quiet "$upstream_ref^{tree}" >/dev/null ||
        die "could not resolve wasm-bindgen ref '$upstream_ref'"

    git -C "$wasm_bindgen_submodule" archive "$upstream_ref" | tar -x -C "$checkout_dir"

    if [[ -f "$wasm_bindgen_submodule/Cargo.lock" ]]; then
        cp "$wasm_bindgen_submodule/Cargo.lock" "$checkout_dir/Cargo.lock"
    fi

    prune_upstream_workspace "$checkout_dir/Cargo.toml"

    (
        cd "$checkout_dir"
        run_public_api "$output" "$output.log" -p wasm-bindgen
    )
}

generate_wry_api() {
    local output="$1"

    run_public_api "$output" "$output.log" --manifest-path "$repo_root/wry-bindgen/Cargo.toml"
}

main() {
    local upstream_ref
    upstream_ref="$(resolve_wasm_bindgen_ref "$@")"

    [[ -d "$wasm_bindgen_submodule" ]] || die "missing wasm-bindgen submodule at $wasm_bindgen_submodule"

    tmp_dir="$(mktemp -d)"
    trap cleanup EXIT

    local upstream_api="$tmp_dir/wasm-bindgen.api.txt"
    local wry_api="$tmp_dir/wry-bindgen.api.txt"
    local upstream_filtered="$tmp_dir/wasm-bindgen.filtered.txt"
    local wry_filtered="$tmp_dir/wry-bindgen.filtered.txt"

    echo "Generating wasm-bindgen public API from $upstream_ref..."
    generate_upstream_api "$upstream_ref" "$tmp_dir/upstream-wasm-bindgen" "$upstream_api"

    echo "Generating wry-bindgen public API..."
    generate_wry_api "$wry_api"

    normalize_api <"$upstream_api" | filter_cosmetic | sort -u >"$upstream_filtered"
    normalize_api <"$wry_api" | filter_cosmetic | sort -u >"$wry_filtered"

    echo ""
    echo "APIs in wasm-bindgen but NOT in wry-bindgen:"
    echo "============================================="

    local missing_api="$tmp_dir/missing-api.txt"
    comm -23 "$upstream_filtered" "$wry_filtered" >"$missing_api"

    if [[ -s "$missing_api" ]]; then
        cat "$missing_api"
    else
        echo "No missing APIs found after filters."
    fi
}

main "$@"
