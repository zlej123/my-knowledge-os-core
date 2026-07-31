# My Knowledge OS — Daily Home UX Addendum

Date: 2026-07-31
Status: Phase A and the Phase B usability slice implemented and locally verified; Phase C requires
separate owner review
Applies to: `mko` after v0.3.0
Extends: `2026-07-22-v0.3-knowledge-ux-design.md`

## 1. Decision

MKO is one Personal knowledge system that may contain investment knowledge. Thesis remains a
separate investment-decision system.

- MKO stores material, grounded summaries, reusable knowledge, analysis, and exact user-authored
  thoughts.
- Investment is one confirmed perspective inside the Personal KB, not a separate MKO scope.
- Thesis receives only an explicitly promoted, approved, lineage-complete evidence package.
- Nothing is promoted, approved, committed, pushed, deleted, or moved across scopes automatically.

The normal user surface is no longer a list of Core commands. Running `mko` in a real terminal
opens a concise home screen organized around four user actions:

1. put something in;
2. find something;
3. review something;
4. remember or revisit something.

Asset, Source, Knowledge, Review, revision, bundle, and projection remain canonical Core concepts,
but ordinary users do not need to type their IDs or pipeline commands.

## 2. Evidence from the current product

This addendum responds to observed behavior in the active installation, not a hypothetical
redesign:

- the installed CLI is `mko 0.3.0`;
- the configured Personal KB is a v0.1 repository;
- `mko status` and `mko inbox` still render legacy state;
- `mko queue` fails while parsing the legacy `core_version` field as a v0.3 config;
- `mko dashboard` reports that a v0.3 Personal KB is required;
- bare `mko` displays fourteen command families instead of current state or a next action;
- one registered PDF currently stops at the prose next action `내용 추출`, which is not directly
  actionable from the screen;
- current v0.3 Knowledge projection hard-codes `domain: uncategorized`;
- current v0.3 domain configuration has only one KB-wide default policy.

The result is a split experience: old read surfaces partly work, new review surfaces fail, and the
owner must understand contract versions before continuing.

## 3. Goals

### 3.1 Primary goals

- Make `mko` itself the only command a person must remember.
- Detect setup, compatibility, health, Inbox, review, and repair states before presenting actions.
- Give every safe stop one concrete next action that can be selected in place.
- Treat general, learning, technical, project, and investment knowledge as perspectives in one
  Personal KB.
- Keep Source-grounded content, LLM analysis, and exact user-authored thoughts visibly separate.
- Make approved knowledge easy to retrieve and deliberately resurface.
- Close the request-changes to replacement-revision loop.

### 3.2 Success criteria

- A configured owner can reach Inbox processing, review, or search from bare `mko` without an ID,
  path, or flag.
- A legacy KB never produces a raw schema parser error on a normal human surface.
- A blocked state names one owner-comprehensible recovery action.
- A user can understand whether a result is document evidence, LLM analysis, or their own thought
  without opening raw Markdown.
- Confirming the investment perspective activates the high-risk knowledge requirements.
- Final approval remains bound to the exact displayed revision in a real TTY.

## 4. Non-goals

This addendum does not authorize:

- silent v0.1/v0.2 to v0.3 migration;
- mutation of a legacy KB;
- automatic Git initialization, commit, pull, push, or conflict resolution;
- automatic Thesis promotion;
- a background watcher or daemon;
- Telegram ingestion;
- vector search or a RAG service;
- Work-to-Personal, Work-to-Shared, or Personal-to-Work copying;
- conversational final approval;
- LLM-authored user judgment;
- direct edits to canonical generated Markdown or YAML.

## 5. Product model

### 5.1 Scope and perspectives

`Personal` remains the repository scope. Perspective is a many-to-many classification within that
scope.

Initial trusted vocabulary:

| Perspective | User label | Default policy |
|---|---|---|
| `life` | 생활 | standard |
| `learning` | 학습 | standard |
| `technical` | 기술 | standard |
| `project` | 프로젝트 | standard |
| `investment` | 투자 | high-risk |

A record may carry more than one perspective. A semiconductor paper may be both `technical` and
`investment`; a productivity paper may be both `learning` and `project`.

Perspective differs from a free-form tag:

- perspectives drive trusted policy and top-level navigation;
- tags improve retrieval but never relax or activate policy;
- the document or LLM may propose perspectives;
- only trusted configuration or an explicit user confirmation activates a perspective;
- activating `investment` requires the existing policy-confirmation boundary and enforces at least
  one counterargument and one open question.

`Work` and `Shared` are not perspectives. They remain separate scopes with no automatic search,
copy, or promotion.

Phase B stores only owner-confirmed values in the optional `perspectives` array on a Knowledge
revision. Empty legacy arrays are omitted during serialization so existing v0.3 revision bytes and
digests remain valid. A real-TTY confirmation card binds the current revision, sorted perspective
set, derived policy, and replacement effect. Exact confirmation publishes a new pending revision
with compare-at-commit pointer replacement. `investment` always derives `high_risk`; it cannot be
confirmed until the current Knowledge response already contains both a counterargument and an open
question. Document and LLM strings never populate this field.

### 5.2 Human concepts

Normal UI terms:

| Core term | Normal UI term |
|---|---|
| Asset | 원본 자료 |
| Source | 근거 요약 |
| Knowledge | 지식 노트 |
| Review | 검토 |
| Judgment | 내 생각 |
| Projection | 읽기 화면 |

The machine contracts retain the Core names.

## 6. Bare `mko` routing

Bare `mko` is interactive only when stdin and stdout are real terminals. Existing explicit
subcommands and machine contracts remain compatible.

The home router resolves these states in order:

1. CLI or profile unavailable;
2. configured repository missing or unreadable;
3. legacy KB detected;
4. v0.3 KB blocked or projection repair required;
5. new Inbox material;
6. review or revision work;
7. ready Personal KB.

It never begins a mutation merely because the home screen was opened.

### 6.1 Missing setup

```text
$ mko

개인 지식함이 아직 연결되지 않았습니다.

[1] 새 Personal KB 만들기
[2] 기존 Personal KB 연결하기
[q] 종료
›
```

Selecting an option creates and displays a non-mutating setup plan. It does not apply the plan.

Setup order:

1. choose create-new or connect-existing;
2. choose the local Personal KB path;
3. connect the Google Drive Inbox needed for provider-backed documents;
4. offer a private GitHub remote as recommended, optional history synchronization;
5. display the exact destinations and effects;
6. apply only through the existing real-TTY setup approval.

A private GitHub URL is not a precondition for creating the first local KB or producing the first
pending summary. Git initialization, remote configuration, commit, and push each remain separate
actions.

### 6.2 Ready state

```text
$ mko

내 지식함
새 자료 1 · 검토 2 · 수정 요청 1 · 막힘 0

[1] 새 자료 정리
[2] 검토 계속
[3] 지식 찾기
[4] 빠른 메모
[5] 다시 볼 지식
[q] 종료
›
```

Rules:

- counts come from canonical Core state;
- menu numbers are session-local presentation aliases;
- durable operations use Core-returned IDs and revisions;
- zero-count actions may remain visible but explain the empty state;
- no advanced command name appears in normal output;
- `q`, EOF, and Ctrl-C leave state unchanged.

### 6.3 Legacy KB state

```text
$ mko

기존 지식함(v0.1)을 발견했습니다.
현재 mko는 v0.3입니다. 기존 파일은 변경하지 않습니다.

[1] 기존 지식함 읽기
[2] 새 v0.3 지식함 만들기
[3] 안전한 전환 계획 보기
[q] 종료
›
```

Requirements:

- detection happens before attempting v0.3 queue or dashboard parsing;
- raw YAML parser diagnostics are available only in an advanced diagnostic view;
- option 1 is read-only;
- option 2 creates only a non-mutating setup plan;
- option 3 creates a non-mutating transition inventory and plan;
- no option edits, deletes, or initializes Git without a later, exact approval step.

### 6.4 Blocked state

```text
내 지식함을 사용할 준비가 되지 않았습니다.

문제: Google Drive Inbox를 현재 프로세스에서 쓸 수 없습니다.
[1] 해결 방법 보기
[2] 다시 확인
[q] 종료
›
```

The human surface maps typed error codes to one plain-language cause and one safe next action. It
does not invent recovery beyond the Core-returned action.

## 7. Legacy transition concierge

The v0.3 clean-break rule remains authoritative. Transition means preserving the old KB and
constructing a reviewed plan for a new KB; it does not mean in-place migration.

### 7.1 Transition plan

The plan inventories:

- legacy KB path and detected contract;
- Git state and remote, read-only;
- registered Assets;
- pending and approved Sources;
- original provider files still available by fingerprint;
- records that can be recreated from original bytes;
- records requiring owner review because the original is missing or changed;
- new local KB destination;
- new provider Inbox destination;
- every prospective create/read/network effect;
- items explicitly not carried forward.

### 7.2 Apply boundary

Applying a transition plan:

1. requires a real-TTY, expiry-bound, effect-bound confirmation;
2. leaves the old KB byte-for-byte unchanged;
3. scaffolds a new v0.3 KB at a different path;
4. registers only exact provider bytes that still match the inventory;
5. creates pending drafts rather than copying old approval state;
6. never initializes or changes Git remotes without a separate approval.

Recreated content must cite the old stable ID and revision as transition provenance, but old
approval does not automatically approve new v0.3 records.

## 8. Primary home actions

### 8.1 새 자료 정리

This action routes through the existing bounded Inbox registration contract. The Core returns
per-item outcomes and next actions. The conversation Skill performs semantic preparation only for
items the Core marks ready.

Human result:

```text
새 자료 3건

1. IMM Tracking Survey — 요약 준비
2. Sensor Manual — 요약 준비
3. Offline Paper — 다운로드 확인 필요

정리할 번호를 선택하세요. [1,2 / all / later] ›
```

`all` registers and summarizes eligible items but never promotes all items to Knowledge and never
approves them.

### 8.2 검토 계속

The home opens the canonical combined Source/Knowledge review card without requiring the user to
type an ID.

```text
[1/2] Signals and Systems
근거 요약 · 지식 노트

요약: ...
핵심: ...
반론: ...
열린 질문: ...
내 생각: 없음

승인 / 수정 요청 / 나중에  [a/r/s] ›
```

- `a` enters the existing real-TTY revision-bound approval flow;
- `r` records feedback against the exact displayed revision;
- `s` publishes a defer decision;
- a combined card clearly shows which targets each decision affects;
- ambiguous input changes nothing.

### 8.3 지식 찾기

The human alias is `find`; the existing machine search may remain nested.

```text
$ mko find "원심분리기 경제성"
```

Search behavior:

- approved Knowledge only by default;
- lexical search for the first slice; no vector infrastructure;
- title, perspective, tags, grounded units, and exact user thoughts are searchable;
- unreviewed records require an explicit advanced option and are visually marked;
- results show provenance and locators;
- results visually separate document evidence, LLM analysis, and user-authored thought;
- investment results show unresolved counterarguments and open questions.

Bare-home search accepts a natural phrase and uses the same deterministic search result set.

### 8.4 빠른 메모

Quick note captures exact user text without asking an LLM to rewrite it.

```text
무엇을 기억할까요?
› FMCW 최대거리는 ADC sampling rate와 usable IF bandwidth를 구분해야 한다.

입력한 문장 그대로 저장할까요? [y/N] ›
```

The quick-note record contract is deferred to Phase B because it requires a new canonical record
or a generalized user-authored capture contract. Its minimum invariants are:

- exact UTF-8 bytes are echoed before confirmation;
- normalization is limited to the canonical line-ending contract and is shown if it changes bytes;
- authorship is `user_confirmed`;
- no Source evidence is fabricated;
- the LLM may later suggest links or perspectives but cannot alter the original note;
- cancel or ambiguity writes nothing.

Phase B fixes the first canonical contract as follows:

- path: `notes/personal-note-<sha256>.md`;
- front matter: `schema_version`, `record_type: quick_note`, content-addressed `id`, exact
  `text_digest`, `authorship: user_confirmed_via_tty`, and `created_at`;
- body: the normalized exact user text under a fixed `# Quick note` heading;
- normalization: CRLF/CR to LF and Unicode NFC only, with leading/trailing newlines removed;
- identity: canonical JSON digest of `text_digest`, `authorship`, and `created_at`;
- confirmation: Core prepares an exact-text card and digest-bound confirmation phrase; publication
  accepts only that exact prepared record and phrase;
- update policy: immutable and idempotent; editing or replacing an existing note is not supported;
- compatibility: an absent `notes/` directory in an older v0.3 KB means no notes, while the first
  explicitly confirmed note may create that Core-owned directory under the repository lock.

The first Phase B slice does not attach a quick note to Source evidence and does not let an LLM
populate or rewrite its text.

### 8.5 다시 볼 지식

Resurfacing selection and filtering do not mutate canonical Knowledge or Review records. Opening a
displayed item records only its exact Knowledge revision and timestamp in bounded, owner-private,
Git-ignored `.mko/runtime/resurface-history.json`. A stale selection is rejected. The owner may
then explicitly enter `p` to branch to the separate perspective-confirmation action.

The deterministic ordering is deliberately visible and model-free:

- explicitly deferred Knowledge first;
- never-opened Knowledge, then least-recently opened Knowledge;
- records with unresolved open questions;
- most recently approved or deferred review;
- stable Knowledge ID.

An optional perspective filter is applied before the bounded result is returned. The list labels
deferred items as `나중에 보기`; selecting one shows the full synthesis, review date, and previous
open date. The owner selects the displayed item and perspectives by number without typing a
Knowledge ID. Search remains approved-only.

## 9. Investment perspective and Thesis boundary

Investment knowledge remains useful in the same Personal KB search and navigation as other
knowledge. It receives stricter review, not a separate hidden store.

When an owner confirms `investment`:

- Knowledge validation uses the high-risk policy;
- counterargument and open-question units are mandatory;
- the UI shows the investment perspective prominently;
- absence of sufficient evidence is represented as missing or conflicting evidence, not a low
  confidence fact.

Promotion to Thesis is a separate explicit action available only for approved records:

```text
Thesis 근거 후보로 보낼까요? [y/N]
```

The resulting package must contain exact MKO record ID, content revision, approval Review ID,
fingerprint, evidence locators, perspective, and export classification. MKO creates the package;
Thesis decides whether to stage it as Evidence. MKO never creates a Thesis decision and Thesis
never writes back to canonical MKO records.

Promotion is not part of Phase A or Phase B implementation in this addendum. The text above freezes
the boundary only.

## 10. Feedback regeneration

After `request_changes`, the queue must expose one executable next action rather than only the
label `regenerate`.

Conversation flow:

1. reopen the exact current card and unresolved feedback;
2. prepare or reuse a valid, unexpired grounded bundle;
3. pass bundle, current revision, and feedback as distinct typed inputs;
4. produce a bounded semantic replacement;
5. publish with `expected_revision`;
6. publish the review-resolution link;
7. show an exact diff;
8. open a fresh review session.

Failure preserves the unresolved feedback and prior revision. Retry resumes from the persisted
review event. It never replaces the pointer without compare-at-commit validation.

The human terminal home may display and record feedback, but semantic regeneration is performed by
the Skill. The CLI must say that clearly instead of implying that the Core itself writes analysis.

## 11. Command-surface policy

Normal help:

```text
Usage: mko [ACTION]

Actions:
  add       자료 넣기
  find      지식 찾기
  review    검토 계속
  remember  빠른 메모
  doctor    문제 확인
```

Advanced Core and machine commands remain callable and documented in an advanced reference:

- setup plan/apply;
- source prepare/write-draft;
- knowledge write;
- review-open/review-feedback;
- asset repair;
- dashboard repair;
- check and hooks.

Hiding a command from normal help does not remove it or change its existing parse contract.

## 12. Delivery phases

### Phase A — no schema change

1. make `Cli.command` optional for human mode;
2. add a read-only home-state aggregator;
3. detect legacy KB before v0.3 queue/dashboard parsing;
4. render ready, legacy, blocked, and empty home states;
5. make menu routes call existing human commands;
6. add a top-level human `find` alias over approved Knowledge search;
7. hide advanced commands from normal help without breaking compatibility;
8. map typed errors to concrete safe next actions;
9. align setup Skill order with create/connect first and optional Git remote;
10. update README and Skill language;
11. add integration tests against the active v0.1-shaped fixture and a v0.3 fixture.

Phase A must not write a transition plan or change the configured KB merely by opening `mko`.

### Phase B — reviewed schema extension

1. perspective vocabulary and many-to-many record field;
2. trusted policy derivation from confirmed perspectives;
3. perspective confirmation contract;
4. quick-note canonical record;
5. exact user-text confirmation;
6. perspective-aware search and deterministic resurfacing;
7. Obsidian projection columns and filters.

The authorized slice implements items 1–7, including approval/defer recency, revision-scoped local
open history, and deterministic resurfacing of deferred Knowledge. Obsidian record projections expose a canonical
`perspectives` list, while the approved-Knowledge Base provides all, life, learning, technical,
project, and investment views. Schema compatibility and deterministic digests are covered by the
v2 contract, record round-trip, perspective-replacement, projection, and cross-platform
normalization tests.

### Phase C — workflow closure

1. transition inventory and plan/apply contract;
2. feedback regeneration orchestration in the Skill;
3. fresh review-session handoff after replacement;
4. explicit approved-MKO to Thesis promotion package.

## 13. Acceptance criteria

### 13.1 Phase A

- Bare `mko` on an active legacy KB exits without mutation and never prints a raw YAML parser
  diagnostic in normal mode.
- Bare `mko` on a healthy v0.3 KB displays canonical counts and at most five primary actions.
- Bare `mko` in a non-TTY context does not prompt or mutate.
- Opening and quitting every home state leaves the repository and profile byte-identical.
- A registered-but-unprepared Asset is shown with an in-place, plain-language next action.
- Queue selection never requires the user to type a stable ID.
- Existing explicit subcommands and JSON v1/v2 golden fixtures remain byte-compatible.
- Final approval still fails outside a real TTY.
- Work and Shared scopes are neither searched nor copied.

### 13.2 Phase B

- One Knowledge revision may contain both `technical` and `investment` perspectives.
- LLM- or document-proposed `investment` cannot activate high-risk policy.
- User-confirmed `investment` requires counterargument and open-question units.
- The home flow can filter, select, and confirm Knowledge perspectives without requiring an ID.
- Quick-note tests prove exact-byte echo, ambiguity cancellation, and no LLM paraphrase.
- Search separates grounded evidence, LLM analysis, and user thought in every result.
- Obsidian projections expose the complete perspective list and generated Bases provide one
  filter view per perspective.

### 13.3 Phase C

- Transition apply preserves the legacy KB byte-for-byte.
- Recreated v0.3 drafts remain pending regardless of legacy approval.
- Request changes can reach a replacement revision, exact diff, and fresh review session without
  an ID or file path supplied by the user.
- Thesis promotion is explicit, approved-only, revision-bound, and one-way.

## 14. Verification

Implementation must run:

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

In addition, Phase A requires real-binary smoke tests for:

- the current v0.1-shaped Personal KB fixture;
- a healthy v0.3 KB;
- an invalid profile;
- a read-only provider;
- non-TTY execution;
- quit/EOF with zero mutations.

Passing unit tests is not proof that the active profile, Google Drive Inbox, Obsidian projection,
or Thesis promotion works. Those require separate live smoke evidence.

## 15. Phase A implementation map

The owner authorized Phase A and then the first Phase B slice on 2026-07-31. This table records the
Phase A change map; Phase C remains unauthorized.

| Surface | Current implementation | Phase A change |
|---|---|---|
| CLI parse | `mko-cli/src/cli.rs` requires `Command` | make the human command optional and route `None` to Home |
| Dispatch | `run` dispatches explicit commands only | add a Home branch without changing explicit command behavior |
| Context | `context.rs` can read legacy and v0.3 config | expose a read-only context classification instead of forcing a v0.3 parse |
| Queue | `queue_v2` calls the v0.3 queue directly | Home checks repository generation before queue derivation |
| Legacy | status/inbox still operate against v0.1 | wrap them in the read-only legacy Home state |
| Home state | absent | add one bounded Core report with compatibility, health, Inbox, queue, and projection counts |
| Search | nested `knowledge search` | add a human `find` alias; preserve the existing nested command |
| Help | Core commands are all visible | show primary human actions; keep advanced commands callable |
| Setup Skill | Drive then mandatory GitHub URL | create/connect first, Drive for documents, optional Git remote |
| Tests | command-specific fixtures | add no-arg legacy/v0.3/non-TTY/quit byte-preservation tests |

The Home report belongs in Core because compatibility and counts must not be inferred by the CLI.
The CLI owns only terminal presentation and session-local numeric choices.

## 16. Owner review gates

Before Phase A implementation, approve or revise:

1. `mko` as the default home;
2. the five visible home actions;
3. read-only legacy handling before any transition tooling;
4. create/connect-first setup with Git remote optional;
5. top-level `find` as the search label.

The owner approved these Phase B first-slice gates on 2026-07-31:

1. the initial perspective vocabulary;
2. the quick-note record contract;
3. the policy-confirmation surface.

Before Phase C implementation, separately approve:

1. legacy transition effects;
2. feedback regeneration authority;
3. the MKO-to-Thesis package contract.
