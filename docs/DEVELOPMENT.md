# 개발 가이드 (DEVELOPMENT)

scv 에 기여하거나 소스에서 빌드/실행하는 사람을 위한 문서. **사용자용 설치·사용법은
[`../README.md`](../README.md)** 를 본다. 아래는 기여자 관점의 진입점이며, 세부는 각 SSOT 문서로
링크한다(중복 금지 — `AGENTS.md` § 단일 출처 규칙).

## 요구사항 / 툴체인

- Rust 툴체인은 `rust-toolchain.toml` 로 **1.96.0 고정**(edition 2024 의존성). 설정·설치 세부는
  [`SETUP.md`](./SETUP.md) §1.
- 기본 프로바이더는 로컬 Ollama(`qwen3.5:9b`) — 키 없이 동작. 클라우드는 환경변수 키.

## 빌드 / 개발 중 실행

```bash
cargo build                       # 디버그 빌드
cargo run --bin scv -- "..."      # 원샷 실행
cargo run --bin scv               # 인터랙티브 TUI
```

> **자기 레포 안에서는 실행이 거부된다**(자기 코드를 작업 대상으로 삼는 사고 방지). 레포 안에서
> 개발 목적으로 돌리려면 `SCV_ALLOW_IN_REPO=1 cargo run --bin scv -- "..."`. 구현은
> `crates/scv-cli/src/main.rs` 의 `scv_repo_root`/`is_within`.

설치(다른 디렉터리에서 `scv` 로 쓰기)는 README §설치, 또는 심볼릭 링크 스크립트:

```bash
sh scripts/scv-link.sh install    # target/release/scv 를 PATH 에 링크(재빌드 시 자동 반영)
sh scripts/scv-link.sh uninstall  # 링크 제거(설치한 스킬은 남김 — 사용자 데이터)
```

`install` 은 레포의 **기본 스킬**(`skills/<name>/SKILL.md` — 현재 `commit`·`review`)을
`~/.scv/skills/` 로 복사한다(기존 같은 이름은 보존). 새 기본 스킬은 `skills/` 에 디렉터리만
추가하면 된다(코드 변경 없음). `.claude/skills/`(이 repo 의 Claude Code 용)와는 별개다.

## "끝남"의 정의 — lint 게이트

변경을 끝났다고 부르기 전 통과해야 하는 것(절차·보고 형식은 `.claude/skills/lint`):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings   # 경고 0
cargo test --workspace
scripts/coverage.sh                                        # 티어별 커버리지(unit≥95·integration≥78·e2e≥85, blocking)
```

- 바뀐 동작은 **같은 변경 안에서 SSOT 문서**(ARCHITECTURE/ROADMAP/SETUP/CODING_RULES)에 반영.
- 비밀(키/토큰)이 코드·로그·설정·커밋에 섞이지 않게. `.env` 는 커밋 금지.
- 테스트 티어/커버리지 임계의 정의는 [`CODING_RULES.md`](./CODING_RULES.md) §10.

## 커밋 / 기여 규약

- 작업 규약의 단일 출처는 [`../AGENTS.md`](../AGENTS.md). 커밋 컨벤션은 `.claude/skills/commit`
  (Conventional Commits, 헤더 ≤72자, 한 커밋=한 의도, 코드+SSOT 같은 커밋).
- 이 레포는 로컬 작업이라 **main 에 직접 커밋**한다(별도 기능 브랜치 불필요).

## 워크스페이스 구조

의존성은 항상 `scv-core` 를 향한다(의존성 역전). 새 프로바이더/도구/스킬은 core 변경 없이
각 크레이트에서 trait 을 구현해 추가한다.

```
scv-core        도메인 모델 + trait(Provider/Tool/Skill/ContextManager/SessionStore/...) + agentic loop  ← 추상의 중심
scv-providers   Provider 구현 — openai · openai-compat · ollama(openai 어댑터 재사용) · anthropic
scv-tools       Tool 구현 — read/write/edit/bash/glob/grep/web_fetch/transcript_search + 권한 정책
scv-skills      SKILL.md 로더(사용자 스킬) — progressive disclosure
scv-config      설정 로드 + 다단계 병합(figment)
scv-tui         ratatui 기반 인터랙티브 UI — 스트림/사고 렌더 · 승인 모달 · 진행 표시 · 인터럽트
scv-cli         바이너리 `scv` — 합성 루트(조립/부트스트랩)
```

## 더 읽기

| 문서 | 내용 |
|------|------|
| [`SETUP.md`](./SETUP.md) | 툴체인·빌드·설정·수동 테스트 세부 |
| [`ARCHITECTURE.md`](./ARCHITECTURE.md) | 설계 — agentic loop · 4대 기능 · 멀티 프로바이더 · TUI 런타임 |
| [`ROADMAP.md`](./ROADMAP.md) | 구현 우선순위 / 진행 상태 |
| [`CODING_RULES.md`](./CODING_RULES.md) | Rust 컨벤션 · 에러/async/보안 · LLM 연동 · 테스트 티어 |
| [`../AGENTS.md`](../AGENTS.md) | 에이전트(기여자) 작업 규약 — 단일 출처 규칙 |
