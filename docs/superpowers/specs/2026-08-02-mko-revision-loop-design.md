# My Knowledge OS — Revision Loop Completion Design

Date: 2026-08-02
Status: Proposed; implementation follows owner review of the decision points in §6
Applies to: `mko` after 0.3.1 (PR #7: version handshake, embedded schema surface)
Extends: `2026-07-22-v0.3-knowledge-ux-design.md`
Responds to: usability finding 2 in `docs/BACKLOG.md` ("Revision-request loop is not closed
for the user"); external-review priority 3 (request_changes → new draft → diff → re-review)

## 1. Problem

When the owner requests changes on a pending Source or Knowledge draft, the loop dead-ends:

- the Skill stores feedback through `mko review-feedback` and stops;
- the queue then shows the item as `수정 요청` with next action `수정본 생성`, but no surface,
  Skill section, or document names the flow that produces that replacement;
- after a replacement is written, the review card loses the feedback text that prompted it
  (§2, gap G1), so the owner cannot check the new draft against what they asked for.

The rendered-document review idea in `docs/BACKLOG.md` explicitly depends on this loop being
closed: a delivered document invites "fix this here", which currently has nowhere to go.

## 2. Evidence: what the Core already provides

The record layer already implements the version-succession shape this loop needs (the same
shape as the Thesis precedent: immutable records, a new version supersedes the old, history
stays queryable). Verified against `main` at 0.3.1:

- Revisions are immutable files under `revisions/{digest}.md`; a replacement writes a new
  revision and compare-and-swaps `current.yaml`, so prior revisions are retained
  (`records_v2.rs`, `write_semantic_record_v2`).
- Both writers accept the binding flag: `mko source write-draft --expected-revision` and
  `mko knowledge write --expected-revision`. Replacing without it fails
  `replacement_revision_required`; a stale binding fails `record_revision_stale`. Outcome
  `replaced` is already typed in the json-v2 envelope.
- `request_changes` requires non-empty feedback (`review_v2.rs`, `validate_feedback`), and the
  canonical card renders it ("Current feedback for …") together with the full previous
  reviewed content when one exists (`queue_v2.rs`, `render_card`).
- State derivation already closes the cycle: a replacement of a `changes_requested` revision
  derives `revised_unreviewed` (unreviewed + `previous_reviewed_revision` present,
  `review_v2.rs`/`queue_v2.rs`), and TTY approval applies to whatever revision the card
  displays. `mko source prepare` is re-runnable after runtime cleanup (`created`/`existing`).

The gaps are all at the seams, not in the record model:

- **G1 — feedback vanishes on re-review.** `current_feedback` derives only from a review head
  targeting the *current* revision (`review_v2.rs`, `review_history_from_graph`). After a
  replacement, the new revision has no review events, so the card for `revised_unreviewed`
  shows the previous content but not the feedback the owner gave on it.
- **G2 — no typed regeneration context.** The Skill is forbidden to parse card prose, but
  feedback and the asset ID exist only there; `show --format json-v2` data carries neither,
  and nothing tells the agent which bundle to re-prepare.
- **G3 — no deterministic diff.** The re-review card shows two full JSON sections; nothing
  renders what actually changed between the reviewed revision and its replacement.
- **G4 — no named flow.** SKILL.md has no regeneration section; the queue's `Regenerate`
  next action is undefined for agents, and the human path is undocumented.

## 3. Design

### 3.1 Typed regeneration context: extend `mko show --format json-v2`

Add to the existing show envelope (no new command):

- item-level `asset_id` (already computed for the card);
- per-target `current_feedback: string | null` — the feedback on the head targeting the
  displayed revision (non-null exactly in `changes_requested`);
- per-target `addressed_feedback: string | null` — the feedback on the head targeting
  `previous_reviewed_revision` (non-null in `revised_unreviewed`; closes G1 for machines);
- per-target `previous_reviewed_revision: string | null`.

Regeneration then needs no new read surface: `queue` finds the item, `show` supplies the
feedback, the revision to bind, and the asset to re-prepare. Alternative considered: a
separate `mko revise open` command mirroring `review-open`. Rejected for now — the context is
read-shaped, `show` is already the review read surface, and a second display-bound session
type would add machinery without adding safety (decision point D1).

### 3.2 Re-review card: addressed feedback + deterministic diff

For `revised_unreviewed` targets the canonical card gains two sections, both deterministic
projections of immutable inputs (never stored as records — the rendered-document line 1
holds):

- **"Feedback addressed by this revision"** — the feedback text from the previous reviewed
  revision's head (closes G1 for humans);
- **"Changes since the reviewed revision"** — a Core-rendered unified diff between the
  pretty-printed semantic JSON of `previous_reviewed_revision` and the current revision,
  line-based, bounded by the existing card byte limit with an explicit truncation marker.
  Diffing the semantic JSON (not the projected Markdown) keeps the diff exactly over what the
  owner approves.

The card digest continues to bind review sessions to the exact rendered bytes, so a diff
change is a card change and re-review sessions stay display-bound.

### 3.3 Skill flow: "Regeneration after requested changes" (new SKILL.md section)

1. Entry: the user asks to apply their requested changes, or picks a `수정 요청` queue item.
2. `mko show "STABLE_ID" --format json-v2` → read `current_feedback`, `displayed_revision`,
   `asset_id`. If `current_feedback` is null, stop: there is nothing to regenerate against.
3. `mko source prepare --asset-id … --format json-v2` → fresh `bundle_path` (hydration
   confirmations handled exactly as in first registration).
4. Author the replacement response. Trust model: the owner's feedback is trusted *direction*
   (what to emphasize, restructure, correct); the prepared bundle remains the only source of
   evidence; document content stays untrusted data; grounding and schema rules are unchanged
   (`mko schema show …` serves the contract). Feedback can remove or reframe claims, but every
   surviving claim still needs bundle evidence.
5. Write bound to the reviewed revision: `mko source write-draft … --expected-revision
   "DISPLAYED_REVISION" --format json-v2` (or `mko knowledge write …`). Expect outcome
   `replaced`. On `record_revision_stale` or `replacement_revision_required`, re-run `show`
   and reconcile; never retry blindly and never write without the binding.
6. Report: what the feedback asked, what changed, and that the item is now `수정 후 미검토`
   pending human review. Exactly one replacement per explicit request; approval remains a
   real-TTY act (`mko review` shows the addressed feedback and the diff).

The human path needs no new commands: bare `mko` → `검토` already routes to the same card.

### 3.4 Failure mapping

`record_revision_stale` and `replacement_revision_required` currently map to
`next_action: none`; both become `review` (re-read the card/context before acting). No new
error codes are needed.

### 3.5 Version discipline

The show-envelope fields, card sections, and SKILL.md flow are machine-surface changes:
one implementation PR, workspace version 0.3.1 → 0.3.2, machine-output schema updated in
lockstep, Skill handshake pin moves with it (per AGENTS.md "Version discipline").

## 4. Invariants that do not change

- Approval is revision-bound and real-TTY only; regeneration never approves, and
  `revised_unreviewed` still requires the owner.
- Records and revisions stay immutable; the diff and feedback sections are projections.
- The agent never copies feedback text into record fields; feedback lives in review records.
- One bounded write per explicit request; no automatic retry, recovery, Git, or promotion.

## 5. Tests, and the path to the recorded E2E (priority 4)

- Core: card golden for a `revised_unreviewed` item (addressed feedback + diff, truncation
  bound); `review_history_from_graph` addressed-feedback derivation; show-data round-trip
  with the new nullable fields (required-nullable, like `next_cursor`).
- CLI: regeneration path test — request_changes → write with stale binding (typed failure) →
  write with correct binding → `replaced` + `revised_unreviewed` in queue → card contains
  both new sections.
- Skill surfaces: adapter tests pin the new SKILL.md section and the exact write commands;
  scenario/rubric docs gain a regeneration scenario (future-blind, like the batch scenarios).
- With this loop closed, the recorded live E2E (clean install → PDF → Source/Knowledge →
  request changes → regenerate → TTY approve → search) exercises every seam; it becomes the
  acceptance gate for the implementation PR, not a separate later phase.

## 6. Decision points for the owner

- **D1 — context surface.** Extend `show` (recommended, §3.1) vs. a separate `mko revise
  open` session command. Extending `show` is smaller and read-only; a session command would
  only pay off if regeneration later needs single-use binding like approval does.
- **D2 — diff representation.** Unified diff over pretty-printed semantic JSON (recommended)
  vs. only the two full sections (status quo) vs. diff over projected Markdown. JSON diff is
  exact but more technical; Markdown diff reads better but diverges from what approval binds.
- **D3 — scope guard.** Recommended: a replacement may change content only — perspective
  confirmation and domain policy stay separate real-TTY flows even if feedback mentions them
  (the agent reports the request instead of acting on it).
