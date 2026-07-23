# Task 4 Fix 2 Report: Owner-Executable Managed Hook

## Status

Complete. On Unix, a setup-managed hook is now classified as managed only when
its owner execute bit is set. This keeps the existing non-Unix behavior and
Task 4 setup architecture unchanged.

## TDD evidence

The focused regression was added before the production change. It installs the
managed hook through setup, changes its mode to `0601`, confirms inspection
treats it as repairable, reruns setup, and verifies the owner execute bit is
restored.

RED command:

```text
cd rust && cargo test -p mko-core --test setup managed_hook_without_owner_execute_permission_is_repaired_on_rerun -- --exact
```

Observed RED: exit `101`; the assertion expected `HookState::Missing` after
mode `0601`, but received `HookState::Managed`.

GREEN command:

```text
cd rust && cargo test -p mko-core --test setup managed_hook_without_owner_execute_permission_is_repaired_on_rerun -- --exact
```

Observed GREEN: exit `0`; 1 passed, 0 failed.

## Implementation

The Unix-only `hook_is_executable` predicate now checks `0o100` instead of any
bit in `0o111`. Setup-created hooks are owner-owned, so a group/other execute
bit alone can no longer suppress the existing repair path. No Windows or other
non-Unix behavior changed.

## Verification

```text
cd rust && cargo test -p mko-core --test setup                         exit 0 (32 passed)
cd rust && cargo test -p mko-core hooks::tests                         exit 0 (2 hook tests passed)
scripts/fmt.sh --check                                                 exit 0
cd rust && cargo clippy --workspace --all-targets -- -D warnings       exit 0
cd rust && cargo test --workspace                                      exit 0
git diff --check                                                       exit 0
```

The full workspace run reported zero failures; one synchronized acceptance
helper remained ignored by design.

## Platform limit

The permission regression and owner-bit behavior are Unix-only and were run on
macOS. The existing non-Unix branch remains unchanged; native Windows execution
is not claimed.
