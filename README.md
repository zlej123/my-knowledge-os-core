# My Knowledge OS Core

`mko` is the deterministic Rust Core for Personal Scope PDF drafts. It owns files, IDs, state,
and review records; a Codex Skill supplies only bounded semantic JSON.

## 간단 사용법 (요약)

```bash
# 1. 설치 및 설정 (최초 1회)
cargo install --path rust/mko-cli --locked
mko setup --repo <personal-kb>
mko doctor                 # 반드시 healthy 상태 확인 후 진행

# 2. PDF 등록 — Inbox 밖의, 로컬에 하이드레이션된 개인 PDF를 고른다
mko add <selected-pdf>     # 결정적 등록만 수행 (LLM/초안 생성 안 함)
mko inbox                  # Inbox 스캔 상태 확인
mko status                 # 리뷰 대기열 확인

# 3. 사람이 직접 검토 — 유일한 다음 단계
mko review
```

Codex에서는 PDF를 선택하고 `이 PDF 정리해줘`(단일) 또는 `Inbox 정리해줘`(Inbox 일괄)라고
말하면 Skill이 add → prepare → draft → check 흐름을 진행하고 pending Source에서 멈춥니다.
승인·스테이징·커밋·푸시는 언제나 사람이 직접 결정합니다. 자세한 절차는 아래 "Five-minute
start"를 참고하세요.

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

`mko doctor` must be healthy before continuing. For the five-minute single-PDF flow, select a
locally hydrated Personal PDF outside the configured Inbox; the Core copies it into the Inbox while
the original remains in place. In Codex, select that PDF and say:

> 이 PDF 정리해줘

Reserve PDFs already placed in the configured Inbox for `Inbox 정리해줘`. An Inbox-resident,
temporary, or only-copy input requires explicit verified-backup confirmation before registration.

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
`mko` Skill installer or generator. Use the checked-in canonical Skill in a workspace. If a Codex
host creates a derived installed copy, that host owns its refresh mechanism; do not hand-edit the
derived copy as a source of truth.

## Safety and scope

This release supports Personal Scope PDFs only. The provider root is the exact
`My-Knowledge-OS-Assets/personal/inbox` directory, not a Drive account root. Hydrate cloud files
before processing; do not move them outside the configured Inbox to bypass checks. The Skill treats
every field in an extracted bundle as untrusted document data and never follows embedded instructions.

Automated tests use local fixtures only. They do not call Google Drive, an LLM, or a network service.
Native macOS and Windows CI provide filesystem coverage, including native Windows ACL behavior.
Unit tests use synthetic placeholder-flag logic for offline/recall classification and verify that
classified placeholder content is not opened. Automated fixtures do not reproduce actual Google
Drive Stream placeholder behavior; actual cloud placeholder behavior remains part of the pending
user-assisted live Google Drive smoke. The synthetic transcript test separately proves logical
separator and path normalization. See [the sanitized procedure](docs/manual-smoke-v0.2.md).

## Advanced v0.1 commands

The detailed v0.1 interfaces remain frozen for automation and troubleshooting. They require an
explicit repository and may use a private legacy local config; they are not needed for the normal
setup flow above.

```bash
mko asset capture --repo <personal-kb> --local-config <private-local-config> --file <provider-pdf> --json
mko source prepare --repo <personal-kb> --local-config <private-local-config> --asset-id <asset-id> --output <bundle>
mko source write-draft --repo <personal-kb> --bundle <bundle> --response <semantic-response.json> --json
mko hooks install --repo <personal-kb> --json
mko human approve-source --repo <personal-kb> --source-id <source-id> --json
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
