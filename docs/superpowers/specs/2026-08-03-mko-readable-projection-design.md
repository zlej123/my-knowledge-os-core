# My Knowledge OS — Readable Projection Design

Date: 2026-08-03
Status: Proposed; implementation follows owner decisions in §5
Applies to: `mko` after the 2026-08-03 usability lane (PRs #8–#13)
Responds to: usability review item 5 ("Obsidian이 아직 읽기 화면 역할을 못 합니다")
Related: the queued "Rendered-document review" proposal in `docs/BACKLOG.md` — the
same design problem approached from the other side

## 1. Problem

The projection is the only surface where an owner reads their knowledge outside
a terminal, and it contains no knowledge. This is the file generated for an
approved Source in the owner's own KB on 2026-08-03, in full:

```
---
projection_schema_version: 2
record_type: source
record_id: "personal-source-5cbe4ea6…"
title: "MKO v0.3.2 Revision Loop E2E Note (2026-08-03)"
current_revision: "sha256:4d0f4d7c…"
review_head_id: "personal-review-162a93fe…"
derived_state: approved
domain: "uncategorized"
tags: ["e2e","mko","pdf-extraction"]
record_link: "sources/…/current.yaml"
asset_link: "assets/registry/…json"
projection_digest: "sha256:1aa8cf3b…"
---

# MKO v0.3.2 Revision Loop E2E Note (2026-08-03)

- Record: [[sources/…/current.yaml]]
- Asset: [[assets/registry/….json]]
- Current revision: `sha256:4d0f4d7c…`
```

That record has a one-sentence summary, a general summary, four key claims each
with an evidence locator, and a stated limitation. None of it appears. The
`record_link` points at `current.yaml`, which holds only a revision digest, so
following it does not reach the content either — the owner has to hunt for the
revision file by hand. The Base table over these files is a usable state index
and nothing more.

## 2. Evidence: what exists and what is missing

- `ProjectionInputV2` (`rust/mko-core/src/projection_v2.rs`) carries record type,
  id, title, current revision, review head, derived state, domain, perspectives,
  tags, and two links. There is no field in which content could travel.
- `render_projection_unchecked` formats exactly those fields; the body is three
  bullet lines.
- The content the owner wants is already available at every call site: the
  Source and Knowledge revisions carry the full typed response, and callers
  (`records_v2`, `review_v2`, `queue_v2`) build the projection input while
  holding that revision.
- The review card already renders this material for the terminal, including the
  addressed feedback and the diff added in PR #8. The projection is the same
  rendering problem for a different reader.

## 3. Design

Extend the projection input with a **rendered body derived deterministically
from the exact current revision**, and render it as a document a person can
read:

1. one-sentence summary and general summary;
2. key claims, each with its evidence locator, and stated limitations;
3. for Knowledge: grounded units, LLM analysis, counterarguments, uncertainty,
   and open questions, kept visually separate from the grounded material —
   the separation is a product invariant, not a formatting preference;
4. a link that opens the original PDF (the Asset's provider locator), alongside
   the existing record and asset links;
5. review state and the next action in the owner's language, so the file says
   whether it is waiting on them.

The renderer stays a pure function of the input, the input stays a pure function
of the revision, and the digest keeps binding the whole thing. Nothing about the
"projection is a projection, not a record" line changes: the file remains
regenerable from the revision, and the revision remains the record.

## 4. Consequence that cannot be avoided

`projection_digest` is `canonical_json_sha256(input)`, and the digest is written
into the file. Adding fields changes it for **every** projection, so every
existing projection in every existing KB becomes drift the moment the new
binary runs. The machinery to handle it exists — `mko dashboard --repair`
regenerates manifest-owned files, and files the owner edited are preserved
through the existing `dashboard_user_modified` path — but the owner will meet a
one-time repair, and it must be announced rather than discovered.

This is the reason this design is queued rather than implemented directly.

## 5. Decisions for the owner

- **D1 — schema version.** Bump `projection_schema_version` 2 → 3 (recommended)
  or extend within 2. A bump makes the change legible in the files themselves
  and lets `mko dashboard` explain the repair as a format upgrade rather than as
  drift. Extending within 2 avoids touching the projection schema contract but
  leaves an owner staring at unexplained drift.
- **D2 — how much content.** Full body (recommended: the point is to stop the
  terminal being the only place knowledge is legible), or summary plus a link
  into the revision. Full body duplicates text that also lives in the revision;
  that duplication is already accepted for the review card, and the byte bound
  that guards the card applies here too.
- **D3 — relationship to the rendered-document review.** The backlog item asks
  for "자료가 들어오면 읽기 좋은 문서로 전달받아 승인" with the design line that
  the default delivery target is local (browser/Obsidian projection). If this
  design lands, the Obsidian projection *is* that local delivery surface for
  approved records, and the remaining piece of that item is delivery of *pending
  drafts* for review. Recommended: treat them as one lane and build this first,
  since a pending-draft document is the same renderer pointed at a pending
  revision.

## 6. Invariants that do not change

- The projection is generated, never authored; the revision stays the record.
- Approval stays revision-bound and real-TTY; a readable file changes what the
  owner can read, not what they can approve.
- Grounded content and LLM analysis stay visibly separate.
- Generated files stay manifest-owned, and an owner's edit is preserved rather
  than overwritten.

## 7. Implementation and migration plan

- Extend the input and renderer; update `schemas/v2/projection.schema.json` and
  the projection goldens.
- Update the three call sites that build projection inputs so each passes the
  revision content it already holds.
- Golden a Source and a Knowledge projection end to end, including the
  grounded/analysis separation and the original-document link.
- Migration test: an existing v2 projection is detected as a format upgrade, is
  regenerated by repair when manifest-owned and unmodified, and is preserved
  when the owner has edited it.
- Announce the one-time repair in the release note for the version that carries
  it.
