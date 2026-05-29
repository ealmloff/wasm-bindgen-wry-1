#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
submodule_dir="$repo_root/wasm-bindgen"
patch_dir="$repo_root/patches/wasm-bindgen"
default_base="0.2.122"

usage() {
    cat <<'USAGE'
Usage:
  scripts/wasm-bindgen-patches.sh regen [base-ref]
  scripts/wasm-bindgen-patches.sh apply [base-ref]

Commands:
  regen   Regenerate patches/wasm-bindgen/*.patch from wasm-bindgen [base-ref]..HEAD.
  apply   Reset wasm-bindgen to [base-ref] and apply patches/wasm-bindgen/*.patch.

If [base-ref] is omitted, apply uses patches/wasm-bindgen/BASE when present,
otherwise both commands default to 0.2.122.
USAGE
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

ensure_submodule() {
    [[ -d "$submodule_dir" ]] || die "missing wasm-bindgen submodule at $submodule_dir"
    git -C "$submodule_dir" rev-parse --git-dir >/dev/null
}

ensure_clean_submodule() {
    local status
    status="$(git -C "$submodule_dir" status --porcelain)"
    if [[ -n "$status" ]]; then
        printf '%s\n' "$status" >&2
        die "wasm-bindgen submodule has uncommitted changes"
    fi
}

resolve_base_ref() {
    local requested="${1:-}"
    if [[ -n "$requested" ]]; then
        printf '%s\n' "$requested"
    elif [[ -f "$patch_dir/BASE" ]]; then
        head -n 1 "$patch_dir/BASE"
    else
        printf '%s\n' "$default_base"
    fi
}

resolve_commit() {
    local ref="$1"
    git -C "$submodule_dir" rev-parse --verify "$ref^{commit}"
}

regen() {
    local base_ref base_commit head_commit
    base_ref="$(resolve_base_ref "${1:-}")"

    ensure_submodule
    ensure_clean_submodule

    base_commit="$(resolve_commit "$base_ref")"
    head_commit="$(git -C "$submodule_dir" rev-parse HEAD)"
    [[ "$base_commit" != "$head_commit" ]] || die "base-ref resolves to HEAD; no patches to generate"

    mkdir -p "$patch_dir"
    rm -f "$patch_dir"/*.patch "$patch_dir/BASE"
    git -C "$submodule_dir" format-patch --output-directory "$patch_dir" "$base_commit..HEAD"
    printf '%s\n' "$base_commit" >"$patch_dir/BASE"

    printf 'generated patches from %s..%s in %s\n' "$base_commit" "$head_commit" "$patch_dir"
}

apply_patches() {
    local base_ref base_commit
    base_ref="$(resolve_base_ref "${1:-}")"

    ensure_submodule
    ensure_clean_submodule

    base_commit="$(resolve_commit "$base_ref")"
    shopt -s nullglob
    local patches=("$patch_dir"/*.patch)
    shopt -u nullglob
    ((${#patches[@]} > 0)) || die "no patchfiles found in $patch_dir"

    git -C "$submodule_dir" reset --hard "$base_commit"
    git -C "$submodule_dir" am -3 "${patches[@]}"

    printf 'applied %s patches on %s\n' "${#patches[@]}" "$base_commit"
    printf 'wasm-bindgen HEAD is now %s\n' "$(git -C "$submodule_dir" rev-parse HEAD)"
    printf 'commit the updated submodule pointer in the parent repo when ready\n'
}

command="${1:-}"
case "$command" in
regen)
    shift
    (($# <= 1)) || die "regen accepts at most one base-ref"
    regen "${1:-}"
    ;;
apply)
    shift
    (($# <= 1)) || die "apply accepts at most one base-ref"
    apply_patches "${1:-}"
    ;;
-h | --help | help)
    usage
    ;;
*)
    usage >&2
    exit 2
    ;;
esac
