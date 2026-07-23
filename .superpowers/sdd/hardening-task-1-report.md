# Hardening Task 1 Report: Publication-lock disappearance race

## Scope

Implemented the publication-lock recovery requirement from
`.superpowers/sdd/hardening-task-1-brief.md`. No Knowledge files or manual-smoke records were
changed. The existing `rust/mko-core/tests/capture.rs` concurrency regression was exercised rather
than modified because the deterministic race belongs at the lower-level publication-lock boundary.

## Root cause

`scan_publication_quarantines` records a cleanup quarantine by name and identity. Before recovery
can atomically rename that public quarantine to its private reap name, the cleanup owner can remove
it. `reap_publication_quarantine` previously passed that rename error through `lock_error`, which
turned `ErrorKind::NotFound` into the unrelated terminal code `registry_write_failed`.

An entry that still exists is different: malformed metadata, an identity replacement, or changed
owner contents must continue to fail closed.

## RED evidence

The regression test
`atomic::tests::publication_lock_retries_when_discovered_cleanup_quarantine_vanishes` uses an
observer immediately after quarantine discovery. It removes the discovered quarantine exactly once
and drives the real `CapabilityPublicationLock` acquisition loop.

The first test invocation failed to compile because the observer entry point did not exist. I then
added only the observer plumbing, without changing error handling, and reran:

```text
cargo test -p mko-core atomic::tests::publication_lock_retries_when_discovered_cleanup_quarantine_vanishes -- --exact --nocapture

a vanished cleanup quarantine must be retried: MkoError {
    code: "registry_write_failed",
    message: "No such file or directory (os error 2)"
}
test result: FAILED. 0 passed; 1 failed
```

This reproduced the reported ENOENT boundary deterministically.

## Implementation

- Added a quarantine-discovery observer to the capability lock's internal acquisition path so the
  race can be reproduced without timing or probabilistic scheduling.
- Added the corresponding resolver observer while retaining the existing no-op production wrapper.
- Changed only the reap rename's `ErrorKind::NotFound` handling to return the existing
  `registry_locked` retry signal. Both ambient and capability acquisition loops already retry that
  signal within their bounded deadline.
- Left every other reap error and every post-rename identity/content validation unchanged. Existing
  malformed, special-file, active-owner, and replacement-sensitive tests therefore retain their
  fail-closed behavior.

## Verification

All commands were run in the v0.2 implementation worktree.

1. Focused deterministic GREEN:

   ```text
   cargo test -p mko-core atomic::tests::publication_lock_retries_when_discovered_cleanup_quarantine_vanishes -- --exact --nocapture
   test result: ok. 1 passed; 0 failed
   ```

2. Atomic unit surface:

   ```text
   cargo test -p mko-core --lib atomic::tests:: -- --nocapture
   test result: ok. 16 passed; 0 failed
   ```

   This includes the existing malformed quarantine, FIFO, symlink, active quarantine, stable
   recovery, scan-bound, and replacement-cleanup cases.

3. Capture integration surface:

   ```text
   cargo test -p mko-core --test capture -- --nocapture
   test result: ok. 9 passed; 0 failed
   ```

4. Bounded capture-concurrency stress:

   ```text
   for iteration in {1..50}; do cargo test -p mko-core --test capture concurrent_capture_creates_one_intact_registry_record -- --exact --quiet || exit 1; done
   exit 0; 50/50 iterations passed (400 concurrent capture operations total)
   ```

5. Formatting and lint:

   ```text
   scripts/fmt.sh --check
   exit 0

   cargo clippy -p mko-core --all-targets -- -D warnings
   exit 0
   ```

## Files changed

- `rust/mko-core/src/atomic.rs`
- `.superpowers/sdd/hardening-task-1-report.md`

## Remaining verification

No Task 1 concern remains from targeted verification. Per controller direction, full workspace test
and Clippy gates are deferred to the coordinating task.
