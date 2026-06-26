# scv

터미널에서 동작하는 멀티 프로바이더 코딩 에이전트(Claude Code / Codex 류). Rust + Tokio.

시스템 프롬프트 · 세션 · 도구(tool) · 스킬(skill)을 1급 기능으로 제공하고, LLM 프로바이더를
추상화해 교체할 수 있다. LLM 은 **모델(토큰)** 로만 쓰고 루프·도구·승인은 scv 가 소유한다
(raw API 에 가장 가까운 동작). **기본 프로바이더는 로컬 Ollama(`qwen3.5:9b`)** — 키·네트워크
없이 오프라인으로 돈다. OpenAI / Anthropic 은 플래그로 전환.

---

## 설치

### 방법 A — 심볼릭 링크 스크립트 (권장)

`scv` 를 PATH 의 bin 디렉터리에 **심볼릭 링크**로 걸어 어디서든 `scv` 로 부른다. 링크가
release 바이너리를 가리키므로 **코드 수정 후 `cargo build --release` 만 다시 하면 재설치 없이**
최신 바이너리가 반영된다.

```bash
sh scripts/scv-link.sh install      # release 빌드 + 링크 생성
sh scripts/scv-link.sh status       # 링크/PATH 상태 확인
sh scripts/scv-link.sh uninstall    # 링크 제거
```

- bin 디렉터리는 **PATH 에 이미 있는 곳을 자동 선택**한다(`~/.local/bin` → `~/.cargo/bin` 순,
  과거 설치본이 있으면 그 자리를 덮어 섀도잉 방지). 직접 지정: `SCV_BIN_DIR=/path sh scripts/scv-link.sh install`.
- 다른 `scv` 가 PATH 우선순위로 링크를 가리면 경고한다.

### 방법 B — cargo install (복사본)

```bash
cargo install --path crates/scv-cli         # → ~/.cargo/bin/scv (복사본)
cargo install --path crates/scv-cli --force # 코드 수정 후 재설치(복사본은 그 시점 스냅샷)
```

`scv` 가 안 잡히면 `~/.cargo/bin`(또는 위 bin 디렉터리)이 PATH 에 있는지 확인한다(보통
`~/.cargo/env` 가 셸 프로필에서 로드됨). 없으면 셸 설정에 추가:

```bash
echo '. "$HOME/.cargo/env"' >> ~/.zshrc   # 또는: export PATH="$HOME/.cargo/bin:$PATH"
```

### 로컬 모델 준비(기본 프로바이더)

```bash
# https://ollama.com 설치 후
ollama serve                 # 데몬(이미 떠 있으면 생략)
ollama pull qwen3.5:9b       # 기본 모델 — tool calling 지원(코딩 에이전트에 필수)
```

키·환경변수는 **불필요**하다(ollama 프로바이더는 무인증). 클라우드로 전환할 때만 키를 둔다(아래 §프로바이더).

---

## 빠른 시작

```bash
cd ~/work/my-project     # 작업할 프로젝트로 이동 — 이 디렉터리가 scv 의 작업 대상(cwd)
scv                      # 인터랙티브 TUI
scv "이 저장소 구조를 한 문단으로 설명해줘"   # 원샷(한 번 묻고 끝)
```

- **설정**은 `~/.config/scv/config.toml`(홈 기준, cwd 무관)에서 읽는다 → 어디서 실행하든 동일.
- **작업 대상**은 실행한 디렉터리(cwd) → 도구의 파일 읽기/쓰기 루트.
- 첫 실행에 설정 파일이 없으면 [`config/config.example.toml`](./config/config.example.toml) 을
  `~/.config/scv/config.toml` 로 복사한다(없어도 기본값으로 동작하지만 프로바이더 정의는 필요).

> ⚠ **scv 는 자기 소스 레포 안에서는 실행을 거부한다**(자기 코드를 작업 대상으로 삼는 사고 방지).
> 이 레포(`scv` 개발 디렉터리)에서 돌리면 거부 메시지가 뜬다 — **다른 프로젝트 디렉터리에서** 쓰자.
> 개발 중 의도적으로 레포 안에서 돌려야 하면 `SCV_ALLOW_IN_REPO=1 scv ...`.

---

## 실행 모드

| 모드 | 명령 | 설명 |
|------|------|------|
| 인터랙티브 TUI | `scv` | 대화 루프 + 승인 모달 + 진행 표시 + 인터럽트 |
| 원샷 | `scv "프롬프트"` | 한 턴 실행 후 종료(스트림을 stdout 으로) |
| 세션 재개 | `scv --resume <id>` | 이전 세션 이어가기 |
| 세션 격리 | `scv --isolate` | 세션별 git worktree 에서 작업(아래 §세션 격리) |

주요 플래그: `--provider <id>` · `--model <id>` · `--effort <none\|low\|medium\|high\|xhigh\|max>`
· `--no-tools`(도구 스키마 미전송 — tool calling 미지원 모델용). 전체는 `scv --help`.

---

## 도구와 승인(권한)

모델이 호출하는 내장 도구:

| 도구 | 권한 | 설명 |
|------|------|------|
| `read` · `glob` · `grep` | **Allow**(자동) | 읽기 전용 — 부작용 없음, 병렬 실행 |
| `transcript_search` | **Allow**(자동) | 과거 세션 JSONL 정밀 검색(요약 손실 보완) |
| `write` · `edit` · `bash` | **Ask**(승인) | 파일 수정·셸 — 되돌리기 어려워 매번 승인 |
| `web_fetch` | **Ask**(승인) | HTTP(S) GET — 네트워크 egress |

**승인이 곧 사용 조건이다.** `Ask` 도구는 사용자가 허용해야만 실행된다:

- **TUI**: 호출 시 모달이 뜬다 — 무엇을 실행하는지(예: `bash: ls -la`)를 보여주고 `[y]` 허용 / `[n]` 거부.
- **원샷**: 대화형 승인 경로가 없어 `Ask` 도구는 **거부**되고 그 턴이 끝난다 → 원샷은 읽기 위주 프롬프트가 매끄럽다.
- **자동 허용/거부**: 설정 `[permissions.tools]` 에서 도구별로 `allow`/`deny`/`ask` 를 지정한다.
  예) `bash = "allow"` 면 묻지 않고 실행. 프로젝트별로 `./.scv/config.toml` 에 둘 수도 있다.

```toml
# 예: 작업 프로젝트의 ./.scv/config.toml — 이 repo 에서만 bash 자동 허용
[permissions.tools]
bash = "allow"
```

---

## 인터랙티브 TUI 키 / 명령

- 입력 후 **Enter** 로 전송.
- **Ctrl-C**: 턴 진행 중 = 현재 턴 중단(앱 유지) / 입력 대기 중 = **두 번** 누르면 종료.
- 승인 모달에서 **y** = 허용, **n**(또는 Esc) = 거부.
- 진행 표시: 스피너(유니코드/ascii, `[ui].spinner`) + 상태줄. 색은 `NO_COLOR` 존중. 입력창
  제목에 현재 `프로바이더·모델` 표시.

**슬래시 명령**(입력창에 `/` 로 시작해 전송 — 실행 중 전환):

| 명령 | 동작 |
|------|------|
| `/provider <id>` (`/p`) | 그 프로바이더로 전환(그 프로바이더의 설정 모델로 켜짐) |
| `/model <id>` (`/m`) | 현재 프로바이더에서 모델만 전환 |
| `/providers` | 사용 가능한 프로바이더 id 목록 |
| `/skills` | 사용 가능한 스킬 목록(없으면 비어 있음) |
| `/<skill>` | `/skills` 에 보이는 사용자 스킬을 발동 — 본문 절차를 주입해 그 턴에 적용 |
| `/help` | 명령 도움말 |
| **PageUp / PageDown** | 대화 로그 스크롤(이전 대화 보기 / 하단 복귀) |

예: `/provider openai` → openai 설정 모델로 전환, `/model gpt-5.4-mini` → 모델만 교체.
전환은 현재 세션을 유지한 채 다음 턴부터 적용된다. (클라우드 프로바이더는 해당 키 환경변수가
있어야 함.) 컨텍스트 압축은 명령이 아니라 자동이다(§세션).

---

## 세션

- 매 턴 `~/.scv/sessions/<id>.jsonl` 로 저장(트랜스크립트, 재개·감사 가능).
- `scv --resume <id> "..."` 로 이어간다. 종료 시 출력되는 `[session <id>]` 의 id 사용.
- 긴 대화는 임계(`[session].compact_threshold_tokens`, 기본 150k) 초과 시 오래된 앞부분을 모델로
  **자동 요약(compaction)** 하고 최근 메시지는 verbatim 유지. 요약이 놓친 디테일은
  `transcript_search` 로 원문 재조회.

### 세션 격리(`--isolate`)

cwd 가 git repo 면 세션마다 **별도 git worktree**(`~/.scv/worktrees/<id>`, 같은 커밋의 독립
체크아웃)를 만들어 그 안에서 작업한다 → 동시 세션이 같은 작업 파일을 건드리는 충돌 방지. 종료 시
worktree 를 자동 정리한다. 비-git 디렉터리면 격리 없이 cwd 를 그대로 쓴다.

---

## 설정

`~/.config/scv/config.toml`. 예시는 [`config/config.example.toml`](./config/config.example.toml).
**다단계 병합**(뒤가 앞을 덮음): 내장 기본값 → 사용자(`~/.config/scv/config.toml`, `SCV_CONFIG`
로 경로 변경) → 프로젝트(`./.scv/config.toml`, cwd 기준) → 환경변수(`SCV_*`) → CLI 플래그.

```bash
# 환경변수 오버라이드(중첩 키는 __): 출력 토큰 상한을 일시적으로 낮춤
SCV_AGENT__MAX_TOKENS=200 scv --no-tools "긴 얘기 해줘"
```

비밀(API 키)은 설정 파일에 두지 않는다 — 설정엔 "키를 읽을 환경변수 이름"(`api_key_env`)만 두고
실제 값은 환경변수로 주입한다.

---

## 스킬(skills)

스킬은 "특정 작업을 위한 절차/지식 묶음"이다. 디렉터리 하나가 한 스킬(`<name>/SKILL.md`)이고,
모델에는 평소 이름+설명만 노출하다가 필요할 때 본문을 주입한다(progressive disclosure).

- **발동(사용)**: TUI 에서 **`/<스킬이름>`** 으로 호출한다. 그 스킬의 본문 절차를 그 턴에
  주입해 모델이 따르게 한다. `/skills` 로 목록 확인.
- **전역**: `~/.config/scv/skills/<name>/SKILL.md` — 모든 프로젝트에서 공통.
- **프로젝트 로컬**: scv 를 연 폴더의 `./.scv/skills/<name>/SKILL.md` — 그 프로젝트에서만.
  같은 이름이면 프로젝트 로컬이 전역을 덮어쓴다.

> 내장 스킬은 없다. **컨텍스트 압축은 스킬이 아니라 자동**으로 한다 —
> `SummarizingContextManager` 가 입력 토큰이 `[session].compact_threshold_tokens`(기본 150k)를
> 넘으면 오래된 앞부분을 요약으로 바꿔 **전송 입력을 줄인다**(세션/JSONL 원본은 보존,
> `transcript_search` 로 복구). 별도 `/compact` 명령은 없다.

```bash
mkdir -p .scv/skills/my-skill
cat > .scv/skills/my-skill/SKILL.md <<'EOF'
---
name: my-skill
description: 이 스킬이 무엇을 하는지 한 줄
when_to_use: 언제 발동해야 하는지
---
(본문: 절차/지침)
EOF
```

추가 스킬 디렉터리는 설정 `[skills].dirs` 로 더할 수 있다.

---

## 프로바이더 전환(클라우드)

```bash
# OpenAI (effort 가 reasoning_effort 로 매핑: low|medium|high|xhigh)
export OPENAI_API_KEY=sk-...
scv --provider openai --model gpt-5.5 "이 코드베이스 설명해줘"
# 저비용 예시: --model gpt-5.4-mini. 비-reasoning 모델(gpt-4o 등)은 --effort none

# Anthropic
export ANTHROPIC_API_KEY=sk-ant-...
scv --provider anthropic --model claude-opus-4-8 "..."
```

OpenAI-호환 게이트웨이(OpenRouter·사내 LLM 등)는 `[[providers]].base_url` 로 바꾼다.

실행 중에는 TUI 에서 **`/provider <id>` · `/model <id>`** 슬래시 명령으로 전환한다(위 §TUI 키/명령).
`--provider` 만 줘도 그 프로바이더의 설정 모델로 켜진다.

---

## 문제 해결

- **`scv: command not found`** → `~/.cargo/bin` 이 PATH 에 없음(위 §설치).
- **"자기 소스 레포 안에서는 실행하지 않는다"** → 다른 프로젝트 디렉터리에서 실행(또는 `SCV_ALLOW_IN_REPO=1`).
- **`환경변수 OPENAI_API_KEY 미설정`** → 기본 프로바이더가 ollama 가 아님(설정의 `default_provider`
  확인) 또는 클라우드 사용 시 키 누락.
- **로컬 모델 무응답/연결 오류** → `ollama serve` 떠 있는지, `ollama pull qwen3.5:9b` 받았는지 확인.
- **진단 로그**: `SCV_LOG=debug scv ...`(stderr).

---

## 문서

| 문서 | 내용 |
|------|------|
| [`docs/DEVELOPMENT.md`](./docs/DEVELOPMENT.md) | 개발 가이드 — 소스 빌드/실행, lint 게이트, 크레이트 구조, 기여 규약 |
| [`docs/SETUP.md`](./docs/SETUP.md) | 세팅 가이드 — 툴체인, 빌드, 설정, 수동 테스트, 개발 워크플로 |
| [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) | 설계 — agentic loop, 4대 기능, 멀티 프로바이더, TUI 런타임, 크레이트 구조 |
| [`docs/ROADMAP.md`](./docs/ROADMAP.md) | 구현 우선순위/진행 상태 |
| [`docs/CODING_RULES.md`](./docs/CODING_RULES.md) | 코딩 규칙 — Rust 컨벤션, 에러/async/보안, LLM 연동 |
| [`AGENTS.md`](./AGENTS.md) | 에이전트(기여자) 작업 규약 — 단일 출처 규칙 |

## 개발

소스 빌드·실행·테스트·크레이트 구조·기여 규약은 **[`docs/DEVELOPMENT.md`](./docs/DEVELOPMENT.md)**.
설계는 [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md), 작업 규약은 [`AGENTS.md`](./AGENTS.md).
