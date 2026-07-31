# Backlog

Ideas that came out of conversation but are not committed work. Nothing here
is scheduled. An item earns design work only when the current workflow has
actually hurt in practice — record the occurrence rather than arguing from
anticipation.

## Messenger capture into the Personal Inbox

**Idea.** Send a PDF (or link) to a messaging app such as Telegram and have
MKO register it into the Personal Inbox automatically, with the domain
perspective — investment, study, work — proposed at capture time.

**Recorded 2026-07-27.** The owner believed this was already part of the
design. `main` itself has no messenger ingest, but a prototype exists on the
unmerged branch `feature/delivery-engine-design` (`50cb7a6`, "feat: add
capture delivery and Telegram onboarding"): a `mko-telegram` crate with `mko
telegram connect/status/disconnect`, credentials held only in the OS
keychain, plus capture-envelope and delivery-package schemas. That prototype
stops at safe channel connection and status — the polling worker that would
pull Telegram messages into General/Finance Capture is explicitly deferred
there as a later milestone, so it does not perform automatic ingest yet. The
branch remains unscheduled and unmerged. The actual v0.2 ingest path on
`main` is still the Google Drive `personal/inbox` folder plus `mko inbox` /
`mko add`.

**Why it is not scheduled.**

1. The capability it targets — capturing material away from the desk —
   already works: the Google Drive share sheet on a phone saves straight into
   the configured Personal Inbox, and the next `mko inbox` run picks it up.
   No new code is required to get that benefit today.
2. A bot endpoint is reachable by anyone who knows it, so provenance would
   shift from "a file I placed" to "a file someone sent." For a Core whose
   value rests on fingerprints and source custody, that is a boundary change,
   not a convenience feature.
3. Automatic routing by category would conflict with the responsibility
   boundary: an LLM supplies meaning only, and the human approves. The
   defensible shape is a *proposed* domain perspective that a human confirms
   — which is what the existing pipeline already does.

**Revisit when.** Saving to Drive from a phone has proven annoying in real
use (record the occurrences). If built then, the first design question is
provenance — how a messenger-supplied file earns the same custody guarantees
as one placed in the Inbox directly — not the transport.

**Related.** Thesis references MKO records through `mko://` IDs only (see the
MKO reference contract in the Thesis repository). Investment material
reaching Thesis is a human promotion of an approved MKO source into evidence;
it is never automatic, so messenger capture would not change that path.

## Usability review findings — 2026-07-29

An external usability review (verified against the cited files) queued three
items. The owner resumed this work on 2026-07-31. The product decisions and
delivery phases are now specified in
`docs/superpowers/specs/2026-07-31-mko-daily-home-ux-design.md`; implementation
of Phase A and the authorized Phase B slice was completed locally on 2026-07-31. Phase C still
requires the owner-review gate in that addendum.

1. **Setup contract conflict (P1, resolved in Phase A).** The v0.3 UX design offers
   "clone an existing private GitHub KB or scaffold a new local Personal KB",
   but `skills/codex/my-knowledge-os/SKILL.md` hard-requires an absolute
   Google Drive sync root ("Stop until that path exists") and a private
   GitHub URL before the first PDF. Git should be the recommended sync
   option, not a precondition for the first summary. The Skill now completes
   local KB and Drive setup first and asks about a private Git remote only
   after an explicit user choice.
2. **Revision-request loop is not closed for the user (P1).** The Skill
   stores feedback and stops; replacement needs the current revision, and
   the queue only says "수정본 생성" without naming the regeneration flow.
   Fix direction: one guided path from feedback to replacement revision.
3. **Flaky test.** `registry_scan_limit` failed once in a full-workspace run
   and passed in isolation — likely load/timing sensitivity. Track before
   trusting full-suite green on slow machines.
