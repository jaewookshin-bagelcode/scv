# scv 세팅 가이드

> 개발 환경 구성 → 빌드 → 설정 → 실행까지. macOS / Linux 기준(Windows 는 WSL 권장).

## 1. 사전 요구 사항

### 1.1 Rust 툴체인 설치 (rustup)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# 새 셸을 열거나:
source "$HOME/.cargo/env"
```

이 저장소는 `rust-toolchain.toml` 로 버전을 고정한다. 저장소 안에서 `cargo` 를 처음
실행하면 rustup 이 **알맞은 버전(1.96.0)과 컴포넌트(clippy/rustfmt/rust-src)를 자동
설치/전환**한다. 별도 작업은 필요 없다. (의존성 트리가 edition2024 를 요구해 Rust
1.85+ 가 필요하다 — 일부 크레이트는 1.87+.)

확인:

```bash
rustc --version     # 1.96.0 이어야 함(저장소 디렉터리 안에서)
cargo --version
```

### 1.2 시스템 의존성

- C 링커/컴파일러: macOS 는 `xcode-select --install`, Debian/Ubuntu 는
  `sudo apt install build-essential pkg-config`.
- TLS: `reqwest` 를 `rustls-tls` 로 쓰므로 OpenSSL 시스템 설치는 필요 없다.

## 2. 빌드

```bash
git clone <repo-url> scv && cd scv

# 워크스페이스 전체 빌드(디버그)
cargo build

# 릴리스 빌드(최적화 — 배포/벤치용)
cargo build --release
```

> 첫 빌드는 의존성 컴파일로 수 분 걸릴 수 있다. 이후엔 증분 빌드라 빠르다.
>
> 현재는 스캐폴드 단계다. 일부 함수는 `todo!()`/빈 스트림이며, 빌드는 통과하지만
> 실제 LLM 호출/도구 실행은 아직 채워야 한다(우선순위는 `docs/ROADMAP.md` 참고).

## 3. 설정

### 3.1 환경변수 (로컬 모델 / API 키)

기본 프로바이더는 **로컬 Ollama** 라 API 키·네트워크 없이 오프라인으로 돈다. Ollama 를
설치(https://ollama.com)하고 모델을 받는다:

```bash
ollama serve                 # 백그라운드 데몬(이미 떠 있으면 생략)
ollama pull qwen3.5:9b       # 기본 모델(tool calling 지원 — 코딩 에이전트에 필수)

# scv 는 api_key_env 를 요구하므로 아무 값이나 넣는다(Ollama 는 키를 무시).
export OLLAMA_API_KEY=ollama
export SCV_LOG=info          # trace|debug|info|warn|error
```

클라우드(OpenAI/Anthropic)로 전환해 쓰려면 해당 키를 환경변수로 둔다. 비밀은
**환경변수로만** 주입한다 — `.env.example` 을 복사해 채운다:

```bash
cp .env.example .env                  # .env 를 열어 키를 채운다(커밋되지 않음)
# 또는 직접:
export OPENAI_API_KEY="sk-..."        # --provider openai (ChatGPT 5.5)
export ANTHROPIC_API_KEY="sk-ant-..." # --provider anthropic
```

> 로그인형 CLI 인증(예: `gcloud auth login`)처럼 이 세션에서 명령을 직접 실행해야
> 하면, 프롬프트에 `! <command>` 를 입력하면 출력이 대화에 바로 들어온다.

### 3.2 config.toml

**현재**: `~/.config/scv/config.toml` 한 곳만 읽는다(단일 파일).

**계획**(다단계 병합, 뒤가 앞을 덮어씀 — `docs/ROADMAP.md` 4d):

```
내장 기본값 → ~/.config/scv/config.toml → ./.scv/config.toml(프로젝트) → 환경변수(SCV_*) → CLI 플래그
```

예시를 사용자 설정 위치로 복사:

```bash
mkdir -p ~/.config/scv
cp config/config.example.toml ~/.config/scv/config.toml
```

주요 항목(`config/config.example.toml` 주석 참고):
- `default_provider` — 기본 프로바이더 id
- `[agent]` — `max_tokens`, `effort`, `max_tool_iterations`
- `[permissions]` — 도구별 자동 허용/질문/거부
- `[ui]` — 진행 표시 스피너 스타일(`spinner`: `auto`/`unicode`/`ascii`). 색은 `NO_COLOR` 존중
- `[[providers]]` — 프로바이더별 `kind`/`model`/`api_key_env`/`base_url`

### 3.3 스킬 추가 (선택)

`[skills].dirs` 의 디렉터리 아래에 스킬 폴더를 둔다:

```
~/.config/scv/skills/
  pdf-report/
    SKILL.md
```

`SKILL.md` 형식:

```markdown
---
name: pdf-report
description: PDF 보고서를 생성하고 검증한다
when_to_use: 사용자가 "PDF 보고서"를 요청할 때
---

(본문: 절차 설명...)
```

코드 변경 없이 디렉터리만 추가하면 로드된다.

## 4. 실행

```bash
# 인터랙티브 TUI (인자 없이)
cargo run --bin scv

# 원샷 모드
cargo run --bin scv -- "이 저장소의 빌드 방법을 알려줘"

# 다른 프로바이더/모델로 전환하거나 세션 재개:
cargo run --bin scv -- --provider anthropic --model claude-opus-4-8 "..."
cargo run --bin scv -- --resume <session-id>

# 릴리스 바이너리 직접 실행
./target/release/scv "..."
```

### 4.1 실제 모델로 수동 테스트

원샷 모드(`stream` 구현 완료)로 실제 모델을 호출할 수 있다.

**기본(로컬 Ollama).** §3.1 대로 `ollama serve` + `ollama pull qwen3.5:9b` + `OLLAMA_API_KEY`
만 되어 있으면 키·네트워크 없이 바로 돈다. 로컬은 호환 모드라 `reasoning_effort`·
`stream_options` 를 **자동 생략**하므로 `--effort` 설정과 무관하게 동작한다:

```bash
cargo run --bin scv -- "이 repo 구조를 한 문단으로 설명해줘"
# 받아둔 다른 로컬 모델로:
cargo run --bin scv -- --model llama3.1 "안녕, 한 줄로 자기소개 해줘"
```

**클라우드(OpenAI).** `--provider openai` 로 전환한다. **설정의 예시 모델 `gpt-5.5` 는
플레이스홀더**이므로 실재 모델을 `--model` 로 지정한다:

```bash
export OPENAI_API_KEY="sk-..."

# reasoning(추론) 계열 모델: effort 가 reasoning_effort 로 매핑된다.
cargo run --bin scv -- --provider openai --model o4-mini "이 repo 구조를 한 문단으로 설명해줘"

# 비-reasoning 모델(gpt-4o 등)은 reasoning_effort 를 거부(400)하므로 --effort none 으로 끈다:
cargo run --bin scv -- --provider openai --model gpt-4o --effort none "안녕, 한 줄로 자기소개 해줘"
```

- 설정 파일은 **cwd 와 무관하게** `~/.config/scv/config.toml` 을 읽는다(`SCV_CONFIG`
  환경변수로 다른 경로 지정 가능). 없으면 그 경로를 알려주는 에러가 난다 — §3.2 로 만든다.
- 다른 OpenAI 호환 엔드포인트/게이트웨이(사내·OpenRouter·로컬 LLM)는
  `[[providers]].base_url` 로 바꾼다(경로 A). 단 게이트웨이가 `max_completion_tokens`·
  `stream_options.include_usage`·`reasoning_effort`·`tool_calls` 를 받아줘야 하고, 미지원이면
  어댑터에 호환 옵션을 추가해야 한다. 비-reasoning 백엔드는 `--effort none`.
- `write`/`edit`/`bash` 는 `Ask` 도구다. **인터랙티브 TUI**(인자 없이 실행)에선 모델이
  이 도구를 부르면 **승인 모달**이 떠 `y`(허용)/`n`(거부)로 결정한다(§4.5). **원샷 모드**엔
  대화형 경로가 없어 그 턴이 거부로 끝나므로 → **원샷 수동 테스트는 읽기 위주 프롬프트**가
  매끄럽다. 모달 없이 자동 허용하려면 `[permissions.tools]` 에서 해당 도구를 `allow` 로 둔다.
- 요청/응답 진단은 `SCV_LOG=debug` 로 stderr 에서 본다. HTTP 오류는 본문째 보고된다.

### 4.2 다른 프로젝트 디렉터리에서 쓰기 (설치)

scv 는 **현재 디렉터리(cwd)를 작업 대상**으로 본다(도구의 파일 읽기/쓰기 루트, `AGENTS.md`
탐색 기준). 따라서 scv 저장소가 아니라 **대상 프로젝트 디렉터리에서** 실행한다. 설정은
cwd 와 무관한 `~/.config/scv/config.toml` 을 쓰므로 어느 디렉터리에서 실행해도 동일하다.

```bash
# 한 번 설치 → ~/.cargo/bin/scv (이 경로가 PATH 에 있어야 어디서든 `scv` 로 실행된다)
cargo install --path crates/scv-cli

# 또는 빌드만 하고 절대경로로 실행
cargo build --release        # → target/release/scv

# 대상 프로젝트로 가서 실행(여기서 cwd = 그 프로젝트)
cd /path/to/your/project
scv --model o4-mini "이 코드베이스 구조를 설명해줘"
# 설치 안 했으면 절대경로로:
/path/to/scv-repo/target/release/scv --model o4-mini "..."
```

> 실행 중 **Ctrl-C** 는 진행 중인 턴을 중단한다(앱은 유지). 대기(idle) 상태에서 두 번
> 누르면 종료한다. 원샷 모드에서는 Ctrl-C 가 그 호출을 중단한다. (동작 설계는
> `docs/ARCHITECTURE.md` §4.5.)

도움말:

```bash
cargo run --bin scv -- --help
```

## 5. 개발 워크플로

PR 전 로컬에서 다음을 통과시킨다:

```bash
cargo fmt --all                                   # 포맷
cargo clippy --all-targets --all-features -- -D warnings   # 린트(무경고)
cargo test --workspace                            # 테스트
scripts/coverage.sh                               # 커버리지 게이트(blocking)
```

`scripts/coverage.sh` 는 테스트 티어별 라인 커버리지를 강제한다 — **unit ≥ 95% ·
integration ≥ 90% · e2e ≥ 85%**(미달 시 비-0 종료, 임계의 SSOT 는 `docs/CODING_RULES.md`
§10). 전제 도구는 한 번만 설치한다:

```bash
cargo install cargo-llvm-cov --locked             # 커버리지 측정기(컴포넌트 llvm-tools 는
                                                  # rust-toolchain.toml 에 고정 → 자동 설치)
SCV_COV_UNIT=80 scripts/coverage.sh               # 임계 임시 조정(INTEGRATION/E2E 도 동일)
```

자주 쓰는 보조 명령:

```bash
cargo check --workspace          # 빠른 타입 체크(코드 작성 중)
cargo doc --no-deps --open       # 문서 빌드/열기
cargo run -p scv-cli -- ...      # 특정 크레이트 바이너리 실행
```

권장 도구(선택):

```bash
cargo install cargo-watch        # 파일 변경 시 자동 재빌드/테스트
cargo watch -x check -x test
```

## 6. 문제 해결

| 증상 | 원인/해결 |
|------|----------|
| `환경변수 ... 미설정` | `.env`/export 로 해당 API 키를 설정했는지 확인 |
| `프로바이더 ... 설정 없음` | `~/.config/scv/config.toml` 의 `[[providers]]` id 확인 |
| 빌드 시 링커 오류 | §1.2 시스템 의존성(build-essential/xcode) 설치 |
| clippy 경고로 CI 실패 | 로컬에서 `cargo clippy -- -D warnings` 로 재현 후 수정 |
| 첫 빌드가 너무 느림 | 정상(의존성 컴파일). 이후 증분 빌드는 빠름 |

## 7. 다음 읽을거리

- 설계 전반: [`docs/ARCHITECTURE.md`](./ARCHITECTURE.md)
- 코딩 규칙: [`docs/CODING_RULES.md`](./CODING_RULES.md)
