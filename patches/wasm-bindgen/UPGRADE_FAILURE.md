# wasm-bindgen upgrade to 0.2.128

- Current vendored version: 0.2.121
- Target upstream release/ref: 0.2.128
- Workflow run: https://github.com/ealmloff/wasm-bindgen-wry-1/actions/runs/34136199696

## Result

The script applies patches/wasm-bindgen onto the target upstream release/ref, replaces the tracked wasm-bindgen directory with the patched result, regenerates patches against the new upstream base, and bumps local crate versions.

### Cloned upstream wasm-bindgen

```text
Cloning into '/home/runner/work/_temp/wasm-bindgen-upgrade.Roda4t/upstream-wasm-bindgen'...
```

### Checked out upstream 0.2.128

```text
Note: switching to '0.2.128'.

You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by using -c with the switch command. Example:

  git switch -c <new-branch-name>

Or undo this operation with:

  git switch -

Turn off this advice by setting config variable advice.detachedHead to false

HEAD is now at 246946fdd Release 0.2.128 (#5319)
```

### Normalized wasm-bindgen patch versions

```text
Normalized 4 patch file(s) from base 49457f2db4465688cb597e9030ccfdefbd2b662e to 246946fddd62163e778c3a1f6afe7264347adceb.
Committed patch files were not modified before patch application.
```

### Failed to apply wasm-bindgen patch stack

```text
Applying: Prepare root workspace for wry patching
Using index info to reconstruct a base tree...
M	Cargo.toml
Falling back to patching base and 3-way merge...
Auto-merging Cargo.toml
CONFLICT (content): Merge conflict in Cargo.toml
error: Failed to merge in the changes.
hint: Use 'git am --show-current-patch=diff' to see the failed patch
hint: When you have resolved this problem, run "git am --continue".
hint: If you prefer to skip this patch, run "git am --skip" instead.
hint: To restore the original branch and stop patching, run "git am --abort".
hint: Disable this message with "git config set advice.mergeConflict false"
Patch failed at 0001 Prepare root workspace for wry patching

Upstream worktree status:
UU Cargo.toml
```

## Manual work required

Failed step: Failed to apply wasm-bindgen patch stack

The tracked wasm-bindgen directory may contain a partial update if the failure occurred after replacement. See this report for logs.
