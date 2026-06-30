# scv 세팅 가이드

> 개발 환경 구성 → 빌드 → 설정 → 실행까지. macOS / Linux 기준(Windows 는 WSL 권장).

## 1. 사전 요구 사항

**Rust 툴체인** — rustup 으로 설치한다. 버전은 `rust-toolchain.toml` 이 고정하므로, 저장소
안에서 `cargo` 를 처음 실행하면 알맞은 버전(1.96.0)과 컴포넌트(clippy/rustfmt/rust-src)가
자동 설치된다.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"     # 또는 새 셸
rustc --version               # 저장소 안에서 1.96.0 확인
```

**시스템 의존성** — C 링커/컴파일러(macOS `xcode-select --install`, Debian/Ubuntu
`sudo apt install build-essential pkg-config`). TLS 는 `rustls-tls` 라 OpenSSL 설치 불필요.

## 2. 빌드

```bash
git clone <repo-url> scv && cd scv
cargo build              # 디버그
cargo build --release    # 릴리스(최적화 — 배포/벤치용)
```

> 첫 빌드는 의존성 컴파일로 수 분 걸린다(이후 증분 빌드는 빠름). 핵심 경로는 구현 완료다 —
> 어댑터 `stream`, 도구 8종, 권한 게이트·세션 영속화·컨텍스트 압축이 모두 동작한다
> (`todo!()` 없음). 남은 우선순위는 [`docs/ROADMAP.md`](./ROADMAP.md).

## 3. 설정

### 3.1 환경변수 (로컬 모델 / API 키)

기본 프로바이더는 **로컬 Ollama** 라 API 키·네트워크 없이 오프라인으로 돈다:

```bash
ollama serve                 # 데몬(이미 떠 있으면 생략) — https://ollama.com
ollama pull qwen3.5:9b       # 기본 모델(tool calling 지원 — 코딩 에이전트에 필수)
export SCV_LOG=info          # (선택) trace|debug|info|warn|error
```

클라우드로 전환할 때만 키를 둔다. 비밀은 **환경변수로만** 주입한다(설정 파일에 두지 않음):

```bash
cp .env.example .env                  # .env 에 키를 채운다(커밋되지 않음). 또는 직접 export:
export OPENAI_API_KEY="sk-..."        # --provider openai
export ANTHROPIC_API_KEY="sk-ant-..." # --provider anthropic (직결, x-api-key)
export GEMINI_API_KEY="..."           # --provider gemini (무료 티어, §4.1)
```

**aiproxy(사내 게이트웨이) 경유 Anthropic** — 개인 Anthropic 키 없이 사내 토큰으로 Sonnet/Haiku 를
쓴다. `config.example.toml` 의 `aiproxy` 프로바이더(`kind="anthropic"` + `base_url` 끝 `/anthropic`
+ `auth_style="bearer"`)를 쓰고, 토큰만 환경변수로 주입한다:

```bash
codeb login --token aiproxy_xxx       # 토큰 발급/로그인(Cloudflare VPN + Okta 필요)
export CODEB_TOKEN="aiproxy_xxx"       # config 의 api_key_env 가 이 변수를 Bearer 로 전송
scv --provider aiproxy "..."          # --model claude-sonnet-4-6 | claude-haiku-4-5
```

> **서버사이드 web_search**(선택): aiproxy 프로바이더에 `web_search = true` 를 주면(anthropic 전용)
> 모델이 **서버에서** 웹 검색을 실행하고 결과·인용을 응답에 실어 보낸다(로컬 `web_fetch` 도구와
> 달리 권한 모달·왕복 없음). 기본 off.

### 3.2 config.toml

scv 의 설정·스킬·세션·worktree 는 모두 `~/.scv/` 아래 모인다(Claude `~/.claude`,
Codex `~/.codex` 처럼). 설정은 **다단계 병합**(뒤가 앞을 덮음):

```
내장 기본값 → ~/.scv/config.toml → ./.scv/config.toml(프로젝트) → 환경변수(SCV_*, 중첩은 __) → CLI 플래그
```

```bash
mkdir -p ~/.scv && cp config/config.example.toml ~/.scv/config.toml
```

주요 항목(`config/config.example.toml` 주석 참고): `default_provider` · `[agent]`(`max_tokens`·
`effort`·`max_tool_iterations`) · `[permissions]`(도구별 허용/질문/거부) · `[ui]`(스피너 스타일,
색은 `NO_COLOR` 존중) · `[[providers]]`(`kind`/`model`/`api_key_env`/`base_url`).

> **프로젝트 마커 `./.scv/`**: 어떤 디렉터리에서 처음 실행하면 그 cwd 에 빈 `./.scv/` 를 만든다
> (프로젝트 로컬 `config.toml`·`skills/` 를 둘 자리, `.gitignore` 대상). 핵심 설정·세션은 `~/.scv/`
> 에 있으므로 생성 실패해도 실행은 막지 않는다.

### 3.3 스킬 추가 (선택)

`[skills].dirs` 의 디렉터리(`~/.scv/skills/` 등) 아래에 스킬 폴더를 둔다. 코드 변경 없이
디렉터리만 추가하면 로드된다.

```markdown
<!-- ~/.scv/skills/pdf-report/SKILL.md -->
---
name: pdf-report
description: PDF 보고서를 생성하고 검증한다
when_to_use: 사용자가 "PDF 보고서"를 요청할 때
---
(본문: 절차 설명...)
```

## 4. 실행

scv 는 **현재 디렉터리(cwd)를 작업 대상**으로 본다(파일 읽기/쓰기 루트, `AGENTS.md` 탐색 기준).
따라서 scv 저장소가 아니라 **대상 프로젝트 디렉터리에서** 실행한다. 설정은 cwd 와 무관한
`~/.scv/config.toml` 을 쓰므로 어디서 실행해도 동일하다.

**설치** — `scv` 를 어디서든 부르려면 PATH 에 올린다:

```bash
sh scripts/scv-link.sh install   # 권장: release 빌드 + PATH 에 심볼릭 링크
                                 # → 코드 수정 후 cargo build --release 만 다시 하면 반영
cargo install --path crates/scv-cli   # 대안: 복사본(--force 로 재설치). 수정분 반영 안 자동
```

**실행** — 설치했으면 `scv`, 개발 중엔 `cargo run --bin scv --`:

```bash
cd /path/to/your/project              # 작업 대상 프로젝트(= cwd)
scv                                   # 인터랙티브 TUI
scv "이 코드베이스 구조를 설명해줘"        # 원샷 — 기본 로컬 ollama(qwen3.5:9b)
scv --provider openai --model gpt-5.5 "..."   # 프로바이더/모델 전환
scv --resume <session-id>             # 세션 재개
scv --help                            # 전체 플래그
```

> **scv 는 자기 소스 레포 안에서는 실행을 거부한다**(자기 코드를 작업 대상으로 삼는 사고 방지).
> 다른 디렉터리에서 쓰자 — 개발 중 의도적으로 돌리려면 `SCV_ALLOW_IN_REPO=1`.
> **Ctrl-C**: 진행 중인 턴을 중단(앱 유지), idle 에서 두 번 누르면 종료.

### 4.1 실제 모델로 수동 테스트

원샷 모드로 실제 모델을 호출해 확인한다.

```bash
# 로컬 Ollama(기본) — 키·네트워크 불필요. 호환 모드라 --effort 무관하게 동작
scv "이 repo 구조를 한 문단으로 설명해줘"

# OpenAI — effort 가 reasoning_effort 로 매핑(low|medium|high|xhigh). 저비용: --model gpt-5.4-mini
scv --provider openai --model gpt-5.5 "..."
# 비-reasoning 모델(gpt-4o 등)은 reasoning_effort 를 거부(400) → --effort none 으로 끈다
scv --provider openai --model gpt-4o --effort none "안녕, 한 줄로 자기소개"

# Gemini(무료 티어) — OpenAI-호환 엔드포인트. 로컬보다 지시 준수가 나아 무료 대안으로 안정적.
# 키: https://aistudio.google.com → "Get API key"(카드 불필요). config 의 gemini 프로바이더 참고
scv --provider gemini --model gemini-2.5-flash "..."
# 무료 모델: gemini-2.5-flash | gemini-3.5-flash | *-flash-lite | gemma-4 (Pro 계열은 유료)
```

- **다른 OpenAI-호환 게이트웨이**(사내·OpenRouter·로컬 LLM)는 `[[providers]].base_url` 로 붙인다.
  게이트웨이가 `max_completion_tokens`·`stream_options.include_usage`·`reasoning_effort`·
  `tool_calls` 를 받아줘야 하며, 비-reasoning 백엔드는 `--effort none`.
- `write`/`edit`/`bash` 는 `Ask` 도구다. **TUI** 는 호출 시 승인 모달(`y`/`n`)을 띄우고,
  **원샷** 은 승인 경로가 없어 그 턴이 거부로 끝난다 → 원샷 테스트는 **읽기 위주 프롬프트**가
  매끄럽다. 자동 허용은 `[permissions.tools]` 에서 `allow`(동작 설계는 ARCHITECTURE §4.5).
- 요청/응답 진단은 `SCV_LOG=debug` 로 stderr 에서 본다(HTTP 오류는 본문째 보고).

## 5. 개발 워크플로

PR 전 로컬에서 다음을 통과시킨다(`scripts/coverage.sh` 는 티어별 라인 커버리지 강제 —
**unit ≥ 95% · integration ≥ 78% · e2e ≥ 85%**, 임계 SSOT 는 [`docs/CODING_RULES.md`](./CODING_RULES.md) §10):

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace
scripts/coverage.sh                               # blocking
cargo install cargo-llvm-cov --locked             # (최초 1회) 커버리지 측정기
```

보조: `cargo check --workspace`(빠른 타입 체크) · `cargo doc --no-deps --open` ·
`cargo watch -x check -x test`(`cargo install cargo-watch`).

## 6. 문제 해결

| 증상 | 원인/해결 |
|------|----------|
| `환경변수 ... 미설정` | `.env`/export 로 해당 API 키를 설정했는지 확인 |
| `프로바이더 ... 설정 없음` | `~/.scv/config.toml` 의 `[[providers]]` id 확인 |
| 빌드 시 링커 오류 | §1 시스템 의존성(build-essential/xcode) 설치 |
| clippy 경고로 실패 | `cargo clippy -- -D warnings` 로 재현 후 수정 |
| 첫 빌드가 느림 | 정상(의존성 컴파일). 이후 증분 빌드는 빠름 |

## 7. 다음 읽을거리

- 설계 전반: [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md)
- 코딩 규칙: [`docs/CODING_RULES.md`](./CODING_RULES.md)
