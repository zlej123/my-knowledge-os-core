# Frictionless Knowledge Design ("넣으면 저장된다")

Date: 2026-08-28
Status: Design approved by the owner in conversation on 2026-08-28. This
authorizes design and planning work; each phase still lands through its own
reviewed pull request with its own version bump.
Applies to: MKO ingestion, approval semantics, retrieval, and the Skill
workflow contract. Thesis promotion semantics are referenced but not changed
here.

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

These were made explicitly by the owner during the 2026-08-28 design
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
- **D4 — Retrieval is phased first.** The recall contract (Phase 1) precedes
  ingestion breadth (Phases 2–4), because retrieval is what makes deposits
  worth anything. "사실 꺼낸 적이 거의 없음" is the occurrence that earns
  this priority.
- **D5 — Volume is not gated at the entrance.** The owner raised the worry
  that frictionless capture bloats the base. The costs were examined:
  storage is negligible, pile pressure is removed by D1, and search
  signal-to-noise is real but is a ranking/filter problem to be tuned when
  it actually hurts (occurrence rule), not a reason to narrow the entrance.

## 4. Approval as a badge (Phase 0)

### 4.1 Record semantics

- A Source or Knowledge revision written through the Core is complete at
  write time. There is no pending-until-approved state in the lifecycle.
- Every revision carries provenance: `authored_by: ai` (or `human` for
  owner-typed records such as `remember` notes) and a human-check field that
  is either absent or `confirmed` with the confirming timestamp bound to the
  exact revision.
- The real-TTY approval command becomes the confirmation command. It remains
  human-only, revision-bound, and TTY-guarded exactly as today; only its
  consequence changes (a badge, not a state transition that completes the
  record).
- High-risk domain requirements (at least one counterargument and one open
  question for finance/medical/legal) remain **schema requirements enforced
  at write time**. The only thing that can block a save is a contract
  violation, never the absence of a human.

### 4.2 Reinterpreted surfaces

- The review queue becomes "what a human has not yet looked at" — an
  optional reading list, not a debt ledger. The home screen must not present
  unconfirmed records as unfinished work.
- Search, projections, and listings include unconfirmed records by default,
  visibly labelled (e.g. `AI 작성 · 미검토`). Filtering to confirmed-only is
  an option, not the default.

### 4.3 Migration

- Existing records in `pending` states are migrated to complete with no
  confirmation badge. Existing approved records keep their approval as a
  confirmation badge with its original timestamp.
- This changes the on-disk KB contract, so the migration is designed against
  `CONTRACT_VERSION_V2` in `config_v2.rs` explicitly: the plan for Phase 0
  must state whether the version increments and how an old KB is detected
  and migrated. This is the one place this design touches the on-disk
  contract version; product-version bumps happen every phase regardless.

### 4.4 Thesis boundary

Thesis (separate repository) promotes "an approved MKO source" into
evidence. Under this design that contract reads: **only records carrying the
human-confirmation badge are eligible for promotion.** The gate the owner
valued lives at the Thesis boundary, where money is involved, instead of in
front of every save. No Thesis-side change is made or authorized here.

## 5. Recall contract (Phase 1)

The cheapest, highest-leverage phase: it closes the loop with almost no Core
change.

- **Recall before answering.** When the owner asks the agent a question in a
  domain the KB may cover, the Skill directs the agent to search the Core
  (`mko find` / `mko knowledge search`, json-v2) before answering, and to
  ground its answer in what it finds, citing records by their `mko://` IDs
  and stating the confirmation label of each cited record.
- **Store on miss.** When the KB has nothing relevant and the conversation
  produced knowledge worth keeping, the agent offers once: "저장소에 없네요
  — 이번에 정리한 내용을 저장할까요?" A yes routes the conversation content
  through the normal ingestion path (Phase 2 makes this a first-class input
  form). The agent never stores on its own initiative.
- **Core support.** Search gains the filters retrieval needs: by topic, by
  origin form, by confirmation state, by time range. Deterministic text
  search and filtering only — no semantic search engine enters the Core;
  semantic interpretation of the owner's question is the agent's job
  (working rules: Core deterministic, LLM semantic).

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
- Topics become the everyday sorting axis for search and listings. The
  existing human-confirmed `perspective` remains unchanged and separate.

## 7. Phases

Each phase changes an agent-facing surface (CLI, envelopes, schemas, or the
SKILL.md contract) and therefore bumps `workspace.package.version` per the
version discipline. Each phase is its own plan and its own PR.

| Phase | Delivers | Why this order |
|---|---|---|
| 0 | Approval badge semantics + migration + reinterpreted queue/home | Unconfirmed knowledge must be usable before recall can cite it |
| 1 | Recall contract in the Skill + search filters in Core | Closes the retrieval loop; nearly free; the product's value moment |
| 2 | Pasted text, md/txt files, conversation capture, `topics` | Minimal Core change, maximal daily reach; store-on-miss becomes real |
| 3 | Images/screenshots + docx/hwp with original sidecar | Adds the original-bytes sidecar structure |
| 4 | YouTube/video via URL + transcript | Same model as web pages; no original bytes |

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
  occurs; record occurrences in the backlog.
- **A weaker human gate.** D1 removes the save-time gate everywhere. The
  compensations are honest labelling, the confirmation badge, and the
  Thesis-boundary rule in §4.4. The owner chose this trade explicitly.

## 9. Out of scope

- Messenger capture (Telegram) — unchanged, stays in the backlog under its
  occurrence rule.
- Any Thesis repository change.
- A semantic search engine, embeddings, or any nondeterministic index inside
  the Core.
- Automatic storage without the owner's yes (store-on-miss always asks).

## 10. Testing

- Core unit tests per phase: badge/migration state machine (Phase 0),
  search filters (Phase 1), registration identity rules and sidecar
  round-trips (Phases 2–3), snapshot-model registration for transcripts
  (Phase 4).
- `tests/skill-forward/` gains scenarios per phase: recall-before-answer,
  store-on-miss consent, extraction honesty (agent must not follow
  instructions embedded in extracted text), and label display in answers.
- Version pins: each phase updates the three pinned version sites
  (`contract_version.rs`, the CLI `--version` test, the Skill handshake
  pin) in the same change, per the version discipline.
