# Task 11 Report — Snapshot-Bound Human Review

## Scope

- Added the interactive, human-only `mko review` transaction.
- Bound the displayed Source, Asset, working diff, staged diff, ID, path, and revision to final validation immediately before Source publication.
- Preserved the frozen legacy `human approve-source` confirmation path while delegating its final write to the same non-locking locked-publication primitive.
- Strengthened capability-relative atomic replacement and lock cleanup.

## RED evidence

The required tests were added before production implementation and run with the specified focused commands.

- `review_transaction`: failed to compile because `review`, `GitSnapshot`, and `GitSnapshotProvider` did not exist.
- `atomic_publication`: failed to compile because `write_replace_capability_validated_at_commit` did not exist.
- CLI `review`: failed because `review` was not a recognized subcommand.

## GREEN evidence

Focused verification after implementation:

- Core atomic publication: 3 passed.
- Core review transaction: 17 passed.
- Legacy Core check/approval: 31 passed.
- Contract-version compatibility: 3 passed.
- CLI review: 1 passed.
- Adapter policy: 21 passed.

Coverage includes duplicate valid pending titles, numeric selection with ID/path/revision disambiguation, Source/Asset/working/staged changes after display and immediately before publication, initial Git incoherence, exact-token mismatch, TTY rejection before repository access, DEFER zero mutation, exact approval, one Asset lock in both new and legacy paths, retained no-follow Source/Asset access, lock-directory symlink escape rejection, terminal control/bidi escaping, Git argv/pathspec safety, aggregate overflow, strict UTF-8, and unmerged state rejection.

Full verification run from the repository-defined locations:

- `scripts/fmt.sh --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: passed.

## Security and durability notes

- Fixed lock order: caller-owned Asset lock, then Source publication lock, then (after durable Source verification) Asset publication lock. Hooks execute while the Source publication lock is held and must not acquire either lock.
- The locked primitive proves the supplied Asset lock owns the same canonical repository/Asset record and owner token.
- Source and Asset are reread through retained, no-follow capabilities. The Asset processed transition is an expected-byte capability CAS after durable Source publication; failure leaves the existing repairable Source-approved/Asset-pending mismatch and never overwrites an external Asset edit.
- Git is invoked directly without a shell with fixed flags, pathspecs after `--`, a five-second kill/wait timeout, aggregate stdout/stderr bounds, strict UTF-8, and unmerged-state rejection. Initial snapshots are double-collected for coherence and final snapshots are double-collected again.
- Human display escapes terminal controls and bidi formatting characters while approval remains bound to raw bytes.
- Publication lock cleanup removes only its own owner token.

## Residual gates

- Native Windows CI remains required for the complete v0.2 release gate.
- A live human TTY review/defer/approve smoke against a non-sensitive hydrated Google Drive PDF remains a Task 12/manual release gate.
- The timeout kill path is implemented and bounded; deterministic unit coverage for a deliberately hanging replacement Git executable is deferred because the production runner intentionally has no executable-injection surface.

## Review cycles

- Pre-implementation design review findings were incorporated before final verification.
- Independent post-commit code/spec review: pending.

## Commit

- Commit message: `feat: add snapshot-bound human review`
- Final hash is reported after commit rather than embedded here to avoid a self-referential commit hash.
