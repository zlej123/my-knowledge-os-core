# My Knowledge OS

My Knowledge OS는 PDF 원본, 근거 기반 요약, LLM 분석, 사람의 판단을 섞지 않고 보존하는
Git + Markdown 개인 지식 시스템입니다.

- Google Drive Inbox에는 원본 PDF를 둡니다.
- Private Git 저장소에는 Asset 메타데이터와 불변 Source/Knowledge revision을 둡니다.
- Obsidian은 생성된 projection을 읽는 화면입니다.
- LLM은 초안을 만들지만 승인·커밋·푸시하지 않습니다.

## 가장 쉬운 시작

개발 설치:

```bash
cargo install --path rust/mko-cli --locked
```

실제 터미널에서 최초 설정을 한 번 실행합니다.

```bash
mko setup
```

설정 화면은 사용할 Personal KB와 Google Drive Inbox의 정확한 경로를 보여주고 `y` 확인
전에는 아무것도 만들지 않습니다. 기본 KB는 `~/My-Knowledge-OS`이고 Git 저장소는 Google
Drive 바깥에 둡니다. 다른 위치는 `mko setup --repo <path>`로 선택할 수 있습니다.

성공하면 출력된 Personal Inbox에 PDF를 복사한 뒤 등록합니다.

```bash
mko add "/Google Drive/.../My-Knowledge-OS-Assets/personal/inbox/paper.pdf"
```

10 MiB보다 큰 스트리밍 파일은 전체 다운로드/읽기 확인 후 한 번만 다시 실행합니다.

```bash
mko add "/path/in/inbox/paper.pdf" --confirm-download
```

Asset 등록은 PDF를 이동·삭제하지 않고 SHA-256 기반 메타데이터만 기록합니다.

## Codex에서 사용

정식 스킬 원본은 [skills/codex/my-knowledge-os/SKILL.md](skills/codex/my-knowledge-os/SKILL.md)입니다.
설치한 뒤 평소 말로 요청합니다.

```text
이 PDF 요약해줘
```

스킬은 다음 순서로 동작합니다.

```text
Asset 등록
  → 정확한 PDF 내용 추출
  → 근거 기반 Source 요약
  → “이 내용을 지식 노트로도 등록할까요?”
  → 사용자가 동의하면 Knowledge 초안
  → 통합 검토 대기열
```

Knowledge는 다음 네 층을 구분합니다.

1. 문서 근거가 있는 사실·정의·공식·결과
2. `interpretation`/`hypothesis`로 표시한 LLM 분석
3. 반론·불확실성·검증 질문
4. 별도 승인 경로로 기록하는 사용자의 판단

## 확인과 피드백

```bash
mko queue
mko show <stable-id>
mko dashboard
```

`mko queue`와 Obsidian `HOME.md`는 같은 검토 상태를 보여줍니다. Source와 Knowledge가 같은
PDF에서 나왔다면 하나의 결합 카드로 표시됩니다.

Codex는 정확한 카드를 보여준 뒤 `request_changes` 또는 `defer` 피드백만 전달할 수 있습니다.
최종 승인은 실제 터미널에서 수행합니다.

```bash
mko review <stable-id>
```

이 명령은 현재 revision과 효과를 다시 표시하고 revision-bound 확인을 요구합니다. 비대화형
명령에는 `approve` 경로가 없습니다.

## 기계 계약

에이전트는 사람용 출력 대신 엄격한 JSON v2 envelope를 사용합니다.

```bash
mko add <inbox-pdf> --format json-v2
mko source prepare --asset-id <asset-id> --format json-v2
mko source write-draft --bundle <bundle> --response <source-response.json> --format json-v2
mko knowledge write --asset-id <asset-id> --bundle <bundle> --response <knowledge-response.json> --format json-v2
mko queue --format json-v2
mko show <stable-id> --format json-v2
mko review-open <stable-id> --format json-v2
mko review-feedback --input <decision.json> --format json-v2
```

계약은 [schemas/v2](schemas/v2), 예시는 [tests/fixtures/json-v2](tests/fixtures/json-v2)에 있습니다.
추출 전문은 `.mko/runtime/` 아래의 기기 로컬 캐시이며 Git과 Drive에서 제외됩니다.

## 저장소와 동기화

KB는 Private GitHub 저장소로 관리하는 것을 권장합니다. Google Drive는 Asset 원본 저장소일
뿐 Git history를 동기화하지 않습니다. 커밋·pull·push는 v0.3.0에서 수동입니다.

## 개발 검증

저장소 루트에서:

```bash
scripts/fmt.sh --check
```

`rust/`에서:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

설계 기준은
[v0.3 Knowledge UX spec](docs/superpowers/specs/2026-07-22-v0.3-knowledge-ux-design.md)에 있습니다.
