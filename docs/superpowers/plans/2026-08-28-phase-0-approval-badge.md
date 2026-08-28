# Phase 0 — Approval as a Badge, Implementation Plan

Date: 2026-08-28
Design: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md` §4 (phases table row 0),
decisions D1, D6, D8, D9, D12
Strategy: reinterpret the v3 review-derivation vocabulary from a blocking gate to a recorded
confirmation badge, rename the human-only confirmation command, and gate the contract-version bump
behind a git-clean-tree migration — without rewriting any existing record file.

## Task 1 — Record semantics (§4.1)

- Added `AuthoredByV2` (`ai` | `human`, default `ai`) to `SourceRevisionV2` and
  `KnowledgeRevisionV2`, skipped on serialization when `Ai` (mirrors `AssetOriginV2`'s
  `ProviderPdf` default) so a revision written before this field existed still round-trips to its
  original, still content-addressed bytes. Every current producer sets `Ai`; `Human` exists for a
  later phase's owner-typed producer.
- Left the human-check field derived, not stored: the append-only review event graph already binds
  an `approve` event to its exact target revision and timestamp (`review_v2::ReviewTargetHistoryV2`),
  so that *is* the confirmation badge — Phase 0 only renames how it is derived and surfaced.
- Renamed the derivation vocabulary throughout `review_v2.rs`, `queue_v2.rs`, `projection_v2.rs`,
  and their `json_v2.rs` mirrors: `ReviewDerivedStateV2`/`ReviewCardTargetStateV2`/
  `ProjectionStateV2`/`QueueItemStateV2`/`ReviewTargetStateV2`/`ResurfacedKnowledgeStateV2` all
  rename `Unreviewed → Unconfirmed`, `RevisedUnreviewed → RevisedUnconfirmed`,
  `Approved → Confirmed` (and the matching lowercase wire strings). The on-disk `ReviewDecisionV2`
  enum keeps its literal `approve`/`request_changes`/`defer` — history is append-only and must stay
  parseable — only the vocabulary derived *from* it changed.
- High-risk schema requirements (counterargument + open question) remain enforced at write time in
  `records_v2::validate_knowledge_response`; nothing about that changed.

Gate: `cargo test -p mko-core` round-trips a pre-Phase-0 revision file with no `authored_by` field
and derives `unconfirmed`/`confirmed` from the existing `approve` events unchanged.

## Task 2 — Confirmation command rename (D8)

- Renamed the CLI command from `Command::Review`/`ReviewArgs`/`fn review` to
  `Command::Confirm`/`ConfirmArgs`/`fn confirm` in `mko-cli/src/cli.rs`, and
  `cli_v2::review` to `cli_v2::confirm`. The old `mko review` name is removed, not aliased — it now
  parses as an unrecognized subcommand.
- `TtyReviewOutcomeV2::Approved` → `Confirmed`; the internal `TtyReviewChoiceV2::Approve` choice
  (bound to the `[a]` keystroke) → `Confirm`. Korean TTY prompt: `[a] 승인` → `[a] 확인`; the
  displayed effect name `approve_current_revision_via_tty` → `confirm_current_revision_via_tty`.
- The command stays human-only, revision-bound, and TTY-guarded exactly as before — only its name
  and the vocabulary describing its consequence changed; `publish_tty_review_v2`'s validation,
  locking, and stale-snapshot rejection are untouched.

Gate: `mko confirm "STABLE_ID"` runs the exact prior TTY flow; `mko review` fails as an unknown
subcommand; `rust/mko-cli/tests/review_v2_cli.rs` and `rust/mko-cli/tests/review.rs` exercise both.

## Task 3 — Reinterpreted surfaces (§4.2)

- `V3HomeReport` (mko-core `home.rs`) drops `review_pending`/`changes_requested`; `next_action()`
  no longer offers `HomeNextAction::Review` for a v3 repository. The terminal home screen
  (`mko-cli/src/cli.rs`) removes the `[2] 검토 계속` menu entry and its status-line counts,
  renumbering the remaining V3 menu to `[1] 자료 정리 · [2] 지식 찾기 · [3] 빠른 메모 · [4] 문제
  확인/다시 볼 지식`. The `mko ui` read-only web dashboard (`ui.rs`/`ui.html`) drops the matching
  `review_pending`/`changes_requested` counts and its "지식 검토" pending-count tile, replacing it
  with a "확인된 지식" count. `mko queue` and `mko confirm` remain fully functional, reachable only
  by command.
- Consolidated the generated Obsidian dashboard body into one definition,
  `dashboard_v2::GENERATED_FILES` (previously duplicated, and already drifted, between
  `dashboard_v2.rs` and `setup_plan_v2.rs`); `setup_plan_v2.rs` now composes its own extra
  `.mko/.gitignore` scaffold entry on top of it instead of carrying a second copy.
- Renamed the generated view `views/review-queue.base` to `views/unconfirmed.base`
  (`derived_state != "confirmed"`, view name `Unconfirmed`); `views/knowledge-library.base`'s filter
  becomes `derived_state == "confirmed"`. `HOME.md` headings become `## 미확인 지식` /
  `## 확인된 지식`. `projection_v2::is_canonical_dashboard_path` tracks the renamed path.
- Search stays confirmed-only in Phase 0 (unconfirmed-inclusive search is Phase 1a per the phases
  table); only its labels changed: `search_approved_knowledge_v2` →
  `search_confirmed_knowledge_v2` (and the `_by_perspective_v2` variant), `resurface_approved_*` →
  `resurface_confirmed_*`, `HomeQueueSummaryV2.approved_knowledge` → `.confirmed_knowledge`.

Gate: `rust/mko-cli/tests/home_cli.rs` asserts the V3 home screen never contains "검토 계속" or
"확인 계속"; `rust/mko-core/tests/dashboard_v2.rs` and `setup_dashboard_v2.rs` exercise the renamed
view end to end.

## Task 4 — Migration + contract version (§4.3, D12)

- `CONTRACT_VERSION_V2` increments `0.3.0 → 0.3.1` in `config_v2.rs` (a patch-level bump within the
  same "v0.3 Personal KB" generation label — Phase 0 does not rename that epithet anywhere else in
  the codebase). Added `MIGRATABLE_CONTRACT_VERSION_V2 = "0.3.0"`, the one prior contract this
  migration knows how to upgrade.
- Split `KnowledgeConfigV2::read`/`validate` into a shape-only `validate_shape` plus the
  contract-version check, and added `read_for_migration`, which accepts exactly the migratable
  contract instead of the current one — every other check stays identical to `validate`. A KB
  declaring the migratable contract now gets a distinct, loud `kb_contract_outdated` refusal (naming
  `mko migrate` and "clean git tree") from `KnowledgeConfigV2::read`, instead of the generic
  `kb_schema_unsupported` a truly foreign KB gets.
- Added `mko_core::migrate_v2::migrate_v2` (new module) and the `mko migrate --repo <path>` CLI
  command (human-only, no `--format`, like `mko setup`): refuses unless `git status --porcelain` is
  empty, stamps the new contract version via compare-and-swap, retires
  `views/review-queue.base` (`projection_v2::retire_generated_dashboard_file_locked_v2`, new), then
  regenerates every projection and dashboard file under the new vocabulary through the same
  `repair_dashboard_v2` machinery `mko dashboard --repair` uses. Record revision files are never
  rewritten — only the derivation and the generated views change.
- No per-record trace of the prior contract is kept; git is the rollback, matching D6.

Gate: `rust/mko-core/tests/migrate_v2.rs` (old-KB detection with `mko migrate` guidance, dirty-tree
refusal, already-current refusal, and full stamp + regenerate + retire) and
`rust/mko-cli/tests/migrate_v2_cli.rs` (same, through the `mko` binary) are new and pass.

## Task 5 — Contract counterpart doc (D9)

- Searched this repository's working tree and full git history for an existing MKO-side counterpart
  to Thesis's `docs/MKO_REFERENCE_CONTRACT.md`; none exists. Added
  `docs/THESIS_REFERENCE_CONTRACT.md`: records enter Thesis as opaque `mko://` URIs; only
  human-confirmed records are eligible for promotion, as owner operating discipline today (Thesis's
  own human review status is the enforcing gate; MKO has no Thesis-side write path); the former
  "MKO approval state" precondition is replaced by the human-confirmation badge. References this
  spec. No Thesis code change is made or authorized.

## Task 6 — Version discipline

- Bumped `workspace.package.version` `0.3.25 → 0.4.0` in `rust/Cargo.toml` and the three pinned
  sites: `rust/mko-core/tests/contract_version.rs`, the `mko --version` assertion in
  `rust/mko-cli/tests/cli.rs`, and the handshake pin in `skills/codex/my-knowledge-os/SKILL.md`.
  Updated every golden fixture under `tests/fixtures/` and `tests/skill-forward/harness/` carrying
  the old product-version string.
- Updated `schemas/v2/projection.schema.json` and `schemas/v2/machine-output.schema.json`'s
  `derived_state`/queue-item `state` enums to the renamed values, and their matching goldens under
  `tests/fixtures/json-v2/`. Added the optional, non-required `authored_by` property to
  `schemas/v2/source-revision.schema.json` and `knowledge-revision.schema.json` (documentary — not
  embedded in the agent-facing `mko schema show` surface, which only serves `source-response-v2`,
  `knowledge-response-v2`, and `review-feedback-input-v2`).
- Updated `skills/codex/my-knowledge-os/SKILL.md` wherever it described approval/queue semantics or
  the renamed command: the confirmation-command rename (`mko review` → `mko confirm`), the
  "no pending human review" record-semantics reword throughout, the `수정 후 미검토` → `수정 후
  미확인` queue-state label, and the "no automatic approval" boundary line → "no automatic
  confirmation". Left `skills/codex/capture-asset` and `skills/codex/process-asset` untouched — both
  target the frozen legacy v0.1 pipeline (`semantic-response-v1`, `--json`), out of Phase 0's v3
  scope. Updated `README.md`'s matching Korean prose and the `mko review` → `mko confirm` example.
- `rust/mko-cli/tests/adapter_policy.rs` pins exact substrings from `SKILL.md`/`README.md`; every
  assertion that pinned old vocabulary (`"pending human review"`, `"No automatic approval..."`,
  `"mko review"` in the command allowlists) was updated to the new wording rather than left to
  freeze the retired vocabulary in place.

Gate: `scripts/fmt.sh --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` (74 suites) are green from this worktree's root.

## Deliberately out of scope

- Search behavior (unconfirmed-inclusive search, query NFC normalization, whitespace-token AND
  matching, source-record search, `mko find`/`mko knowledge search` unification) — Phase 1a, per the
  phases table and this task's explicit instruction.
- Renaming the "v0.3" generation epithet used throughout error messages, docs, and specs to "v0.4" —
  `CONTRACT_VERSION_V2` moved to `0.3.1`, a patch-level bump within the same generation label, so no
  such rename was needed or made.
- The legacy v0.1 pipeline (`approve.rs`, `review.rs`, `knowledge.rs`, `config.rs`, the
  `capture-asset`/`process-asset` Skills, `HumanCommand::ApproveSource`) — a frozen, historical
  surface with its own separate contract, untouched by this design.
- Any Thesis repository code change — the docs-only contract revision in
  `docs/THESIS_REFERENCE_CONTRACT.md` is the entire Phase 0 scope on that boundary.
