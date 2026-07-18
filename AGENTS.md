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
