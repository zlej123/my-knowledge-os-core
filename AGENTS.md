# AGENTS.md

## Specification authority

Design work lives in `docs/superpowers/specs/`, one dated file per area; the
matching plan, where one exists, sits in `docs/superpowers/plans/`. Resolve
conflicts in this order:

1. The newest dated design in `docs/superpowers/specs/` covering the area.
2. `docs/BACKLOG.md` for why something is deliberately *not* built — it carries
   the occurrence rule: an item earns design work only after the workflow has
   actually hurt in practice.
3. Older dated designs, which are historical and not implementation authority.

Earlier revisions of this file named four `My Knowledge OS v0.x …` documents as
the authority. They have never existed in this repository — not in the working
tree, not anywhere in git history. They are retired; do not go looking for them.

## Where things are

| Path | Contents |
|---|---|
| `rust/mko-core` | The Core. Deterministic mutations; `version::PRODUCT_VERSION`; `config_v2.rs` holds `CONTRACT_VERSION_V2` (on-disk KB contract, see below); `recall_log_v2.rs` (append-only recall log), `local_file_v2.rs` (local-file Asset registration), and `migrate_v2.rs` (the v0.3→v3 migration) are also here. |
| `rust/mko-cli` | The `mko` binary. `cli.rs` carries command dispatch and `handshake`. |
| `rust/mko-windows-acl` | Windows file-permission handling. |
| `schemas/` | json-v2 envelope schemas — an agent-facing surface, so edits here force a version bump. |
| `skills/codex/` | The installed Skills. `my-knowledge-os` is the current, actively developed Skill (json-v2). `capture-asset` and `process-asset` are frozen legacy v0.1 Skills — not part of ongoing development. Each has its own `SKILL.md`. |
| `tests/skill-forward/` | Forward tests for the Skill: `*-scenarios.md`, `*-rubric.md`, and `harness/` fixtures. Not run by `cargo test`. |
| `docs/superpowers/specs/`, `docs/superpowers/plans/` | Dated design and plan documents (see above). |
| `scripts/pre-commit` | Shipped for the *user's* knowledge repository (`mko check --staged`), not a hook for this repo's own development. |

## Working rules

- The Rust Core owns deterministic mutations: IDs, Registry YAML, states, revisions, approval metadata, and final Markdown. Do not mutate Registry or Source YAML directly outside the Core.
- LLMs and adapters may provide typed semantic JSON only. They must not automatically approve, commit, or push.
- Human approval remains revision-bound and requires the human-only Core command.

## Version discipline

The installed CLI and the installed Skill are one contract, enforced at runtime by
`mko handshake` (exact match against the version the Skill pins). Therefore any change to an
agent-facing machine surface — CLI commands or flags, json-v2 envelopes, `schemas/`, or the
SKILL.md workflow contract — must bump `workspace.package.version` in `rust/Cargo.toml`
(patch level at minimum) in the same change. Tests pin the version in three places
(`contract_version.rs`, the CLI `--version` test, and the Skill handshake pin) so a bump is
always an explicit, reviewed act. `contract_version.rs` pins both: `PRODUCT_VERSION` (and the
legacy v1 `KNOWLEDGE_CONTRACT_VERSION`) alongside `CONTRACT_VERSION_V2` in `config_v2.rs`, the
on-disk KB contract — a separate value, not the product version, that must not change for a
surface bump.

## Verification

From the repository root, run:

```bash
scripts/fmt.sh --check
```

From `rust/`, run:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` does not cover `tests/skill-forward/` — those are scenario/rubric
tests for the Skill and are exercised separately against `harness/` fixtures.

To install the CLI the Skill handshake will check against:

```bash
./scripts/install.sh --plan     # dry run first
./scripts/install.sh --yes
```

## Branch baseline

`main` is the product baseline: the primary checkout sits on it, and all
verification above runs against it. Feature work happens on branches (as
worktrees under `.worktrees/`) and lands through pull requests. Leave the
primary checkout on `main` — do the work in a worktree.

Run `git worktree list` for what is in flight. Do not trust a list written
here; this line previously named one worktree and was wrong within days.
`feature/delivery-engine-design` is the long-lived exception — the unscheduled
capture-delivery/Telegram prototype, kept unmerged (see `docs/BACKLOG.md`).
