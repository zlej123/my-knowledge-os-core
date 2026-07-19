# Task 7: Actionable Doctor Diagnostics Report

## Status

Implemented `mko doctor` as a strictly read-only diagnostic facade. It reports product and contract versions, profile, repository, provider, hydration, managed-hook, and lock state; it never repairs, creates, writes, invokes setup, or runs Git mutation commands.

## TDD evidence

### RED

Command run from `rust/`:

```text
cargo test -p mko-core --test doctor && cargo test -p mko-cli --test doctor
```

Result: failed as expected before implementation. `mko_core::doctor` was unresolved in `mko-core/tests/doctor.rs` (`E0432`, `E0433`), so the CLI command could not yet be reached.

### GREEN

Command run from `rust/`:

```text
cargo test -p mko-core --test doctor && cargo test -p mko-cli --test doctor
```

Result: passed. Core: 7 passed, 0 failed. CLI: 1 passed, 0 failed.

### Full verification

Command run from `rust/`:

```text
../scripts/fmt.sh && cargo test -p mko-core --test doctor && cargo test -p mko-cli --test doctor && ../scripts/fmt.sh --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Result: passed (formatting check, clippy with warnings denied, and full workspace test suite). The suite includes the existing setup, v0.1 compatibility, legacy JSON, adapter-policy, and acceptance tests.

## Fixed primary-action priority

| Priority | Diagnostic code(s) | Next action |
| --- | --- | --- |
| 1 | `profile_missing`, `profile_unreadable` | `configure` |
| 2 | `repository_incompatible` | `configure` |
| 3 | `provider_missing` | `configure` |
| 4 | `provider_unreadable`, `provider_unwritable` | `repair` |
| 5 | `provider_hydration_failed` | `hydrate` |
| 6 | `hook_conflict`, `hook_missing`, `hook_unreadable` | `repair` |
| 7 | `stale_lock`, `lock_unreadable` | `repair` |
| 8 | `lock_active` | `retry` |

All checks run independently. The priority table selects only `next_action`; it does not suppress lower-priority check records.

## Read-only evidence

- `doctor.rs` uses filesystem metadata, directory reads, profile/config reads, hook inspection, and lock inspection only.
- `lock::inspect_locks` reads lock records without takeover or stale-lock removal.
- The Core test snapshots the complete non-Git repository tree before and after diagnosis and asserts exact equality.
- `setup::apply_setup` now calls the same final repository/provider/hook/lock check set after its existing writes, preventing setup and doctor divergence.

## Self-review

- Core table tests cover versions, absent profile, incompatible repository, missing/unreadable/unwritable provider, hydration, missing/custom/managed hooks, stale lock, healthy state, priority, and no mutation.
- CLI test separates Korean-first human output from JSON-v1 stable codes and typed next-action output.
- Existing v0.1 command behavior is protected by the full legacy CLI and acceptance suites.

## Concerns

- Hydration is conservatively diagnosed for a zero-byte PDF in the Personal Inbox because the provider abstraction has no platform-specific cloud-hydration API. A future provider integration can replace that read-only predicate without changing the diagnostic code or recovery kind.

## Independent review fixes

This section supersedes the original zero-byte hydration concern above.

### Finding dispositions

1. **Repository/provider context — fixed.** Doctor now shares the explicit → ancestor → profile selector with normal context resolution. Explicit and ancestor repositories derive the provider only from that repository's `root_env`; a mismatched profile is diagnosed independently and cannot supply the provider. The exact `My-Knowledge-OS-Assets/personal/inbox` suffix is enforced, and an account root is blocked as `provider_root_invalid`.
2. **Independent provider checks — fixed.** Inbox identity, effective read access, effective write access, and bounded entry/hydration inspection always run independently and accumulate results. One central priority table chooses the primary issue without suppressing simultaneous failures.
3. **Effective access — fixed without write probes.** Unix uses the safe `nix::unistd::faccessat` wrapper with `AT_EACCESS`, so the kernel evaluates effective identity, supplementary groups, and ACLs. Windows uses the existing `mko-windows-acl` unsafe boundary and non-mutating `CreateFileW` access opens for list/add/read rights, letting Windows evaluate the current process token and ACL. Indeterminate inspection has distinct blocked codes and is never healthy. `mko-core` remains unsafe-free. The only new runtime dependency is pinned `nix 0.30.1` with `fs`, already present in the workspace lock graph.
4. **Hydration — fixed.** The zero-byte rule is removed. `DoctorEnvironment` now injects deterministic access and entry inspection. Production inspection is recursive and bounded to 4,096 entries/depth 32, never follows links, and surfaces entry/traversal errors. macOS reads `SF_DATALESS`; Windows reads offline/recall attributes. Hydrated, placeholder, corrupt/non-regular, unreadable, and unknown states remain distinct.
5. **Takeover locks — fixed.** Read-only lock inspection covers both `.lock` and `.lock.takeover`, including active, stale, and unreadable crashed-takeover records.
6. **Setup/doctor parity — fixed.** One health predicate, priority table, and primary-issue selection now serve both doctor reports and setup final checks. Warnings are unhealthy under the existing doctor semantics, so setup consistently rejects active and stale locks.
7. **JSON-v1 goldens — fixed.** Healthy and blocked CLI output are path-normalized and compared as exact JSON values against full ten-check ordered goldens. Each live output validates against `machine-output-v1.schema.json`; both goldens also typed-round-trip in the Core contract suite. `jsonschema` is test-only and already a workspace dependency.

Minor findings are covered by a whole-root snapshot containing repository, profile, and provider; healthy `profile_valid` schema reporting; unreadable-profile first-priority assertions; and simultaneous provider failure assertions.

### Review-fix TDD evidence

**RED — diagnostic abstraction/context:**

```text
cargo test -p mko-core --test doctor
```

Result: failed before production changes with unresolved `ProviderAccessInspection`, `ProviderEntryInspection`, and `ProviderEntryState`; the three required `DoctorEnvironment` methods and `DoctorReport::primary_issue` were absent.

**RED — exact machine goldens/schema:**

```text
cargo test -p mko-cli --test doctor
```

Result: failed before golden/dependency changes because `doctor-blocked.json` did not exist and `jsonschema` was unavailable to the CLI test.

**RED — setup parity regression check using the prior blocked-only predicate:**

```text
cargo test -p mko-core --test setup setup_final_health_rejects_ -- --nocapture
```

Result: failed 0 passed, 2 failed; active and stale lock warnings produced no setup failure. Restoring the centralized all-healthy predicate made the same command pass 2 passed, 0 failed.

**GREEN — focused Core/CLI and parity:**

```text
cargo test -p mko-core --test doctor --test setup --test profile_and_context --test json_v1_contract
cargo test -p mko-cli --test doctor
```

Result: doctor 12/12, setup 34/34, profile/context 11/11, JSON-v1 contract 8/8, and CLI doctor 2/2 passed.

### Platform limits

- Native verification ran on macOS. Windows-only access and recall-attribute code is isolated in `mko-windows-acl` and has Windows-gated tests, but the Windows Rust target is not installed on this machine; Windows execution remains a platform-CI responsibility.
- macOS and Windows expose supported placeholder metadata. Other Unix platforms report each PDF's hydration state as explicit `provider_inspection_failed` unknown rather than claiming health. Access inspection remains effective-ID/ACL-aware on those Unix platforms.
- Production inspection intentionally does not open PDF contents, because opening a cloud placeholder can trigger hydration and violate strict read-only diagnosis. Structural PDF corruption supplied by a platform adapter/test fake is distinct; production metadata-only inspection does not claim content validation.

### Full verification

Run from `rust/`:

```text
../scripts/fmt.sh --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -q
```

Result: all commands exited 0. Full workspace tests passed, including v0.1 acceptance/legacy CLI contracts, Task 6 facade tests, setup, JSON schema, Core doctor, and CLI doctor. No push was performed.

## Second independent review fixes

This section supersedes the first review section where platform behavior or focused test counts differ.

### Finding dispositions

1. **Windows placeholders are classified before effective-access opens.** The metadata classifier covers `FILE_ATTRIBUTE_OFFLINE` (`0x00001000`), `FILE_ATTRIBUTE_RECALL_ON_OPEN` (`0x00040000`), and `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` (`0x00400000`). Each placeholder flag returns `NotHydrated` without invoking the injected access closure, so the `ReadFile` access path cannot trigger recall. A non-placeholder denied-access seam test confirms that access is still checked exactly once and reported unreadable. The production Windows access implementation and its unsafe boundary remain isolated in `mko-windows-acl`; no unsafe code was added to `mko-core`.
2. **Doctor now uses a capability-relative metadata walker.** The walker lives with `provider_scan` policy, reuses the same hidden/temporary exclusions and scan limits, opens the root and descendants with no-follow semantics, and performs capability-relative metadata queries without opening PDF data. Invalid or symlink roots are rejected before access or traversal. A deterministic directory-swap test verifies that replacing a discovered directory with an outside symlink cannot redirect traversal. Doctor tests cover root symlinks plus hidden, temporary, partial, and visible PDFs.
3. **Diagnostic ownership is typed.** Every `DoctorCheck` carries a `DiagnosticArea`; constructors require an explicit area instead of inferring ownership from stable-code strings. Setup maps `Provider` to `Inbox`, `Hook` to `Hook`, and `Lock` to the non-mutating `Runtime` category. Active and stale lock parity tests assert both the diagnostic code and `SetupStep::Runtime`.
4. **CLI golden normalization is structural.** Tests parse JSON first and normalize only `data.checks[*].path`. A Windows-style path containing backslashes and quotes verifies that an identical literal in `message` is left unchanged. The blocked golden no longer claims read/write/hydration health after an invalid provider root, because those inspections are intentionally skipped.
5. **Minor review items are closed.** The no-mutation snapshot now includes selected stable `.git` files (`HEAD`, `config`, `index`, and `packed-refs`). Platforms without macOS/Windows placeholder metadata emit healthy `provider_hydration_unsupported` after successful access inspection instead of a false blocking failure.

### Second-review TDD evidence

**RED — new review seams:**

```text
cargo test -p mko-core --lib
```

Result: exited 101 as expected. The compiler reported the intentionally absent `inspect_windows_pdf_attributes`, `inspect_provider_metadata_with_observer`, `DiagnosticArea`, `setup_step_for_diagnostic_area`, and `SetupStep::Runtime` surfaces.

**GREEN — focused review group:**

```text
cargo test -p mko-core --lib
cargo test -p mko-core --test doctor --test setup
cargo test -p mko-core --test add provider_scan
cargo test -p mko-core --test profile_and_context
cargo test -p mko-core --test json_v1_contract
cargo test -p mko-cli --test doctor
```

Result: Core library 36/36, Core doctor 14/14, setup 34/34, provider-scan filters 3/3, profile/context 11/11, JSON-v1 contract 8/8, and CLI doctor 3/3 passed.

### Final verification

```text
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace -q
```

Result: formatting, warning-denied clippy, and the full workspace suite exited successfully. Native execution was on macOS; the Windows Rust target is not installed, so the pure Windows attribute/access seams ran here while native Windows filesystem/ACL execution remains a platform-CI responsibility. No push was performed.
