# Knowledge Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-asset knowledge-note artifact (synthesis + structured concept units) with lightweight review and cross-document concept search, without touching the frozen Asset/Source schema.

**Architecture:** A new `knowledge` record type parallel to `source`, stored at `knowledge/<file>.md` (front matter + rendered body), linked to its document by shared `asset_id`. A new `mko knowledge` CLI family (write/review/search/show/list) mirrors the existing `mko source` pipeline. The agent supplies a bounded `knowledge-response-v1` JSON; the Core owns all durable writes, review-state transitions, and search.

**Tech Stack:** Rust workspace (`mko-core`, `mko-cli`), serde/serde_yaml, chrono, sha2, cap-std patterns already used in `source.rs`/`registry.rs`.

**Spec:** `docs/superpowers/specs/2026-07-21-knowledge-extraction-design.md` (read it first).

## Global Constraints

- Do NOT modify the frozen Asset/Source contract: `schema_version: 1`, `core_version: 0.1.0` unchanged. Knowledge is a NEW record type with its OWN `schema_version: 1`.
- All new records: YAML front matter + rendered Markdown body, following `source.rs`/`registry.rs` conventions. Use `safe_yaml` for parsing, `front_matter` for splitting.
- `knowledge-response-v1` and `KnowledgeRecord`/`SourceMetadata`-style structs use `#[serde(deny_unknown_fields)]`.
- Reuse `source.rs::validate_semantic_size` size discipline for `synthesis` and each concept `body`.
- Bounded scans (deadline + entry/byte/depth limits) and no-follow (`symlink_metadata`) for any directory traversal, mirroring `provider_scan.rs`/`review.rs`.
- JSON-v1 failure messages are path-free and command/code-bound (mirror `mko-cli/src/output.rs::json_v1_failure_message`).
- Verify from `rust/`: `cargo test --workspace`; format `bash scripts/fmt.sh` (CJK comment alignment needs the auto-formatter); lint `cargo clippy --workspace --all-targets -- -D warnings`. No `timeout` binary — use background runs.
- TDD: every step writes the failing test first, watches it fail, then minimal code. Commit per task.

---

### Task 1: Knowledge model + `knowledge-response-v1` parsing/validation

**Files:**
- Create: `rust/mko-core/src/knowledge.rs`
- Modify: `rust/mko-core/src/lib.rs` (add `pub mod knowledge;` in alphabetical position, after `pub mod json_v1;`)
- Test: `rust/mko-core/tests/knowledge.rs` (new)

**Interfaces:**
- Produces:
  - `pub enum ConceptKind { Definition, Formula, Concept, Result, Theorem }` (serde `rename_all = "snake_case"`)
  - `pub struct Concept { pub id: String, pub name: String, pub kind: ConceptKind, pub body: String, pub tags: Vec<String>, pub locator: Option<String> }`
  - `pub struct KnowledgeResponse { pub synthesis: String, pub concepts: Vec<ConceptInput> }` where `pub struct ConceptInput { pub name: String, pub kind: ConceptKind, pub body: String, pub tags: Vec<String>, pub locator: Option<String> }` — response has NO `id` (Core assigns concept `id` by slugifying `name`). All `#[serde(deny_unknown_fields)]`.
  - `pub struct KnowledgeReview { pub status: ReviewState, pub reviewed_at: Option<DateTime<Utc>> }`, `pub enum ReviewState { Unreviewed, Reviewed }` (serde snake_case).
  - `pub struct KnowledgeGeneration { pub processor_version: String, pub prompt_version: String, pub asset_fingerprint: String }`
  - `pub struct KnowledgeRecord { pub id: String, pub record_type: String, pub schema_version: u32, pub asset_id: String, pub title: String, pub review: KnowledgeReview, pub content_revision: String, pub approved_revision: Option<String>, pub generation: KnowledgeGeneration, pub concepts: Vec<Concept> }` with `#[serde(deny_unknown_fields)]`.
  - `pub fn parse_knowledge_response(input: &[u8]) -> Result<KnowledgeResponse, MkoError>`
  - `pub fn normalize_and_validate_knowledge(response: &mut KnowledgeResponse) -> Result<(), MkoError>`

- [ ] **Step 1: Write failing tests for response parsing/validation**

In `rust/mko-core/tests/knowledge.rs`:
```rust
use mko_core::knowledge::{parse_knowledge_response, normalize_and_validate_knowledge, ConceptKind};

const VALID: &str = r#"{
  "synthesis": "A signals-and-systems text covering LTI systems and transforms.",
  "concepts": [
    {"name": "Convolution", "kind": "formula", "body": "x*h(t)=∫x(τ)h(t−τ)dτ", "tags": ["LTI"], "locator": "§4.2"},
    {"name": "Causal signal", "kind": "definition", "body": "x(t)=0 for t<0", "tags": [], "locator": null}
  ]
}"#;

#[test]
fn parses_and_validates_a_well_formed_response() {
    let mut r = parse_knowledge_response(VALID.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
    assert_eq!(r.synthesis.is_empty(), false);
    assert_eq!(r.concepts.len(), 2);
    assert_eq!(r.concepts[0].kind, ConceptKind::Formula);
}

#[test]
fn rejects_unknown_fields() {
    let bad = r#"{"synthesis":"x","concepts":[],"extra":1}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn rejects_empty_synthesis() {
    let bad = r#"{"synthesis":"   ","concepts":[]}"#;
    let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
    assert_eq!(normalize_and_validate_knowledge(&mut r).unwrap_err().code(), "semantic_schema_invalid");
}

#[test]
fn rejects_concept_with_empty_body_or_multiline_name() {
    for bad in [
        r#"{"synthesis":"x","concepts":[{"name":"A","kind":"concept","body":"  ","tags":[],"locator":null}]}"#,
        r#"{"synthesis":"x","concepts":[{"name":"A\nB","kind":"concept","body":"y","tags":[],"locator":null}]}"#,
    ] {
        let mut r = parse_knowledge_response(bad.as_bytes()).unwrap();
        assert!(normalize_and_validate_knowledge(&mut r).is_err());
    }
}

#[test]
fn rejects_invalid_kind() {
    let bad = r#"{"synthesis":"x","concepts":[{"name":"A","kind":"joke","body":"y","tags":[],"locator":null}]}"#;
    assert!(parse_knowledge_response(bad.as_bytes()).is_err());
}

#[test]
fn allows_empty_concepts() {
    let mut r = parse_knowledge_response(r#"{"synthesis":"x","concepts":[]}"#.as_bytes()).unwrap();
    normalize_and_validate_knowledge(&mut r).unwrap();
}
```

- [ ] **Step 2: Run tests, verify they fail** — Run (from `rust/`): `cargo test -p mko-core --test knowledge`. Expected: compile error (module `knowledge` missing).

- [ ] **Step 3: Implement `knowledge.rs` model + parse + validate**

Mirror `source.rs`:
- `parse_knowledge_response`: mirror `source.rs::parse_semantic_response` (line 122) — `serde_json::from_slice` mapping errors to `MkoError::new("semantic_schema_invalid", ...)`.
- `normalize_and_validate_knowledge`: mirror `source.rs::normalize_and_validate_response` (line 515): `normalize_string` each `synthesis`, each concept `name`/`body`/`tags`, reject empty/`\n` in `synthesis` and concept `name`, reject empty concept `body`; validate `kind` is already enforced by the enum at parse; enforce `validate_semantic_size` on `synthesis`, each `body`, and the aggregate. Use error code `semantic_schema_invalid` (reuse `source.rs::schema_error` style).
- Add `pub mod knowledge;` to `lib.rs`.
- `Concept.id` is NOT part of `ConceptInput`; it is assigned later (Task 2) via slugify. Keep `KnowledgeResponse`/`ConceptInput` as the parse target; `KnowledgeRecord`/`Concept` as the durable target.

- [ ] **Step 4: Run tests, verify pass** — Run: `cargo test -p mko-core --test knowledge`. Expected: 6 passed. Then `bash scripts/fmt.sh` and `cargo clippy -p mko-core --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**
```bash
git add rust/mko-core/src/knowledge.rs rust/mko-core/src/lib.rs rust/mko-core/tests/knowledge.rs
git commit -m "feat: knowledge-response-v1 model, parsing, and validation"
```

---

### Task 2: `write_knowledge_note` (create / replace / idempotent)

**Files:**
- Modify: `rust/mko-core/src/knowledge.rs`
- Test: `rust/mko-core/tests/knowledge.rs`

**Interfaces:**
- Consumes: Task 1 types; `clock::SystemClock`, `context::resolve_personal_context`/repository root resolution as `source.rs` does; `registry::read_asset` to fetch the asset (title, fingerprint) by `asset_id`.
- Produces:
  - `pub struct WriteKnowledgeRequest { repository_root, asset_id, response: Vec<u8> (raw json), replace: bool }` with builder `new(repo, asset_id, response)` + `with_replace(bool)`.
  - `pub struct WriteKnowledgeResult { pub result: String /* "created"|"replaced"|"existing" */, pub knowledge_id: String, pub knowledge_path: String, pub content_revision: String }`
  - `pub fn write_knowledge_note(req: WriteKnowledgeRequest) -> Result<WriteKnowledgeResult, MkoError>` and `..._with_clock(req, &dyn Clock)`.
  - `fn knowledge_directory(repo: &Path) -> Result<PathBuf, MkoError>` → `<repo>/knowledge`, created no-follow like `source.rs::sources_directory`.
  - `fn slugify_concept(name: &str) -> String` (lowercase, non-alnum→`-`, dedup dashes) with per-note uniqueness suffixes.
  - `fn calculate_knowledge_revision(record: &KnowledgeRecord) -> Result<String, MkoError>` (sha256 over canonical content: title+synthesis+concepts, excluding review/timestamps), mirror `revision.rs`/`source.rs::calculate_source_revision`.

- [ ] **Step 1: Write failing tests**

Add to `tests/knowledge.rs` a helper that builds a KB fixture with one asset (reuse the pattern from `rust/mko-core/tests/write_source.rs` — copy its fixture setup for repository + an asset registry record; read that file to get the exact helper). Then:
```rust
#[test]
fn write_creates_an_unreviewed_note_with_a_content_revision() {
    let kb = knowledge_fixture(); // repo with asset ASSET_ID present
    let res = write_knowledge_note(
        WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())
    ).unwrap();
    assert_eq!(res.result, "created");
    assert!(res.content_revision.starts_with("sha256:"));
    let doc = std::fs::read_to_string(kb.repo().join(&res.knowledge_path)).unwrap();
    assert!(doc.contains("status: unreviewed"));
    assert!(doc.contains("approved_revision: null"));
    assert!(doc.contains("# ") && doc.contains("## Synthesis") && doc.contains("## Concepts"));
    assert!(doc.contains("Convolution"));
}

#[test]
fn write_is_idempotent_for_identical_content() {
    let kb = knowledge_fixture();
    let a = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    let b = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    assert_eq!(b.result, "existing");
    assert_eq!(a.content_revision, b.content_revision);
}

#[test]
fn regenerating_requires_replace_and_resets_to_unreviewed_keeping_prior_approved_revision() {
    let kb = knowledge_fixture();
    write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    // approve it out-of-band via Task 3 API OR by asserting the replace-guard error here:
    let other = VALID.replace("LTI systems and transforms", "LTI systems, transforms, and sampling");
    let err = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), other.as_bytes().to_vec())).unwrap_err();
    assert_eq!(err.code(), "replace_required");
    let ok = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), other.as_bytes().to_vec()).with_replace(true)).unwrap();
    assert_eq!(ok.result, "replaced");
    let doc = std::fs::read_to_string(kb.repo().join(&ok.knowledge_path)).unwrap();
    assert!(doc.contains("status: unreviewed"));
}

#[test]
fn write_rejects_unknown_asset() {
    let kb = knowledge_fixture();
    let err = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), "personal-asset-deadbeef", VALID.as_bytes().to_vec())).unwrap_err();
    assert_eq!(err.code(), "asset_not_found");
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test -p mko-core --test knowledge`. Expected: unresolved `write_knowledge_note`.

- [ ] **Step 3: Implement `write_knowledge_note`**

Mirror `source.rs::write_source_draft_with_clock` (line 136) and `source_record` (line 450):
- Resolve repo root; `registry::read_asset(repo, asset_id)` → error `asset_not_found` if missing (map read error). Take `title`, `fingerprint` from the asset.
- `parse_knowledge_response` + `normalize_and_validate_knowledge` on `req.response`.
- Build `KnowledgeRecord`: `record_type="knowledge"`, `schema_version=1`, `id = format!("personal-knowledge-{}", asset_hash)` (derive hash portion from `asset_id` after its `personal-asset-` prefix), assign each concept `id` via `slugify_concept` (dedup within the note), `review = {status: Unreviewed, reviewed_at: None}`, `approved_revision = None`, `generation = {processor_version:"knowledge-v1", prompt_version:"codex-knowledge-v1", asset_fingerprint: <asset fingerprint>}`. Set `content_revision = calculate_knowledge_revision(&record)`.
- Filename: `knowledge/{date}-{slug(title)}-{shortid}.md` via a helper mirroring `source.rs::source_filename`.
- Existing note (find by `asset_id` in `knowledge/`): if `content_revision` matches → return `"existing"` (idempotent); else require `req.replace` (else `MkoError::new("replace_required", ...)`); on replace, keep the existing note's `approved_revision` value in the new record, set `status: Unreviewed`, write via `atomic`/`write_replace` pattern used in `source.rs`.
- Render body with a `render_knowledge_markdown` helper (mirror `source.rs::render_markdown`/`render_source_body`).
- Return `WriteKnowledgeResult`.

- [ ] **Step 4: Run, verify pass** — `cargo test -p mko-core --test knowledge`; then `bash scripts/fmt.sh` + clippy.

- [ ] **Step 5: Commit**
```bash
git add rust/mko-core/src/knowledge.rs rust/mko-core/tests/knowledge.rs
git commit -m "feat: write knowledge notes (create/replace/idempotent)"
```

---

### Task 3: Review lifecycle (single + batch, approve/defer)

**Files:**
- Modify: `rust/mko-core/src/knowledge.rs`
- Test: `rust/mko-core/tests/knowledge.rs`

**Interfaces:**
- Produces:
  - `pub struct PendingKnowledge { pub knowledge_id: String, pub asset_id: String, pub title: String, pub knowledge_path: String, pub content_revision: String }`
  - `pub fn list_unreviewed_knowledge(repo: &Path) -> Result<Vec<PendingKnowledge>, MkoError>` (bounded, no-follow enumeration of `knowledge/`, filter `status: unreviewed`).
  - `pub fn approve_knowledge(repo: &Path, knowledge_id: &str, content_revision: &str) -> Result<(), MkoError>` and `..._with_clock` — verifies the id+revision match the on-disk note (else `knowledge_revision_mismatch`), sets `status: Reviewed`, `approved_revision = content_revision`, `reviewed_at = now`.
  - (`defer` is a no-op at the Core level — the CLI simply does not call approve; no Core function needed.)

- [ ] **Step 1: Write failing tests**
```rust
#[test]
fn approve_marks_reviewed_and_records_approved_revision() {
    let kb = knowledge_fixture();
    let w = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    approve_knowledge(kb.repo(), &w.knowledge_id, &w.content_revision).unwrap();
    let doc = std::fs::read_to_string(kb.repo().join(&w.knowledge_path)).unwrap();
    assert!(doc.contains("status: reviewed"));
    assert!(doc.contains(&format!("approved_revision: {}", w.content_revision)));
    assert!(doc.contains("reviewed_at:") && !doc.contains("reviewed_at: null"));
}

#[test]
fn approve_rejects_a_stale_revision() {
    let kb = knowledge_fixture();
    let w = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    let err = approve_knowledge(kb.repo(), &w.knowledge_id, "sha256:0000").unwrap_err();
    assert_eq!(err.code(), "knowledge_revision_mismatch");
}

#[test]
fn list_unreviewed_returns_only_unreviewed_notes() {
    let kb = knowledge_fixture();
    let w = write_knowledge_note(WriteKnowledgeRequest::new(kb.repo(), kb.asset_id(), VALID.as_bytes().to_vec())).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 1);
    approve_knowledge(kb.repo(), &w.knowledge_id, &w.content_revision).unwrap();
    assert_eq!(list_unreviewed_knowledge(kb.repo()).unwrap().len(), 0);
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test -p mko-core --test knowledge`.

- [ ] **Step 3: Implement** — read the note (front_matter split + safe_yaml parse into `KnowledgeRecord`), mutate review fields, re-render, `write_replace`. `list_unreviewed_knowledge` enumerates `knowledge/` bounded + no-follow (mirror `review.rs::list_pending_sources`, including `MAX_SOURCE_ENTRIES`-style `.take(limit+1)`).

- [ ] **Step 4: Run, verify pass** — tests + fmt + clippy.

- [ ] **Step 5: Commit**
```bash
git add rust/mko-core/src/knowledge.rs rust/mko-core/tests/knowledge.rs
git commit -m "feat: knowledge review lifecycle (approve/defer, single/batch)"
```

---

### Task 4: Cross-document concept search

**Files:**
- Modify: `rust/mko-core/src/knowledge.rs`
- Test: `rust/mko-core/tests/knowledge.rs`

**Interfaces:**
- Produces:
  - `pub struct ConceptMatch { pub asset_id: String, pub title: String, pub name: String, pub kind: ConceptKind, pub locator: Option<String>, pub knowledge_path: String }`
  - `pub struct KnowledgeSearchQuery { pub term: String, pub kind: Option<ConceptKind>, pub tag: Option<String> }`
  - `pub fn search_knowledge(repo: &Path, query: &KnowledgeSearchQuery) -> Result<Vec<ConceptMatch>, MkoError>` — bounded/no-follow scan of all `knowledge/` notes; case-insensitive substring match of `term` against concept `name`+`body`+`tags`; AND-filter by `kind`/`tag` when present.

- [ ] **Step 1: Write failing tests** — fixture with TWO assets, each written a note (one with a "Convolution" formula tagged `LTI`, another with a different concept). Assert `search_knowledge` finds cross-note matches, respects `--kind`/`--tag` filters, returns empty on no-match, and does not exceed a bounded entry count (add >limit notes and assert it errors with `knowledge_scan_limit` OR caps — mirror the existing bound test in `atomic.rs`). Provide concrete assertions like:
```rust
let hits = search_knowledge(kb.repo(), &KnowledgeSearchQuery{ term: "convolution".into(), kind: None, tag: None }).unwrap();
assert_eq!(hits.iter().any(|h| h.name == "Convolution"), true);
let formulas = search_knowledge(kb.repo(), &KnowledgeSearchQuery{ term: "x".into(), kind: Some(ConceptKind::Formula), tag: None }).unwrap();
assert!(formulas.iter().all(|h| h.kind == ConceptKind::Formula));
```

- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — bounded no-follow enumeration + parse each note + flat-map concept matches. Reuse a `ScanDeadline`/limit pattern (see `provider_scan.rs::DEFAULT_SCAN_LIMITS` and `add.rs` usage).
- [ ] **Step 4: Run, verify pass** — tests + fmt + clippy.
- [ ] **Step 5: Commit**
```bash
git add rust/mko-core/src/knowledge.rs rust/mko-core/tests/knowledge.rs
git commit -m "feat: cross-document knowledge concept search"
```

---

### Task 5: `mko check` integration

**Files:**
- Modify: `rust/mko-core/src/check.rs` (add knowledge validation into the existing check walk), `rust/mko-core/src/knowledge.rs` (add `pub fn validate_knowledge_record(path: &str, record: &KnowledgeRecord) -> Vec<KnowledgeValidationIssue>` mirroring `asset_validation.rs`).
- Test: `rust/mko-core/tests/knowledge.rs` and/or `rust/mko-core/tests/check_and_approve.rs`

**Interfaces:**
- Produces: `pub struct KnowledgeValidationIssue { pub path: String, pub message: String }`; validation rules: `record_type=="knowledge"`, `schema_version==1`, `content_revision` recomputes, `approved_revision.is_some() == (status==Reviewed)`, `asset_id` refers to an existing asset registry record, concept `id`s unique/non-empty.

- [ ] **Step 1: Write failing test** — write a note, hand-corrupt it (e.g., set `status: reviewed` but `approved_revision: null`), assert `mko_core::check::run_check(...)` (use the same entry the CLI `check` uses — read `check.rs` for the exact public fn/signature) reports an error mentioning the knowledge inconsistency; a clean note passes.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — add a knowledge pass to the check walk mirroring how `check.rs` validates sources/assets; surface issues in the existing report structure.
- [ ] **Step 4: Run, verify pass** — `cargo test -p mko-core`; fmt + clippy.
- [ ] **Step 5: Commit**
```bash
git add rust/mko-core/src/check.rs rust/mko-core/src/knowledge.rs rust/mko-core/tests/*.rs
git commit -m "feat: validate knowledge notes in mko check"
```

---

### Task 6: CLI `mko knowledge` family + JSON-v1

**Files:**
- Modify: `rust/mko-cli/src/cli.rs` (add `Knowledge(KnowledgeCommand)` to `Commands`, a `KnowledgeCommand` subcommand enum: `Write`, `Review`, `Search`, `Show`, `List`, and dispatch fns mirroring `fn write_draft`/`fn prepare`), `rust/mko-core/src/json_v1.rs` (add `JsonV1Command` variants: `KnowledgeWrite` = "knowledge.write", `KnowledgeReview`="knowledge.review", `KnowledgeSearch`="knowledge.search", `KnowledgeShow`="knowledge.show", `KnowledgeList`="knowledge.list"), `rust/mko-cli/src/output.rs` (`json_v1_failure_message` arms for the new commands, path-free), plus success payload structs.
- Test: `rust/mko-cli/tests/cli.rs`, `rust/mko-cli/tests/json_v1_cli.rs`, new goldens under `rust/mko-cli/tests/fixtures/json-v1/` and repo `tests/fixtures/json-v1/`.

**Interfaces:**
- Consumes: all Task 1–5 Core fns.
- `knowledge review` is TTY-gated exactly like `mko review` — reuse the interactive-guard pattern in `cli.rs` (search for `human_confirmation_required`/`tty_required`); single mode with `--asset-id`, batch mode without. Approval is a lightweight per-note `approve`/`DEFER` prompt (does NOT require the Source-style exact APPROVE token — a `y`/`approve` confirmation per listed note is sufficient), then calls `approve_knowledge`.

- [ ] **Step 1: Write failing CLI tests** — invoke the built binary via the existing CLI test harness (read `rust/mko-cli/tests/cli.rs` for the `Command::cargo_bin`/helper pattern). Assert:
  - `mko knowledge write --asset-id <id> --bundle <b> --response <r> --format json-v1` emits `{"command":"knowledge.write","result":"ok",...}`.
  - `mko knowledge search convolution --format json-v1` emits matches.
  - A failure (unknown asset) emits a path-free `{"result":"error", ...}` envelope; add a golden `tests/fixtures/json-v1/knowledge-asset-missing.json`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — wire subcommands + dispatch + JSON-v1 success/failure envelopes mirroring `source`/`add`. Add `JsonV1Command` variants and `output.rs` message arms (path-free).
- [ ] **Step 4: Run, verify pass** — `cargo test -p mko-cli`; fmt + clippy.
- [ ] **Step 5: Commit**
```bash
git add rust/mko-cli/src rust/mko-core/src/json_v1.rs rust/mko-cli/tests tests/fixtures/json-v1/knowledge-asset-missing.json
git commit -m "feat: mko knowledge CLI family with JSON-v1 output"
```

---

### Task 7: Skill contract + adapter/forward coverage

**Files:**
- Modify: `skills/codex/my-knowledge-os/SKILL.md` (add the knowledge-extraction flow), `tests/skill-forward/my-knowledge-os-scenarios.md`, `tests/skill-forward/my-knowledge-os-rubric.md`, `rust/mko-cli/tests/adapter_policy.rs` (allowlist + assertions for the knowledge command family).
- Test: `rust/mko-cli/tests/adapter_policy.rs`, `rust/mko-cli/tests/my_knowledge_os_skill.rs`.

- [ ] **Step 1: Write failing adapter tests** — extend the allowlist checks in `adapter_policy.rs` to require the knowledge commands (`mko knowledge write`, `mko knowledge review`) appear, that the Skill treats the bundle as `untrusted_document_text`, produces only `knowledge-response-v1`, and never approves/commits/pushes; add a forward scenario + rubric field (e.g., `knowledge_untrusted_bundle`, `knowledge_no_review_execution`). Follow the exact assertion style already in `adapter_policy.rs`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — add the "Knowledge extraction" section to `SKILL.md`: from an added asset with an existing prepared bundle, require `trust == untrusted_document_text`, emit `knowledge-response-v1` (synthesis + concepts, `Interpretation:` prefix for inferences, "Not stated in the document" discipline), run `mko knowledge write`, then name `mko knowledge review` as the human's single next action; extend Boundaries to cover the new command family. Update scenarios/rubric.
- [ ] **Step 4: Run, verify pass** — `cargo test -p mko-cli`; fmt + clippy.
- [ ] **Step 5: Commit**
```bash
git add skills/codex/my-knowledge-os/SKILL.md tests/skill-forward/*.md rust/mko-cli/tests/*.rs
git commit -m "feat: knowledge-extraction Skill flow and adapter coverage"
```

---

### Task 8: Full verification

- [ ] **Step 1:** `bash scripts/fmt.sh --check` → exit 0.
- [ ] **Step 2:** from `rust/`: `cargo clippy --workspace --all-targets -- -D warnings` → exit 0.
- [ ] **Step 3:** from `rust/`: `cargo test --workspace` → all suites 0 failed.
- [ ] **Step 4: Commit any formatting fixups** (if `fmt.sh` changed files).
```bash
git add -A && git commit -m "chore: knowledge extraction verification pass" || true
```

## Self-review notes (traceability to spec)

- Spec §3 data model → Task 1 (model) + Task 2 (record write, normalization: only `title` denormalized, linked by `asset_id`).
- Spec §4 `knowledge-response-v1` → Task 1.
- Spec §5 commands → Task 2 (write), 3 (review), 4 (search), 6 (show/list + CLI).
- Spec §6 review lifecycle (single/batch, replace resets to unreviewed, keeps prior approved_revision) → Task 2 + Task 3.
- Spec §7 check integration → Task 5.
- Spec §8 Skill → Task 7.
- Spec §9 JSON-v1/CLI → Task 6.
- Spec §10 testing → tests embedded in every task.
- Spec §11 security (bounded/no-follow/size/frozen contract) → Global Constraints + Tasks 2/3/4.
- Spec §12 Phase 2 out of scope → not implemented; only the two-revision substrate (Task 2/3) is preserved.
