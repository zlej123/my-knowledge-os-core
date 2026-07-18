# My Knowledge OS Core

`mko` is the versioned, deterministic Rust Core for My Knowledge OS.

## Responsibility boundary

The Core owns deterministic work: validating provider paths and scope, calculating PDF fingerprints and IDs, reading and writing Registry records, extracting text, validating schemas and state transitions, calculating content revisions, managing locks and atomic writes, running checks, and performing human-only approval.

An LLM supplies meaning only: structured semantic JSON containing a General Summary, Domain Perspective, and related knowledge candidates. It never writes Registry YAML or Source Markdown, assigns IDs or states, changes approval metadata, approves content, commits, or pushes. A Codex adapter orchestrates the Core and LLM; it does not bypass the Core.

## v0.1 boundary

v0.1 is a Personal PDF vertical slice: one `personal-kb`, local-readable PDFs from the Google Drive streaming filesystem adapter, typed Source drafts, revision-bound human approval, and manual Git commit/push. It excludes Shared and Work scopes, other document formats, Drive API/OAuth/watchers, agent approval, automatic commit/push, automatic regeneration of approved Sources, databases, vector search, and RAG.

The stable top-level command groups are `asset`, `source`, `check`, `human`, and `hooks`.

## Install

Install [Rust](https://www.rust-lang.org/tools/install) with `rustup`. This repository pins Rust 1.97.0 in `rust-toolchain.toml`. From the repository root, install the CLI with the locked dependency graph:

```bash
cargo install --path rust/mko-cli --locked
```

The Personal KB is a separate private Git repository. Its committed `knowledge-os.yaml` names the Google Drive streaming provider, but the machine-specific provider root belongs in an untracked local file. For example, create `~/.config/mko/personal.yaml` with:

```yaml
provider_root: <absolute-path-to-google-drive-personal-inbox>
```

Replace the angle-bracket value with the locally mounted, fully hydrated Personal Inbox directory. Do not commit this file or an absolute provider path. You may pass it with `--local-config` or set `MKO_LOCAL_CONFIG` to its path.

Initialize the managed pre-commit hook once in the Personal KB:

```bash
mko hooks install --repo <personal-kb>
```

The installer writes `.githooks/pre-commit` and configures `core.hooksPath`. `mko check` reports `hook_missing` or `hook_not_configured` if that protection is absent.

## Operate the Personal PDF workflow

Capture a hydrated PDF from within the configured provider root:

```bash
mko asset capture --repo <personal-kb> --local-config ~/.config/mko/personal.yaml --file <provider-pdf> --json
```

Record the returned Asset ID. Prepare the bounded, untrusted extraction bundle at its canonical runtime location:

```bash
mko source prepare --repo <personal-kb> --local-config ~/.config/mko/personal.yaml --asset-id <asset-id> --output <personal-kb>/.knowledge-os/runtime/prepared/<asset-id>.json
```

Run the Codex `process-asset` adapter to produce typed semantic JSON from that bundle, then let the Core write the canonical pending Source:

```bash
mko source write-draft --repo <personal-kb> --bundle <personal-kb>/.knowledge-os/runtime/prepared/<asset-id>.json --response <semantic-response.json> --json
```

The semantic response is input only. The Core owns Source Markdown, IDs, relations, states, and `content_revision`. If a pending draft would change, inspect it first and rerun with `--replace-pending`; an approved Source is immutable.

Review the Source text and the repository diff manually. Approval is deliberately interactive and revision-bound:

```bash
git -C <personal-kb> diff -- sources assets/registry
mko human approve-source --repo <personal-kb> --source-id <source-id>
```

At the terminal prompt, confirm only after the displayed Source ID, current revision, status transition, and Git summary match the reviewed diff. Codex and other agents must never run approval, `git commit`, or `git push` for the user.

After approval, review the final diff and stage it manually. The managed hook runs the staged check before a human-created commit:

```bash
git -C <personal-kb> diff --check
git -C <personal-kb> add assets/registry sources .githooks
mko check --repo <personal-kb> --staged
git -C <personal-kb> diff --cached
git -C <personal-kb> commit
```

Pushing remains a separate manual decision.

## Provider and size failures

Google Drive streaming files must be downloaded locally before capture. On `provider_not_hydrated` or `provider_content_unavailable`, use the Google Drive client to make the PDF available offline, wait for hydration to finish, and retry without changing the logical provider path. Do not move the document outside the configured provider root to bypass this check.

The automated path accepts PDFs up to 50 MiB. `file_too_large` directs larger PDFs to a manual path: verify the file and its backup, extract or split it with a trusted local tool, review the derived material, and capture only a supported PDF whose provenance is recorded. Do not add the original large binary or an extracted-text cache to Git.

Before registering an Asset that has no copy anywhere else, stop and create a verified backup outside the Personal KB. This backup trigger is mandatory for unique Assets; Google Drive sync alone is not proof of a recoverable second copy.

## Tests and live smoke checks

Automated tests never call an LLM or the network. They stub LLM output with committed, versioned golden semantic JSON and use generated PDF fixtures, a fixed contract, and deterministic Core operations. A release smoke test is separate: use one user-owned, non-sensitive, hydrated Google Drive PDF; capture, prepare, run the live Codex semantic step, inspect and approve the Source interactively, run the hook, and create the commit manually. Record review time, but never store the PDF, provider absolute path, extracted-text cache, credentials, or runtime locks in Git.

## Development

Rust 1.97.0 is pinned in `rust-toolchain.toml`. Verify the workspace with:

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
