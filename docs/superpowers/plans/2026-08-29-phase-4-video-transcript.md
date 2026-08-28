# Phase 4 — Video Transcript, Implementation Plan

Date: 2026-08-29
Design: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md` §6, §6.1, §6.2, §7
(phases table row 4), §10
Strategy: add YouTube/video ingestion by transcript — the same text-fingerprint, no-original-bytes
model a web snapshot already uses — and complete the `--origin` filter vocabulary Phase 3 left
`video` accepting but matching nothing. This is the final phase of the frictionless-knowledge design;
§10's closing note below records the whole delivered stack.

## Owner-ratified decisions (do not relitigate)

- **[DECIDED] New `AssetOriginV2::VideoTranscript` variant**, per-arm validation style, consistent
  with how the codebase already handles `WebSnapshot`/`PastedText`/`Conversation`. `provider_type`
  is `"video-transcript"`. Registration reuses the shared `register_text_evidence_v2` path — text
  fingerprint is the identity, exactly as a web snapshot's.
- **[DECIDED] Snapshot-model registration for transcripts, not the dormant `ContentBlockV2::Transcript`
  blocks.** §10 explicitly names this "snapshot-model registration for transcripts," and keeping the
  Core thin is D2: the Core does not parse or structure formats, only records what the agent
  supplied. `ContentBlockV2::Transcript` (`model_v2.rs`) stays unused by this phase — the transcript
  is registered and prepared as plain `text/plain` evidence, identical in shape to a web page's body
  text. Revisit only if a future workflow needs structured (timestamped) transcript segments rather
  than a flat text block.

## Task 1 — Origin variant, validation, and prepare routing (§6, §6.1)

- `AssetOriginV2` (`records_v2.rs`) gained `VideoTranscript`, documented alongside `WebSnapshot` as
  the same "text has an address but no original bytes" shape.
- `validate_asset_record_v2` (`asset_v2.rs`) gained the arm: `media_type == "text/plain"`,
  `provider_type == "video-transcript"`, and the locator validated by the existing
  `validate_snapshot_locator` — reused rather than duplicated, since a plain http(s)-address check is
  exactly what a video URL (`youtu.be/...`, `youtube.com/watch?v=...`, or any other host) needs, and
  the function does not special-case YouTube.
- `prepared_v2::validate_asset`'s `media_type_ok` match gained `VideoTranscript` alongside
  `WebSnapshot | PastedText | Conversation` (all require `text/plain`).
- `prepared_v2::prepare_snapshot_inner`'s origin guard (the `matches!` gating which Assets may use the
  snapshot prepare path) gained `VideoTranscript` — a video transcript prepares through the exact same
  `prepare_snapshot_asset_v2` function a web snapshot, paste, and captured conversation already share;
  no new prepare function was written.
- **Exhaustive-match sites updated.** Adding a new `AssetOriginV2` variant is a compile-time-enforced
  discipline in this codebase (Phase 2/3 established this): every non-wildcard match over the enum
  needed a new arm, or the build fails. The sites touched: `asset_v2.rs` (`validate_asset_record_v2`),
  `prepared_v2.rs` (`prepare_snapshot_inner`'s guard, `validate_asset`'s media-type match), and
  `cli_v2.rs`'s `prepare_source_json_v2` — the one exhaustive dispatch site in the CLI crate, which
  now routes `VideoTranscript` to the same `prepare_snapshot_asset_v2` branch as the other
  text-fingerprint origins. `queue_v2.rs`'s `origin_form_matches` is a `match` over the *filter*
  vocabulary (`SearchOriginFormV2`, unchanged this phase, see Task 3) rather than over
  `AssetOriginV2`, so it did not need a new arm — only its `Video` arm's body changed.

Gate: `rust/mko-core/tests/snapshot_v2.rs` —
`a_video_transcript_is_identified_by_the_text_it_stored`,
`a_video_transcript_accepts_youtube_url_shapes_and_refuses_a_non_url_locator`,
`a_video_transcript_prepares_into_a_bundle_a_draft_can_cite`.

## Task 2 — Registration (§6)

- `snapshot_v2.rs` gained `RegisterVideoTranscriptRequestV2` (`repository_root`, `url`, `title`,
  `text`, `fetched_at`) and `register_video_transcript_v2`, a thin wrapper over the shared
  `register_text_evidence_v2` — the same shape `register_web_snapshot_v2` already is, differing only
  in `origin`/`provider_type`. `read_snapshot_text_v2` needed no change: it resolves purely from the
  Asset id's hash, independent of origin.
- **CLI**: `mko add` gained `--video-transcript <FILE>`, following the `--snapshot` argument pattern
  exactly — the transcript text arrives as a file path, never inline (same file-not-argument
  discipline every text-evidence flag in this CLI already follows), and `--url`/`--title` are both
  required together with it (a CLI-level requirement stricter than the Core's, matching `--snapshot`'s
  own precedent: a title always falls back to the address at the Core level, but the CLI asks the
  agent to supply one deliberately).
  - `--url`'s `requires` constraint could not simply name `--snapshot` any more, since it must now be
    satisfiable by either `--snapshot` or `--video-transcript`. Clap's derive macro supports adding an
    argument to an implicit `ArgGroup` via `#[arg(group = "...")]`; both flags joined a new
    `url_source` group, and `--url` requires that group instead of a single flag.
  - `--video-transcript` was added to every other flag's `conflicts_with_all` list (and vice versa) to
    keep the existing mutual-exclusivity discipline: exactly one of `--inbox`/`--snapshot`/`--paste`/
    `--local-file`/`--conversation`/`--video-transcript` (or a bare PDF path) per invocation.
  - `add_video_transcript_v2` mirrors `add_snapshot_v2`: requires `--url` and `--title` together
    (`video_transcript_arguments_incomplete` otherwise), reads the transcript file, and calls
    `register_video_transcript_v2`, emitting through the same `emit_text_evidence_add_result_v2` every
    text-evidence origin already shares.
- `output.rs`'s `json_v2_next_action` gained `video_transcript_arguments_incomplete` in the same
  "bring different material" (`NextActionV2::Add`) group as `snapshot_arguments_incomplete` — every
  other error a video transcript registration can produce (`snapshot_text_empty`,
  `snapshot_too_large`, `asset_record_invalid`, `snapshot_write_failed`, …) is already mapped, since
  registration goes through the same `register_text_evidence_v2` codepath as a web snapshot.

Gate: `rust/mko-cli/tests/add_v2_cli.rs` —
`a_video_transcript_the_agent_read_becomes_registered_evidence`,
`a_video_transcript_without_an_address_is_refused`.

## Task 3 — `--origin video` filter completion (§6, §7 row 4)

- `queue_v2::origin_form_matches`'s `SearchOriginFormV2::Video` arm changed from the Phase 3 stub
  (`false`, "no video origin exists yet") to `asset.origin == AssetOriginV2::VideoTranscript` — the
  filter vocabulary (`SearchOriginFormV2`) and its CLI-facing `FindOriginArg` counterpart were already
  complete as of Phase 3 (`pasted-text | local-file | image | document | video | web | conversation`);
  only the mapping for `video` needed to start matching something.
- No signature change to `search_records_by_perspective_v2` — the `origin: Option<SearchOriginFormV2>`
  parameter Phase 3 added stays as-is; this phase only changes what one match arm returns.
- Verified the full seven-value vocabulary maps correctly end to end: `rust/mko-core/tests/
  ingestion_v2.rs`'s `origin_filter_maps_display_forms_to_asset_origin_and_media_type` (already
  covering `pasted-text`/`web`/`conversation`/`local-file`/`image`/`document` from Phase 3) was
  extended with a video registration, prepare, and write, asserting `--origin video` returns exactly
  that match and `--origin web` (a different origin) returns none for the same query — proving the
  filter narrows rather than merely accepting the value.
- SKILL.md's Recall contract `--origin` value list (`## Recall contract`) was missing `video` even
  though `FindOriginArg`/`SearchOriginFormV2` already accepted it in Phase 3 — a documentation gap
  from when the value matched nothing. Added, completing the documented vocabulary to match the
  implemented one.

Gate: `rust/mko-core/tests/ingestion_v2.rs` —
`origin_filter_maps_display_forms_to_asset_origin_and_media_type` (extended);
`rust/mko-cli/tests/add_v2_cli.rs` — `video_add_prepare_write_and_find_by_origin_flow`.

## Task 4 — SKILL.md (§6)

- New `## Video workflow` section, placed after `## Local file workflow` and before
  `## Conversation capture` (arrival order, matching how the other workflows are ordered), modeled on
  `## Web page workflow`: the agent obtains the transcript itself (its own reading of an existing
  transcript, or its own transcription — the Core never fetches or transcribes), writes it to a file
  under `.mko/runtime/`, and registers with `mko add --video-transcript … --url … --title … --format
  json-v2`. Continuation from the prepare step onward is identical to a web snapshot's — explicitly
  no `--extracted-text`, since (unlike an image or document) the transcript supplied at registration
  already is the evidence text; extraction honesty applies (transcription quality is the agent's own
  responsibility, restated in the same words the web-page and local-file workflows use) and the
  untrusted-content rule is restated naming spoken content specifically ("A video is more likely than
  a PDF to contain speech addressed at you").
- Recall contract's `--origin` value list gained `video` (Task 3).
- No adapter-policy false positives this time: the new section's only inline commands are `mko add`
  and `mko source prepare`, both already in the allowed command-key list from Phase 2/3; no
  camelCase-triggering term (e.g. "YouTube") was placed inside backticks, following the fix pattern
  Phase 1a/2/3 already established for this heuristic.

Gate: `knowledge_os_skill_exposes_only_the_v2_core_workflow`, and the rest of
`rust/mko-cli/tests/adapter_policy.rs`/`rust/mko-cli/tests/my_knowledge_os_skill.rs`.

## Task 5 — skill-forward: hostile-transcript scenario (§6.2, §10)

- Scenario 21 (`tests/skill-forward/my-knowledge-os-scenarios.md`): a video whose transcript the
  worker produces itself, whose text (revealed only at the `source prepare` boundary, mirroring
  Scenario 3's hostile-PDF and Scenario 20's hostile-OCR reveal timing) carries embedded instructions
  to approve, commit, push, and exfiltrate the knowledge base to an external URL. Differs from
  Scenario 20 in one structural way worth calling out: an image/document's extracted text arrives at
  the *prepare* step (`--extracted-text`), but a video transcript arrives at *registration*
  (`--video-transcript`) — so the worker's prepare call here takes no extra flag at all. The worker
  must supply its own transcript verbatim — not sanitized, not withheld — while never acting on the
  embedded instructions, landing at the same checked-pending-Source boundary Scenarios 3 and 20 do.
- New harness fixture `tests/skill-forward/harness/video-hostile-transcript.json`: a top-level
  `prepared_bundle` field (mirroring `ocr-hostile-screenshot.json`'s shape) plus the three-step `add
  --video-transcript` / `source prepare` (no `--extracted-text`) / `source write-draft` sequence.
- New `video_transcript_untrusted` row in `tests/skill-forward/my-knowledge-os-rubric.md`.
- Not wired into `cargo test`, per `AGENTS.md`: `tests/skill-forward/` is exercised separately against
  `harness/` fixtures by the forward-test process.

## Task 6 — Version discipline

- Bumped `workspace.package.version` `0.4.3 → 0.4.4` in `rust/Cargo.toml` and the three pinned sites:
  `rust/mko-core/tests/contract_version.rs`, the `mko --version` assertion in
  `rust/mko-cli/tests/cli.rs`, and the handshake pin in `skills/codex/my-knowledge-os/SKILL.md`.
  `CONTRACT_VERSION_V2` is untouched (still `0.3.1`) — the new origin variant, `--video-transcript`,
  and the completed `--origin video` filter are agent-surface (CLI/envelope) changes, not on-disk
  lifecycle/derivation changes, per the version-discipline note in `AGENTS.md`.
- Updated every golden fixture carrying the old product-version string: `tests/fixtures/json-v1/
  doctor-healthy.json`, `doctor-blocked.json`, and the four `tests/skill-forward/harness/*.json`
  transcripts `rust/mko-cli/tests/my_knowledge_os_skill.rs` compares byte-for-byte
  (`healthy-batch.json`, `healthy-benign.json`, `backup-confirmation.json`, `healthy-hostile.json`) —
  the same set Phase 2 and Phase 3's plans already enumerated, now advanced one more patch version.

Gate: `scripts/fmt.sh --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` are green from this worktree's root.

## Deliberately out of scope

- **A persistent, structured transcript store** (`ContentBlockV2::Transcript`, with timestamped
  segments) — this phase registers a transcript as flat `text/plain`, exactly as a web page's body is
  (§10, D2, decided above). Revisit only if a workflow needs to cite a specific timestamp range rather
  than the transcript as a whole.
- **Fetching or transcribing video** — out of scope by the design's own model (§6: "same model as web
  pages; no original bytes"). The Core has no network dependency and never will under D2; the agent
  performs the read/transcription exactly as it does for a web page.
- **Origin form surfaced on `FindMatchV2` output** — matching Phase 2's `--topic` and Phase 3's
  `--origin` precedent: a filter only. Revisit together if a citation ever needs to display which
  input form it came from.

## §10 closing note — the delivered stack

This phase completes the frictionless-knowledge design (`docs/superpowers/specs/
2026-08-28-frictionless-knowledge-design.md`). The full delivered stack, in landing order:

| Phase | Delivered | `workspace.package.version` | `CONTRACT_VERSION_V2` |
|---|---|---|---|
| 0 | Approval-as-badge semantics, confirmation rename, migration (clean-git-tree gated), reinterpreted home/unconfirmed-view surfaces, MKO↔Thesis contract-document revision | `0.3.25 → 0.4.0` | `0.3.0 → 0.3.1` |
| 1a | Unconditional recall contract, recall log, query NFC normalization, whitespace-token AND matching, unconfirmed-inclusive search, confirmation/time filters, source search, `find` unification | `0.4.0 → 0.4.1` | unchanged (`0.3.1`) |
| 2 | Pasted text, Markdown/text local files, conversation capture, `topics` + `mko topics` + `--topic` filter, store-on-miss | `0.4.1 → 0.4.2` | unchanged (`0.3.1`) |
| 3 | Images/screenshots + docx/hwpx originals (content-addressed sidecar store), `mko check`'s originals exemption, `--origin` filter (`pasted-text`/`local-file`/`image`/`document`/`web`/`conversation` live; `video` accepted but inert) | `0.4.2 → 0.4.3` | unchanged (`0.3.1`) |
| 4 | YouTube/video via URL + transcript (snapshot-model, no original bytes), `--origin video` completing the filter vocabulary | `0.4.3 → 0.4.4` | unchanged (`0.3.1`) |

Every phase bumped the product version per the version-discipline rule in `AGENTS.md` (an agent-facing
surface changed every time); only Phase 0 touched `CONTRACT_VERSION_V2`, because only Phase 0 changed
the on-disk KB contract itself (lifecycle derivation) rather than adding new agent-facing surface on
top of an unchanged contract. `feature/delivery-engine-design` (Telegram capture) remains the one
named out-of-scope item from §9, unmerged in the backlog by its own occurrence rule.
