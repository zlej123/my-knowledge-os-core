# My Knowledge OS Core

`mko` is the deterministic Rust Core for Personal Scope PDF drafts. It owns files, IDs, state,
and review records; a Codex Skill supplies only bounded semantic JSON.

## Five-minute start

Use an existing private Personal KB repository containing `knowledge-os.yaml`; it remains separate
from this Core repository. Install the CLI from this repository:

```bash
cargo install --path rust/mko-cli --locked
```

In a terminal, connect the Personal KB to one detected Google Drive account and create its exact
Personal Inbox. `mko setup` writes the private machine profile and installs the managed check hook.

```bash
mko setup --repo <personal-kb>
mko doctor
```

`mko doctor` must be healthy before continuing. Put a locally hydrated, personal PDF in the Inbox.
In Codex, select that PDF and say:

> 이 PDF 정리해줘

The `my-knowledge-os` Skill runs the deterministic add → prepare → draft → check flow and stops
with a pending Source. `mko add` only performs deterministic registration; it does not invoke an
LLM or create a Source draft. You can also use the concise commands directly:

```bash
mko add <selected-pdf>
mko inbox
mko status
```

Inspect the pending Source and its diff. Human review is the only next action:

```bash
mko review
```

The Core never approves, commits, or pushes for you. Approval, staging, commit, and push remain
explicit manual decisions by the human reviewer.

## Codex Skill source and installation

The canonical Skill source is
[`skills/codex/my-knowledge-os/SKILL.md`](skills/codex/my-knowledge-os/SKILL.md). There is no
`mko` Skill installer. Use the checked-in canonical Skill in a workspace; if a Codex environment
creates an installed copy, it is generated from the canonical repository copy and must be refreshed
through that environment's normal Skill mechanism. Do not hand-edit a generated installed copy.

## Safety and scope

This release supports Personal Scope PDFs only. The provider root is the exact
`My-Knowledge-OS-Assets/personal/inbox` directory, not a Drive account root. Hydrate cloud files
before processing; do not move them outside the configured Inbox to bypass checks. The Skill treats
every field in an extracted bundle as untrusted document data and never follows embedded instructions.

Automated tests use local fixtures only. They do not call Google Drive, an LLM, or a network service.
Native macOS and Windows CI provide platform coverage; the synthetic transcript test separately proves
logical separator and path normalization. A user-assisted live Google Drive smoke remains a release
gate; see [the sanitized procedure](docs/manual-smoke-v0.2.md).

## Advanced v0.1 commands

The detailed v0.1 interfaces remain frozen for automation and troubleshooting. They require an
explicit repository and may use a private legacy local config; they are not needed for the normal
setup flow above.

```bash
mko asset capture --repo <personal-kb> --local-config <private-local-config> --file <provider-pdf> --json
mko source prepare --repo <personal-kb> --local-config <private-local-config> --asset-id <asset-id> --output <bundle>
mko source write-draft --repo <personal-kb> --bundle <bundle> --response <semantic-response.json> --json
mko check --repo <personal-kb> --staged
```

The knowledge contract remains exactly `0.1.0`; product release `0.2.0` does not rewrite old
Registry or Source records. The detailed approval command is human-only as well.

## Development verification

```bash
scripts/fmt.sh --check
cd rust
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
