# My Knowledge OS — Why Material Stopped

Date: 2026-08-04
Status: Implemented 2026-08-04 with the recommended answer to §5 — attempts are
knowledge-base content, under `assets/attempts/`
Applies to: `mko` after 0.3.6
Responds to: the open half of usability item 3 — per-item next actions for
material that stopped (다시 추출 / 지원되지 않는 PDF / 복구 방법 보기)

## 1. Problem

Home now says how much material is registered but unfinished (0.3.6). It cannot
say why any of it stopped, so it cannot offer the actions the owner asked for.
The failure that stranded the material is reported once, to whoever ran the
command, and then discarded: `mko source prepare` returns
`pdf_text_unreadable`, `prepared_text_invalid`, or
`hydration_confirmation_required`, and nothing in the knowledge base remembers
it. A day later the owner sees a count and no explanation.

Guessing is worse than silence here. "다시 추출" offered on a document whose text
layer cannot be parsed sends the owner around a loop that will fail again — the
same unactionable advice this lane has spent its time removing.

## 2. The constraint that shapes the answer

v0.1 kept `AssetStatus` — `Registered`, `Extracted`, `Failed`, and five more —
inside the asset record. **v0.3 deliberately dropped it.** The v2 asset record
carries identity and provider binding only, and every state in the system is
*derived*: queue state comes from review events, projections are rendered from
revisions, home counts are computed at read time. Nothing stores a status field
that a later fact could contradict.

Putting a mutable `processing` block back on the asset would reintroduce exactly
what that design removed, and would need a lock, an atomic rewrite, and a story
for what happens when the stored status and the world disagree.

## 3. What makes this recordable anyway

An asset's identity **is** its content: `asset_id` is derived from the file's
fingerprint. So "this exact content could not be extracted, with this code" is
not mutable state — it is an immutable fact about an immutable thing. Replacing
the PDF with an OCR'd copy does not falsify it; it produces a *different* asset
with a different id, and the old fact remains true about the old bytes.

That is the same shape as a review: an append-only observation, content-
addressed, from which current state is derived. The system already has this
pattern and the machinery for it.

## 4. Design

A preparation attempt becomes an append-only record, and "why did this stop" is
derived from the attempts the way review state is derived from reviews.

**Record.** `assets/attempts/{id}.json`, content-addressed like reviews:

```
schema_version: 2
id: personal-attempt-<sha256 of the canonical record>
record_type: attempt
asset_id: personal-asset-…
outcome: prepared | failed
code: <the typed error code, absent when prepared>
observed_at: <RFC3339>
```

The message is deliberately not stored: it can quote document bytes, and the
code is what any surface should branch on.

**Write.** `prepare_pdf_asset_v2` appends one attempt on both paths, success and
typed failure, before returning. A failure to write the attempt never masks the
original error — the owner still gets the real reason, and the missing attempt
only means home stays as uninformative as it is today.

**Derivation.** For a registered asset with no Source record, the latest attempt
explains it. From the code, the next action follows without guessing:

| code | what home says | action |
| --- | --- | --- |
| `pdf_text_unreadable` | 이 PDF의 텍스트를 읽을 수 없습니다 | 새 사본을 Inbox에 넣고 등록 |
| `hydration_confirmation_required` | 원본을 내려받아야 합니다 | 내려받고 계속 |
| `prepared_text_invalid`, extraction faults | 정리하다 멈췄습니다 | 다시 시도 |
| no attempt recorded | 아직 정리하지 않았습니다 | 정리 계속 |

The last row matters: an asset registered before this change, or one whose
attempt record is missing, degrades to exactly today's behaviour rather than to
a wrong claim.

**Bounds.** The attempts directory is scanned under the existing record scan
limits. An asset that fails repeatedly appends identical records, which
content-addressing collapses to one file, so a retry loop cannot grow the
directory.

## 5. The decision worth taking

**Is a failed attempt knowledge-base content, or machine-local runtime?**

Recommended: knowledge base. The fact is durable, content-addressed, and worth
carrying to another machine — "this document defeated the extractor" is exactly
what an owner should not have to rediscover. It also keeps the derive-don't-
store property intact, because an attempt is an observation, not a status.

The alternative — `.knowledge-os/runtime/attempts/` — keeps the KB smaller and
avoids a new synced collection, at the cost of every machine relearning the same
failures and of losing the record on cleanup.

Everything else in §4 is the same either way; only the path and whether `check`
validates the collection change.

## 6. Invariants that do not change

- Assets stay identity records; no status field returns to them.
- Attempts are append-only and content-addressed; nothing is rewritten.
- A stored code never contradicts the world, because the asset it describes is
  its content.
- Surfaces branch on codes, never on stored prose, and an absent attempt yields
  today's neutral wording rather than an invented one.

## 7. Implementation sketch

- Model and schema for the attempt record; fixture and contract test.
- Append on both outcomes in `prepare_pdf_asset_v2`, with the write never
  masking the original error.
- Derivation next to the home summary: for each registered asset without a
  record, resolve the latest attempt to a reason and an action.
- Home renders per-item lines instead of only a count, and the search dead-end
  guidance can name stuck material by reason.
- Tests: each code maps to its stated action; a missing attempt degrades to the
  neutral line; repeated identical failures collapse to one record.
