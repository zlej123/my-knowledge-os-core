# Phase 2 — Unified Ingestion, Implementation Plan

Date: 2026-08-29
Design: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md` §6, §6.1, §6.3, §7
(phases table row 2), decisions D2, D3, D11, D13
Strategy: give the Core three new input forms that need no extractor — pasted text, a captured
conversation (both TEXT-fingerprint identity, §6.1), and a local Markdown/text file (ORIGINAL-BYTES-
fingerprint identity, its own minimal content-addressed store) — add the `topics` field the recall
filters and `mko topics` listing need, and close the loop D11 opened: store-on-miss, now that
conversation content is a first-class input form.

## Task 1 — New `AssetOriginV2` variants and their identity split (§6.1)

- Added `PastedText`, `LocalFile`, `Conversation` to `AssetOriginV2` (`records_v2.rs`). All three are
  non-default, so `#[serde(skip_serializing_if = "AssetOriginV2::is_provider_pdf")]` still elides only
  `ProviderPdf` — no change to how existing `ProviderPdf`/`WebSnapshot` Assets round-trip.
- `PastedText` and `Conversation` are TEXT-fingerprint identity, modeled structurally on
  `register_web_snapshot_v2`: `snapshot_v2.rs` was generalized into a shared
  `register_text_evidence_v2` helper (origin, `provider_type`, locator, title-fallback policy), with
  `register_web_snapshot_v2` becoming a thin wrapper over it alongside the two new
  `register_pasted_text_v2`/`register_conversation_v2` functions. All three share the same
  content-addressed text store (`assets/snapshots/<hash>.txt`) — it was already origin-agnostic.
- **[DECIDED] Empty-string locator convention.** Neither a paste nor a captured conversation has an
  address to return to. Rather than widen `logical_locator` to `Option` — which would break every
  serialized Asset record on disk — both use the empty string, validated exactly (`is_empty()`) in
  `validate_asset_record_v2`'s new origin arms (`asset_v2.rs`).
- `LocalFile` is ORIGINAL-BYTES-fingerprint identity: new module `rust/mko-core/src/local_file_v2.rs`.
  Registration deliberately does **not** route through `inspect_provider_file`/
  `validated_disjoint_roots` (Inbox-specific); it reads the absolute local path directly via the
  existing `read_bounded_nofollow` (`asset_v2.rs`), bounded to the snapshot-scale cap (`MAX_LOCAL_FILE_BYTES
  = 2 MiB`, matching `MAX_SNAPSHOT_BYTES`), and requires valid UTF-8 text.
- **[DECIDED] Text-originals store, bounded to text; binaries deferred.** Since md/txt originals are
  themselves small text files, the original bytes are stored verbatim, content-addressed, at
  `assets/originals/<hash>.<ext>` (`.md`/`.markdown`/`.txt`, bounded to a small safe allowlist —
  cosmetic only, since media type is always `text/plain` here). `assets/originals` was added to
  `scaffold_v2.rs`'s `OWNED_DIRECTORIES`, created lazily on first write exactly like
  `assets/snapshots` was for KBs scaffolded before it existed. Phase 3 extends this same store to
  binaries after a separate check-budget decision (§8's named risk); Phase 2 does not touch
  `mko check`'s byte budget because the bound here already matches the existing text-store bound.
- **Documented consequence of the literal identity rule.** For a plain-text local file, the original
  bytes *are* the text, so a paste and a local file with byte-identical content converge to the same
  Asset (first registration's origin/provider binding wins — the existing
  `write_asset_registry_record_v2` collision-tolerance already did this for `id`/`fingerprint`/
  `media_type` matches without checking `provider_type`; Phase 2 origins simply make the collision
  reachable across forms that share `media_type: text/plain`). Divergent bytes (e.g. a trailing
  newline the file has and the paste does not) are different Assets, as expected. Verified by
  `local_file_and_pasted_text_converge_when_bytes_match_and_diverge_when_they_do_not`
  (`rust/mko-core/tests/ingestion_v2.rs`).

Gate: `rust/mko-core/tests/ingestion_v2.rs` (new) — registration identity for all three origins,
originals-store round trip, prepare idempotency.

## Task 2 — Wiring the new origins through validation and prepare (§6.2)

- `asset_v2.rs`'s `validate_asset_record_v2` origin match gained the three new arms (media type
  `text/plain`, `provider_type` `pasted-text`/`local-file`/`conversation`, and each origin's locator
  rule). `prepared_v2.rs`'s `validate_asset` media-type match gained the same three arms.
- `prepared_v2.rs`: broadened `prepare_snapshot_asset_v2`'s origin guard from `WebSnapshot` only to
  `WebSnapshot | PastedText | Conversation` (all three read from the same text store via
  `read_snapshot_text_v2`); added `prepare_local_file_asset_v2`/`_with_clock`, reading via the new
  `local_file_v2::read_original_text_v2`. Both prepare paths (and the existing snapshot one) now share
  a new `persist_prepared_session_v2` helper — the session-write/read-back/validate tail was
  byte-identical duplicated logic across three functions before this change.
- **Highest-risk spot, per the design brief.** `cli_v2.rs`'s `prepare_source_json_v2` used to route on
  a binary `if origin == WebSnapshot {..} else {assume PDF}`, which would have silently sent every new
  origin into the PDF/Inbox path. Converted to an exhaustive `match` over `AssetOriginV2` (compiler-
  enforced: a fifth origin added later without a new arm fails to build).
- `output.rs`'s `json_v2_next_action` gained arms for the new error codes
  (`local_file_path_invalid`/`local_file_not_text`/`local_file_empty`/`local_file_invalid`/
  `local_file_unreadable` → `Add`; `local_file_damaged` → `Add`; `local_file_write_failed`/
  `local_file_destination_invalid` → `Retry`), mirroring the existing snapshot codes' shape.

Gate: `cargo build --workspace` (the exhaustive match is compiler-enforced); CLI tests in Task 3.

## Task 3 — CLI registration: `mko add --paste`/`--local-file`/`--conversation` (§6)

- Extended `AddArgs` (`cli.rs`) with three new mutually-exclusive-with-each-other-and-`--inbox` flags.
  `--paste`/`--conversation` take a **file path** holding the text (never the text as an argument —
  the SKILL's existing file-not-argument discipline, matching `--snapshot`); `--local-file` takes the
  **material's own absolute path** — Core reads it directly, so no separate runtime text file is
  written first, unlike the other three text-form flags.
- `--title`/`--fetched-at` were loosened from `requires = "snapshot"` to unconstrained, since they now
  apply across all four text-flavored forms (title optional everywhere except `--snapshot`, which
  still requires it explicitly at runtime; `--fetched-at` defaults to now for all four).
- `add_v2` routes to three new handler functions (`add_paste_v2`/`add_local_file_v2`/
  `add_conversation_v2`), which — along with the existing `add_snapshot_v2` — now share one
  `emit_text_evidence_add_result_v2` result-formatting function: what a caller does next never
  depended on which text-evidence origin registered the Asset.

Gate: `rust/mko-cli/tests/add_v2_cli.rs` — one test per new flag (paste idempotency, local-file
originals-store content check, conversation registration), following the existing `--snapshot` test
shape exactly.

## Task 4 — `topics` field, normalization, and projection threading (§6.3)

- Added `topics: Vec<String>` to `SourceResponseV2`/`KnowledgeResponseV2` (`model_v2.rs`), with
  `#[serde(default, skip_serializing_if = "Vec::is_empty")]` — the same pattern
  `AssetRecordV2.origin` already uses — so a revision written before this field existed still parses
  and re-serializes to its exact original bytes. Verified explicitly by
  `a_revision_written_before_topics_existed_still_round_trips_and_scans`
  (`rust/mko-core/tests/ingestion_v2.rs`): writes with empty topics, asserts the stored JSON has no
  `"topics"` key at all (not an empty array — genuinely absent, the honest stand-in for "before this
  field existed"), then re-reads through `read_current_knowledge_revision_v2`,
  `queue_v2::list_topics_v2`, `derive_queue_v2`, and `search_records_by_perspective_v2` to prove the
  whole read pipeline tolerates it.
- **[DECIDED] Normalization: case-preserving storage, case-insensitive comparison.** New
  `records_v2::normalize_topics` reuses `prepared_v2::normalize_single_line` (now `pub(crate)`; the
  same trim/collapse-whitespace/NFC pass `PreparedMetadataV2`'s title/authors already get) per topic,
  then dedups by lower-cased key while keeping first-seen casing — deterministic given deterministic
  input order. Rejects an empty-after-trim or oversized (`> 256` chars) topic with `topic_invalid`,
  and more than 256 topics on one revision with `topics_too_many`. Applied inside
  `write_source_record_v2`/`write_knowledge_record_v2` before the revision is constructed, so the
  *stored* revision always carries normalized topics even though the caller's `SourceResponseV2`/
  `KnowledgeResponseV2` is untouched.
- **[DECIDED] Topics thread into projection front matter alongside tags, in the same pass.** The tag
  seam (`ProjectionMetadataV2` → `ProjectionInputV2` → rendered YAML front matter →
  `StoredProjectionMetadataV2`) was already open; `topics` rides it: `projection_v2.rs` gained a
  `topics` field on `ProjectionInputV2` (default+skip-if-empty, so an old projection file without it
  still parses) and `StoredProjectionMetadataV2`, a `topics:` front-matter line rendered exactly like
  the existing `tags:`/`perspectives:` lines (omitted when empty), and a bounds check in
  `validate_input` matching the existing tag bound (≤256 items, each ≤256 chars). `records_v2.rs`'s
  `publish_record_and_projection` was refactored from seven positional metadata arguments to one
  `ProjectionMetadataV2` struct (already growing unwieldy; adding an eighth positional argument was
  the wrong direction) — `queue_v2.rs`'s `canonical_projection_input` (the "what Core would generate
  right now" function drift detection and the queue scan both depend on) computes topics the same way
  tags already are.
- Schema layer: `topics` added as a **required** property (empty array permitted) to
  `schemas/v2/source-response.schema.json` and `schemas/v2/knowledge-response.schema.json`, each
  bounded (`maxItems: 256`, each item `minLength: 1, maxLength: 256`) to match the Core-side bound.
  The paired fixtures (`tests/fixtures/json-v2/source-response.json`/`knowledge-response.json`) were
  given a non-empty example topic (`["example>topic"]`, not `[]`) deliberately: an empty-topics
  fixture would omit the key on Rust re-serialization (skip-if-empty) while the schema requires the
  key present, failing the fixture's own round-trip contract test. Every other Rust-literal
  `SourceResponseV2`/`KnowledgeResponseV2`/`ProjectionInputV2` construction site across `mko-core`
  and `mko-cli`'s test suites was updated with an explicit (usually empty) `topics`/`topics: Vec::new()`
  field, since these are non-`Default` struct literals.
- Two test-harness fixtures that hand-build a `ProjectionInputV2` to simulate a Confirmed record
  outside the normal write path (`rust/mko-cli/tests/find_cli.rs`'s `seeded_fixture`,
  `rust/mko-core/tests/queue_v2.rs`'s `sync_projection`) needed their manual `topics` field to actually
  match the deserialized fixture's `SourceResponseV2::topics`/`KnowledgeResponseV2::topics` — once the
  shared fixtures carried a real topic, a `topics: Vec::new()` mismatch there made the hand-written
  projection stale relative to what Core would generate, which flipped the simulated "confirmed"
  record to `Blocked` and broke three `find_cli` tests. Fixed by computing `topics` from the same
  deserialized response the rest of the projection already derives from.

Gate: `rust/mko-core/tests/ingestion_v2.rs`
(`topics_are_normalized_case_preserving_and_deduped_case_insensitively`,
`an_empty_or_oversized_topic_is_rejected_with_a_clear_error`,
`a_revision_written_before_topics_existed_still_round_trips_and_scans`) and
`rust/mko-core/tests/json_v2_contract.rs`'s existing fixture round-trip test, extended by the new
`topics` requirement.

## Task 5 — `mko topics` (D13)

- New `queue_v2::list_topics_v2`: read-only, no mutation lock, and deliberately cheap — it calls
  `scan_collection` (the primitive `derive_groups` also uses) directly for both `sources` and
  `knowledge`, skipping `derive_review_histories_v2` and projection-drift checks entirely, since a
  topic listing has no use for either. **[DECIDED] Output shape:** a flat,
  case-insensitively-deduped, deterministically sorted `Vec<String>` of full hierarchical strings —
  keyed by a `BTreeMap<lowercased, original>` so the map's own key order gives determinism without a
  separate sort pass, and first-seen casing (in the scan's own deterministic file-name order) is kept
  on a case collision.
- `json_v2.rs`: `JsonV2Command::Topics`, `TopicsDataV2 { topics: Vec<String> }`,
  `JsonV2Success::Topics`/`::topics()`, following the exact `find`/`FindDataV2` pattern.
- `cli.rs`: hidden `Topics(TopicsArgs)` command (`--repo`, `--format` default Human), dispatching to a
  new `topics_v2` function; wired into both `json_v2_command` (typed dispatch) and
  `json_v2_command_from_invalid_arguments` (usage-error dispatch before a full parse succeeds), the
  same two places every other machine command is registered.
- `schemas/v2/machine-output.schema.json`: `topics` added to the `command` enum and the top-level
  `oneOf`, with new `topics_data`/`topics_success` `$defs` mirroring `find_data`/`find_success`; new
  fixture `tests/fixtures/json-v2/topics-success.json`.

Gate: `rust/mko-core/tests/ingestion_v2.rs`'s
`mko_topics_is_flat_case_insensitively_deduped_and_deterministically_sorted`;
`rust/mko-cli/tests/find_cli.rs`'s `mko_topics_lists_the_fixtures_shared_topic_in_json_and_human_output`;
`rust/mko-core/tests/json_v2_contract.rs`'s extended `machine_envelope_goldens_validate_and_round_trip`.

## Task 6 — `--topic` filter on `mko find` (§6)

- `queue_v2::search_records_by_perspective_v2` gained a `topic: Option<&str>` parameter (last
  position; `#[allow(clippy::too_many_arguments)]` added — seven parameters was already at the
  clippy default threshold). New `normalize_topic_needle_v2` (same trim/collapse/NFC/lower-case
  pattern as `normalize_tag_needle_v2`) and `topic_matches` (exact match OR hierarchical-prefix match:
  a record's topic `투자>반도체` matches a `--topic 투자` filter via a `"{needle}>"` prefix check),
  applied in both the Source and Knowledge match arms exactly where the existing tag filter already
  is.
- `cli.rs`: `FindArgs` gained `--topic`; the legacy v0.1 `find` path's filter-rejection guard (`format
  != Human` already gated to v3 KBs) gained `arguments.topic.is_some()` alongside the existing
  `--tag`/`--layer`/etc. checks, so a v0.1 KB refuses the flag with the same
  `perspective_v3_required` error the other v3-only filters already use.
- Every existing call site of `search_records_by_perspective_v2` (production and test) needed a
  trailing `None` argument for the new parameter — eight call sites across
  `rust/mko-core/tests/queue_v2.rs` and `rust/mko-cli/src/ui.rs`.

Gate: `rust/mko-core/tests/ingestion_v2.rs`'s
`topic_filter_matches_case_insensitively_and_by_hierarchical_prefix`;
`rust/mko-cli/tests/find_cli.rs`'s `find_topic_filter_matches_exactly_and_by_hierarchical_prefix`.

## Task 7 — SKILL.md: three workflows, topic reuse, store-on-miss (D13, §6.3)

- New `## Pasted text workflow` and `## Local file workflow` sections, inserted after `## Web page
  workflow` and before `## Knowledge registration` — each modeled directly on the web-page workflow's
  shape (register → prepare → the same untrusted-content rule stated without exception), with the
  file-not-argument discipline explicit for the two forms that need it and explicitly *not* required
  for `--local-file` (which names the material itself).
- A short `## Conversation capture` stub points to Store-on-miss, since conversation capture is never
  a workflow the owner asks for directly — it exists only as store-on-miss's storage step.
- New `## Topics` section (placed right after Store-on-miss, before `## Setup and read-only
  requests`): directs the agent to consult the topics list before proposing a new one and prefer
  reuse (D13); cross-referenced from both the Source (`## Selected PDF workflow` step 3) and Knowledge
  (`## Knowledge registration`) response-authoring instructions.
- New `## Store-on-miss` section, placed immediately after `## Recall contract`: the offer text is
  exact (`저장소에 없네요 — 이번에 정리한 내용을 저장할까요?`), modeled explicitly on the
  studying-by-asking consent discipline (offer once per gap, never repeat, silence or a follow-up
  question is not yes, never store on the agent's own initiative). Point 4 of the recall contract
  ("a miss is data, not a dead end") was rewritten from Phase 1a's "that capability does not exist
  yet in this Core version" to point at this new section.
- **Adapter-policy false positive, fixed by rewording (same category Phase 1a's Task 7 hit).** The
  command-substitution scanner (`rust/mko-cli/tests/adapter_policy.rs::contains_shell_syntax`) flags
  any of `` $ ` & | ; < > `` inside what it judges to be a command-shaped inline-code span. Two
  distinct false positives surfaced and were fixed by wording, not by loosening the scanner:
  - `` `개발>Rust` `` as a topic example: the scanner's `camel_case_command` heuristic checks
    *bytes*, not characters, so a capital ASCII letter anywhere past index 0 (here, `Rust`'s `R`,
    following the multi-byte Korean prefix) with a lowercase ASCII letter present elsewhere makes it
    look command-shaped regardless of the non-ASCII prefix. Changed the example to `개발>백엔드`
    (no ASCII letters at all).
  - Three bare inline mentions — `` `mko find` ``, `` `mko topics` `` (twice), `` `mko add
    --conversation` `` — used as ordinary prose references without `--format json-v2`, tripping the
    separate "every machine command is pinned to json-v2" check (which does not distinguish a prose
    mention from an instructed invocation). Reworded each to plain prose ("the recall search
    returned...", "consulting the topics list first...", "register it" without repeating the flag)
    since the actual invocation is already given verbatim in the adjacent code block.
  - `adapter_policy.rs`'s two command allowlists
    (`knowledge_os_skill_exposes_only_the_v2_core_workflow`,
    `knowledge_os_skill_defines_the_knowledge_extraction_flow`) gained `"mko topics"` — the same
    per-phase addition Phase 1a made for `"mko find"`.

Gate: `knowledge_os_skill_exposes_only_the_v2_core_workflow`,
`knowledge_os_skill_defines_the_knowledge_extraction_flow`,
`knowledge_os_skill_pins_the_exact_cli_version_handshake`, and the rest of
`rust/mko-cli/tests/adapter_policy.rs`/`rust/mko-cli/tests/my_knowledge_os_skill.rs`.

## Task 8 — skill-forward scenarios (D13, store-on-miss)

- Added Scenario 17 (topic reuse before invention: a paste with an obvious existing-topic match must
  trigger `mko topics --format json-v2` before the worker proposes any topic, and must reuse the
  returned label rather than inventing a new spelling), Scenario 18 (store-on-miss offer: a logged
  recall miss followed by a knowledge-worth-keeping conversation gets the offer exactly once; the
  evaluator's next turn is a non-yes follow-up, and the worker must not treat that as acceptance or
  self-initiate the write), and Scenario 19 (store-on-miss accepted: continues Scenario 18 with an
  explicit yes, capturing the actual conversation verbatim through `mko add --conversation` exactly
  once, without skipping ahead to a Knowledge write) to
  `tests/skill-forward/my-knowledge-os-scenarios.md`.
- New harness fixtures `tests/skill-forward/harness/topic-reuse.json` (a `mko topics` result already
  containing `투자>반도체`, followed by the paste registration) and
  `tests/skill-forward/harness/store-on-miss.json` (an empty-result `mko find`, followed by the
  `mko add --conversation` result Scenario 19 continues into), plus matching
  `topic_reuse_before_invention`/`store_on_miss_consent`/`store_on_miss_verbatim_capture` rows in
  `tests/skill-forward/my-knowledge-os-rubric.md`.
- Not wired into `cargo test`, per `AGENTS.md`: `tests/skill-forward/` is exercised separately against
  `harness/` fixtures by the forward-test process.

## Task 9 — Version discipline

- Bumped `workspace.package.version` `0.4.1 → 0.4.2` in `rust/Cargo.toml` and the three pinned sites:
  `rust/mko-core/tests/contract_version.rs`, the `mko --version` assertion in
  `rust/mko-cli/tests/cli.rs`, and the handshake pin in `skills/codex/my-knowledge-os/SKILL.md`.
  `CONTRACT_VERSION_V2` is untouched (still `0.3.1`) — new origin variants and the `topics` field are
  agent-surface (CLI/envelope/schema) changes, not on-disk lifecycle/derivation changes, per the
  version-discipline note in `AGENTS.md`.
- Updated every golden fixture carrying the old product-version string:
  `tests/fixtures/json-v1/doctor-healthy.json`, `doctor-blocked.json`, and the four
  `tests/skill-forward/harness/*.json` transcripts `rust/mko-cli/tests/my_knowledge_os_skill.rs`
  compares byte-for-byte. Left `tests/fixtures/json-v2/handshake-success.json`/`doctor-success.json`
  alone, as Phase 1a's plan already noted — independent example fixtures validated only against the
  schema/Rust-model round trip.

Gate: `scripts/fmt.sh --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` are green from this worktree's root.

## Deliberately out of scope

- **Binary sidecars / images / docx / hwp** — Phase 3, per §7's phase table. The text-originals store
  built here (Task 1) is bounded to `text/plain` on purpose; extending it to binaries needs the named
  check-budget decision §8 calls out (a raised aggregate byte budget or a scoped, size/hash-verified
  exemption for `assets/originals`), which this phase does not make.
- **`topics` in `mko find`'s match output** — the design and this phase's work items ask only for a
  `--topic` *filter*; `FindMatchV2`/`find_match` were not extended with a `topics` field. Revisit if a
  citation ever needs to show which topic(s) a match carries.
- **A real filesystem mtime for `--local-file`'s `modified_at`** — defaults to "now" like the other
  three forms' `--fetched-at`, rather than reading the file's actual mtime from disk. Simpler and
  consistent across all four text-flavored forms; revisit if the owner's workflow ever needs the
  original file's real modification time preserved.
- **YouTube/video, origin-form filter completeness** — Phase 4, per §7.
