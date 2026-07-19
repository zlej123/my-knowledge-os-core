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
- The first independent post-commit code/spec review was not approved and found two Important race conditions.
- Follow-up RED tests proved that the former implementation returned from the retained lock directory to an ambient path and that name-only cleanup deleted same-bytes/same-owner replacement files.
- `AssetLock` now retains the final lock-directory capability for create, read, takeover, ownership assertion, and removal. A directory rename followed by an external symlink replacement cannot redirect those operations; a copied owner record outside remains untouched.
- Capability publication temp and lock cleanup now bind cryptographically random owner names to stable file identity. Cleanup atomically renames the public name to a private quarantine before verification. A foreign replacement is restored with create-new hard-link semantics when possible, or deliberately preserved as an orphan when safe restoration is impossible.
- The same owner-bound cleanup primitive now also covers the existing ambient-entry `write_new` temporary and publication-lock surfaces; they retain a parent directory capability instead of using name-only cleanup.
- Follow-up focused verification: atomic publication 5 passed, retained lock-directory race 1 passed, state/lock 19 passed, review transaction 17 passed, legacy approval 31 passed.
- Second independent post-commit code/spec review: pending after the follow-up commit.
- The second independent review was not approved. It identified an exclusivity
  gap while a canonical lock name was quarantined, remaining identity-blind
  write/stale-clear cleanup paths, missing directory durability evidence, and
  implausible Windows-only identity calls.
- Cleanup quarantine entries are now authoritative lock claims. Main Asset,
  takeover, capability publication, and ambient publication acquisition scan
  before create and again after a durable create; a post-create conflict
  releases only the new identity-bound claim and fails closed. Synchronized
  tests prove a third acquirer is rejected and an orphan quarantine remains
  authoritative when create-new restoration is deliberately made to fail.
- Main-lock and takeover write failures now quarantine and compare the stable
  identity captured from the created handle. Stale/takeover clear first moves
  the candidate to a private quarantine and validates that retained entry;
  replacement races cannot delete a new canonical claimant.
- Every successful quarantine rename, canonical restoration, and quarantine
  removal is followed by a directory sync on Unix. Observer seams emit only
  after the corresponding sync succeeds; tests cover both the complete
  quarantine/restore/remove sequence and the restore-failure sequence.
- Stable identity on Unix and Windows now comes from metadata on a cloned
  standard file handle. Windows volume serial values are widened from `u32` to
  `u64`; file indices remain `u64`. Native Windows CI is still the release gate
  because this development machine has no Windows Rust target installed.
- Second-follow-up focused verification: lock unit tests 10 passed, atomic unit
  tests 5 passed, atomic publication 5 passed, state/lock 19 passed, review
  transaction 17 passed, and legacy approval 31 passed. Full formatting,
  workspace Clippy with warnings denied, and workspace tests all passed.
- Third independent post-commit code/spec review: pending after the second
  follow-up commit.

## Commit

- Commit message: `feat: add snapshot-bound human review`
- Final hash is reported after commit rather than embedded here to avoid a self-referential commit hash.
- Follow-up commit hash is likewise reported after commit.
