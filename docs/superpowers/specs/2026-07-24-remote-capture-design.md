# My Knowledge OS Remote Capture v0.1 — Design Spec

Date: 2026-07-24
Status: approved direction, implementation contract
Target: post-`mko 0.3.0`; `CaptureEnvelope` schema version 1

## 1. Purpose

Remote Capture accepts owner-sent Telegram documents, text, and allowlisted URLs, then routes each
item by subject:

- stocks, companies, markets, investing, macroeconomics, and economics go to MKO Finance
  Knowledge;
- lectures, recipes, books, engineering, climbing, hobbies, and other learning go to MKO General
  Knowledge.

Capture creates knowledge intake state. It never creates or approves a Project 2035 financial
decision.

## 2. Component boundary

```text
Shared infrastructure
|-- Telegram channel primitives
|-- Capture
`-- Delivery

MKO
|-- General Knowledge
`-- Finance Knowledge

Project 2035
`-- Financial Decisions
```

Capture is inbound and Delivery is outbound. They may share bounded Telegram transport, secret
lookup, identity allowlisting, retry, and redacted audit primitives. They do not share job state,
authorization evidence, or domain mutation authority.

## 3. First implementation slice

The first slice freezes and implements:

1. strict `CaptureEnvelope` JSON Schema and Rust DTOs;
2. a pure General/Finance routing resolver;
3. explicit user selection and confirmation semantics;
4. typed CLI validation and route outcomes;
5. negative guarantees proving that routing cannot create Assets, Knowledge, Delivery packages, or
   Project 2035 decisions.

This slice performs no Telegram HTTP request and no durable domain mutation. It exists so later
network and persistence code cannot invent a weaker routing contract.

`mko capture validate` and `mko capture route` are non-mutating contract/debug surfaces. A
`ready_*` result from these commands is not durable approval evidence and must never be consumed
directly by Asset or Knowledge writers. The later persistence service must independently verify
the Telegram command identity or obtain a real TTY/host confirmation bound to the exact Capture
revision before creating a route binding.

## 4. CaptureEnvelope v1

The normative contract is `schemas/capture/v1/capture-envelope.schema.json`.

An envelope contains:

- `schema_version`, exactly `1`;
- `capture_id`, an opaque correlation ID;
- `channel`, initially `telegram`;
- channel identity: profile, chat, sender, update, and message IDs;
- one bounded input descriptor;
- an optional explicit subject scope selected by the user;
- `received_at`.

Inputs are one of:

- Telegram PDF document reference;
- canonical YouTube reference;
- bounded plain text.

The envelope contains no bot token, API credential, local filesystem path, Telegram download path,
raw response body, executable command, or Project 2035 decision instruction.

## 5. Subject routing

`SubjectScope` has exactly two v1 values:

- `general`;
- `finance`.

Media type does not determine the scope. A finance lecture is Finance and a recipe PDF is General.

The routing resolver accepts an envelope and, when no explicit subject exists, an untrusted
classifier proposal. The proposal has a proposed scope, confidence, and mixed-subject flag. It is
never human authorization.

### 5.1 Explicit selection

Telegram commands `/general` and `/finance`, or an equivalent trusted UI action, produce
`routing_authority: user_selected`. Explicit selection wins unless policy rejects the item.

### 5.2 Classifier proposal

Without explicit selection:

- a high-confidence General result becomes `general_confirmation_required`;
- every Finance result becomes `finance_confirmation_required`;
- mixed, conflicting, low-confidence, or unavailable results become
  `routing_confirmation_required`.

No classifier-only result may create a durable Asset, Source, Knowledge, or decision. Confirmation
produces `routing_authority: user_confirmed_proposal`.

This deliberately requires one owner confirmation for the initial release. A later release may
allow high-confidence General auto-staging after measured false-routing rates, but not auto-
publication.

## 6. Route outcomes

The pure resolver returns exactly one of:

- `ready_general`;
- `ready_finance`;
- `general_confirmation_required`;
- `finance_confirmation_required`;
- `routing_confirmation_required`;
- `rejected`.

Ready outcomes include the confirmed scope and routing authority. Confirmation-required outcomes
include only a proposal and a next action; they are not durable domain identity.

## 7. Durable route binding

The second implementation slice adds one authoritative immutable route binding. It is referenced
by Asset and carried into Source and Knowledge revisions. Projections and Markdown views are
derived from that binding.

Finance maps to the existing high-risk policy and therefore requires:

- source date or freshness evidence when making time-sensitive claims;
- counterargument units;
- open questions or uncertainty units;
- pending human review.

General maps to the standard policy, while medical and legal material may select additional
domain-specific caution later.

Two Inbox folders are not a source of truth. A folder path may be a projection, but moving a file
cannot silently change its confirmed subject scope.

## 8. Telegram channel and pull adapter

The shared `mko-telegram` crate lives outside `mko-core`. Its `capture` module exposes the inbound
normalization and pull adapter, while a later `delivery` module owns outbound rendering and API
calls. They may share Telegram wire types, identity checks, and secret lookup, but not Capture or
Delivery authorization/state.

The later network adapter exposes:

```text
mko capture telegram sync
```

It performs one bounded `getUpdates` pull, processes updates in ascending ID order, and persists an
owner-local cursor and terminal receipts atomically. Only allowlisted chat and sender IDs are
accepted.

For PDFs, it:

1. checks declared and streamed size limits;
2. validates PDF content;
3. generates a safe locator independent of the sender filename;
4. materializes atomically into the configured Provider Inbox;
5. invokes the existing content-addressed Asset registration only after routing is ready.

For YouTube, it:

1. accepts only exact allowlisted HTTPS host/path forms;
2. canonicalizes a stable video ID and removes tracking parameters;
3. performs no redirect resolution, page scraping, or media download in v0.1;
4. creates a pending Reference intake only after the Reference contract is frozen.

An unsupported URL is rejected. A URL is never represented as a fake PDF Asset.

### 8.1 Telegram onboarding and pairing

Telegram onboarding is a machine-local pairing operation, initiated either by a natural-language
request or by the `mko telegram connect` wizard. Creating the bot in BotFather is the one manual
external step; MKO never automates BotFather, asks an agent to create a bot, or stores a BotFather
conversation.

The wizard requires a real TTY for the following sequence:

1. It accepts the bot token only through hidden terminal input and keeps it in one zeroizing,
   non-serializable memory value until final approval. The token must never enter configuration, audit,
   command-line arguments, environment variables, standard output, logs, a QR/deep-link payload,
   or a crash report.
2. It calls `getMe` to verify the secure-store token and displays only non-secret bot identity
   metadata.
3. It creates a cryptographically random, one-time pairing nonce with at least 128 bits of entropy
   and a five-minute expiry. The owner opens the generated Telegram deep link or scans its QR code
   from the intended private chat. The nonce binds the pairing attempt; it is not a credential and
   is invalidated on first use, expiry, cancellation, or any failed binding check.
4. Before showing the link, the wizard collects the intended owner's Telegram username. The adapter
   detects the resulting chat, sender ID, and sender username itself. Only that exact username in a
   private chat is eligible; groups, channels, forwarded messages, a different sender, or a different
   chat fail pairing.
5. Before persisting the binding, the wizard shows the exact non-secret effects—bot identity,
   paired private chat/sender, General and Finance capture targets, primary polling device, and
   secure-store reference—and requires a real-TTY exact-effects confirmation. Chat text, a deep
   link visit, a QR scan, host command approval, or a model response is never confirmation.
6. After confirmation it sends one bounded test message. Only after that succeeds does one atomic
   OS-credential-store write persist the token and binding together. It records only a redacted
   result and exposes `mko telegram status` and `mko telegram disconnect` for inspection and
   revocation. Failed delivery or storage leaves no durable connection; disconnect deletes the one
   combined credential.

The Telegram HTTP client ignores ambient proxy environment variables and uses bounded connect/read
timeouts. Because the Bot API token is carried in the request path, proxy support requires a later
explicit design and approval rather than an implicit environment-derived fallback.

The paired private chat exposes separate General and Finance targets. Target selection still follows
the Capture routing contract; pairing must not infer a subject from a chat name or grant automatic
routing, Knowledge approval, external publication, Project 2035 decision, or trade authority.

One bot has exactly one configured primary polling device. Other local devices may inspect pairing
status but have Capture sync disabled and must not call `getUpdates` for that bot. A later hosted
relay may replace this single-device rule only with a separately specified server-side cursor,
authorization, and delivery contract; it is not an implicit fallback.

## 9. Secrets and audit

Machine-local configuration stores only secret references. The atomic Telegram connection
credential contains the token and validated private-chat binding in an OS secure store. A connected
Telegram bot token is never accepted from an environment variable,
including for normal development commands; test fixtures must use a non-production test secret
provider and must not serialize token-shaped values. Tokens are never written to config or audit.

Audit records contain IDs, route outcome, routing authority, timestamps, and redacted error codes.
They do not contain bot tokens, captions, full message text, filenames, downloaded bytes, or
provider response bodies.

## 10. Idempotency

Telegram receipt identity is:

- document: `(chat_id, message_id, file_unique_id)`;
- YouTube: `(chat_id, message_id, canonical_video_id)`.

The adapter requests `offset = last_committed_update_id + 1`. It advances the cursor only after an
observed update has a durable terminal receipt. Transient API or download failure leaves the cursor
unchanged. Replays return the existing receipt.

## 11. Acceptance criteria

The first slice is complete when:

- the schema accepts General and Finance examples;
- the schema rejects unknown fields, tokens, local paths, and decision actions;
- Rust DTO parsing is strict and bounded;
- explicit General and Finance selections return ready outcomes;
- classifier proposals never return a ready outcome without confirmation;
- all Finance proposals require Finance confirmation;
- mixed, low-confidence, conflicting, and unavailable classifications require a user choice;
- routing has no filesystem or network side effect;
- route processing cannot import or create a Project 2035 decision;
- JSON CLI output is typed and cross-platform stable.

The Telegram slice is complete only after owner allowlisting, cursor/receipt crash recovery, safe
PDF materialization, token redaction, YouTube canonicalization, and replay tests pass.

Telegram onboarding and pairing are complete only after:

- BotFather remains manual and `mko telegram connect` requires hidden real-TTY token input;
- `getMe` succeeds before any pairing state is proposed or persisted;
- a one-time 128-bit-or-stronger nonce expires within five minutes and cannot be replayed;
- a QR/deep link can pair only the intended private chat and owner sender;
- the real-TTY exact-effects confirmation precedes the test message and durable binding;
- status reveals no secret, disconnect revokes the local binding, and only the primary polling
  device can sync;
- paired General and Finance targets do not create Knowledge approval or Project 2035 decision
  authority.

Required threat cases include token entry echoed by a terminal, config, argv, environment, log,
audit, QR/deep-link, or error; a stolen, expired, or replayed nonce; a QR/deep link opened by a
different Telegram account; group, channel, forwarded, or sender-mismatched messages; a changed
`getMe` identity after token rotation; a second device attempting to poll; cancellation before
confirmation; and a failed test message. Each case must fail closed without a durable pairing or
secret disclosure.
