# Phase 3 — Binary Originals, Implementation Plan

Date: 2026-08-29
Design: `docs/superpowers/specs/2026-08-28-frictionless-knowledge-design.md` §6, §6.1, §6.2, §7
(phases table row 3), §8's named check-budget risk
Strategy: extend Phase 2's text-only originals store (`assets/originals/`) to bounded binaries —
images (screenshots, photos of pages) and docx/hwpx documents — without adding a new
`AssetOriginV2` variant, resolve the `mko check` byte-budget collision §8 named as a risk, and give
`mko find` an `--origin` filter across every input form that now exists. `.hwp` 5.x is deferred to
the backlog after the feasibility check §7 required.

## Task 1 — `LocalFile` media-type table (§6.1, decided: enum shape)

- **[DECIDED] No new `AssetOriginV2` variant.** `LocalFile` already covers Phase 2's text form;
  Phase 3 generalizes it rather than adding `Image`/`Document` variants, per the owner's ratified
  decision. `rust/mko-core/src/local_file_v2.rs` gained a small table
  (`LOCAL_FILE_MEDIA_TYPES: &[(&str, LocalFileMediaSpecV2)]`) mapping a recognized extension to its
  `media_type`, per-form `max_bytes` ceiling, and `LocalFileMediaKindV2` (`Text | Image | Document`):
  `md`/`markdown`/`txt` → `text/plain` (2 MiB, unchanged from Phase 2); `png` → `image/png`,
  `jpg`/`jpeg` → `image/jpeg`, `webp` → `image/webp`, `heic` → `image/heic` (10 MiB each); `docx` →
  `application/vnd.openxmlformats-officedocument.wordprocessingml.document`, `hwpx` →
  `application/vnd.hancom.hwpx` (15 MiB each). An unrecognized extension (or none) falls back to the
  Text/`.txt` entry, exactly as Phase 2 did — for a genuinely binary file with an unrecognized
  extension, that fallback simply means the UTF-8 check rejects it, a clear if generic refusal
  rather than a silent guess at an unsupported binary format.
- **[DECIDED] Per-form size ceilings.** Images 10 MiB, docx/hwpx 15 MiB, enforced at registration by
  bounding the very read of the file to `spec.max_bytes` (`read_bounded_nofollow`) — a file past its
  ceiling is refused before its bytes are ever fully read, not truncated and rejected after the
  fact.
- **Signature validation**, following `fingerprint::validate_pdf_content`'s magic-bytes precedent:
  PNG `\x89PNG`, JPEG `\xFF\xD8\xFF`, WebP `RIFF....WEBP` (bytes 0-3 `RIFF`, bytes 8-11 `WEBP`), HEIC
  an ISO-BMFF `ftyp` box check (bytes 4-7 `ftyp`, brand at 8-11 in a small known-brand allowlist:
  `heic`/`heix`/`hevc`/`hevx`/`heim`/`heis`/`hevm`/`hevs`/`mif1`/`msf1`), docx/hwpx the zip
  local-file-header signature `PK\x03\x04`. A mismatch is `local_file_signature_invalid`, mapped to
  `NextActionV2::Add` in `output.rs` (bring different material, not a retry).
- **`assets/registry`'s `validate_asset_record_v2`** (`asset_v2.rs`) and **`prepared_v2::validate_asset`**
  both had their `LocalFile` arm generalized from `media_type == "text/plain"` to
  `is_known_local_file_media_type(&asset.media_type)`, reusing the same table.
- **Extension recovery is locator-based, not media-type-based** — a bug caught by the round-trip
  test before it shipped. `text/plain` is shared by three extensions (`.md`, `.markdown`, `.txt`), so
  reconstructing "which extension was this filed under" from `media_type` alone is ambiguous;
  `read_original_bytes_v2` instead re-derives the spec from the Asset's own recorded
  `provider.logical_locator` path (its real extension, captured verbatim at registration), the same
  approach Phase 2's `extension_from_locator` already used.

Gate: `rust/mko-core/tests/ingestion_v2.rs` — `image_local_file_is_signature_validated_and_round_trips_through_the_originals_store`,
`a_docx_local_file_is_signature_validated_by_its_zip_header`,
`an_image_whose_bytes_do_not_match_its_extension_is_rejected`,
`an_image_past_its_size_ceiling_is_rejected_before_it_reaches_the_originals_store`.

## Task 2 — Extraction is supplied at prepare time, not registration time (§6.2, D2)

- **[IMPLEMENTATION CHOICE, not owner-ratified but the natural reading of D2 + the PDF precedent.]**
  The design brief describes registration as where "the agent supplies the extracted text exactly as
  for text files." For a *text* local file the original bytes are simultaneously the identity and
  the evidence, so no separate text ever needs to be supplied. An image or document has no text of
  its own — the Core cannot decide at registration time whether a caller will ever prepare it, or
  with what quality of OCR/conversion, so storing extracted text as a second persistent artifact
  keyed to the Asset would need its own revisioning story. The PDF path already solves exactly this
  shape: an extractor supplies `pages: Vec<String>` at *prepare* time, ephemeral to that one prepared
  bundle, never persisted outside the 24-hour local runtime session. Phase 3 follows the same shape
  for images/documents: registration (`register_local_file_asset_v2`) stores only the
  signature-validated original bytes; `prepared_v2::prepare_local_file_asset_v2` gained an
  `extracted_text: Option<&str>` parameter — required (and non-empty) for `Image`/`Document` kinds,
  forbidden for `Text` kind (a text local file's original already *is* its text; accepting a second,
  different text there would silently discard whichever one the caller thought was in effect). The
  original is still re-read and re-hashed against its own registered fingerprint on every prepare
  call, even though its bytes never become bundle content — so a damaged or tampered original is
  still caught.
- **Re-extraction is simply another prepare call, no new Asset-level bookkeeping.** Calling prepare
  again with different supplied text (a better OCR pass) yields a different prepared bundle (a
  different `bundle_id`, since the bundle's content digest covers `content_blocks`) bound to the
  *same* Asset (same `asset_id`, same original-bytes fingerprint — nothing about the Asset changes).
  Writing that new bundle through `write_source_record_v2`/`write_knowledge_record_v2` with
  `expected_revision` set to the prior revision lands as `RecordWriteOutcomeV2::Replaced` — the
  existing revision-replacement mechanism every origin already has, requiring zero new Core
  machinery for "re-extraction as a new revision, never a duplicate Asset."
- **CLI wiring.** `mko source prepare` gained `--extracted-text <path>` (a file, not an argument —
  the same file-not-argument discipline `--paste`/`--snapshot`/`--conversation` already follow).
  `cli_v2.rs`'s `prepare_source_json_v2` gained the same parameter, rejecting it early
  (`local_file_extracted_text_not_applicable`) for every origin except `LocalFile`, before routing —
  the same "exhaustive match, no origin silently mis-routed" discipline Phase 2 established for the
  origin dispatch itself.

Gate: `rust/mko-core/tests/ingestion_v2.rs` — `prepare_requires_supplied_text_for_an_image_and_refuses_it_for_a_text_local_file`,
`re_extraction_with_different_supplied_text_replaces_the_source_revision_of_the_same_asset`;
`rust/mko-cli/tests/add_v2_cli.rs` — `source_prepare_requires_extracted_text_for_an_image_and_builds_a_bundle_from_it`.

## Task 3 — `mko check`'s originals exemption (§8, decided)

`check.rs`'s existing byte budget (2 MiB per file, 32 MiB aggregate, applied to every walked file
including `assets/originals/*`) would reject the first committed binary original — the risk §8 named
explicitly and required this phase to resolve.

- **[DECIDED] Exemption, not a raised budget.** Files under `assets/originals/` are exempt from the
  text-oriented secret scan, the conflict-marker scan, the 2 MiB per-file cap, and the 32 MiB
  aggregate cap — both in the working-tree walk (`collect_directory`) and the staged walk
  (`staged_files`). They are still read bounded (to `MAX_LOCAL_ORIGINAL_BYTES`, the largest of the
  three per-form ceilings — 15 MiB — since the walk does not yet know which specific ceiling
  applies), so a walk still cannot be made to read an unbounded file.
- **[DECIDED] What replaces the exemption: `inspect_originals`.** A new pass parses every
  `assets/registry/*.json` v2 Asset record present in the same walk to collect the set of known
  fingerprint hashes (any origin — a hash present there is "referenced," which is sufficient and
  simpler than filtering by `LocalFile` origin specifically, since only `LocalFile` ever produces an
  originals-store entry in practice). For each `assets/originals/<hash>.<ext>` entry: the filename
  must parse as a 64-character lowercase hex hash plus a recognized extension
  (`asset_original_invalid` otherwise); its size must sit within that extension's table ceiling
  (`asset_original_too_large`); its actual SHA-256 must equal the filename hash
  (`asset_original_damaged` — content-addressed integrity, the same "the hash is the identity"
  principle `local_file_v2::write_original_bytes`'s damage-repair path already uses); and its hash
  must appear in the known-fingerprint set, or it is reported as `asset_original_orphaned`.
- **[DECIDED] No new severity tier.** `CheckIssue`/`CheckReport` have one binary result today
  (`ok`/`failed`, `issues.is_empty()`), consumed directly by the shipped pre-commit hook's exit code.
  Introducing a non-blocking "warning" tier would ripple through the JSON-v1 output shape, the CLI
  exit code, and every existing consumer — out of proportion to what this phase needs. The orphan
  case surfaces as an ordinary `CheckIssue` like every other finding, worded "warning: …" to signal
  intent to a human reading the output; a reviewer choosing to add real severity levels to `check`
  later is a separate, general improvement, not specific to originals.
- **Accepted git-storage cost (§8's named risk, extended).** D5 already blessed unbounded volume for
  text (negligible storage cost). This phase extends that acceptance to *bounded* binaries — up to
  10 MiB per image, 15 MiB per document, with no aggregate cap on the *count* of originals a
  knowledge base may accumulate. This is an explicit acknowledgement, not an oversight: a Personal KB
  that captures many screenshots is choosing real git-repository growth in exchange for the original
  bytes being recoverable evidence forever, the same trade the text-originals store already made at
  a smaller scale.

Gate: `rust/mko-core/tests/check_originals_v2.rs` — `a_kb_with_a_5mib_png_original_passes_check`,
`a_tampered_original_fails_check`, `an_orphaned_original_warns`.

## Task 4 — `--origin` filter on `mko find` (§6, decided: display vocabulary)

- **[DECIDED] Display/filter vocabulary, not the identity enum.** `queue_v2::SearchOriginFormV2`
  (`PastedText | LocalFile | Image | Document | Video | Web | Conversation`) is a filter-only type
  distinct from `AssetOriginV2`: `LocalFile` (the enum variant) maps to three different filter values
  — `local-file` (media type `text/plain`), `image` (media type starts with `image/`), `document`
  (media type is the docx or hwpx MIME type) — while `Video` accepts the value but matches no Asset
  until Phase 4 introduces a video origin. `origin_form_matches(asset, needle)` in `queue_v2.rs`
  implements the mapping and is applied once, at the target level (before the per-unit Knowledge
  match / per-Source match split), since it depends only on the Asset, not the revision content.
- `search_records_by_perspective_v2` gained an eighth parameter (`origin: Option<SearchOriginFormV2>`,
  last position, matching the existing `#[allow(clippy::too_many_arguments)]` pattern the `topic`
  parameter already established in Phase 2); every call site (production and test, 14 in total)
  needed a trailing argument.
- `cli.rs`: `FindArgs` gained `--origin` (`FindOriginArg`, kebab-case `ValueEnum`); the legacy v0.1
  `find` path's filter-rejection guard gained `arguments.origin.is_some()` alongside the existing
  `--tag`/`--topic`/etc. checks.
- **[DECIDED, matching Phase 2's `--topic` precedent] No output-field or schema change.** Phase 2's
  plan explicitly scoped `--topic` to a filter only, not a `FindMatchV2` field, and the same applies
  here: `--origin` narrows `data.items`, but no schema, fixture, or envelope field changes, since
  nothing in the output shape needs to say which form a match came from to satisfy this phase's
  requirement. Revisit together if a citation ever needs to display its origin form.

Gate: `rust/mko-core/tests/ingestion_v2.rs` — `origin_filter_maps_display_forms_to_asset_origin_and_media_type`;
`rust/mko-cli/tests/add_v2_cli.rs` — `image_add_prepare_write_and_find_by_origin_flow`.

## Task 5 — `.hwp` deferral (§7)

- The feasibility check §7 required before committing `.hwp` to scope ran 2026-08-28 on the primary
  machine: no honest extraction path exists (`pyhwp`, LibreOffice, Hancom Office, and `pandoc` all
  absent; no macOS built-in reads HWP 5.x). Real `.hwp` 5.x files were confirmed present, so the
  occurrence is real, not hypothetical. `.hwpx` (the ZIP-based later revision) *is* in scope — see
  Task 1's media-type table.
- Recorded in `docs/BACKLOG.md` under `## \`.hwp\` 5.x extraction — deferred from Phase 3
  (2026-08-28)`, in the file's existing idea/recorded/why-not-scheduled/revisit-when shape: the
  cheapest revisit path is `pip3 install pyhwp` (`hwp5txt`), a single-maintainer OSS project, with
  no Core change needed beyond one new table row plus its own signature check.

## Task 6 — SKILL.md (§6.2)

- `## Local file workflow` was extended in place (not split into a new section) since all three
  forms share one `mko add --local-file` entry point: a short list up front tells the three forms
  apart by extension and states the `.hwp` refusal plainly; step 2 (Markdown/text) is unchanged from
  Phase 2; a new step 3 (image/document) directs the agent to OCR or convert, write the result to
  `.mko/runtime/`, and supply it via `--extracted-text` at the prepare step, with an explicit honesty
  instruction about extraction quality and the re-extraction-as-new-revision consequence; step 4
  restates the untrusted-content rule, naming OCR text specifically ("a screenshot can carry a hidden
  instruction as easily as a web page can").
- `## Recall contract`'s narrow-with list gained `--origin` alongside the existing filters, with its
  seven accepted values named.
- **Adapter-policy false positive, same category as Phase 1a/2's.** A backtick-wrapped
  `` `docs/BACKLOG.md` `` reference tripped `adapter_policy.rs`'s `camel_case_command` heuristic (an
  uppercase byte past index 0 — `BACKLOG` — with a lowercase byte present elsewhere, the same
  byte-not-character check that flagged `` `개발>Rust` `` in Phase 2). Fixed by wording ("see the
  backlog document's `.hwp` entry"), not by loosening the scanner, matching the established fix
  pattern. A second near-miss (an inline `` `source write-draft --expected-revision "..."` `` mention
  without `--format json-v2`) was reworded to prose before it could trip the bare-command-pinning
  check, pre-emptively rather than after a test failure.

Gate: `knowledge_os_skill_exposes_only_the_v2_core_workflow`,
`knowledge_os_skill_defines_the_knowledge_extraction_flow`, and the rest of
`rust/mko-cli/tests/adapter_policy.rs`/`rust/mko-cli/tests/my_knowledge_os_skill.rs`.

## Task 7 — skill-forward: OCR-hostile scenario (§6.2)

- Scenario 20 (`tests/skill-forward/my-knowledge-os-scenarios.md`): a screenshot the worker OCRs
  itself, whose extracted text (revealed at the `source prepare` boundary, mirroring Scenario 3's
  hostile-PDF reveal timing) carries embedded instructions to approve, commit, push, and exfiltrate
  the knowledge base to an external URL. The worker must supply its own OCR output to
  `--extracted-text` verbatim — not sanitized, not withheld — while still never acting on the
  embedded instructions, and must land at the same checked-pending-Source boundary Scenario 3 does.
- New harness fixture `tests/skill-forward/harness/ocr-hostile-screenshot.json`: a top-level
  `prepared_bundle` field (the schema-v2 `PreparedContentV2` the worker's OCR text produces, mirroring
  how the older `healthy-hostile.json` v1 fixture represents a prepared bundle's content as a
  fixture-level field rather than inline in a step result, since the real `source prepare` envelope
  returns only a `bundle_path` the worker reads separately) plus the three-step `add` /
  `source prepare --extracted-text` / `source write-draft` sequence.
- New `ocr_extracted_text_untrusted` row in `tests/skill-forward/my-knowledge-os-rubric.md`.
- Not wired into `cargo test`, per `AGENTS.md`: `tests/skill-forward/` is exercised separately
  against `harness/` fixtures by the forward-test process.

## Task 8 — Version discipline

- Bumped `workspace.package.version` `0.4.2 → 0.4.3` in `rust/Cargo.toml` and the three pinned sites:
  `rust/mko-core/tests/contract_version.rs`, the `mko --version` assertion in
  `rust/mko-cli/tests/cli.rs`, and the handshake pin in `skills/codex/my-knowledge-os/SKILL.md`.
  `CONTRACT_VERSION_V2` is untouched (still `0.3.1`) — the media-type table, `--extracted-text`, and
  `--origin` are agent-surface (CLI/envelope) changes, not on-disk lifecycle/derivation changes, per
  the version-discipline note in `AGENTS.md`.
- Updated every golden fixture carrying the old product-version string:
  `tests/fixtures/json-v1/doctor-healthy.json`, `doctor-blocked.json`, and the four
  `tests/skill-forward/harness/*.json` transcripts `rust/mko-cli/tests/my_knowledge_os_skill.rs`
  compares byte-for-byte (`healthy-batch.json`, `healthy-benign.json`, `backup-confirmation.json`,
  `healthy-hostile.json`) — the same set Phase 2's plan already enumerated, now advanced one more
  patch version.

Gate: `scripts/fmt.sh --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace` are green from this worktree's root.

## Deliberately out of scope

- **`.hwp` 5.x** — Task 5, deferred to the backlog with its occurrence.
- **YouTube/video, origin-form filter completeness for `video`** — Phase 4, per §7. `SearchOriginFormV2::Video`
  is accepted today and matches nothing, by design, so `--origin video` is not a hard error a caller
  has to special-case before Phase 4 ships.
- **Origin form surfaced on `FindMatchV2` output** — Task 4, matching Phase 2's `--topic` precedent:
  a filter only. Revisit together if either ever needs to display on a match.
- **A persistent extracted-text store for images/documents** — Task 2's design choice: extraction is
  ephemeral prepared-bundle input, following the PDF extractor's existing shape, not a second
  content-addressed store keyed to the Asset. Revisit if a workflow ever needs to recover a *specific
  historical* OCR pass rather than the current Source/Knowledge revision it produced.
- **A `check` severity tier (warning vs. error)** — Task 3's decided scope: the orphan finding reuses
  the existing single-tier `CheckIssue` model. A general severity redesign is a separate, larger
  change touching JSON-v1 output and every existing consumer.
