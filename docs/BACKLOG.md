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

**2026-08-02.** The review's synchronization priority is closed in two halves: the
installed CLI and Skill were re-synced to current `main` (done outside this repo),
and `mko handshake` (9d41079) now makes a stale half impossible to miss — the Skill
pins its exact Core version and any mismatch answers a typed
`skill_version_mismatch` failure with `next_action: reinstall`. Skill
self-containment followed (8a3a6b3): `mko schema list/show` serves the
source-response, knowledge-response, and new review-feedback-input contracts plus
minimal valid examples from the installed binary, so the Skill no longer references
repository schema paths that do not exist on user machines. Item 2 below (the
revision-request loop) remains open and still gates the rendered-document review
idea in the next section.

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
3. **Flaky test (resolved 2026-08-03).** `registry_scan_limit` failed once in a
   full-workspace run and passed in isolation. Confirmed on 2026-08-03: the same
   commit failed a Windows CI job and passed on re-run, twice, in two different
   acceptance tests. Cause: acquiring a publication lock gives each quarantine
   scan a slice of the one-second acquire budget, and when that slice expired
   the raw scan failure escaped as the caller's answer — even though the lock
   file was plainly present and the acquire budget still had room. A scan that
   ran out of time now says so distinctly and the acquire loop retries within
   its budget, so a held lock is reported as `registry_locked` whatever the
   machine speed. The work limit (entry count) still reports a directory that
   genuinely needs attention.

## Owner usability review — 2026-08-03 (live 0.3.2 run)

The owner ran the first recorded live journey on the installed 0.3.2 build
(`feat/revision-loop`, gates green) and reviewed the product as a daily tool.
**Verdict: strong safety structure, not yet a daily product — an alpha a
technical user verifies.** The core problem is not missing features but
*recovery after interruption and connection to the next action*. Every item
below was reproduced on a real KB with real documents, and each cited claim
was verified against the code.

**P1 — cancelling the approval screen locks the whole KB.** `mko` → 검토 계속
→ Ctrl-C leaves `.knowledge-os/runtime/locks/repository-mutation.lock` behind;
afterwards even reads (`mko queue`, 새 자료 정리) fail with
`repository_lock_held`. Cause: `publish_tty_approval_review_v2`
(`rust/mko-core/src/review_v2.rs`) takes the repository mutation lock *before*
rendering the card and waiting for input, so any cancel strands it. Two
aggravating factors: `repository_lock_held` is absent from the failure mapping
in `rust/mko-cli/src/output.rs`, so it answers `retryable: false`,
`next_action: none`; and **no caller anywhere passes
`StaleRepositoryLockPolicy::Clear`** — every call site is `Preserve`, so the
stale-lock takeover machinery in `lock.rs` is unreachable from any command.
Core would classify the stranded lock as stale (dead PID, past the 15-minute
TTL) but nothing can act on that. Direction: display and confirm without the
lock; acquire only after the phrase is typed, then re-validate card and
revision under it; treat Ctrl-C, EOF, and `q` as clean cancel; surface owner
PID and time in `mko doctor` with an explicit, safe recovery path.

**P1 — registered-but-unprocessed PDFs vanish from home (partly resolved
2026-08-03).** The KB held three
registered Assets and one Source (two PDFs stopped at extraction), yet home
printed `새 자료 0 · 검토 1 · 수정 필요 0 · 승인된 지식 0 · 문제 0`.
`inspect_home` computes `registered` (`rust/mko-core/src/home.rs`) but the
render and recommendation logic in `rust/mko-cli/src/cli.rs` drops it. After an
extraction failure or an interrupted run the owner cannot tell that material is
waiting. Home needs a 정리 중 / 문제 count and per-item next actions (다시 추출,
지원되지 않는 PDF, 복구 방법 보기). Fixed for the count: `inspect_home` now
distinguishes registered Assets that have become a record from those that have
not, home shows 정리 중, recommends 멈춘 자료 계속 정리 when nothing else is
pending, and the 자료 정리 action says how much is waiting. **Still open:**
per-item next actions need Core to remember *why* an Asset stopped — a
preparation failure is not recorded anywhere today, so a listing can say an item
is unfinished but not whether it needs a re-run, a new copy, or a repair. That
per-Asset processing state is the design question to settle before building the
per-item recovery UI.

**P1 — `doctor` misdiagnoses a current v3 KB.** A real schema-v2 KB that bare
`mko` opens fine reports `repository_incompatible` / `next_action: configure`
through `mko doctor --format json-v1`, because doctor still reads only the v1
`KnowledgeConfig` (`rust/mko-core/src/doctor.rs`). `--format json-v2` is
accepted but emits a human line with exit 0 instead of JSON
(`rust/mko-cli/src/cli.rs`). The tool that should guide recovery instead tells
the owner to redo setup.

**P1 (found in the same run) — real PDFs fail extraction with no way
forward (resolved 2026-08-03).** Both documents already in the owner's Inbox failed
`mko source prepare`: `Signals and Systems.pdf` with `prepared_text_invalid`
("unsupported control characters"), and
`CalterahRhineRadarBasebandUserGuide_v1.0.2-1.pdf` with `pdf_extraction_failed`
(worker failed in under a second). Both answer `next_action: none`. Registration
accepts these files but preparation rejects them, so the product's main path
does not work on the owner's actual library. The two files are the natural test
corpus. This is the reason items get stuck in the item above; surfacing stuck
work does not fix it.

Both were reproduced against the owner's own files and diagnosed. The first was
ours: a 728-page book extracted cleanly and was then rejected whole over 24
meaningless control bytes (0x01 x17, 0x00 x3, 0x10 x2, 0x11 x2, first on page
196). Normalizing them away — the same pass that already collapses whitespace
and applies NFC — keeps the property that canonical text carries no ambiguous
controls, and the book now prepares into 728 blocks of readable text. The second
is upstream: the extraction worker panics inside `adobe-cmap-parser` 0.4.1
(`src/lib.rs:213`, "bad length of hexstring") on that document's CMap, which is
why it died in under a second. Running the extractor in its own process already
kept the CLI alive; the failure is now reported as `pdf_text_unreadable` with
`next_action: add` and tells the owner to export or scan a new copy and register
that, instead of `pdf_extraction_failed` with nothing to do. **Still open:**
making that document itself readable needs an upstream fix or a different
extractor, which is a dependency decision rather than a bug in this repository —
the pinned `pdf-extract` 0.12.0 is the current constraint.

**P2 — 검토 계속 is a machine-contract screen.** Choosing review drops the
owner straight into long internal IDs and SHA-256 digests, raw Source/Knowledge
JSON, the full previous revision, English contract vocabulary, and a long
approval phrase — and although it says "잘못되면 수정 요청", the screen offers no
수정 요청 or 나중에 choice (`rust/mko-cli/src/cli_v2.rs` goes straight to the TTY
approval function). Direction: a Korean human summary, evidence / LLM analysis /
uncertainty separated, what changed since the previous version, then
`[a] 승인 · [c] 수정 요청 · [d] 나중에 · [q] 취소`, with digest-bound confirmation
kept but shown only after the owner chooses approve.

**P2 — Obsidian is not yet a reading surface.** Generated record projections
carry only title, internal record link, asset link, and revision hash; the
summary, claims, and analysis are absent, and `current.yaml` holds only a
revision digest, so the owner must hunt for the revision file. Projections need
the one-sentence and general summary, key claims with evidence locators, LLM
analysis / counterarguments / open questions, a link to open the original PDF,
and review state with the next action.

**P3 — search and empty states dead-end.** 지식 찾기 with no match ends at
`승인된 지식에서 찾지 못했습니다` — no mention that pending knowledge exists and no
route back to 검토 계속. Matches show a 140-character excerpt with no way to open
the full knowledge item (`rust/mko-cli/src/cli.rs`).

**What works.** Bare `mko` turned a command-centred tool into an
action-centred one; `remember` re-displays the exact text and takes only a
clear `y`; document evidence, LLM analysis, and owner judgment stay
structurally separate; the revision loop (revision, feedback, addressed
feedback, diff) is technically closed in 0.3.2; final approval, perspective,
and investment high-risk policy correctly stay real-TTY; the embedded schema
surface and handshake catch Core/Skill contract drift early.

**Agreed order.** 1) Ctrl-C stale lock and a safe recovery path; 2) v3-aware
`doctor` with accurate typed next actions; 3) show stuck-after-registration
PDFs on home and make them resumable — with extraction robustness for the two
failing real PDFs as its prerequisite; 4) split the review screen into a human
decision UI; 5) make Obsidian projections readable knowledge documents;
6) connect search results to detail, original, and review; 7) only then signed
binary installation and promoting `feat/revision-loop` to `main`.

The product's strongest property today is that it prevents a wrong approval.
The next thing to earn is that an owner who stopped safely can start again.

## Rendered-document review (owner-proposed, 2026-08-01)

**Idea.** When material arrives, the LLM's draft is delivered to the owner
as a readable HTML/MD document; the owner reads and approves. Review should
feel like receiving a document, not operating structures in a terminal.

**Assessment: right direction — this is a review-surface change, not a
loop change.** The ingest→draft→approve loop already exists; the guided
daily workflow already moved toward it. Three lines to hold when building:

1. The rendered document is a PROJECTION, not the record. The Core renders
   it deterministically from the draft revision, so approving the document
   is approving the record — an LLM-authored "pretty version" alongside the
   structured record would create two versions of the truth.
2. Delivery may push anywhere; approval stays a local real-TTY act.
3. Default delivery target is local (browser/Obsidian projection). External
   channels (e.g. Telegram) expose personal material to third-party servers
   and must be an explicit owner opt-in per channel — see the Thesis
   notification-boundary precedent and the mko:// export fence.

Depends on the request_changes→new-draft→diff→re-review loop (already
queued): a delivered document invites "fix this here", which dead-ends
without it.
