# Knowledge Extraction Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the reviewed correctness, provenance, concurrency, query, validation, Skill, and release-evidence gaps before installing My Knowledge OS.

**Architecture:** Keep the existing Knowledge record type, but bind writes to the canonical prepared bundle and move mutable operations onto capability-based compare-and-swap publication. Reuse established Source/Registry safety patterns rather than inventing a second persistence mechanism.

**Tech Stack:** Rust, clap, serde, cap-std, Git Markdown records, Codex Skill Markdown.

## Global Constraints

- Asset and Source contracts remain unchanged.
- Human review cannot be executed by a Skill or non-TTY process.
- JSON-v1 output is one path-free stdout object with no mixed prompts.
- Every production behavior change starts with a failing regression test.
- No automatic commit, push, approval, or publication.

---

### Task 1: Publication-lock disappearance race

**Files:**
- Modify: `rust/mko-core/src/atomic.rs`
- Test: `rust/mko-core/src/atomic.rs`
- Test: `rust/mko-core/tests/capture.rs`

- [ ] Add a deterministic hook-based test that removes a cleanup quarantine after discovery and proves lock acquisition retries instead of returning `registry_write_failed`.
- [ ] Run the new test and confirm the ENOENT failure.
- [ ] Treat a quarantine that vanishes during owner cleanup as a retry while preserving fail-closed behavior for existing malformed/replaced entries.
- [ ] Run atomic and capture tests, including a bounded stress repetition.

### Task 2: Bundle-bound Knowledge writes and mutation CAS

**Files:**
- Modify: `rust/mko-core/src/knowledge.rs`
- Modify: `rust/mko-cli/src/cli.rs`
- Modify: `rust/mko-core/src/prepare.rs` or reuse its public validator
- Test: `rust/mko-core/tests/knowledge.rs`
- Test: `rust/mko-cli/tests/knowledge_cli.rs`

- [ ] Add failing tests for missing/wrong/untrusted bundle, approval after body tampering, concurrent replace/approve, concurrent first creation, correct `existing` result, and approve-replace-check validity.
- [ ] Require `--bundle` and validate canonical runtime location, Asset identity/fingerprint, and trust marker in Core.
- [ ] Use retained capabilities and compare-at-commit revision validation for replace and approve.
- [ ] Preserve prior `approved_revision` on regeneration and update validation to accept that comparison-history state.
- [ ] Run Knowledge Core and CLI tests.

### Task 3: Complete query, scan, and repository validation contracts

**Files:**
- Modify: `rust/mko-core/src/knowledge.rs`
- Modify: `rust/mko-core/src/check.rs`
- Modify: `rust/mko-cli/src/cli.rs`
- Test: `rust/mko-core/tests/knowledge.rs`
- Test: `rust/mko-cli/tests/knowledge_cli.rs`

- [ ] Add failing tests for reviewed/empty-concept show and list, kind search, scan deadline, symlink swap, file growth, canonical ID/path/generation/fingerprint checks, duplicate Asset notes, and single-object review JSON.
- [ ] Implement all-note projections for show/list and include kind in search.
- [ ] Replace ambient scan reads with bounded no-follow capability reads and an elapsed deadline.
- [ ] Expand `mko check` to validate the full durable Knowledge contract.
- [ ] Keep interactive prompts off JSON-v1 stdout and run targeted tests.

### Task 4: Skill intent, behavioral harness, and release evidence

**Files:**
- Modify: `skills/codex/my-knowledge-os/SKILL.md`
- Modify: `rust/mko-cli/tests/adapter_policy.rs`
- Modify: `rust/mko-cli/tests/my_knowledge_os_skill.rs`
- Modify: `tests/skill-forward/my-knowledge-os-scenarios.md`
- Modify: `tests/skill-forward/my-knowledge-os-rubric.md`
- Modify: `docs/manual-smoke-v0.2.md`
- Modify: `docs/manual-smoke-v0.2-record.md`

- [ ] Add failing policy/harness tests requiring explicit knowledge-extraction intent, canonical `--bundle`, hostile-bundle resistance, exactly one write, no review execution, and pending output.
- [ ] Update the Skill and adapters to the frozen command contract.
- [ ] Extend the smoke procedure with Knowledge write/check/human review and mark the existing incomplete record `pending`, not `PASS`.
- [ ] Run adapter, harness, and Skill validation.

### Task 5: Final verification and local deployment

**Files:**
- No production changes expected.

- [ ] Run `scripts/fmt.sh --check` from repository root.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings` from `rust/`.
- [ ] Run `cargo test --workspace` twice and repeat the capture concurrency regression.
- [ ] Run the official Skill validator.
- [ ] Request final independent code review and close all Critical/Important findings.
- [ ] Install the verified `mko` binary and `my-knowledge-os` Skill only after user approval for writes outside the workspace.
- [ ] Confirm `mko knowledge --help` and Skill discovery from the installed locations.
