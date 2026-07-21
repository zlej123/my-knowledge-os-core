# Knowledge Extraction Hardening Design

## Goal

Bring the knowledge-extraction implementation back into alignment with its approved contract and
make review, discovery, and concurrent publication safe enough for release.

## Decisions

1. `mko knowledge write` requires the canonical prepared bundle. The Core verifies that the bundle
   belongs to the requested Asset and has `trust: untrusted_document_text`; the Skill cannot replace
   this invariant with instructions alone.
2. Knowledge mutation is compare-and-swap. Approval recomputes the current content revision and
   validates it again while holding the publication capability/lock. Regeneration preserves the
   previous `approved_revision` as comparison history while resetting the review state.
3. Knowledge directory access uses retained directory capabilities, no-follow file opens, entry and
   byte bounds, and an elapsed deadline. Concurrent first creation and identical writes converge.
4. `show` and `list` operate on every knowledge note, regardless of review state or concept count.
   Search includes the concept kind. `mko check` validates canonical IDs, paths, generation metadata,
   asset fingerprint linkage, durable concept constraints, and one-note-per-Asset uniqueness.
5. Ordinary PDF summarization stops at Source review. Knowledge extraction runs only when the user
   explicitly requests it. Forward tests and the live smoke procedure cover the knowledge path.
6. The pre-existing publication-quarantine disappearance race treats an entry that vanishes during
   owner cleanup as a retry, while malformed or replaced entries remain fail-closed.

## Compatibility

- Asset and Source schemas remain frozen.
- Existing valid knowledge notes remain readable.
- `approved_revision` may be present on an `unreviewed` regenerated note; `reviewed_at` remains null
  until the new revision is approved.
- JSON-v1 remains path-free and emits exactly one JSON object.

## Verification

- Regression tests first for every finding.
- Targeted Core/CLI/Skill tests after each task.
- Final `scripts/fmt.sh --check`, Clippy with warnings denied, full workspace tests, Skill validator,
  and a sanitized user-assisted smoke test before reinstalling the CLI and Skill.
