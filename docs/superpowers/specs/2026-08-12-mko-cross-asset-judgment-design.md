# My Knowledge OS — Judgments Across Documents

Date: 2026-08-12
Status: Design approved by the owner 2026-08-12; not implemented
Applies to: `mko` after 0.3.17
Responds to: the two recorded occurrences of the cross-asset evidence gap in
`docs/BACKLOG.md` — earned under the occurrence rule

## 1. Problem

A note can say what its document said, and it can carry what the model added.
It cannot say how its document relates to *another* document — and that
relation is the entire product of the prediction-scoring workflow the owner is
running.

The gap was hit twice, in both time directions, during the qalert analysis:

- **Backward.** The verdict on the 2024-06-27 post — Biden did not resign
  August 19; he withdrew from the candidacy on July 21 — is a claim comparing
  the prediction post to the record of what happened. The evidence model binds
  every reference to the one bundle the response was drafted from
  (`validate_evidence_refs(bundle, …)`), so the verdict entered the note as
  `counterargument`/`model_knowledge` prose: honest, but uncitable, even
  though the outcome could be snapshotted into the same knowledge base.
- **Forward.** The 2026-08-11 Korea post's 선관위-압수수색 claim is recorded
  as an `open_question`. The Korean news snapshot that eventually resolves it
  will be a registered Asset in this same knowledge base — and the resolving
  revision still could not cite it.

What must not happen is the cheap fix: letting any unit cite anything. A
`fact` that can cite a second document is no longer "what this document said",
and that distinction is the product.

## 2. Decisions

Settled with the owner on 2026-08-12.

### 2.1 A new `judgment` kind with a both-sides rule — not loose cross-refs

A fact cites one document. A judgment asserts a **relationship between two**.
Those are different epistemic positions, and the reader must be able to tell
them apart at a glance — which means the comparison gets its own kind, its own
vault section, and a mechanical rule:

> A `judgment` unit must cite at least one block from the subject document
> **and** at least one block from a supporting document.

The rule kills both degenerate forms: a judgment citing only the subject is a
`fact`; one citing only the outcome belongs on the outcome's own note.
Cross-asset references are permitted **only** on `judgment` units, so the
existing invariant — `fact`/`definition`/`formula`/`result` mean "this
document supports it" — survives unweakened.

This is the same move `background`/`model_knowledge` made: a narrow new kind
with a mechanical rule, instead of loosening an existing one.

### 2.2 v1 scope: per-post judgments, subject 1 + supporting N

A judgment note lives on the document being judged, and may cite **several**
supporting documents — one post carries several predictions, and each needs
its own outcome evidence (the 2023-07-09 post alone needs Biden, McCarthy,
and the Supreme Court answered separately).

**Account-level judgments are deliberately out.** "스톰라이더 적중률 0/9" is a
claim about no single asset, and the knowledge base's spine is asset↔record
1:1 (`knowledge_record_id_v2(asset_id)`); queue, home, projections, and
review all stand on it. A record with no subject asset is a spine redesign,
an order of magnitude larger, and the need has not hurt twice — the aggregate
picture is served today by the `판정` tag across notes, search, chat reports,
and `mko remember` for owner-authored conclusions. If the aggregate note
hurts twice for real, it earns its own design then.

### 2.3 Supporting evidence is a registered Asset — no approved record required

Approval gates the **note**, not the evidence: today's evidence references
already point at unapproved prepared-bundle text of the subject. Requiring the
outcome document to first pass its own summary-and-approval would double the
ceremony per verdict — the exact cost the 2026-08-05 approval-ceremony work
existed to remove. A registered Asset with a preparable bundle is sufficient
standing to be cited.

### 2.4 Judgments are Knowledge-only

`SourceRevisionV2` is untouched. A Source is the summary of one document;
relating documents is Knowledge work.

### 2.5 The ladder completes

`background → fact` gained a ladder in the 2026-08-06 design: an unverified
claim becomes grounded when its page is snapshotted. Judgments get the
symmetric one:

> Outcome evidence not in the knowledge base → the verdict stays a
> `counterargument` with `model_knowledge` basis, in prose.
> Snapshot the outcome → upgrade the verdict to a `judgment` with real
> citations on both sides.

Both rungs of both ladders are honest; the upgrade is always optional and
always an improvement.

## 3. Components

### 3.1 Data model (`model_v2.rs`, `records_v2.rs`)

- `EvidenceRefV2` gains an optional `asset_id`. **Absent means the subject
  bundle** — today's meaning, byte-for-byte. The field uses the
  `AssetOriginV2` compatibility pattern (`#[serde(default,
  skip_serializing_if = "Option::is_none")]`, mirrored in the wire struct):
  every existing revision parses and round-trips to its exact stored bytes.
- `KnowledgeUnitKindV2` gains `Judgment` (wire `judgment`, owner-facing
  **판정**).
- `KnowledgeRevisionV2` gains
  `supporting_evidence: Vec<SupportingEvidenceV2>` (defaulted, omitted when
  empty) where `SupportingEvidenceV2 { asset_id, evidence_basis:
  EvidenceBasisV2 }`. Each cited supporting document's bundle identity —
  `bundle_id`, `content_digest`, `asset_fingerprint`, extractor name and
  version — is pinned in the revision, exactly as the subject's already is.

Integrity follows from what exists: a snapshot's asset id *is* the hash of
its text, so a supporting citation is self-verifying, and the stored-text
integrity check (0.3.17) refuses to prepare a tampered snapshot. A PDF
supporting asset is pinned by its fingerprint the same way the subject is.

### 3.2 Validation (`records_v2.rs`)

`validate_knowledge_response` takes the subject bundle plus the supporting
bundles, keyed by asset id, and enforces:

1. A `judgment` requires `basis: evidence`, at least one reference resolving
   in the subject bundle, and at least one resolving in a supporting bundle.
   Block id and locator must match exactly, as today.
2. A reference carrying `asset_id` on any non-`judgment` unit is rejected.
3. A reference naming an asset that is not among the passed supporting
   bundles is rejected.
4. A supporting bundle no reference cites is rejected — nothing gets pinned
   into a revision as noise.
5. `model_knowledge`, `missing_evidence`, and `conflicting_evidence` remain
   restricted exactly as they are; a `judgment` carrying any of them is
   rejected.
6. The subject cannot appear as a supporting bundle, and a supporting asset
   may appear only once.

High-risk policy requirements (counterargument + open question) are
unchanged; a judgment satisfies neither.

### 3.3 Write path (`cli.rs`, `cli_v2.rs`)

`mko knowledge write` gains a repeatable `--supporting-bundle PATH`. The
agent prepares the subject and each supporting asset in-session — both
registration paths already produce preparable bundles (0.3.17) — and passes
the session files. Core reads each bundle, validates references against the
bundle their `asset_id` names, and records the pins. The envelope reports
the supporting asset ids alongside the existing fields.

### 3.4 Projection (`projection_v2.rs`)

New section **`판정 (문서 대조)`**, rendered after `문서가 뒷받침하는 내용`
and before `LLM 분석`. Each evidence line is labeled with which document it
cites — `근거(이 문서)` for the subject, `근거(<대조 문서 제목>)` for a
supporting document — and the section closes with the list of supporting
documents linked to their registry entries. Titles come from the supporting
Assets' `title_fallback`, read at write time and carried through the
projection input so the digest binds them.

### 3.5 Search (`queue_v2.rs`)

`Judgment` files under the grounded-evidence layer: it is fully evidenced,
and hiding verdicts from "what do I actually have evidence for" would defeat
the point.

### 3.6 SKILL.md

The qalert-style flow gains the ladder rule (§2.5) and the command shape:
prepare the subject, prepare each cited outcome document, write one judgment
note. The offer rule and one-revision-per-session discipline are unchanged.

## 4. Data flow

1. Owner asks for a verdict on registered material (or the agent proposes one
   during study).
2. Agent ensures the outcome evidence is in the knowledge base — snapshotting
   the page it will cite, or using an already-registered Asset.
3. Agent prepares subject and supporting assets; drafts a response where each
   verdict is a `judgment` citing blocks on both sides.
4. `mko knowledge write --bundle … --supporting-bundle …` validates every
   reference in its own bundle and writes one revision pinning every cited
   bundle's identity.
5. The note joins the review queue as one item; the owner approves once, in
   the terminal, as always.

## 5. Failure handling

| Situation | Behaviour |
|---|---|
| Judgment cites only the subject, or only supporting documents | `knowledge_grounding_invalid` — the both-sides rule is the kind's definition |
| Cross-asset reference on a non-judgment unit | `knowledge_grounding_invalid` |
| Reference names an asset with no passed bundle | `evidence_reference_invalid` |
| Passed supporting bundle nothing cites | `knowledge_grounding_invalid` |
| Block or locator mismatch in any bundle | `evidence_reference_invalid`, exactly as today |
| Supporting bundle expired or malformed | existing prepared-session errors |
| Subject passed as its own supporting bundle, or duplicates | `knowledge_grounding_invalid` |
| Older CLI reads a judgment revision | parse failure surfaces as the existing invalid-revision path; the version handshake prevents the mixed-install session in the first place |

## 6. Testing

- Both-sides rule: subject-only and supporting-only judgments rejected; the
  full shape accepted and round-tripped.
- Invariant: a `fact` carrying `asset_id` is rejected.
- Compatibility: a pre-existing revision (no `supporting_evidence`, refs
  without `asset_id`) parses and re-serializes to its exact stored bytes.
- Unused and duplicate supporting bundles rejected.
- Projection renders the 판정 section with per-document labels and the
  supporting-document list; goldens updated and diffed by eye.
- **Real-KB end to end:** the FEMA EAS announcement snapshot already sits in
  the owner's knowledge base beside the 2023-08-30 qalert post. Rewrite that
  repackaging verdict as a true `judgment` citing both documents, approve it
  in the terminal, and confirm the vault shows the double-cited verdict.
  Verification runs profile-only, `MKO_PERSONAL_PROVIDER_ROOT` removed.

## 7. Not building

- **Account-level judgment records.** §2.2; deferred under the occurrence
  rule.
- **Source-side judgments.** §2.4.
- **Automatic outcome fetching.** The agent fetches inside the session the
  owner is in, as everywhere else in this product.
- **Cross-knowledge-base references.** One repository is the universe.

## 8. Version

New unit kind, new revision field, new envelope field, new CLI flag, schema
changes, SKILL.md flow change — all agent-facing. Workspace bumps to 0.3.18
with the three pinned places and the Skill handshake pin, per `AGENTS.md`.
`CONTRACT_VERSION_V2` is unchanged: existing knowledge bases need no
migration, because absent fields mean exactly what the old bytes meant.
