# scv

터미널에서 동작하는 멀티 프로바이더 코딩 에이전트(Claude Code / Codex 류). Rust + Tokio.

시스템 프롬프트 · 세션 · 도구(tool) · 스킬(skill)을 1급 기능으로 제공하고, LLM
프로바이더를 추상화해 교체할 수 있다. **기본 프로바이더는 OpenAI(ChatGPT 5.5)**,
Anthropic 은 대체.

## 빠른 시작

```bash
cp .env.example .env            # OPENAI_API_KEY 등 채우기
cargo build
cargo run --bin scv -- "이 저장소 구조를 설명해줘"   # 원샷
cargo run --bin scv                                   # 인터랙티브 TUI
```

자세한 설치/설정/실행은 [`docs/SETUP.md`](./docs/SETUP.md).

## 문서

| 문서 | 내용 |
|------|------|
| [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) | 설계 개요 — agentic loop, 4대 기능, 멀티 프로바이더, 크레이트 구조 |
| [`docs/CODING_RULES.md`](./docs/CODING_RULES.md) | 코딩 규칙 — Rust 컨벤션, 에러/async/보안, LLM 연동 규칙 |
| [`docs/SETUP.md`](./docs/SETUP.md) | 세팅 가이드 — 툴체인, 빌드, 설정, 개발 워크플로 |

## 워크스페이스 구조

```
scv-core        도메인 모델 + trait(Provider/Tool/Skill/...) + agentic loop  ← 모든 추상의 중심
scv-providers   Provider 구현 (anthropic, openai)
scv-tools       Tool 구현 (read/write/edit/bash/glob/grep) + 권한 정책
scv-skills      SKILL.md 로더 (progressive disclosure)
scv-config      설정 로드/병합
scv-tui         ratatui 기반 인터랙티브 UI + 스트림 렌더
scv-cli         바이너리 `scv` — 합성 루트(조립/부트스트랩)
```

의존성은 항상 `scv-core` 를 향한다(의존성 역전). 새 프로바이더/도구/스킬 추가 시
core 와 다른 크레이트를 건드릴 필요가 없다.

## 상태

설계 + 코드 스캐폴드 단계. 타입·trait·조립 골격이 완성돼 있고, 실제 LLM 호출/도구
실행 채우기가 다음 작업이다 — [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §8.
