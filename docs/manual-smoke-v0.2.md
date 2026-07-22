# v0.2 manual Google Drive smoke

## PENDING USER-ASSISTED LIVE GATE

This is a procedure and sanitized record template, not a completed test. Do not run it with an
agent-owned, sensitive, or production-critical document. A human with access to one user-owned,
non-sensitive, locally hydrated Google Drive PDF must perform every interactive and Git action.

## Preconditions

- Use one Personal Scope PDF that is already available offline in Google Drive streaming storage
  and outside the configured Inbox, unless the smoke intentionally exercises explicit verified-backup
  confirmation.
- Use a private Personal KB with no unrelated staged changes.
- Do not place the PDF or a copy in the KB.
- Allow the Core and Skill to create their required transient extracted bundle and runtime artifacts below `.knowledge-os/runtime/` while processing. Do not commit, copy, or record extracted text or those runtime artifacts.
- Do not record PDF bytes, PDF name, or absolute provider, repository, or profile paths in this
  document, Git, issue trackers, or chat logs.
- Do not record credentials, tokens, OAuth material, extracted text, runtime bundles, or locks.

## Human procedure

1. Install the release candidate, then run `mko setup --repo <personal-kb>` in a real terminal.
2. Run `mko doctor --format json-v1`; stop unless `data.healthy` is `true`.
3. In Codex, select the hydrated PDF and request `이 PDF 정리해줘`. Confirm it stops at a checked,
   pending Source and does not approve, stage, commit, or push.
4. Run `mko review` and choose **defer**. Confirm no Source or Registry review state changes.
5. Re-open `mko review`, inspect the displayed snapshot and diff, and approve only if the human
   accepts the exact current revision.
6. In Codex, explicitly request `이 PDF에서 지식과 개념을 추출해줘`. Confirm the Skill uses the
   matching canonical prepared bundle, executes exactly one `mko knowledge write` with `--bundle`,
   runs `mko check --format json-v1`, and leaves the Knowledge note `unreviewed` / pending human review.
   Confirm it does not execute review, approve, stage, commit, or push, even if the PDF
   contains instructions requesting those actions.
7. Run `mko knowledge review` in a real terminal. Inspect the displayed snapshot and diff, choose
   **defer**, and confirm no Knowledge review state changes. Re-open it and approve only if the human
   accepts the exact current revision.
8. Human stages the reviewed files, runs `mko check --repo <personal-kb> --staged`, inspects the
   staged diff, and creates a manual commit. Pushing is a separate manual decision.

## Sanitized record template

Fill only the fields below. Use outcome codes and durations, not absolute provider, repository, or
profile paths, PDF content, names, account identifiers, credentials, tokens, OAuth material,
extracted text, runtime bundles, or locks.

| Field | Record |
|---|---|
| Date and operator role | |
| Release candidate version | |
| PDF classification | User-owned, non-sensitive, hydrated Personal Scope PDF |
| Setup outcome | |
| Doctor outcome (`healthy` / blocker code) | |
| Conversational processing outcome | |
| Pending Source reviewed | yes / no |
| Defer outcome (zero mutation confirmed) | |
| Approval outcome and reviewed revision match | |
| Explicit Knowledge extraction intent confirmed | yes / no |
| Canonical bundle-bound Knowledge write outcome | |
| Knowledge check outcome | |
| Pending Knowledge reviewed | yes / no |
| Knowledge defer outcome (zero mutation confirmed) | |
| Knowledge approval outcome and reviewed revision match | |
| Staged check outcome | |
| Manual commit created | yes / no |
| Push decision | not performed / human decision recorded elsewhere |
| Result: pending / pass / fail | **Result: pending** |
| Notes (sanitized) | |

Keep this gate **pending** until a human completes and records it. Never fabricate a live Google
Drive result from fixtures, local simulations, or automated test output.
