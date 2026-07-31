# My Knowledge OS

My Knowledge OS는 PDF 원본, 근거 기반 요약, LLM 분석, 사람의 판단을 섞지 않고 보존하는
Git + Markdown 개인 지식 시스템입니다.

- Google Drive Inbox에는 원본 PDF를 둡니다.
- Private Git 저장소에는 Asset 메타데이터와 불변 Source/Knowledge revision을 둡니다.
- Obsidian은 생성된 projection을 읽는 화면입니다.
- LLM은 초안을 만들지만 승인·커밋·푸시하지 않습니다.

## 매일 쓰는 방법

설정 후 실제 터미널에서는 이것 하나로 시작합니다.

```bash
mko
```

현재 저장소 형식을 먼저 구분한 뒤 새 자료, 검토 대기, 수정 필요, 승인된 지식, 문제 수를
한국어로 보여줍니다. 기존 v0.1 저장소는 자동 변환하거나 수정하지 않고 읽기 전용 안내로
엽니다. 메뉴에서 Inbox 등록, 검토, 검색, 점검으로 이동할 수 있습니다. 종료는 `q`입니다.

승인된 지식을 바로 찾고 싶다면 짧은 별칭을 사용할 수 있습니다.

```bash
mko find "찾을 내용"
mko find "찾을 내용" --perspective technical
```

떠오른 생각은 LLM이 다듬지 않은 입력 그대로 저장합니다.

```bash
mko remember
```

실제 터미널에서 원문을 다시 보여준 뒤 `y`로 확인한 경우에만 불변 빠른 메모를 만듭니다.
그 밖의 입력이나 취소는 아무것도 저장하지 않습니다. 빠른 메모는 `mko find` 결과에서
`내 생각`으로 표시되어 문서 근거와 LLM 분석에 섞이지 않습니다.

Knowledge 관점은 `life`, `learning`, `technical`, `project`, `investment` 중 복수 선택할 수
있습니다. 관점 확인은 정확한 현재 revision과 효과를 실제 터미널에 표시하고 새 pending
revision을 만듭니다. `investment`는 Core가 `high_risk`로 파생하며 반론과 열린 질문이 없는
Knowledge에는 적용할 수 없습니다. 평소에는 `mko` → `다시 볼 지식`에서 관점 필터와 지식
번호를 선택하면 되므로 Knowledge ID를 입력할 필요가 없습니다. 승인 지식뿐 아니라
`나중에 보기`로 보류한 지식도 나타납니다. 항목을 열면 전체 synthesis와 검토일을 보여주고,
해당 revision의 마지막 열람 시각만 Git에서 제외된 `.mko/runtime`에 기록합니다. 이어서
`p`를 선택한 경우에만 별도의 관점 확인 흐름으로 들어갑니다.

파이프나 자동화처럼 입출력이 실제 터미널이 아닐 때 무인자 `mko`는 프롬프트를 열지
않습니다. 기존 세부 명령과 JSON 계약은 계속 호출할 수 있지만 기본 도움말에서는 일상
명령만 보여줍니다.

## 가장 쉬운 시작

현재 v0.3 소스 설치는 CLI와 Codex Skill을 함께 설치합니다. 이 경로는 Rust 1.97 이상이
필요하며 `mko setup`이나 Knowledge 저장소 변경을 자동 실행하지 않습니다.

Windows PowerShell:

```powershell
pwsh -File scripts/install.ps1 -PlanOnly
pwsh -File scripts/install.ps1 -Yes
```

macOS:

```bash
./scripts/install.sh --plan
./scripts/install.sh --yes
```

설치 후 Codex를 다시 시작하고 다음처럼 요청합니다.

```text
My Knowledge OS 시작해줘
```

스크립트는 기존 Skill을 삭제하지 않고 먼저 timestamped backup으로 이동합니다. Windows에서는
Cargo bin 경로를 사용자 PATH에 중복 없이 추가하며, macOS에서는 PATH에 없을 경우 추가할 정확한
경로를 안내합니다. Rust가 없는 clean machine용 checksum 고정 릴리스 바이너리 설치는 다음
배포 절취선이며, 그 전까지 Cargo가 개발 설치 fallback입니다.

수동 개발 설치:

```bash
cargo install --path rust/mko-cli --locked
```

Skill만 수동 설치해야 한다면 canonical 폴더
`skills/codex/my-knowledge-os`를 `${CODEX_HOME:-$HOME/.codex}/skills/my-knowledge-os`에
복사합니다. Windows에서 `CODEX_HOME`이 없다면 `%USERPROFILE%\.codex`를 사용합니다.

실제 터미널에서 최초 설정을 한 번 실행합니다.

```bash
mko setup
```

설정은 비변경 계획과 실제 TTY 적용의 두 단계로 실행합니다.

```bash
mko setup plan --format json-v2
# 실제 터미널에서 Core가 다시 표시한 정확한 경로와 효과를 확인한 뒤
mko setup apply --plan <core-plan-id> --format json-v2
```

계획은 15분 뒤 만료되고 한 번만 사용할 수 있으며, 파일·프로필·목적지가 바뀌면 적용 전
무효화됩니다. 채팅 승인이나 호스트의 일반 명령 승인만으로는 적용할 수 없고, Core가 표시한
card/effect digest에 묶인 정확한 문구를 실제 TTY에 입력해야 합니다. 또는 `mko setup`의
사람용 터미널 흐름을 사용할 수 있습니다.

설정 화면은 사용할 Personal KB와 Google Drive Inbox의 정확한 경로를 보여주고 `y` 확인
전에는 아무것도 만들지 않습니다. 기본 KB는 `~/My-Knowledge-OS`이고 Git 저장소는 Google
Drive 바깥에 둡니다. 다른 위치는 `mko setup --repo <path>`로 선택할 수 있습니다.

GitHub 원격은 최초 사용의 필수 입력이 아닙니다. 먼저 로컬 KB와 Drive Inbox를 연결하고,
원격 백업이 필요할 때 별도 요청과 승인으로 private remote를 설정합니다. 설정 승인은 Git
초기화, remote 추가, commit 또는 push를 포함하지 않습니다.

성공하면 출력된 Personal Inbox에 PDF를 복사한 뒤, 폴더 전체를 한 번에 등록할 수 있습니다.

```bash
mko add --inbox
```

한 파일만 선택해 등록할 수도 있습니다.

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
PDF에서 나왔다면 하나의 결합 카드로 표시됩니다. 승인된 지식 Base에는 복수 관점 열과
생활·학습·기술·프로젝트·투자 뷰가 생성됩니다. 생성 파일에 사용자 수정이 있으면 자동으로
덮어쓰지 않습니다.

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
mko setup plan --format json-v2
# 실제 TTY 전용; 비대화형 실행은 setup_tty_required로 실패
mko setup apply --plan <core-plan-id> --format json-v2
mko add --inbox --format json-v2
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
추출 전문은 `.mko/runtime/sessions/` 아래의 24시간 기기 로컬 세션 파일이며 Git과 Drive에서
제외됩니다. 만료되면 Core가 재추출하고, 암호화 없는 영구 평문 캐시는 사용하지 않습니다.

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
