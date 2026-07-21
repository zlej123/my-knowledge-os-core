# Hardening Task 2 Report: Bundle-bound Knowledge writes and mutation CAS

## Outcome

- `mko knowledge write` now requires `--bundle` and passes the canonical prepared-bundle path into Core.
- Core loads the canonical runtime bundle, enforces its untrusted-document contract, and binds its Asset ID, Source ID, fingerprint, title, and logical locator to the requested Asset Registry record.
- Knowledge replace and approve retain the Knowledge directory capability, stage atomic replacements, and compare the original bytes plus a recomputed content revision immediately before commit.
- First creation publishes through the retained directory with create-new hard-link semantics; no-follow opens bind reads to regular entries.
- Concurrent identical first creation converges to one `created` result and one `existing` result even when caller clocks straddle Seoul midnight, because the deterministic filename uses the Asset creation date.
- Regeneration resets review status and `reviewed_at`, preserves the prior `approved_revision` as comparison history, and remains valid under `mko check`.

## RED evidence

Command:

```text
cargo test -p mko-core --test knowledge
```

Observed before production edits: compilation failed with E0432 because the regression suite referenced the absent `KnowledgeMutationObserver`, `write_knowledge_note_with_clock_and_observer`, and `approve_knowledge_with_clock_and_observer` CAS surfaces.

The added regressions cover missing/non-canonical/wrong/untrusted bundles, body tampering before approval, deterministic concurrent replace versus approve, concurrent first creation, correct `existing` reporting, and approve-replace-check comparison history.

## GREEN evidence

```text
cargo test -p mko-core --test knowledge
25 passed; 0 failed

cargo test -p mko-cli --test knowledge_cli
7 passed; 0 failed

cargo clippy -p mko-core --test knowledge -- -D warnings
passed

cargo clippy -p mko-cli --test knowledge_cli -- -D warnings
passed

scripts/fmt.sh --check
passed
```

Focused review initially identified ambient first-creation publication, followable entry opens, and midnight-split filenames as Important. All three were corrected before the final verification above.

## Remaining scope

Full workspace verification is intentionally left to the coordinating task. Task 3 still owns scan deadlines, all-note show/list projections, kind-inclusive search, and expanded repository-wide Knowledge validation.
