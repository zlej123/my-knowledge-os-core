# Phase 1a — Recall Contract, Implementation Plan

Date: 2026-08-28
Design: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md` §5 (phases table row
1a), decisions D7, D10, D11
Strategy: fix the two live search bugs (query NFC normalization, whitespace-token AND matching),
make search unconfirmed-inclusive with a stable confirmation label, add Source-record search, unify
everything behind one `mko find` json-v2 surface with filters, log every recall, surface the recall
metrics on `mko home`, and make the Skill search before answering every substantive question —
without adding a semantic index or store-on-miss (both explicitly out of scope, §5/§9).

## Task 1 — Query NFC normalization and whitespace-token AND matching (D10)

- Added `normalize_query_tokens_v2` (`queue_v2.rs`) and its `quick_note_v2.rs` counterpart: NFC the
  raw query, lowercase it, then split on Unicode whitespace and drop empties — the query is
  normalized exactly once, at the single point it enters the Core, matching the existing
  `path_policy.rs::collision_key` pattern (`value.nfc().collect::<String>().to_lowercase()`).
  Callers never double-normalize.
- Replaced the single-needle `.contains()` check with per-field haystack concatenation
  (`knowledge_unit_haystack`/`source_haystack`) plus `tokens_match_all`: every token must appear
  somewhere in the candidate's searched fields (OR across fields, AND across tokens) — deterministic,
  no index.

Gate: `search_normalizes_nfd_query_against_nfc_stored_text` and
`search_requires_all_whitespace_tokens_to_match` (`rust/mko-core/tests/queue_v2.rs`), plus the
equivalent pair for quick notes (`rust/mko-core/tests/quick_note_v2.rs`) — the Korean acceptance case
"학습률 개선" against "…학습률을 크게 개선하는…" is exercised in both.

## Task 2 — Unconfirmed-inclusive, unified Source+Knowledge search with a confirmation label (§4.2, D10)

- Replaced `search_confirmed_knowledge_v2`/`search_confirmed_knowledge_by_perspective_v2` with
  `search_records_v2`/`search_records_by_perspective_v2` (`queue_v2.rs`): the confirmed-only state
  filter is gone from the default path; a `SearchConfirmationFilterV2` (`Any` | `ConfirmedOnly` |
  `UnconfirmedOnly`) lets a caller narrow explicitly instead.
- Added `ConfirmationLabelV2 { Confirmed { at }, Unconfirmed }` — a stable two-state display label
  derived from `target.state`/`history.current_reviewed_at`, not the whole six-state review
  machinery. Every match carries one.
- Added the Source match arm: matches `title`, `one_sentence_summary`, `general_summary`, each
  key-claim's `text`, and `tags` (`SourceResponseV2`). Renamed `KnowledgeSearchLayerV2` →
  `SearchLayerV2` with a new `SourceOwnWords` variant (a Source hit is the document's own summary,
  never LLM analysis) and `KnowledgeSearchMatchV2` → `SearchMatchV2`, now carrying `record_type`
  (`Source` | `Knowledge`) and `confirmation`. One sorted, deterministic `Vec<SearchMatchV2>` for
  both record types — the CLI, the web UI, and every test now consume one type.
- `--tag` and `--layer` filters ride the same function (`tag_matches`/the `layer` parameter),
  absorbed from the removed `mko knowledge search` per D10.
- Threaded the confirmation label through `rust/mko-cli/src/ui.rs`'s `/api/search` and `ui.html`'s
  result badge (`AI 작성 · 미확인` for unconfirmed, `확인됨 (date)` for confirmed) — the web UI now
  shows Source hits and unconfirmed records too, matching `mko find`'s default scope.

Gate: `search_includes_unconfirmed_knowledge_labelled`, `search_confirmed_only_filter_excludes_unconfirmed`,
`search_matches_source_records`, `search_tag_and_layer_filters_narrow_results`
(`rust/mko-core/tests/queue_v2.rs`).

## Task 3 — `mko find` json-v2 surface and filters (D10, D11)

- `find` had no JSON output before this phase (`FindArgs` was `term`/`perspective`/`repo` only,
  human-text-only). Added `--format` (Human default | JsonV2; JsonV1 rejected, matching `ask`/`queue`),
  `--confirmed`/`--unconfirmed` (mutually exclusive via clap `conflicts_with`), `--tag`, and
  `--layer` (`FindLayerArg`, one clap value per `SearchLayerV2` variant).
- Added `JsonV2Command::Find`, `FindDataV2`/`FindMatchV2`/`FindNoteV2`/`FindConfirmationV2` (and their
  supporting enums) to `json_v2.rs`, `JsonV2Success::find`, the `find`/`find_success` `$defs` and
  `command` enum entry in `schemas/v2/machine-output.schema.json`, and
  `tests/fixtures/json-v2/find-success.json`. No `schema_v2.rs` registration was needed — that module
  only serves agent-authored request-body schemas (`source-response-v2`, `knowledge-response-v2`,
  `review-feedback-input-v2`); `find` is a read command with CLI-flag input, not a JSON body the
  agent authors.
- **No time-range filter (D11, ratified).** The original Phase 1 sketch assumed one. Unconfirmed
  revisions carry no timestamp field at all in this Core version (`history.current_reviewed_at` only
  exists once a record is *confirmed*) — a time-range filter would fail D11's own "only work over
  data that exists today" test. Descoped; the recall log's own `at` timestamps are what will ground
  this filter's design if it is ever built.
- The Legacy v0.1 `find` path (`search_knowledge`/`search_knowledge_with_scan` in `knowledge.rs`) is
  completely untouched: new filters and `--format json-v2` are rejected there with
  `perspective_v3_required`/`format_unsupported`, matching how `--perspective` was already gated.

Gate: `rust/mko-cli/tests/find_cli.rs` (new) — schema-validated json-v2 round trip, confirmed/
unconfirmed/tag/layer filtering through the real binary, and human-format confirmation labels.

## Task 4 — `mko knowledge search` removed (D10)

- It always ran the legacy v1 scan regardless of repository generation and reported "approved only"
  while actually matching unreviewed records too — a live misdescription of its own scope, not just
  redundant now that `find` covers everything. Removed `KnowledgeCommand::Search`, `KnowledgeSearchArgs`,
  the CLI `knowledge_search`/`concept_match_data` functions and their dispatch arms,
  `JsonV1Command::KnowledgeSearch`, `JsonV1Success::KnowledgeSearch`, `ConceptMatchData`,
  `KnowledgeSearchData`, and the now-orphaned `ConceptKindArg`.
- Kept `knowledge.rs`'s `search_knowledge`/`search_knowledge_with_scan`/`KnowledgeSearchQuery` — the
  Legacy v0.1 `mko find` path still calls them, and that path is explicitly untouched by this phase.
  Kept `ConceptKind`/`concept_kind_label` — still used by `mko knowledge show`.

Gate: `cargo build --workspace` (no dangling references); the removed CLI test
(`knowledge_search_finds_cross_document_matches_and_supports_filters` in
`rust/mko-cli/tests/knowledge_cli.rs`) is gone, not disabled.

## Task 5 — Recall log (D7, ratified decisions)

- **New module `rust/mko-core/src/recall_log_v2.rs`.** `logs/` was added to `scaffold_v2.rs`'s
  `OWNED_DIRECTORIES` — tracked by git like any KB content (auditable with `git log -p`), explicitly
  **not** under `.mko/` (the gitignored local-runtime tree), because the recall log is retrieval
  evidence, not runtime state.
- **Single-file JSONL append (ratified).** `logs/recall.jsonl`, one line per `mko find` execution:
  `{"at": RFC3339, "query": verbatim, "results": N, "surfaced": [ids...]}`, written with
  `OpenOptions::create(true).append(true)` while holding `RepositoryMutationLock` (`find` becomes a
  mutating command for this one side effect, same as `mko dashboard --repair`) — the append is the
  first true multi-writer append path in this codebase; the alternative (one file per query) was
  rejected as unauditable sprawl for no benefit, since the lock already serializes writers.
- **`surfaced`, not `cited` (ratified).** The Core knows only what search returned, never whether the
  agent's answer actually used it — tracking real citation would need a second round-trip (the agent
  reporting back which records it cited) that this phase does not add. `surfaced` names what it
  actually is: every returned record ID, Source and Knowledge and quick notes alike.
- **Query text verbatim, no truncation (ratified, §5's own risk acceptance).** The log carries the
  owner's questions permanently in git history at the same sensitivity as the KB itself — accepted
  in the design's Risks section, not re-litigated here.
- A logging failure never withholds the search results: `find` catches the append error, still emits
  the results, and prints a one-line warning to stderr (never into a json-v2 stdout envelope).

Gate: `recall_log_appends_one_line_per_query`/`recall_log_records_zero_results` and the
corrupt-line-survival test (`rust/mko-core/src/recall_log_v2.rs`'s own `#[cfg(test)]` module), plus
the CLI-level append assertion in `rust/mko-cli/tests/find_cli.rs`.

## Task 6 — `mko home` recall metrics (D7)

- Extended `V3HomeReport` with `recall: RecallSummaryV2 { window_days, recall_count,
  zero_result_count, surfaced_total }`, computed by `recall_log_v2::recall_metrics_v2` over a rolling
  30-day window from a single **bounded tail read** (last 8 MiB, not the whole file — the log is
  append-only and grows without bound) of `logs/recall.jsonl`. Malformed or partial lines are skipped
  defensively; a corrupt log line must never crash the home screen, and a missing log (before the
  first `mko find`) reports all zeros rather than erroring.
- Rendered in `render_home` (`cli.rs`), Korean labels matching the surrounding style: "최근 30일
  recall N회 · 빈 결과 M회 · 제시한 기록 K건" — the design's success measure lives on the first
  screen, exactly as §5 asks.

Gate: `home_aggregates_recall_metrics_and_survives_corrupt_lines`/
`home_never_crashes_when_no_recall_log_exists_yet` (`rust/mko-core/tests/home.rs`) and
`home_surfaces_recall_metrics_from_a_prior_find` (`rust/mko-cli/tests/home_cli.rs`, through the real
TTY home screen).

## Task 7 — SKILL.md recall contract (D7)

- Added `## Recall contract` immediately after `## User language` (ratified insertion point — the
  contract is a vocabulary-adjacent rule the agent needs before any domain-specific workflow section,
  not buried after setup/registration prose it might skip while looking for its own task).
- Directs the agent: for every substantive question, run `mko find "QUERY" --format json-v2` first,
  unconditionally, with no domain judgment about whether the base "might" cover it; ground the answer
  in `data.items`/`data.notes`; cite every used record by `mko://` + its exact `record_id`; state each
  cited record's confirmation label in the same sentence as its citation. A miss is named as data, not
  papered over — and store-on-miss is explicitly *not* offered (Phase 2 capability; the design's
  §5 defers it so the recall log's zero-result entries can ground its own design first).
- `rust/mko-cli/tests/adapter_policy.rs`'s command allowlists (`knowledge_os_skill_exposes_only_the_v2_core_workflow`,
  `knowledge_os_skill_defines_the_knowledge_extraction_flow`) gained `"mko find"`. The inline-code
  heuristic that flags bare mixed-case tokens as "commands outside the adapter policy" forced two
  small wording adjustments (an `mko://` example written as prose instead of a mixed-case placeholder
  token, and `--tag`/`--layer` written as bare flags rather than `flag value` pairs inside one
  backtick span) — no behavior change, just avoiding a false positive in that policy scanner.

Gate: `knowledge_os_skill_pins_the_exact_cli_version_handshake`,
`knowledge_os_skill_exposes_only_the_v2_core_workflow`,
`knowledge_os_skill_defines_the_knowledge_extraction_flow` (`rust/mko-cli/tests/adapter_policy.rs`).

## Task 8 — skill-forward scenarios (D7)

- Added Scenario 15 (recall before answering: a conversational-sounding question with no PDF or
  Asset ID in play must still trigger `mko find` first, unconditionally) and Scenario 16 (citation
  confirmation labels: the same result must be cited by `mko://` ID with each record's confirmed/
  unconfirmed label stated inline, not as a separate footnote) to
  `tests/skill-forward/my-knowledge-os-scenarios.md`, with a new harness fixture
  `tests/skill-forward/harness/recall-before-answer.json` (one confirmed Knowledge hit, one
  unconfirmed Source hit) and matching `recall_before_answer`/`citation_confirmation_labels` rows in
  `tests/skill-forward/my-knowledge-os-rubric.md`.
- These scenarios are not wired into `cargo test` — per `AGENTS.md`, `tests/skill-forward/` is
  exercised separately against `harness/` fixtures by the forward-test process, not by
  `cargo test --workspace`. (The five pre-existing harness files that also double as Rust golden
  fixtures for `rust/mko-cli/tests/my_knowledge_os_skill.rs` are a distinct, older overlap this phase
  did not extend.)

## Task 9 — Version discipline

- Bumped `workspace.package.version` `0.4.0 → 0.4.1` in `rust/Cargo.toml` and the three pinned sites:
  `rust/mko-core/tests/contract_version.rs`, the `mko --version` assertion in
  `rust/mko-cli/tests/cli.rs`, and the handshake pin in `skills/codex/my-knowledge-os/SKILL.md`.
  `CONTRACT_VERSION_V2` is untouched (still `0.3.1`) — this phase adds read/query surface only, no
  on-disk lifecycle change, per the version-discipline note in `AGENTS.md`.
- Updated every golden fixture carrying the old product-version string that a real `mko doctor`
  step's output is compared against byte-for-byte: `tests/fixtures/json-v1/doctor-healthy.json`,
  `doctor-blocked.json`, and the four `tests/skill-forward/harness/*.json` transcripts consumed by
  `rust/mko-cli/tests/my_knowledge_os_skill.rs`. Left `tests/fixtures/json-v2/handshake-success.json`
  and `doctor-success.json` alone — both are independent example fixtures validated only against the
  schema/Rust-model round trip, never compared against the live `PRODUCT_VERSION` constant.

Gate: `scripts/fmt.sh --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` are green from this worktree's root.

## Deliberately out of scope

- **Time-range filter** — descoped per D11 (Task 3): no timestamp field exists on unconfirmed
  revisions today, so the filter would have no data to operate on. Recorded above with its reason.
- **Store-on-miss** — Phase 2, per §5/§9: its storage target (conversation content) is not a
  first-class input form until that phase, and the recall log's zero-result entries are meant to
  accumulate as the occurrence data that grounds its design.
- **Topic and origin-form filters** — ride with Phase 2/3, which create the underlying `topics` and
  origin-form fields; nothing to filter on yet.
- **A semantic search engine, embeddings, or any nondeterministic index** — never in scope for the
  Core (§9); Phase 1a's matching stays deterministic substring/token AND, exactly as before.
- **The legacy v0.1 `find` pipeline** — untouched throughout (Task 3, Task 4).
