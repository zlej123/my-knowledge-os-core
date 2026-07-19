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
- Stable identity on Unix comes from metadata on a cloned standard file handle.
  Windows identity is now confined to the safe private `mko-windows-acl` API,
  which uses stable `GetFileInformationByHandle` data and exposes only an opaque,
  equality-comparable identity to Core. Native Windows CI is still the release
  gate because this development machine has no Windows Rust target installed.
- Second-follow-up focused verification: lock unit tests 10 passed, atomic unit
  tests 5 passed, atomic publication 5 passed, state/lock 19 passed, review
  transaction 17 passed, and legacy approval 31 passed. Full formatting,
  workspace Clippy with warnings denied, and workspace tests all passed.
- Third independent post-commit code/spec review: pending after the second
  follow-up commit.
- The third independent review was not approved. It found that prefix-based,
  unbounded quarantine discovery could be forged or could permanently brick a
  record after a cleanup crash; quarantine records were not visible to Asset
  lock inspection, and publication cleanup had no safe stale recovery.
- Authoritative quarantine names now require an exact 32-character lowercase
  hexadecimal token. Asset cleanup names authenticate that token against the
  secure suffix in the bounded, parseable owner record. Publication records are
  structured JSON containing PID, hostname, start time, and a secure owner token;
  the exact filename token must match that owner token.
- Both Asset and publication directory scans have a 64-entry and 100-ms hard
  work bound. Near-prefix noise is ignored; work-limit overflow fails closed
  with stable `lock_scan_limit` or `registry_scan_limit` errors.
- Asset inspection reports exact quarantine entries as active, stale, or
  unreadable. Active entries block even an explicit clear. A stale or malformed
  Asset quarantine is recoverable only through explicit `--clear-stale-lock`:
  it is first renamed to a non-authoritative private reap name, directory-synced,
  then identity/owner revalidated before deletion. A replacement injected after
  the private rename is preserved.
- Valid active publication quarantines block and are retried within the existing
  bounded acquisition wait. Valid stale publication quarantines are recovered on
  the next acquisition. Malformed or token-mismatched entries are moved through
  the same identity-bound private reap path, removed, and return a stable error
  instructing the caller to retry; they cannot become invisible permanent locks.
- Windows parent-directory crash durability is explicitly not claimed: files are
  flushed before atomic rename, but the safe Windows layer has no supported
  POSIX-equivalent parent-directory fsync. Unix continues to sync every durable
  quarantine, restore, and removal transition.
- Third-follow-up focused verification covered 17 Asset-lock unit tests, 11
  publication unit tests, the safe Windows API policy surface, concurrent
  capture, and all prior Task 11 integration suites. Full verification results
  were green: repository formatting, workspace Clippy with warnings denied, and
  the complete workspace test suite all passed.
- Fourth independent post-commit code/spec review: pending after the third
  follow-up commit.
- The fourth independent review was not approved. It found that Asset and
  publication quarantine inspection could block on special files, allocate or
  parse work beyond the intended record bound, and race from metadata to a
  substituted FIFO. It also found one remaining Windows provider-scan use of
  nightly-only metadata identity methods, a quarantine-token classification
  gap, and publication recovery phases that created fresh rather than shared
  deadlines.
- Asset inspection now retains each directory component as a no-follow
  capability and performs exact-name reads only after both pre-open metadata
  and post-open handle metadata prove a regular non-link file. Unix opens are
  nonblocking, reads are capped at 4,097 bytes for a 4,096-byte record, and the
  same deadline spans enumeration, open, read, and parsing. Filename/record
  token mismatches are reported as unreadable.
- Publication quarantine scanning, cleanup, liveness checks, and reaping now
  share the acquisition-bounded deadline. They use the same pre-open,
  no-follow/nonblocking, post-open, stable-handle-identity, and bounded-read
  sequence. Entries that cannot yield a retained identity are moved out of the
  authoritative namespace but deliberately preserved as private orphans rather
  than being deleted without proof of ownership.
- Windows provider scanning now classifies placeholder attributes before any
  content open, then treats the retained no-follow file handle as authoritative
  for size and stable identity. Revalidation reopens the current capability name
  and compares the safe `GetFileInformationByHandle` identity. A recursive
  safety-policy test rejects `volume_serial_number()` and `file_index()`
  anywhere in Core sources.
- Fifth-follow-up focused verification covered 84 Core unit tests, including
  exact-name FIFO, symlink, oversized regular file, metadata-to-FIFO swap,
  retained lock-directory, token-mismatch, publication deadline, and provider
  identity-race cases. The safe Windows API policy test also passed. Native
  Windows compilation remains a release gate because this machine only has the
  Apple Rust target installed.
- Repository formatting, workspace Clippy with warnings denied, and the full
  workspace test suite all passed after the fifth follow-up.
- A final full-suite run exposed an intermittent pre-existing convergence test
  failure: both concurrent explicit stale-clear contenders returned
  `lock_held`, although exactly one must retain the live lock. Parallel replay
  showed that a loser could rename a newly active takeover claim into quarantine
  merely to determine whether it was stale; the winner then observed that
  authoritative quarantine during its post-create validation and also failed.
- Explicit stale recovery is now serialized under the takeover guard. In
  addition, an entry is read and classified as stale before its public name is
  moved; active takeover owners are never transiently quarantined by a losing
  contender. The original handle identity and record are still revalidated
  after the rename before any stale entry is deleted.
- The complete parallel `state_and_lock` test binary passed 100 consecutive
  runs after the convergence fix. The complete workspace test suite then passed
  two consecutive runs; formatting and warnings-denied Clippy remained green.
- Fifth independent post-commit code/spec review: pending after this follow-up
  commit.

## Commit

- Commit message: `feat: add snapshot-bound human review`
- Final hash is reported after commit rather than embedded here to avoid a self-referential commit hash.
- Follow-up commit hash is likewise reported after commit.
