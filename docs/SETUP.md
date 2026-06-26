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
실행하면 rustup 이 **알맞은 버전(1.83.0)과 컴포넌트(clippy/rustfmt)를 자동 설치/전환**
한다. 별도 작업은 필요 없다.

확인:

```bash
rustc --version     # 1.83.0 이어야 함(저장소 디렉터리 안에서)
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

### 3.1 API 키 (환경변수)

비밀은 **환경변수로만** 주입한다. `.env.example` 을 복사해 채운다:

```bash
cp .env.example .env
# .env 를 열어 ANTHROPIC_API_KEY 등을 채운다. .env 는 커밋되지 않는다.
```

셸에서 직접 export 해도 된다:

```bash
export OPENAI_API_KEY="sk-..."        # 기본 프로바이더(ChatGPT 5.5)
export SCV_LOG=info                    # trace|debug|info|warn|error
# (선택) Anthropic 으로 전환해 쓰려면:
# export ANTHROPIC_API_KEY="sk-ant-..."
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

# 기본은 OpenAI(gpt-5.5). 다른 프로바이더/모델로 전환하거나 세션 재개:
cargo run --bin scv -- --provider anthropic --model claude-opus-4-8 "..."
cargo run --bin scv -- --resume <session-id>

# 릴리스 바이너리 직접 실행
./target/release/scv "..."
```

도움말:

```bash
cargo run --bin scv -- --help
```

## 5. 개발 워크플로

PR 전 로컬에서 다음을 통과시킨다(CI 와 동일):

```bash
cargo fmt --all                                   # 포맷
cargo clippy --all-targets --all-features -- -D warnings   # 린트(무경고)
cargo test --workspace                            # 테스트
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
