# v0.2 manual Google Drive smoke

## PENDING USER-ASSISTED LIVE GATE

This is a procedure and sanitized record template, not a completed test. Do not run it with an
agent-owned, sensitive, or production-critical document. A human with access to one user-owned,
non-sensitive, locally hydrated Google Drive PDF must perform every interactive and Git action.

## Preconditions

- Use one Personal Scope PDF that is already available offline in Google Drive streaming storage.
- Use a private Personal KB with no unrelated staged changes.
- Do not place the PDF, a copy, or extracted text in the KB.
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
6. Human stages the reviewed files, runs `mko check --repo <personal-kb> --staged`, inspects the
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
| Staged check outcome | |
| Manual commit created | yes / no |
| Push decision | not performed / human decision recorded elsewhere |
| Result: pending / pass / fail | **Result: pending** |
| Notes (sanitized) | |

Keep this gate **pending** until a human completes and records it. Never fabricate a live Google
Drive result from fixtures, local simulations, or automated test output.
