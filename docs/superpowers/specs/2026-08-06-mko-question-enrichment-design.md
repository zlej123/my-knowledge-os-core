# My Knowledge OS — Studying by Asking

Date: 2026-08-06
Status: Design approved by the owner 2026-08-06; not implemented
Applies to: `mko` after 0.3.12
Responds to: the owner's request to hand in a PDF or a link, get a summary, and
then ask questions that make the knowledge note richer

## 1. Problem

MKO can turn a document into an approved note. It cannot help the owner
*understand* the document, and understanding is the point — the note exists to
be studied, not filed.

The gap is concrete. Every claim in a note today must cite the document, because
Core refuses a `fact`, `definition`, `formula`, or `result` whose evidence list
is empty (`records_v2.rs`, `knowledge_grounding_invalid`). That rule is what
makes an approved note trustworthy. It also means the questions most worth
asking cannot be answered into the note at all:

- *"왜 이렇게 설계했지?"* — reasoning the datasheet does not state
- *"다른 칩은 어떻게 하나?"* — comparison the document has no reason to contain
- *"이거 통상적인 값인가?"* — the judgement that makes a number meaningful

A document is also not automatically right. A datasheet can be stale, can
contradict itself, and can be wrong. A note that can only ever repeat the
document inherits every one of those errors with no way to mark them.

So the feature is not a chat bolted onto a filing cabinet. It is: **let the
owner ask, let the answer reach the note, and keep the note honest about where
each sentence came from.**

## 2. Decisions

Six decisions were settled with the owner on 2026-08-06. Each is recorded with
the reasoning, because the reasoning is what a later change has to argue with.

### 2.1 Model knowledge is a distinct, marked kind — never a `fact`

The owner's position was that mixing in the model's general knowledge is
acceptable, since the PDF can be wrong too. That is right about trust and
incomplete about recovery.

A wrong document claim is **recoverable**: the note carries a locator, the owner
opens page 9, and sees the error. A wrong model claim recorded as a plain `fact`
is **unrecoverable**: six months later nothing distinguishes it from the
document's own words. The failure mode is not a wrong sentence, it is an
unattributable one, and it is silent and permanent.

Banning model knowledge outright would make the feature useless, because the
questions listed in §1 are exactly the unanswerable-from-the-document ones. So
model knowledge is admitted as a first-class unit that is *marked*, rather than
either refused or laundered into `fact`.

### 2.2 The conversation is free; the record changes only when the owner says so

Proposing a revision per answer would put five review items in the queue for
five questions. Reducing exactly that cost is what 0.3.8 and 0.3.9 were for, and
Thesis records the same failure in their friction log: 8+ decision points per
cycle turned "the human judges" into "the human fills out forms".

Most questions are also disposable. "이게 무슨 뜻이야?" is worth asking and not
worth keeping.

The cost of batching is that a good answer disappears if the owner does not say
"넣자". The owner asked for the agent to **offer** — so the shape is: converse
freely, the agent proposes *"이건 남길 만합니다, 넣을까요?"*, and one revision
carries everything accepted. Approval stays one TTY decision per record, however
many questions were asked.

### 2.3 Answering may use the web

Chosen by the owner over a document-and-model-weights-only alternative. The
alternative was recommended on channel-count grounds — untrusted input today is
exactly one thing, the registered document, and SKILL.md is built around that
single channel. The owner chose reach over channel minimalism. §2.4 is what
makes that safe to record.

### 2.4 What the model reads on the web is snapshotted into the knowledge base

Recording only a URL and a timestamp would produce a third category —
model knowledge wearing a citation. A dead link or an edited page six months
later leaves the claim unverifiable, which collapses the §2.1 distinction that
the whole design rests on.

Snapshotting the extracted text as a content-addressed artifact means a
web-sourced claim can be a real `fact`, because its evidence is in the knowledge
base and can be read again. Link ingestion — which the owner asked for in the
same breath as Q&A — then costs nothing extra: it is the same pipeline as a PDF.

The honest cost: the evidence is "the page as read on 2026-08-06", not the live
page, and the note says so.

### 2.5 The conversation happens in an agent session, not a web page

The owner's original request was a local web page. A page would need a model
behind it — a chat client, streaming, a key on the machine. The agent session
*is* that, already. Reading already has a home: the Obsidian vault, which
0.3.9 made carry title, summary, and body. (Thesis builds HTML because they have
no vault.)

The owner's concern was that a new session might behave differently each time.
Split in two:

- **The record cannot drift.** Core is a deterministic binary; a version-mismatched
  session is *blocked*, not subtly different (`skill_version_mismatch`); the
  grounding rule is pinned by `source_and_knowledge_mechanical_grounding_rules_are_core_enforced`
  and runs on every merge across three platforms; revisions are content-addressed
  and approval binds to an id.
- **The conversation does drift**, in tone and thoroughness — and a web page would
  not fix it, because the variance comes from the model, not the container.

What reduces the felt drift is design, not UI: every session opens with the same
three commands (handshake → `queue --pending-drafts` → `queue`); the suggestion
trigger is written as a rule rather than judgement; and the proposal's shape is
fixed by the schema.

### 2.6 Questions are logged; answers are not

Measured on the owner's real knowledge base: 1.6 MB total, of which the durable
records for one textbook are ~92 KB and 1.4 MB is a prepared-session bundle that
expires. A full transcript — 50–100 KB for a twenty-exchange session — would
rival every record in the base. The web snapshots of §2.4 are a larger and
permanent cost, and they earn it by being re-readable.

An answer ends in one of two states: it became a unit, and is kept properly with
its provenance; or it was disposable, by the owner's own decision. Neither needs
a transcript.

The question is the part that is **not** recoverable from anything else. "이 칩의
클럭 도메인을 세 번 물어봤다" is a record of what the owner was trying to
understand, and no note contains it. At roughly 100 bytes per question, that is
1/50 the cost of the transcript.

## 3. Components

### 3.1 Web snapshot as an immutable Asset (Core)

Registration path: fetch → extract text → store content-addressed inside the
knowledge base → register an Asset.

**The agent fetches; Core receives the text.** The workspace has no network
dependency and pins every crate exactly; the Core is offline and deterministic
by construction. Putting an HTTP client inside it would pull a large transitive
tree, make `mko` a tool that egresses, and force Core's tests to mock a network.
The agent already holds web tools, and this is the same boundary the semantic
path already uses: the agent produces, Core validates and records.

Core therefore takes extracted text plus metadata and does the deterministic
part — hash, store, register. Every property below is unaffected, because they
all follow from the content hash rather than from who performed the request.

The honest cost: Core cannot verify that the text came from the URL claimed. An
agent could lie — as it could about any semantic content, which is why grounding
rules and human approval exist. What must hold is that a note's claim is
checkable against the text actually recorded, and that is unchanged.

**Identity is the hash of the extracted text, not the URL.** The URL, fetch
time, HTTP status, and content type are metadata. Consequences, all wanted:

- fetching an unchanged page twice yields one Asset;
- a page that changed yields a *new* Asset, because it is different evidence;
- an old note's evidence still resolves after the live page changes or dies.

Everything downstream is unchanged: prepared content, `evidence_refs`, units,
revisions, projections. A web-sourced claim is an ordinary grounded unit.

Failure — JavaScript-rendered, paywalled, empty, oversized — routes through the
preparation-attempt record from the 2026-08-04 design, with one new reason. It
then appears in `mko queue --pending-drafts` with a `next_action`, like any
other stalled material.

Link ingestion is this path with no question attached.

### 3.2 The `background` unit kind and `model_knowledge` basis (Core)

New unit kind `background`, new basis `model_knowledge`. Owner-facing name:
**배경지식**.

Reusing `uncertainty` or `counterargument` was rejected: *"통상 이런 칩은 이렇게
한다"* is neither uncertain nor a counterargument. It is background, and calling
it that is the honest option.

Validation, alongside the existing rules:

- `model_knowledge` is permitted only on `background` and `counterargument`;
- `background` carries no evidence — a claim that has evidence is a `fact`.

This creates a ladder the agent can walk: an unverified statement is
**배경지식**; if it matters, search, snapshot it (§3.1), and it becomes a
`fact`. *"이건 배경 지식인데, 확인해볼까요?"* is a natural next action rather
than a special case.

### 3.3 Accumulated proposal (SKILL.md, no new Core surface)

The revision loop and `--expected-revision` binding already exist. The agent
holds accepted units in-session and submits one draft. Core needs nothing new.

What is new is written in SKILL.md as a rule, not a judgement call:

> Offer to keep a claim when it is not in the document, does not overlap the
> record's current units (which Core shows), and stands as one sentence.

### 3.4 Question log (Core)

Append-only, content-addressed — the pattern of review events and preparation
attempts.

Attached to the **Asset**. `knowledge_record_id_v2(asset_id)` derives the record
id from the asset id, so the spine is 1:1; attaching to the asset is equivalent
to attaching to the record, and additionally survives record supersession and
exists before any record does.

Stored per entry: question text, timestamp, and whether it became a unit. Read
back when the material is opened next:

```
이 문서에 대해 지난번 물어보신 것
  · ADC 샘플링 레이트가 왜 이 값인지
  · 클럭 도메인 분리 이유  (→ 노트에 반영됨)
```

A question worth keeping in the note itself already has a home: the existing
`open_question` unit kind, which goes through approval like anything else. The
log is for continuity; `open_question` is for knowledge.

## 4. Data flow

1. Owner asks a question about registered material.
2. Agent answers from the prepared document, from model knowledge, or by
   searching. **A page is snapshotted when it is cited, not when it is read** —
   a search that returns ten results and informs one sentence produces at most
   the snapshots that sentence cites. Snapshotting every result would spend the
   storage §2.6 was careful about, on pages nothing refers to.
3. Question is appended to the log (§3.4).
4. If the answer meets the §3.3 rule, the agent offers to keep it.
5. On acceptance, the unit joins an in-session set: grounded → `fact` etc. with
   `evidence_refs`; ungrounded → `background` with `model_knowledge`.
6. At the end, one revision proposal carries the set, bound to the revision the
   session started from.
7. It joins the review queue and is approved by the existing TTY decision.

## 5. Failure handling

| Situation | Behaviour |
|---|---|
| Fetch fails or page unreadable | The agent reports the failure to Core, which records an attempt with a new reason; surfaces in `queue --pending-drafts` with `next_action`. Core never decides a fetch failed — it records what it was told, exactly as it records a PDF that would not parse |
| Snapshot exceeds the size limit | Refused and recorded as an attempt, not silently truncated |
| `model_knowledge` proposed on a grounded kind | Rejected by Core; the new rule is narrower than the existing grounding rule, so a bypass would already have failed |
| Record changed since the session started | Existing stale-revision rejection; the session re-reads and re-proposes |
| Web content contains instructions | Same rule as documents: content is data, never instruction. SKILL.md's existing wording covers snapshots without change |

## 6. Testing

- Snapshot identity is the content hash: two fetches of identical content
  produce one Asset; a changed page produces a new one and the old evidence
  still resolves.
- An unreadable page produces an attempt record with the correct reason and
  `next_action`, and appears in `queue --pending-drafts`.
- `model_knowledge` on `fact` is refused; `background` with evidence is refused.
- A `background` unit renders distinguishably in the Obsidian projection.
- The question log is append-only and survives a revision of the record.
- A session with several accepted units produces exactly one review item.

Verification runs on a knowledge base configured by machine profile with no
`MKO_PERSONAL_PROVIDER_ROOT` set — the 2026-08-06 rule from `docs/BACKLOG.md`.

## 7. Not building

- **A web UI.** §2.5.
- **Unattended fetching or drafting.** The agent acts inside a session the owner
  is in. There is no scheduler and no key at rest.
- **Answer transcripts.** §2.6.
- **Batch approval.** Unchanged from the 2026-08-05 resolution: an "approve all"
  control becomes the default and evidence weakening the owner's position passes
  uncritically.

## 8. Version

New unit kind, new basis, new asset registration path, new question-log surface,
and a changed SKILL.md workflow are all agent-facing. `workspace.package.version`
bumps, with the three pinned places and the Skill handshake pin, per `AGENTS.md`.

## 9. Sequencing

This is four components, not one change, and they are separable. In dependency
order, each landing on its own:

1. **§3.1 web snapshot.** The largest piece, and the only one that is useful
   alone: it delivers link ingestion — "이 링크 정리해줘" — with no Q&A at all.
2. **§3.2 `background` / `model_knowledge`.** Small, and independent of §3.1.
   After it, an answer can reach the note.
3. **§3.4 question log.** Small and independent of both.
4. **§3.3 SKILL.md flow.** Last, because it ties the others together and its
   rule references the units the earlier steps introduce.

A reader deciding what to build first should start at 1 or 2 depending on
whether links or questions are the more pressing want.

## 10. Open for implementation

These are deliberately unsettled here and are decided when the code is written,
because the answer depends on what the implementation finds:

- The wire names for the new kind and basis (`background` / `model_knowledge`
  are the working names).
- Where snapshots live on disk, and whether they share the `prepared` layout.
- The snapshot size limit, and whether Core enforces it on the text it receives
  (it can, and should — the limit is about what the knowledge base stores).
- Whether the question log is exposed as its own command or folded into the
  existing `show` envelope.
