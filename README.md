# My Knowledge OS Core

`mko` is the versioned, deterministic Rust Core for My Knowledge OS.

## Responsibility boundary

The Core owns deterministic work: validating provider paths and scope, calculating PDF fingerprints and IDs, reading and writing Registry records, extracting text, validating schemas and state transitions, calculating content revisions, managing locks and atomic writes, running checks, and performing human-only approval.

An LLM supplies meaning only: structured semantic JSON containing a General Summary, Domain Perspective, and related knowledge candidates. It never writes Registry YAML or Source Markdown, assigns IDs or states, changes approval metadata, approves content, commits, or pushes. A Codex adapter orchestrates the Core and LLM; it does not bypass the Core.

## v0.1 boundary

v0.1 is a Personal PDF vertical slice: one `personal-kb`, local-readable PDFs from the Google Drive streaming filesystem adapter, typed Source drafts, revision-bound human approval, and manual Git commit/push. It excludes Shared and Work scopes, other document formats, Drive API/OAuth/watchers, agent approval, automatic commit/push, automatic regeneration of approved Sources, databases, vector search, and RAG.

The stable top-level command groups are `asset`, `source`, `check`, `human`, and `hooks`.

## Development

Rust 1.97.0 is pinned in `rust-toolchain.toml`. Verify the workspace with:

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
