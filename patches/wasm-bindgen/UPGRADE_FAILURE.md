# wasm-bindgen upgrade to 0.2.122

- Current vendored version: 0.2.121
- Latest upstream release: 0.2.122
- Workflow run: https://github.com/ealmloff/wasm-bindgen-wry-1/actions/runs/26655963305

## Result

The workflow applies patches/wasm-bindgen onto the new upstream release, replaces the tracked wasm-bindgen directory with the patched result, regenerates patches against the new upstream base, and bumps local crate versions.

### Cloned upstream wasm-bindgen

```text
Cloning into '/home/runner/work/_temp/upstream-wasm-bindgen'...
```

### Checked out upstream 0.2.122

```text
Note: switching to '0.2.122'.

You are in 'detached HEAD' state. You can look around, make experimental
changes and commit them, and you can discard any commits you make in this
state without impacting any branches by switching back to a branch.

If you want to create a new branch to retain commits you create, you may
do so (now or later) by using -c with the switch command. Example:

  git switch -c <new-branch-name>

Or undo this operation with:

  git switch -

Turn off this advice by setting config variable advice.detachedHead to false

HEAD is now at ddd322514 release: 0.2.122 - schema bump (#5162)
```

### Normalized wasm-bindgen patch versions

```text
Normalized 4 patch file(s) from base 49457f2db4465688cb597e9030ccfdefbd2b662e to ddd322514d87a4b21342b7ab9a9d70796fc60576.
Committed patch files were not modified before patch application.
```

### Failed to apply wasm-bindgen patch stack

```text
Committer identity unknown

*** Please tell me who you are.

Run

  git config --global user.email "you@example.com"
  git config --global user.name "Your Name"

to set your account's default identity.
Omit --global to set the identity only in this repository.

fatal: empty ident name (for <runner@runnervm3jyl0.slfmtg5fcnvuxdwlsq1e2o14kh.cx.internal.cloudapp.net>) not allowed

Upstream worktree status:
```

## Manual work required

Failed step: Failed to apply wasm-bindgen patch stack

The tracked wasm-bindgen directory was left at the current committed version. See this report for logs.
