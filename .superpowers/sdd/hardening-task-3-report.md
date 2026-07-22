# Hardening Task 3 Report: Query, scan, and repository validation contracts

## Outcome

- `knowledge show` and `knowledge list` now project every note, including reviewed notes whose
  `concepts` array is empty. `show` returns the durable ID and content revision for both review
  states and fails closed if more than one note refers to the requested Asset.
- Concept search now includes the serialized concept kind in its case-insensitive term haystack.
- Knowledge discovery retains the verified Knowledge directory capability, opens each entry with
  platform no-follow flags, and shares one entry/total-byte/elapsed deadline across enumeration and
  reads. Deterministic regressions cover an entry swapped to an external symlink and a retained file
  that grows after opened-handle metadata validation.
- `mko check` now validates the canonical Knowledge ID, deterministic path, schema and generation
  constants, Asset fingerprint/title link, normalized durable concept fields, canonical concept
  IDs, and one-note-per-Asset uniqueness.
- Interactive `knowledge review --format json-v1` keeps the TTY gate, reads the already retained
  scan snapshot for display, writes the note and prompt to stderr, and reserves stdout for exactly
  one JSON-v1 object.

## RED evidence

The first focused Core run failed at compilation with E0432 because the tests referenced the absent
`KnowledgeScanObserver`, `list_knowledge`, and `search_knowledge_with_scan` surfaces:

```text
cargo test -p mko-core --test knowledge
error[E0432]: unresolved imports ... KnowledgeScanObserver ... list_knowledge ...
search_knowledge_with_scan
```

After the scan/query surface was implemented, the same run reached behavioral RED with five
expected failures: canonical Knowledge ID, canonical path, generation/fingerprint, durable concept
constraints, and duplicate Asset-note validation were not yet reported by `mko check`.

The new interactive CLI regression initially observed mixed review text on JSON stdout. The final
macOS PTY harness separates child stderr from raw protocol stdout and enforces the existing
single-newline `one_json` invariant.

## Fresh GREEN evidence

```text
scripts/fmt.sh --check
passed

cargo test -p mko-core --test knowledge
40 passed; 0 failed

cargo test -p mko-cli --test knowledge_cli
9 passed; 0 failed

cargo clippy -p mko-core --test knowledge -- -D warnings
passed

cargo clippy -p mko-cli --test knowledge_cli -- -D warnings
passed
```

## Scope and remaining gates

- Only the Task 3 Core/CLI sources, their focused tests, and this report are included. The existing
  untracked `docs/manual-smoke-v0.2-record.md` was not touched.
- Full workspace tests and workspace-wide Clippy remain with the coordinating final-verification
  task.
- The symlink-entry race runs on Unix; the JSON interactive protocol test uses macOS `script`.
  Native Windows no-follow/reparse behavior and an interactive Windows console remain release
  gates.
