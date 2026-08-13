# Unified MKO + Thesis UI Design

Date: 2026-08-12
Status: Phase 0 prototype and Phase 1 read-only application projection
authorized by the owner. This does not authorize a schema migration, data
import, approval, publication, portfolio decision, execution, or order.
Applies to: My Knowledge OS as the product, with Thesis as its investment
decision module

## 1. Product decision

MKO is the product boundary. Thesis is not a second product joined to MKO; it
is the first specialist decision module inside MKO.

The distinction that remains is not "knowledge backend versus investment
backend." It is:

- shared knowledge custody: Asset, Source, Knowledge, Thought, revisions,
  relationships, review history, and provenance;
- specialist investment judgment: Evidence relation, Claim, Thesis revision,
  Scorecard, Portfolio Decision, Execution Plan, and Outcome.

One UI and one application process are acceptable. The implementation should
remain a modular monolith: shared records are owned by MKO Core, while the
investment module owns its state machine and policies.

```text
MKO
├── Core
│   ├── Asset / Source / Knowledge / Thought
│   ├── revision / lineage / provenance
│   ├── search / relationships / projections
│   └── review sessions and human-only approval
│
└── Investment module (Thesis)
    ├── Evidence / relation judgment
    ├── Claim / Thesis revision / falsification
    ├── deterministic Scorecard
    ├── constrained Portfolio Decision
    └── Execution Plan / paper broker / audit
```

## 2. Why the UI should be unified

The owner performs one continuous task today:

```text
capture material
→ preserve a Source
→ form Knowledge
→ decide whether it matters to an investment claim
→ revise a Thesis
→ score
→ decide or explicitly do nothing
→ observe the outcome
```

The current surfaces split that task. MKO starts from a terminal and reads
through Obsidian projections; Thesis has an HTTP Approval Inbox and a guided
CLI. The split exposes implementation history to the owner and hides the
transition that matters most: an approved piece of knowledge becoming a
reviewable investment judgment.

The unified UI must therefore be organized around the owner's work, not around
repository names.

## 3. Information architecture

The primary navigation is:

1. **오늘** — every item that needs attention, across shared knowledge and the
   investment module;
2. **자료** — Assets and Sources, including processing and extraction failures;
3. **지식** — approved and deferred Knowledge and the owner's unprocessed
   Thoughts;
4. **투자** — industries, companies, Claims, Thesis revisions, falsification
   conditions, and candidate Evidence;
5. **판단** — Scorecards, Portfolio Decisions, Execution Plans, and Outcomes.

"MKO" and "Thesis" are implementation provenance labels, not primary navigation
items. Small type labels remain visible where they explain the meaning of a
card or action.

## 4. The first complete journey

The first implemented journey should be deliberately narrow:

1. Open **투자** and select one active Thesis.
2. See its Claims and falsification conditions next to the current published
   Thesis revision.
3. Search approved investment-perspective Knowledge without leaving the page.
4. Select one Knowledge revision and choose **근거 초안으로 사용**.
5. Review the proposed Claim link and relation (`supports`, `weakens`,
   `contradicts`, `irrelevant`).
6. Save a pending Evidence draft. No Thesis publication, score change, or
   portfolio action follows automatically.
7. The draft appears in **오늘** with one explicit next action.

This journey is the product proof. A dashboard that only combines counts is
not sufficient.

## 5. Record ownership

### Shared Core records

Core continues to own:

- original Asset identity and content fingerprint;
- Source and Knowledge IDs and immutable revisions;
- grounded content versus model analysis separation;
- review history, approval state, scope, and export classification;
- generated human-readable projections;
- search and relationship traversal.

### Investment module records

The investment module owns:

- an Evidence draft's link to an exact approved Source or Knowledge revision;
- the owner's relation judgment and reason;
- Claims, falsification conditions, and versioned Thesis publications;
- deterministic scoring policy and scorecards;
- portfolio policy, decisions, order proposals, approvals, and audit events.

An investment record may reference a Core record. It may never overwrite,
reclassify, approve, or publish that Core record.

## 6. Approval vocabulary

A unified inbox must not create a unified generic **승인** action. Each card
must name the judgment being requested.

| Record | Human decision | Allowed action language |
|---|---|---|
| Source / Knowledge revision | Is this an acceptable durable record? | 지식으로 승인 / 수정 요청 / 나중에 |
| Evidence draft | Is the fact usable, and how does it affect the Claim? | 관계 확정 / 수정 / 제외 |
| Thesis revision | Is this the official current investment view? | Thesis 발행 / 수정 요청 |
| Score adjustment | Does the owner override a factor judgment? | 점수 조정 및 이유 기록 |
| Portfolio Decision | Is this allocation, including all-cash, accepted? | 판단 확정 / 거절 |
| Execution Plan | May these exact proposed paper orders execute? | 실행 승인 / 거절 |

Batch approval remains absent. A card may be compact, but the cost of a human
decision must still scale with the number of items worth reading.

## 7. Unified attention model

The **오늘** screen is a projection over module-owned states. It does not own a
new workflow state.

Each attention item contains:

- record type and domain;
- why it needs attention;
- current immutable revision or policy version;
- grounded summary and material uncertainty;
- exactly one recommended next action;
- recovery action when blocked or interrupted;
- age and source provenance.

Sort order is risk and dependency aware:

1. blocked work with a safe recovery action;
2. execution and portfolio decisions waiting on the owner;
3. Thesis publication and Evidence relation judgments;
4. Source and Knowledge review;
5. low-risk reading and resurfacing.

## 8. Safety and privacy invariants

- Source content remains untrusted input and is never interpreted as an
  instruction.
- Generated UI content never becomes a record by being rendered.
- Every mutation is routed to the module that owns the record.
- Every judgment binds to the exact revision displayed.
- A stale Core revision blocks Evidence promotion until the owner opens the
  current revision.
- Private or non-exportable Core text is not copied into public output.
- Investment relation, score, portfolio, publication, and execution are
  separate decisions.
- No action on the **오늘** screen silently advances a later stage.

## 9. Architecture direction

The target is a single local-first application with internal module ports:

```text
Unified UI
    |
    v
Application layer
    |-------------------------|
    v                         v
MKO Core port          Investment module port
    |                         |
    v                         v
shared record store     investment aggregates
```

This diagram is a responsibility map, not a service topology. The ports may be
Rust traits inside one process. Separate databases, HTTP calls, and duplicate
domain types are not required.

Execution credentials may still earn a later process boundary because their
blast radius differs from reading and judging knowledge. That is a security
boundary, not a reason to split the product.

## 10. Delivery sequence

### Phase 0 — interaction prototype (authorized here)

- static, local, fake data only;
- **오늘**, **지식**, **투자**, and **판단** navigation;
- investment workspace with Knowledge candidate selection;
- explicit simulated Evidence-draft confirmation;
- no backend, schema, approval, publication, or execution changes.

### Phase 1 — read-only application projection

- render current Core home/queue/search state;
- render current investment publications and pending decisions;
- preserve module-specific type and next-action vocabulary;
- deep-link to existing owner-controlled review flows.

Implemented as `mko ui`: one loopback-only application surface, direct Core
reads, and a transitional loopback-only Thesis read adapter. The browser never
receives `THESIS_API_TOKEN`, and the server exposes no mutation route. A Thesis
connection failure degrades only the investment module; Core status and search
remain usable. Existing owner-controlled CLI flows remain the only place for
review, approval, publication, portfolio decisions, and execution.

### Phase 2 — internal promotion contract

- replace the current opaque manual `mko://` handoff with an internal typed
  reference to exact record ID, record type, content revision, approval event,
  content fingerprint, scope, internal investment-use state, and public export
  state;
- create a pending Evidence draft only;
- stale, missing, or `investment_use_state=blocked` references fail closed;
- a private record may be referenced internally without copying its text when
  `investment_use_state=allowed-private-reference`, while its
  `public_export_state=blocked` continues to prevent public output.

### Phase 3 — Core-owned UI review session

- expose a UI review ceremony that retains revision binding and real human
  input;
- do not bypass Core by writing review records from the UI;
- retain separate investment publication, portfolio, and execution decisions.

### Phase 4 — outcome and reflection loop

- connect decisions to observed outcomes;
- resurface disproven assumptions and stale Claims;
- create Reflection as a new reviewed record rather than rewriting history.

## 11. Prototype success criteria

The owner should be able to answer all of these without explanation:

1. What needs my attention today?
2. Is this a knowledge decision or an investment decision?
3. Which exact Source or Knowledge revision supports this Evidence draft?
4. What Claim will it affect, and in which direction?
5. What happens if I confirm this action?
6. What definitely does not happen yet?
7. Where do I go next after confirming or cancelling?

If the prototype does not make those answers obvious, backend integration
should not begin.
