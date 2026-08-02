# AGENTS.md

## Specification authority

Resolve conflicts in this order:

1. `My Knowledge OS v0.1 Detailed Spec.md` for release-specific behavior.
2. `My Knowledge OS Master Architecture v0.5.md` for cross-release invariants.
3. `My Knowledge OS v0.1 Implementation Plan.md` for implementation sequencing and task details.
4. `My Knowledge OS v0.1 Design Spec.md` is historical and is not implementation authority.

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
always an explicit, reviewed act. `CONTRACT_VERSION_V2` in `config_v2.rs` is the on-disk KB
contract, not the product version; it must not change for a surface bump.

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

## Branch baseline

`main` is the product baseline: the primary checkout sits on it, and all
verification above runs against it. Feature work happens on branches (as
worktrees under `.worktrees/`) and lands through pull requests. The only
current worktree is `feature/delivery-engine-design`, the unscheduled
capture-delivery/Telegram prototype (see docs/BACKLOG.md).
