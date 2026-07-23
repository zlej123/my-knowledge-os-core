# Task 3 Fix 2 Report: Windows Unsafe-Code Isolation

## Status

Resolved the remaining Task 3 Windows compile-policy defect without weakening
`mko-core`'s `#![forbid(unsafe_code)]` policy. Raw Win32 ACL operations now live
in the private `mko-windows-acl` workspace crate. `mko-core` depends on that
crate only on Windows, consumes only safe functions and owned inspection data,
and retains the pure ACL policy validator and all Task 3 error semantics.

No Task 4+ behavior or v0.1 behavior was changed.

## Root cause

`mko-core/src/lib.rs` unconditionally forbids unsafe code, but the
`#[cfg(windows)]` implementation in `profile.rs` contained raw Win32 calls in
unsafe blocks. The macOS build did not compile that module; a Windows build
would enforce the crate-level prohibition and reject it.

## TDD RED evidence

The source/compile-policy regression test was added before the crate split.

Command:

```text
cd rust && cargo test -p mko-core --test safety_policy
```

Exit status: `101`.

The test failed with:

```text
unsafe Rust found in .../rust/mko-core/src/profile.rs
test result: FAILED. 0 passed; 1 failed
```

This failure proved the test detected the target-gated unsafe code on the
non-Windows host rather than relying on the host compiler to select it.

## Implementation and safety boundary

- Added workspace crate `mko-windows-acl` with `publish = false`.
- Moved token/SID ownership, ACL allocation, security descriptor inspection,
  handle-to-final-path resolution, and all Win32 FFI into that crate.
- Retained the exact minimal `windows-sys = 0.61.2` feature set: Foundation,
  Security, Security Authorization, Storage FileSystem, and System Threading.
- Added `deny(unsafe_op_in_unsafe_fn)` and
  `deny(clippy::undocumented_unsafe_blocks)` to the helper. Every FFI block has
  a local safety justification; the public API exposes no unsafe function or
  raw pointer.
- Kept protected-DACL/current-owner/single-full-control-ACE policy validation
  in safe `mko-core` code.
- Preserved `profile_write_failed` versus `profile_permissions_invalid` by
  returning a safe error category across the helper boundary.
- Removed the direct `windows-sys` dependency and every unsafe block from
  `mko-core`.

The portable regression test verifies the crate-wide prohibition, scans
`mko-core/src` for unsafe constructs, verifies the dependency boundary and
private helper manifest, and rejects a public unsafe helper function.

## TDD GREEN and focused evidence

After the split, the same command exited `0`: `1 passed; 0 failed`.

Focused command:

```text
cd rust && \
  cargo test -p mko-core --lib profile::tests && \
  cargo test -p mko-core --test profile_and_context \
    --test capture --test pdf_prepare --test safety_policy
```

Exit status: `0`.

- pure ACL policy: `2 passed; 0 failed`;
- profile/context: `11 passed; 0 failed`;
- capture: `9 passed; 0 failed`;
- PDF prepare: `21 passed; 0 failed`;
- safety isolation: `1 passed; 0 failed`.

## Windows-native coverage and host limitation

The helper has `#[cfg(windows)]` native tests proving its three exported ACL
operations coerce to safe function pointers, applying an ACL to a real
directory and file, inspecting both, and retaining write-versus-permission
error categories. Existing Windows `ProfileStore` tests still cover the full
safe integration path and post-commit behavior.

These native tests were not executed on this macOS host. `rustup target list
--installed` returned only `aarch64-apple-darwin`, so the instructed Windows
cross-check was not re-attempted. The prior exact limitation remains:

```text
cargo check -p mko-core --tests --target x86_64-pc-windows-msvc
E0463: can't find crate for `core`
note: the `x86_64-pc-windows-msvc` target may not be installed
```

The portable policy regression proves isolation, but it does not replace
native Windows compilation or execution.

## Final verification

Fresh combined command:

```text
scripts/fmt.sh --check && \
  cd rust && \
  cargo clippy --workspace --all-targets -- -D warnings && \
  cargo test --workspace && \
  git diff --check
```

Exit status: `0`.

Formatting and whitespace checks produced no findings. Clippy completed all
three workspace crates with warnings denied. The full workspace suite passed;
the synchronized acceptance lock-holder helper remained the single intentional
ignored test. On macOS the Windows-only helper correctly compiled as an empty
target-gated crate, so its two native tests remain explicitly unexecuted.
