# Frictionless Knowledge Design ("넣으면 저장된다")

Date: 2026-08-28
Status: Design approved by the owner in conversation on 2026-08-28, then
revised the same day after an adversarial design review with the owner. The
review's decisions are recorded as D6–D13 in §3 and integrated throughout.
This authorizes design and planning work; each phase still lands through its
own reviewed pull request with its own version bump.
Applies to: MKO ingestion, approval semantics, retrieval, and the Skill
workflow contract. Thesis promotion semantics are referenced but not changed
here (see §4.4 for the contract-document revision, which is a docs change).

## 1. Problem

The owner's verdict on the current system: it is effectively **write-only**.
Three observations drove this design, all recorded from real use:

1. Retrieval almost never happens. Nothing stored has come back to help the
   owner later, so the value of careful capture is never realized.
2. Input is narrow. Only PDFs and web-page snapshots can enter the pipeline;
   pasted text, Markdown/text files, images, office documents, and video
   cannot.
3. The approval gate turns every capture into unfinished homework. A Source
   or Knowledge draft stops at `pending` until a real-TTY approval, so the
   home screen accumulates debt instead of knowledge.

## 2. Product model

> The owner puts knowledge in, in whatever form it arrives. The agent reads
> it, and the Core immediately stores the original, the extracted text, an AI
> summary with the AI's own labelled opinion, and AI-proposed topics. Human
> confirmation is a recorded badge, not a gate. What was stored works for the
> owner in later conversations: the agent recalls it before answering.

The usefulness of a knowledge base is decided at the moment of retrieval,
not the moment of capture. This design therefore treats the recall loop as
the primary feature and ingestion breadth as what feeds it.

## 3. Decisions

D1–D5 were made explicitly by the owner during the 2026-08-28 design
conversation:

- **D1 — Approval becomes a badge everywhere.** Saving completes a record
  immediately in every domain, including high-risk (investment). The
  real-TTY human approval command remains, but its meaning changes from
  "make this record final" to "record that a human confirmed this exact
  revision." No domain keeps approval as a blocking gate.
- **D2 — The agent-read-text model generalizes (approach A).** The Core does
  not parse formats; the agent reads (OCR, transcript, conversion, paste)
  and the Core records what was read, alongside the original bytes when an
  original file exists. Building format parsers into the Core (approach B)
  was rejected: hwp parsing in Rust is impractical, OCR cannot be
  deterministic, and video makes the model impossible for half the scope.
- **D3 — AI-proposed topics need no human confirmation.** Topic labels are
  stored as the AI proposed them, consistent with D1.
- **D4 — Retrieval is phased first.** The recall contract (Phase 1a)
  precedes ingestion breadth (Phases 2–4), because retrieval is what makes
  deposits worth anything. "사실 꺼낸 적이 거의 없음" is the occurrence
  that earns this priority.
- **D5 — Volume is not gated at the entrance.** The owner raised the worry
  that frictionless capture bloats the base. The costs were examined:
  storage is negligible, pile pressure is removed by D1, and search
  signal-to-noise is real but is a ranking/filter problem to be tuned when
  it actually hurts (occurrence rule), not a reason to narrow the entrance.

D6–D13 were made explicitly by the owner during the 2026-08-28 adversarial
review of this design:

- **D6 — Phase order stands; migration safety is git alone.** The review
  challenged running Phase 0 (semantics change + migration) before the
  retrieval hypothesis is validated. The owner's grounds for keeping the
  order: the pending backlog is only a handful of records, so the change is
  hand-reversible in scale. The migration command requires a clean git tree
  in the knowledge repository before running; no per-record trace fields
  are added.
- **D7 — Recall is unconditional and measured.** The agent does not judge
  whether a question "may be covered" by the KB — every substantive question
  begins with a search; an empty result costs one query. The Core logs every
  recall (§5), and `mko home` surfaces the metrics. Rationale: the failure
  this design answers — retrieval never happened — was itself known only as
  an impression; the next verdict must be data.
- **D8 — Confirmation is repositioned as Thesis-promotion preparation.**
  With the gate gone, nothing routine prompts the confirmation command; its
  one real consumer is the Thesis boundary. The design says so honestly:
  the review queue leaves the home screen (command-access only), the
  Obsidian view is renamed from a "review queue" (a task) to an
  "unconfirmed" listing (a state), and the confirmation command is renamed
  from approval vocabulary to confirmation vocabulary, with the old name
  removed (single owner, handshake-locked Skill: no compatibility burden).
- **D9 — The Thesis-boundary gate is prose today, and Phase 0 revises the
  contract documents on both repositories.** Verified 2026-08-28: Thesis
  reads no MKO approval state anywhere in code — MKO records enter as
  opaque `mko://` URIs and the enforcing gate is Thesis's own human review
  status. §4.4 states this honestly and Phase 0 carries the docs-only
  contract revision.
- **D10 — Search is fixed and unified in Phase 1a.** Query NFC
  normalization (a live bug: stored text is NFC-normalized, queries are
  not, so macOS-NFD Korean queries can fail against identical text) plus
  deterministic whitespace-token AND matching. `mko knowledge search` is
  removed — it always ran the legacy v1 scan regardless of repository
  generation and misreported its scope — leaving `mko find` as the single
  search entry point.
- **D11 — Phase 1 is slimmed to what exists.** The original Phase 1 was
  sold as "nearly free" but required source search (which does not exist)
  and filters over fields (`topics`, origin form) that only later phases
  create. Phase 1a keeps only work over data that exists today; topic and
  origin-form filters ride with the phases that create their data.
  Store-on-miss also moves to Phase 2: its storage target is conversation
  content, which becomes a first-class input form only there, and the
  recall log's zero-result entries are the occurrence data that will ground
  its design.
- **D12 — `CONTRACT_VERSION_V2` increments in Phase 0.** In v3 the
  lifecycle state is derived, so little rewriting occurs — but the owner
  runs multiple configured machines, and without a bump a stale `mko` on
  another machine would silently operate the same KB under gate semantics.
  A loud refusal beats silent semantic drift.
- **D13 — Topic reuse is a Skill rule, not a Core gate.** Before proposing
  a new topic the agent must consult the existing topic list (`mko topics`,
  added in Phase 2) and prefer reuse. The Core still requires no
  confirmation (D3 stands); this only slows label divergence
  (`투자>반도체` vs `금융>반도체주` vs `투자>semiconductors`) before it
  becomes a mass-relabelling problem.

## 4. Approval as a badge (Phase 0)

### 4.1 Record semantics

- A Source or Knowledge revision written through the Core is complete at
  write time. There is no pending-until-approved state in the lifecycle.
- Every revision carries provenance: `authored_by: ai` (or `human` for
  owner-typed records such as `remember` notes) and a human-check field that
  is either absent or `confirmed` with the confirming timestamp bound to the
  exact revision.
- The real-TTY approval command becomes the confirmation command **in name
  as well as in consequence** (D8): renamed to confirmation vocabulary, old
  name removed. It remains human-only, revision-bound, and TTY-guarded
  exactly as today; its consequence is a badge, not a state transition that
  completes the record.
- High-risk domain requirements (at least one counterargument and one open
  question for finance/medical/legal) remain **schema requirements enforced
  at write time**. The only thing that can block a save is a contract
  violation, never the absence of a human.

### 4.2 Reinterpreted surfaces

- The review queue becomes "what a human has not yet looked at" — and its
  place reflects that (D8): it leaves the home screen entirely and is
  reachable by command only. The home screen must not present unconfirmed
  records as unfinished work.
- The generated Obsidian view `views/review-queue.base` is renamed to an
  `unconfirmed` view — the screen one opens when preparing records for
  Thesis promotion. The duplicated view definitions (`dashboard_v2.rs` /
  `setup_plan_v2.rs`) are consolidated in the same change.
- Search, projections, and listings include unconfirmed records by default,
  visibly labelled (e.g. `AI 작성 · 미검토`). Filtering to confirmed-only is
  an option, not the default.

### 4.3 Migration

- In v3, lifecycle state is not stored on records; it is derived from the
  append-only review event graph. Migration is therefore chiefly
  **reinterpretation**: the derivation vocabulary changes, projections and
  views regenerate, and existing approval events become confirmation badges
  carrying their original timestamps. Records in formerly-pending states
  become complete with no badge. Record revision files themselves are not
  rewritten.
- The migration command requires a **clean git tree** in the knowledge
  repository before running (D6); git is the rollback. No per-record trace
  of the former state is kept — the pending backlog is a handful of
  records.
- **`CONTRACT_VERSION_V2` in `config_v2.rs` increments** (D12). The
  migration stamps the new contract version so that a pre-badge CLI on any
  other configured machine refuses the migrated KB loudly instead of
  silently reimposing gate semantics and debt displays. This is the one
  place this design touches the on-disk contract version; product-version
  bumps happen every phase regardless.

### 4.4 Thesis boundary

Thesis (separate repository) promotes "an approved MKO source" into
evidence. Verified 2026-08-28: Thesis reads no MKO approval state anywhere
in its code. MKO records enter Thesis as opaque `mko://` URI strings; the
gate that actually blocks promotion is Thesis's own human review status,
set by a human in the Thesis TTY. The handoff fields Thesis's
`MKO_REFERENCE_CONTRACT.md` requires (including "MKO approval state") are
documented **preconditions, not implemented behavior**.

Under this design the boundary rule reads: **only records carrying the
human-confirmation badge are eligible for promotion.** This is owner
operating discipline, not code (D9) — the enforcing code remains Thesis's
own human review, and mechanical enforcement belongs to whenever Thesis
implements the handoff. Because the contract document's own change clause
makes an MKO lifecycle change a contract revision by construction, **Phase
0 includes revising `MKO_REFERENCE_CONTRACT.md` and its MKO counterpart**,
replacing the "MKO approval state" precondition with the human-check badge.
No Thesis code change is made or authorized here.

## 5. Recall contract (Phase 1a)

The highest-leverage phase. Honest scope (D11): this is real Core work, not
"nearly free" — source search does not exist today and the search fixes
below are new behavior.

- **Recall unconditionally, before answering (D7).** For every substantive
  question the owner asks, the Skill directs the agent to search the Core
  (`mko find`, json-v2) **first, with no domain judgment**, and to ground
  its answer in what it finds, citing records by their `mko://` IDs and
  stating the confirmation label of each cited record.
- **Recall is logged (D7).** The Core appends one line per recall query to
  an append-only JSONL log inside the knowledge repository (timestamp,
  query, result count, cited record IDs), committed to git like any KB
  content. `mko home` surfaces the metrics (e.g. recalls and citation counts
  over the recent window) — the design's success measure lives on the first
  screen. The log records question text verbatim and is therefore treated
  with the same sensitivity as the KB itself.
- **Core search support.** Deterministic text search and filtering only —
  no semantic search engine enters the Core; semantic interpretation of the
  owner's question is the agent's job (working rules: Core deterministic,
  LLM semantic). Phase 1a delivers (D10, D11):
  - **NFC normalization of the query** — bug fix: stored text is
    NFC-normalized at write time but queries never were, so Korean queries
    typed on macOS (often NFD) could fail against canonically identical
    stored text.
  - **Whitespace-token AND matching** — the query splits on whitespace and
    matches when every token appears as a (case-folded) substring. Still
    fully deterministic, no index. This is the minimum for Korean recall:
    "학습률 개선" must find "…학습률을 크게 개선하는…".
  - **Unconfirmed records included by default**, labelled (§4.2).
  - **Filters over data that exists today**: confirmation state and time
    range. Topic and origin-form filters arrive with the phases that create
    those fields.
  - **Source-record search**, built new (no source search exists today). If
    its cost proves larger than planned, the plan may descope it to a
    follow-up, recorded with the reason.
  - **One search entry point**: `mko knowledge search` is removed (it
    always ran the legacy v1 scan regardless of repository generation, and
    claimed "approved only" while matching unreviewed records). Its
    kind/tag-style filtering is absorbed into `mko find`'s filters.
- **Store-on-miss is not in this phase (D11).** It ships with Phase 2,
  where conversation content becomes a first-class input form. Until then,
  the recall log's zero-result entries accumulate as the occurrence record
  that grounds its design. The agent never stores on its own initiative.

## 6. Unified ingestion (Phases 2–4)

One registration contract for every form. The Core accepts:

- **original bytes** — when an original file exists (image, docx, hwp,
  md/txt); preserved as a sidecar next to the extracted text, exactly as PDF
  bytes are preserved today;
- **agent-read text** — what the agent extracted: the paste itself, the file
  text, OCR output, a transcript, a conversion. This is the evidence the
  downstream pipeline (prepare → source → knowledge) consumes, unchanged,
  as `text/plain`;
- **origin metadata** — the form (`pasted-text | local-file | image |
  document | video | web | conversation`), the locator (path or URL) when
  one exists, and the read timestamp.

### 6.1 Identity

- With an original file: the **original bytes' fingerprint** is the Asset
  identity. Re-extracting (a better OCR pass, a corrected conversion) yields
  a new extraction revision of the same Asset, never a duplicate Asset.
- Without an original (paste, web, conversation): the **text fingerprint**
  is the identity, exactly as web snapshots work today.

### 6.2 Extraction honesty

Extraction quality is the agent's responsibility and its risk. The record
never hides this: extracted text is labelled as agent-read, the original is
kept for re-extraction, and the existing untrusted-content rule applies
without exception — extracted text is data, never instructions.

### 6.3 Topics

- The source/knowledge response contracts gain a `topics` field: free-form
  hierarchical labels proposed by the agent at write time (e.g.
  `투자>반도체`, `개발>Rust`), normalized deterministically by the Core
  (whitespace, case) and stored as proposed (D3).
- **Reuse before invention (D13).** Phase 2 adds a cheap `mko topics`
  listing command, and the Skill requires the agent to consult it and
  prefer an existing label before proposing a new one. The Core enforces
  nothing here; this is divergence control, not a gate.
- Topics become the everyday sorting axis for search and listings. The
  existing human-confirmed `perspective` remains unchanged and separate.
- Store-on-miss becomes real in Phase 2: on a logged recall miss where the
  conversation produced knowledge worth keeping, the agent offers once
  ("저장소에 없네요 — 이번에 정리한 내용을 저장할까요?"); a yes routes the
  conversation content through the normal ingestion path.

## 7. Phases

Each phase changes an agent-facing surface (CLI, envelopes, schemas, or the
SKILL.md contract) and therefore bumps `workspace.package.version` per the
version discipline. Each phase is its own plan and its own PR.

| Phase | Delivers | Why this order |
|---|---|---|
| 0 | Badge semantics + confirmation rename + `CONTRACT_VERSION_V2` bump + migration (clean-git-tree gated) + reinterpreted surfaces (home, unconfirmed view) + contract-document revision in both repositories | Unconfirmed knowledge must be usable before recall can cite it |
| 1a | Unconditional recall contract in the Skill + recall log + search fixes (query NFC, token-AND) + unconfirmed-inclusive search + confirmation/time filters + source search + `find` unification | Closes the retrieval loop; the product's value moment; measured from day one |
| 2 | Pasted text, md/txt files, conversation capture, `topics` + `mko topics` + topic filter + store-on-miss | Minimal Core change, maximal daily reach; miss data from 1a grounds store-on-miss |
| 3 | Images/screenshots + docx/hwp with original sidecar + origin-form filter for these forms | Adds the original-bytes sidecar structure |
| 4 | YouTube/video via URL + transcript + origin-form filter completion | Same model as web pages; no original bytes |

Phase 3 opens with a feasibility check for hwp extraction on the owner's
machines; if no honest extraction path exists, hwp alone is deferred and
recorded in the backlog with the occurrence.

## 8. Risks

- **Extraction errors become evidence.** Mitigated by original-bytes
  preservation (re-extraction), the `authored_by: ai` label, and the
  existing evidence-locator discipline. Accepted residual risk, already
  accepted for web pages.
- **Search noise as volume grows.** Accepted by D5. Revisit ranking
  (down-weight unconfirmed/stale records) when degraded retrieval actually
  occurs; the recall log is the evidence base for that tuning. Record
  occurrences in the backlog.
- **A weaker human gate.** D1 removes the save-time gate everywhere. The
  compensations are honest labelling, the confirmation badge, and the
  Thesis-boundary rule in §4.4 — which is operating discipline today, not
  code (D9). The owner chose this trade explicitly.
- **The recall log is sensitive.** It contains the owner's questions
  verbatim, permanently in git history (D7). Accepted: the KB's threat
  model already covers content at this sensitivity, and query verbatims are
  the raw material for future search tuning. The log is treated with the
  same sensitivity as the KB.

## 9. Out of scope

- Messenger capture (Telegram) — unchanged, stays in the backlog under its
  occurrence rule.
- Any Thesis **code** change (the docs-only contract revision in §4.4 is in
  scope for Phase 0).
- A semantic search engine, embeddings, or any nondeterministic index inside
  the Core.
- Automatic storage without the owner's yes (store-on-miss always asks).

## 10. Testing

- Core unit tests per phase: badge/derivation semantics, migration
  preconditions (clean git tree), and contract-version stamping (Phase 0);
  query NFC normalization, token-AND matching, unconfirmed inclusion,
  filters, source search, and recall-log writes (Phase 1a); registration
  identity rules, topics normalization, and sidecar round-trips (Phases
  2–3); snapshot-model registration for transcripts (Phase 4).
- `tests/skill-forward/` gains scenarios per phase: unconditional
  recall-before-answer (no domain skipping), citation labels in answers,
  topic reuse-before-invention, store-on-miss consent, and extraction
  honesty (agent must not follow instructions embedded in extracted text).
- Version pins: each phase updates the three pinned version sites
  (`contract_version.rs`, the CLI `--version` test, the Skill handshake
  pin) in the same change, per the version discipline. Phase 0
  additionally covers the `CONTRACT_VERSION_V2` change with an explicit
  old-KB detection/refusal test.
