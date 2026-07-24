# My Knowledge OS Delivery Engine v0.1 — Design Spec

Date: 2026-07-24
Status: draft for review
Target: post-`mko 0.3.0`; `DeliveryPackage` schema version 1
Decision: MKO implements and operates Delivery, while keeping it independent from the Knowledge
domain so Project 2035 can use it as an external client.

## 1. Purpose

MKO currently preserves and reviews durable knowledge but does not proactively deliver an approved
result to a remote reading surface. Project 2035 also needs to send time-bounded review requests
without building a second messaging stack.

The Delivery Engine provides one shared outbound infrastructure:

- deterministic preview rendering;
- source-bound authorization verification;
- an Outbox with retry and duplicate suppression;
- channel policy and classification enforcement;
- Notion, Telegram, and later email/webhook adapters;
- external object IDs and delivery audit records;
- machine-local secret handling.

Delivery never decides whether Knowledge is correct, whether an investment action should be taken,
or whether a source-domain record is approved. Source systems own those decisions.

## 2. Product sequencing

Delivery is post-v0.3 work and must not delay or weaken the v0.3 safety baseline.

1. stabilize MKO v0.3 setup, Source/Knowledge review, and cross-platform behavior;
2. freeze `DeliveryPackage` v1, preview, Outbox, duplicate suppression, and audit contracts;
3. publish approved Personal Knowledge to Notion;
4. send Telegram summary notifications;
5. connect Project 2035 decision-review requests;
6. add subject-routed remote Capture for General and Finance knowledge;
7. add strictly limited inbound responses.

The first implementation gate contains only a console/file adapter. No external API is called until
the contract, authorization verifier, Outbox, and audit tests pass.

## 3. Architecture and ownership

```text
MKO Knowledge domain
  `-- approved Knowledge revision
       `-- MKO DeliveryPackage ---------.
                                           \
Project 2035                                +--> MKO Delivery Engine
  `-- decision-review request ------------'     |-- contract validation
                                                  |-- source authorization verifier
                                                  |-- preview and renderer
                                                  |-- policy and classification gate
                                                  |-- Outbox / retry / idempotency
                                                  |-- audit
                                                  `-- channel adapters
                                                       |-- file / console
                                                       |-- Notion
                                                       `-- Telegram
```

The intended MKO code boundary is:

```text
rust/
|-- mko-core/
|   `-- src/
|       `-- delivery/
|           |-- contracts/
|           |-- outbox/
|           |-- policies/
|           |-- renderers/
|           `-- audit/
|-- mko-cli/
|-- mko-telegram/
|   |-- capture/
|   `-- delivery/
`-- mko-delivery-adapters/
    `-- notion/
```

Network adapters remain outside the deterministic Knowledge modules. `mko-core` owns generic
Delivery state transitions and validation; adapter crates own provider-specific HTTP behavior.
Telegram is a shared channel crate because both inbound Capture and outbound Delivery use the same
provider wire protocol, secret profile, and identity allowlist. Its `capture` and `delivery`
modules remain separate and cannot call each other's authorization or state transitions.

### 3.1 MKO Delivery owns

- `DeliveryPackage` parsing and strict schema validation;
- canonical delivery-intent digest calculation;
- source-authority verification through registered verifier interfaces;
- preview materialization;
- classification-to-channel policy checks;
- target-level idempotency keys;
- durable local Outbox state;
- bounded retry scheduling and permanent-failure classification;
- adapter invocation;
- redacted delivery audit events;
- external object IDs returned by adapters;
- secret references and secure-store integration.

### 3.2 MKO Knowledge owns

- whether a Knowledge revision is eligible for publication;
- the exact immutable revision and content digest;
- the human Review event authorizing delivery;
- the MKO publication policy ID;
- creation of an MKO `DeliveryPackage`;
- revocation or supersession signals for future deliveries.

Delivery cannot create or modify Asset, Source, Knowledge, Review, current-pointer, judgment, or
projection state.

### 3.3 Project 2035 owns

- whether a decision-review request should be sent;
- the exact decision revision and expiry;
- financial classification;
- the safe review-surface reference;
- its own human or policy authorization event;
- creation of a Project 2035 `DeliveryPackage`;
- interpretation of any later feedback candidate.

Project 2035 does not implement channel clients, retry logic, Outbox state, or provider audit.

### 3.4 Adapters own

- provider API request construction;
- provider response parsing;
- provider rate-limit and retry hints;
- external object ID extraction;
- provider-specific redaction of error messages.

Adapters cannot read MKO or Project 2035 domain stores directly. They receive an already rendered,
policy-approved message and a secret handle scoped to one destination profile.

## 4. Non-goals

v0.1 does not:

- approve Knowledge or investment decisions;
- accept Telegram or Notion interaction as an MKO Review approval;
- execute trades or mutate a Project 2035 decision;
- interpret Source/Knowledge/portfolio schemas inside Delivery;
- provide a public multi-tenant service;
- run as a mandatory daemon;
- synchronize Delivery Outboxes between machines;
- guarantee deletion from an external provider after successful delivery;
- provide confidential-at-rest protection against a machine administrator;
- implement arbitrary document-to-message prompting inside an adapter.

## 5. Trust boundaries

Every submitted package is untrusted until all gates pass.

```text
Untrusted client package
  -> strict schema and size validation
  -> canonical intent digest verification
  -> registered SourceAuthority verification
  -> expiry check
  -> classification/channel policy check
  -> deterministic preview
  -> verified authorization bound to the same intent digest
  -> Outbox enqueue
  -> adapter
```

The presence of `approved`, `approval_status`, a package ID, or an agent-authored boolean is never
proof of authorization. The Engine recognizes only the `authorization` object defined by the v1
schema and accepts it only after a source-specific verifier confirms the referenced event.

### 5.1 SourceAuthority interface

Each `source_system` has a registered read-only verifier:

```text
verify_source(source_id, revision_id, content_digest)
verify_authorization(policy_id, event_id, intent_digest, authorized_at)
resolve_safe_link(link_ref)
```

The MKO verifier reads immutable Knowledge and Review records. The Project 2035 verifier reads its
immutable decision/policy events through a bounded CLI or local API. A verifier returns evidence;
it never writes source-domain state.

An unknown `source_system`, missing verifier, changed revision, stale digest, expired event, or
intent-digest mismatch blocks enqueue.

### 5.2 Canonical delivery intent

The intent digest is SHA-256 over RFC 8785-style canonical JSON containing exactly:

- `source_system`;
- `payload_type`;
- `source`;
- `classification`;
- `content`;
- `destinations`;
- `interaction_policy`;
- `expires_at`.

It excludes `package_id`, `created_at`, and the `authorization` object. The authorization event is
bound to this digest. A change to content, destination, renderer, interaction policy, or expiry
therefore requires a new authorization event.

Canonicalization rules and cross-platform golden bytes must be frozen before queue implementation.

## 6. DeliveryPackage v1

The normative contract is
`schemas/delivery/v1/delivery-package.schema.json`.

Required top-level fields:

| Field | Meaning |
|---|---|
| `schema_version` | Contract version; exactly `1` |
| `package_id` | Source-generated opaque correlation ID |
| `source_system` | Registered source authority, for example `mko` or `project2035` |
| `payload_type` | Domain-neutral routing label |
| `source` | Immutable source ID, revision ID, and content digest |
| `classification` | Engine-owned policy input |
| `content` | Bounded title, summary, Markdown body, and safe links |
| `destinations` | Named channel/profile/render targets; never credentials |
| `interaction_policy` | Actions external surfaces may expose |
| `authorization` | Source event bound to the exact intent digest |
| `created_at` | Package creation timestamp |
| `expires_at` | Optional delivery expiry |

The package contains no API token, cookie, provider database ID, filesystem path, Git credential,
or arbitrary executable template.

### 6.1 Source identity

`source.revision_id` is a string because MKO uses content-addressed revision identifiers while
Project 2035 may use a numeric revision encoded as text. `source.content_digest` is always
`sha256:<64 lowercase hexadecimal characters>`.

The Engine rejects reuse of a source ID/revision pair with a different content digest.

### 6.2 Content

`content.body_markdown` is source-authored and bounded. Renderers do not call an LLM. Channel
rendering is deterministic from:

- the package content;
- the named `render_profile`;
- the adapter version.

Links use HTTPS and must pass source-authority and destination-profile allowlists. Document-derived
URLs are not copied automatically. Project 2035 decision messages should link only to its configured
review surface.

### 6.3 Destinations

A destination contains:

- `channel`: one of the contract channels;
- `profile_id`: a machine-local configuration name;
- `render_profile`: a versioned renderer name.

The profile resolves to secrets and remote target IDs locally. Those values never enter the
package. A package may request several destinations; each destination is an independent Outbox
target with its own result.

### 6.4 Interaction policy

External message actions are presentation hints, not domain decisions. v0.1 supports:

- `defer`;
- `remind_later`;
- `feedback_candidate`.

`decision_authority` is fixed to `source_system_only`. Neither `approve`, `buy`, `sell`, nor an
equivalent domain action exists in the shared contract.

For Project 2035 v0.1, message actions are limited to `defer` and `remind_later`; the actual
investment decision is available only on its safe review surface.

### 6.5 Authorization

Every package carries one verified source event:

- `human_event`: a person approved this exact delivery intent in the source system;
- `source_policy_event`: an explicit source-owned policy authorized this exact delivery intent.

Both modes require `policy_id`, `event_id`, `authorized_at`, and the exact `intent_digest`. A policy
event is not an authorization bypass; the SourceAuthority must prove that the policy was active,
applicable to the classification and destinations, and bound to the same digest.

## 7. Classification and channel policy

Initial classifications are:

| Classification | Default external delivery |
|---|---|
| `public` | allowed by configured profile |
| `personal` | allowed to owner-scoped profiles |
| `personal_financial` | owner-scoped profiles; no domain-decision actions |
| `work` | denied |
| `restricted` | denied |

Default deny applies when:

- the classification is unknown;
- a destination profile is missing;
- the source verifier is unavailable;
- a channel has no explicit policy;
- a secret is unavailable;
- an adapter is not enabled.

MKO v0.1 emits only approved `personal` Knowledge. Work/Shared promotion and publication require a
separate policy design.

## 8. Preview and authorization UX

The preview is a deterministic artifact containing:

- source system, source ID, and exact revision;
- classification;
- each destination profile;
- renderer version;
- rendered title/body excerpt;
- safe links;
- interaction policy;
- expiry;
- intent digest;
- warnings and blocked-policy reasons.

The user approves the displayed delivery intent in the source system, not in the adapter. If any
previewed field changes, the intent digest changes and the prior authorization is invalid.

MKO's intended flow is:

```text
approved Knowledge revision
  -> mko delivery preview <knowledge-id> --dest notion:personal telegram:personal
  -> display exact preview and intent digest
  -> source-domain delivery authorization
  -> mko delivery enqueue <package>
```

This approval is separate from Knowledge Review approval. Approving Knowledge does not implicitly
authorize external publication unless an explicit source policy says so and emits a verifiable
policy event.

## 9. Outbox state machine

Each package is expanded into one target per destination.

```text
received
  |-- invalid
  `-- awaiting_authorization
         |-- expired
         |-- cancelled
         `-- queued
                `-- delivering
                       |-- delivered
                       |-- retry_wait --> delivering
                       `-- failed_permanent
```

Source verification, authorization, expiry, and policy checks occur again immediately before the
first delivery. Retry does not bypass revalidation of expiry and target policy.

### 9.1 Idempotency

The target idempotency key is SHA-256 over:

```text
source_system
source.id
source.revision_id
source.content_digest
channel
profile_id
render_profile
```

A delivered target with the same key is returned as `already_delivered`. Retrying a transient
failure reuses the same target and attempt sequence. A new renderer or destination profile creates
a new key and therefore requires authorization through the changed intent digest.

Provider-native idempotency keys are used when supported. Otherwise the Engine records the attempt
before the network call and reconciles ambiguous outcomes using provider lookup where available.
An ambiguous outcome is never blindly resent.

### 9.2 Retry policy

Retries are bounded and use exponential backoff with jitter. Adapter errors are normalized as:

- `transient`;
- `rate_limited` with optional retry time;
- `authentication`;
- `policy`;
- `payload_invalid`;
- `ambiguous_outcome`;
- `permanent`.

Authentication, policy, invalid payload, and permanent errors do not retry automatically. Retry
limits and time windows are configuration, not package fields.

## 10. Persistence and confidentiality

The v0.1 Outbox is machine-local and excluded from Git, Google Drive, and Knowledge projections.
It may use SQLite plus owner-only files.

Before enqueue, the package and rendered payload may contain personal or financial information.
Therefore:

- files and database are owner-only;
- payload is removed after the configured delivered-retention window;
- durable audit keeps digests and metadata, not full message bodies;
- logs redact titles, summaries, links, tokens, and provider response bodies;
- crash dumps must not include secrets;
- adapters receive secrets through handles rather than serialized packages.

Owner-only permissions do not protect against the machine administrator or malware running as the
user. Encrypted-at-rest Outbox storage is a later hardening item and must be completed before
supporting classifications beyond `personal` and `personal_financial`.

## 11. Secret management

Secret precedence is:

1. OS secure store (macOS Keychain, Windows Credential Manager, Linux Secret Service);
2. process environment for development and CI only;
3. no plaintext configuration-file fallback.

Destination profiles store only a secret reference and non-secret remote target identifiers.
Secret values never appear in package JSON, CLI arguments, standard output, audit records, Git,
Markdown, YAML, Notion content, or Telegram content.

The Engine supports a read-only credential check and never prints the credential value.

## 12. Adapter contract

The adapter interface receives:

```text
send(rendered_message, destination_profile, idempotency_key, secret_handle)
lookup(idempotency_key or external_object_id)
```

It returns:

```text
delivery_state
external_object_id?
provider_request_id?
retry_hint?
redacted_error?
```

The adapter cannot:

- request a new source authorization;
- modify package content;
- interpret Knowledge or investment fields;
- add interactive domain actions;
- read another destination profile;
- write source-domain state.

### 12.1 Notion

The initial Notion renderer publishes the full approved Personal Knowledge note to an owner-scoped
database/page and records the resulting page ID. Updating an existing page is a separate target
operation with a new revision and authorization; it is not an implicit overwrite.

### 12.2 Telegram

The initial Telegram renderer sends a concise title, summary, classification-safe metadata, and an
optional safe link. It does not include approval or trade buttons. Oversized content is truncated
deterministically with a link to the configured reading surface.

## 13. Audit

Audit events are append-only and contain:

- event ID and timestamp;
- package ID;
- source system, source ID, revision ID, and content digest;
- intent digest;
- target idempotency key;
- channel and profile ID;
- state transition;
- attempt number;
- adapter version;
- external object ID when available;
- normalized result/error code.

Audit does not contain message bodies, credentials, authorization phrases, provider raw responses,
or user financial decisions.

Delivery audit is not an MKO Review event and cannot change Knowledge or Project 2035 state.

## 14. Client surfaces

The first local CLI surface is:

```text
mko delivery validate <package.json>
mko delivery preview <package.json>
mko delivery enqueue <package.json>
mko delivery status <package-id>
mko delivery retry <package-id> --target <target-id>
mko delivery cancel <package-id>
mko delivery credentials check <profile-id>
```

Project 2035 may invoke the same CLI with a package file in an owner-only temporary directory. A
later local API or queue must preserve the identical schema, authorization verifier, and Outbox
gates. No API endpoint may accept an `approved=true` shortcut.

CLI machine output follows a separately versioned strict JSON envelope. Human prose is never parsed
by a client.

## 15. MKO integration

MKO package generation requires:

- a current immutable Knowledge revision;
- an approved Knowledge Review event;
- Personal scope and `personal` classification;
- a separate delivery authorization event or applicable explicit publication policy;
- deterministic source rendering.

The package includes both grounded Knowledge and clearly labelled LLM analysis. Renderer templates
must preserve this distinction. Delivery cannot collapse LLM interpretation into source-grounded
facts.

Delivery success does not mark Knowledge as approved, published, promoted, or synchronized. MKO may
show a derived delivery history, but canonical Knowledge state remains unchanged.

## 16. Project 2035 integration

Project 2035 submits:

- `source_system: project2035`;
- `payload_type: decision_review_request`;
- exact decision revision and digest;
- `personal_financial` classification;
- expiry;
- safe review-surface link;
- a verifiable Project 2035 authorization event;
- no executable trade action.

The Delivery Engine verifies the source and sends the review request. A later `defer` or
`remind_later` response may become an untrusted feedback candidate returned to Project 2035. It
cannot approve, reject, buy, sell, rebalance, or mutate the source decision.

## 17. Failure and recovery

Every failure reports:

- whether any destination succeeded;
- the exact failed target;
- whether retry is safe;
- whether authorization or expiry must be renewed;
- one next action.

Partial success is preserved. A successful Notion target is not repeated because Telegram failed.
Cancelling a package stops undelivered targets but does not delete an already-created external
object.

Changed source bytes, stale authorization, or expired packages require regeneration from the
source system. Delivery never edits the package to make it pass.

## 18. Acceptance criteria

The DeliveryPackage/Outbox milestone is complete when:

- the v1 schema rejects unknown fields, credentials, plain approval flags, and unsupported actions;
- MKO and Project 2035 examples validate;
- canonical intent bytes and digest are identical on macOS, Windows, and Linux;
- a changed destination, renderer, content byte, link, interaction action, or expiry invalidates
  authorization;
- an unknown or unavailable SourceAuthority blocks enqueue;
- an MKO verifier proves the exact approved Knowledge revision;
- a Project 2035 verifier proves the exact decision/policy event;
- `work` and `restricted` are denied by default;
- target idempotency prevents a duplicate provider call;
- retries preserve partial success and never blindly resend an ambiguous outcome;
- audit contains no content body or credential;
- file/console adapter end-to-end tests pass before network adapters are enabled;
- Delivery has no write path to MKO Review/Knowledge or Project 2035 decision state.

The Notion milestone additionally requires exact-page duplicate suppression and provider-object ID
reconciliation. The Telegram milestone additionally requires deterministic truncation, owner chat
allowlisting, and no domain approval actions.

## 19. Implementation sequencing

### Phase A — Contract freeze

1. freeze JSON Schema, positive/negative fixtures, canonical intent field set, and size limits;
2. add Rust DTOs with `deny_unknown_fields`;
3. add cross-platform canonical digest goldens;
4. define `SourceAuthority`, renderer, adapter, and secret-store traits.

### Phase B — Local deterministic engine

1. add machine-local Outbox and audit store;
2. implement state transitions, idempotency, expiry, and bounded retry;
3. add console/file adapter;
4. add CLI validation, preview, enqueue, status, retry, and cancel;
5. run concurrency, crash-recovery, and secret-redaction tests.

### Phase C — MKO publication

1. add MKO Knowledge source verifier;
2. add exact-revision delivery authorization;
3. add Personal Knowledge renderer;
4. integrate Notion;
5. integrate Telegram.

### Phase D — Project 2035 client

1. freeze its decision-review package profile;
2. add read-only SourceAuthority adapter;
3. add safe review links and expiry handling;
4. verify that no message interaction can change investment state.

### Phase E — Limited inbound responses

1. authenticated callback identity;
2. nonce/replay and expiry protection;
3. `defer`, `remind_later`, and feedback-candidate only;
4. explicit source-system ingestion and human review.

No phase may weaken the v0.3 real-TTY Knowledge approval boundary or introduce a Delivery-to-domain
write capability.

## 20. Remote Capture companion boundary

Telegram documents, PDFs, YouTube URLs, videos, audio, links, and text are inbound Capture inputs,
not `DeliveryPackage` values. They use a separately versioned `CaptureEnvelope` and Capture job
state machine. Delivery and Capture may share a Telegram client, secret profile, identity
allowlist, audit primitives, and bounded retry library, but they do not share authorization or
domain-state transitions.

### 20.1 Subject routing

Routing is based on the subject of the material, not its media type:

| Subject | Responsible knowledge scope |
|---|---|
| stocks, companies, markets, investing, macroeconomics, economics | `finance` |
| lectures, recipes, books, engineering, climbing, hobbies, general learning | `general` |

A finance lecture remains `finance`; a PDF recipe remains `general`. Material containing both
subjects is not split or written automatically in v0.1. It becomes `routing_confirmation_required`.

The user may select the scope explicitly with a channel command or UI action:

```text
/finance <file-or-url>
/general <file-or-url>
```

An explicit user selection wins if it passes policy. When the user does not select a scope, a
classifier may create a non-authoritative routing proposal. The proposal cannot create an Asset,
Source, Knowledge, or Project 2035 decision by itself.

The safe default is:

- high-confidence General proposal: show the proposed route and require one owner confirmation
  before durable processing;
- Finance proposal: require explicit Finance confirmation before durable processing;
- mixed, low-confidence, or conflicting proposal: require explicit General/Finance choice;
- unavailable classifier: require explicit choice.

The Core stores the confirmed scope and routing authority (`user_selected` or
`user_confirmed_proposal`) in the Capture audit. It does not treat any LLM-generated scope field as
human confirmation. A later release may allow high-confidence General auto-staging after measured
false-routing rates, but it cannot auto-publish Knowledge.

### 20.2 General route

General Capture covers lectures, recipes, books, papers, engineering, climbing, hobbies, and other
non-financial learning material. The normal pipeline is:

```text
Telegram/file/URL
  -> quarantine and identity checks
  -> General Inbox
  -> Asset
  -> Source summary
  -> explicit Knowledge question
  -> pending General Knowledge
```

Domain-specific policies such as medical or legal caution may still apply inside General. General
does not mean unreviewed or low-risk.

### 20.3 Finance route

Finance Capture covers stock reports, company filings, market commentary, investment lectures,
macroeconomics, economics, and financial research. The normal pipeline is:

```text
Telegram/file/URL
  -> quarantine and identity checks
  -> Finance Inbox
  -> Asset
  -> Source summary
  -> Finance Knowledge with counterargument and open-question gates
  -> pending human review
```

Finance Capture produces knowledge, not an investment decision. It cannot:

- create a buy/sell/rebalance action;
- alter a portfolio;
- create or approve a Project 2035 decision;
- mark information as current without source date/freshness evidence;
- deliver financial content through a non-Finance destination profile.

Project 2035 may later reference an approved Finance Knowledge revision through an explicit,
read-only link. Turning that evidence into a decision remains wholly Project 2035's responsibility.

### 20.4 URL and media safety

URLs are untrusted. Initial URL Capture supports only explicitly allowlisted providers such as
YouTube, canonicalizes the URL, bounds redirects, and never copies page instructions into a
command. A YouTube Capture stores the original URL, stable provider identifier, metadata snapshot,
transcript provenance, and timestamped evidence blocks.

Video and audio processing additionally records:

- original Asset fingerprint;
- extractor/transcription model and version;
- language;
- timestamp ranges;
- low-confidence or missing-transcript regions;
- derived transcript digest.

Large or unsupported media remains a blocked Capture item with one recovery action. It is never
silently downloaded, transcoded, or uploaded to another provider.

### 20.5 Operational modes

The first implementation is pull-based:

```text
mko capture telegram sync
```

Messages may be sent to Telegram at any time and are fetched when an authorized MKO device runs the
sync. This requires no always-on server. A later Capture Worker may provide immediate
acknowledgement and background processing, but it must preserve the same identity, routing,
quarantine, idempotency, and review contracts.

The Remote Capture contract, schema, and implementation plan are a separate follow-up deliverable.
They cannot expand the Delivery Engine's authority over source-domain state.
