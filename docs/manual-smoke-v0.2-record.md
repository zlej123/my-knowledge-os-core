# v0.2 manual Google Drive smoke — sanitized record

Follows the procedure and template in [manual-smoke-v0.2.md](manual-smoke-v0.2.md).
No absolute paths, PDF bytes/names, account identifiers, credentials, tokens, or extracted
text are recorded here — only outcome codes.

## Record

| Field | Record |
|---|---|
| Date and operator role | 2026-07-21; partial agent-driven CLI run with human Source review |
| Release candidate version | `mko 0.2.0`, built from immutable Core commit `09162c2` |
| PDF classification | User-owned, non-sensitive, locally hydrated Personal Scope PDF (an openly/CC-licensed textbook) |
| Setup outcome | observed healthy after a localized Google Drive workaround (see finding L1; now fixed) |
| Doctor outcome (`healthy` / blocker) | `healthy: true`; all checks healthy |
| Conversational processing outcome | partial — the agent stood in for the Codex Skill semantic step; the deterministic Core Source pipeline `add → prepare → write-draft → check` completed with `valid: true`, but the deployed Skill was not exercised |
| Pending Source reviewed | yes |
| Defer outcome (zero mutation confirmed) | observed `deferred`; no durable state changed |
| Approval outcome and reviewed revision match | observed exact revision-bound token acceptance; malformed/IME-garbled input was safely rejected |
| Explicit Knowledge extraction intent confirmed | no — not exercised |
| Canonical bundle-bound Knowledge write outcome | pending — not exercised |
| Knowledge check outcome | pending — not exercised |
| Pending Knowledge reviewed | no |
| Knowledge defer outcome (zero mutation confirmed) | pending — not exercised |
| Knowledge approval outcome and reviewed revision match | pending — not exercised |
| Staged check outcome | pending — final manual commit is the user's step |
| Manual commit created | no (user decision) |
| Push decision | not performed |
| Result: pending / pass / fail | **Result: pending** — Source evidence is partial; the deployed Skill, explicit Knowledge path, Knowledge check/review, staged check, and manual commit remain incomplete |
| Notes (sanitized) | See retained partial evidence, findings, and coverage limits below |

## Partial safety evidence retained

- `mko setup` and `mko review` refuse to run without an interactive TTY (fail-closed).
- `mko review` requires the exact approval token; a non-token / IME-garbled input is rejected.
- A cloud-placeholder PDF in the Inbox makes `setup` fail closed (`provider_hydration_failed`) rather than silently hydrating it.
- Approving a Source mutates only the durable review state; it never stages, commits, or pushes. All records remained untracked until a human git action.
- Approved Sources are immutable (`approved_source_immutable`) — a premature thin draft could not be silently overwritten; regeneration required a fresh cycle.
- JSON-v1 failure envelopes are path-free (e.g. `add` → "The PDF could not be added.").

## Findings

- **L1 (bug, fixed):** macOS Google Drive account-root detection hardcoded the English "My Drive" folder, so `setup` failed with `drive_root_not_found` on localized installs (Korean "내 드라이브"); `--drive-root` could not work around it. Fixed by trying a bounded set of localized My Drive folder names while preserving the account-root security bound (commit `fix: detect localized Google Drive My Drive folders on macOS`). The `GoogleDrive` env override remains for locales outside the known set.
- **L2 (UX wrinkle):** `mko source prepare` resolves the provider root from the ambient repository context when run inside the KB directory, so it needs `MKO_PERSONAL_PROVIDER_ROOT` there; it uses the machine profile only when run outside a KB. Worth aligning so the profile is used consistently.
- **L3 (product/knowledge quality):** the `semantic-response-v1` schema (`problem/method/contributions/reported_evidence/stated_limitations`) is research-paper oriented and yields low-value records for books/manuals. A book-oriented shape (scope, coverage, chapter map) would raise knowledge value for non-paper PDFs.

## Coverage limits

- The real deployed Codex Skill's instruction-adherence was not exercised here; the agent performed the semantic step directly via the Core CLI. The end-to-end Codex flow can be validated separately in the Codex app.
- Explicit Knowledge intent routing, canonical bundle-bound Knowledge write, hostile-bundle resistance,
  pending Knowledge state, and human Knowledge review were not exercised. They remain required
  before this record can pass.
- The partial manual smoke run did not include `quick_validate.py`; automated Skill validation is
  recorded separately from this live-gate evidence.
