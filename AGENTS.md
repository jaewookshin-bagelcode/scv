# AGENTS.md

이 파일은 `scv` 저장소에서 작업하는 **에이전트(및 사람)** 를 위한 작업 규약이다.
코드를 만지기 전에 이 문서를 읽고 따른다. 더 깊은 내용은 `docs/` 를 참조한다.

## 프로젝트 한 줄 요약

`scv` = 터미널에서 도는 **멀티 프로바이더 코딩 에이전트**(Claude Code / Codex 류).
Rust + Tokio. 시스템 프롬프트 · 세션 · 도구 · 스킬을 1급 기능으로 제공한다.

- **기본 LLM 프로바이더: 로컬(Ollama, 모델 `qwen3.5:9b`)** — 무료·오프라인.
  OpenAI(`gpt-5.5`) · Anthropic 은 `--provider openai|anthropic` 으로 전환하는 클라우드 대체.
- 인터페이스: 인터랙티브 CLI/TUI(+ 원샷 모드).

## 워크스페이스 지도

Cargo 워크스페이스. **의존성은 항상 `scv-core` 를 향한다(순환 없음).** 새 프로바이더/
도구/스킬을 추가할 때 core 와 다른 크레이트를 건드리면 안 된다(의존성 역전).

| 크레이트 | 책임 |
|---------|------|
| `crates/scv-core` | 도메인 모델 + trait(Provider/Tool/Skill/...) + **agentic loop**. 내부 의존 0 |
| `crates/scv-providers` | `Provider` 구현 (ollama 기본 — openai 어댑터 재사용, openai/anthropic 클라우드 대체) |
| `crates/scv-tools` | `Tool` 구현(read/write/edit/bash/glob/grep) + 권한 정책 |
| `crates/scv-skills` | `SKILL.md` 로더(progressive disclosure) |
| `crates/scv-config` | 설정 로드/병합 |
| `crates/scv-tui` | ratatui UI + 스트림 Observer |
| `crates/scv-cli` | 바이너리 `scv` — 합성 루트(조립/부트스트랩) |

상태: **Phase 0–4 구현 완료**. 프로바이더 `stream`·도구·권한 게이트·인터랙티브 TUI·
세션·컨텍스트 관리가 동작한다(코드에 `todo!()` 없음). 남은 작업/우선순위는
`docs/ROADMAP.md` 참고(선택적 `4f` Codex 런타임만 보류).

## 단일 출처(SSOT) 규칙 ★

이 프로젝트는 **항상 SSOT(Single Source of Truth)를 남기고, 구현 후 SSOT를 갱신한다.**

- **먼저 문서화한다**: 모든 설계·규약·결정은 문서(SSOT)에 남긴다. 코드만 바꾸고
  문서를 안 고치는 PR 은 미완으로 본다.
- **SSOT 맵**(각 주제의 진실은 단 한 곳):
  | 주제 | SSOT |
  |------|------|
  | 작업 규약 | `AGENTS.md` (`CLAUDE.md` 는 이 파일을 가리키는 포인터일 뿐) |
  | 설계/아키텍처 | `docs/ARCHITECTURE.md` |
  | 코딩 규칙 | `docs/CODING_RULES.md` |
  | 설정/빌드/실행 | `docs/SETUP.md` |
  | 남은 작업/구현 순서 | `docs/ROADMAP.md` |
- **구현 후 SSOT를 수정한다**: 코드가 문서와 어긋나면 **문서가 진실이 되도록** 같은
  PR 에서 SSOT를 갱신한다. 예) 새 기능을 구현했으면 `docs/ROADMAP.md` 에서
  그 항목을 체크하고, 인터페이스/동작/기본값이 바뀌면 해당 문서를 함께 고친다.
- **중복하지 말고 링크한다**: 같은 사실을 두 문서에 베껴 쓰지 않는다(`CLAUDE.md` ↔
  `AGENTS.md` 처럼). 사실은 SSOT 한 곳, 나머지는 참조.
- **충돌은 SSOT가 이긴다**: 두 곳이 어긋나면 SSOT를 기준으로 즉시 일치시킨다.

## 명령어 (완료 전 반드시 통과)

```bash
cargo fmt --all                                            # 포맷
cargo clippy --all-targets --all-features -- -D warnings   # 린트(무경고)
cargo test --workspace                                     # 테스트
scripts/coverage.sh                                        # 커버리지 게이트(blocking, 티어별 임계)
cargo check --workspace                                    # 빠른 타입 체크
cargo run --bin scv -- "..."                               # 원샷 실행
cargo run --bin scv                                        # 인터랙티브 TUI
```

작업을 "완료"라고 말하기 전에 `fmt` + `clippy -D warnings` + `test` + `coverage.sh` 를
통과시킨다. 커버리지 게이트는 테스트 티어별 라인 커버리지(unit/integration/e2e)를 강제하며
임계의 SSOT 는 `docs/CODING_RULES.md` §10 이다. 실패하면 출력과 함께 그대로 보고한다
(통과한 척 금지).

## 반드시 지키는 규칙 (요약 — 전문은 `docs/CODING_RULES.md`)

- **의존성 역지향**: core 가 trait 정의, 바깥 crate 가 구현. core 가 구체 crate 에
  의존하면 안 된다.
- **데이터 지향 + 함수형(DOT·FP)**: 데이터와 동작을 분리해 도메인은 투명한 데이터
  (필드 공개 struct/enum)로 두고, 기초 함수는 단일 책임(가능하면 순수)으로, 상위
  함수가 합성한다. 부작용/IO 는 가장자리로(functional core, imperative shell).
  과한 분해는 지양(§4.1).
- **에러**: 라이브러리는 `thiserror` enum, 바이너리(`scv-cli`)에서만 `anyhow`. 둘을
  섞지 않는다. 비-테스트 코드에 `unwrap()`/`expect()`/`panic!`/인덱싱 패닉 금지
  (증명 가능한 불변식만 예외 + 사유 주석).
- **async**: Tokio. async 안에서 블로킹 IO 금지(`tokio::fs` 사용). trait 의 async 는
  `#[async_trait]`.
- **로깅**: 라이브러리에서 `println!` 금지 → `tracing`. 사용자 출력만 stdout.
- **비밀**: API 키는 **환경변수로만**. 설정/코드/로그/세션 파일에 평문 키 금지.
  `.env` 커밋 금지(`.env.example` 만).
- **보안**: 도구의 모든 경로 입력은 `workdir` 안으로 제한(경로 탈출 방지). bash 입력은
  신뢰 불가 모델 출력으로 취급.
- **의존성**: 루트 `[workspace.dependencies]` 에서 단일 버전 관리, crate 는
  `dep.workspace = true` 로만 참조. `Cargo.lock` 은 커밋한다(애플리케이션).
- **LLM 연동**(전문 `docs/CODING_RULES.md` §9):
  - 스트리밍이 기본. 모델 id 는 설정에서 주입(하드코딩 금지), 기본 `qwen3.5:9b`(로컬 Ollama).
  - `tool_use.input` 은 JSON 파싱으로만(문자열 매칭 금지).
  - 병렬 도구 결과는 **하나의 user 메시지**에 모은다.
  - 종료 사유(stop/finish reason)를 먼저 확인 후 본문을 읽는다.
  - 로컬 Ollama(기본): OpenAI-호환 어댑터 재사용(`kind="ollama"`), `base_url` 자동(`localhost:11434/v1`),
    호환 모드(reasoning_effort·stream_options 미전송).
  - OpenAI(클라우드 대체): `Authorization: Bearer`, `/chat/completions`, 자체 reasoning 파라미터.
  - Anthropic(대체): `x-api-key` + `anthropic-version`, `thinking:{type:"adaptive"}` +
    `output_config.effort`(`budget_tokens`/`temperature`/`top_p` 금지 — 400).

## 무언가를 추가할 때

| 추가 대상 | 할 일 |
|----------|------|
| 새 LLM 프로바이더 | `Provider` 구현 + `scv_providers::build` 에 `kind` 분기 |
| 새 도구 | `Tool` 구현 + `default_registry` 등록 |
| 새 스킬 | `SKILL.md` 디렉터리만 추가(코드 변경 없음) |
| compaction 전략 | `ContextManager` 구현 후 주입 |
| 세션 저장 백엔드 | `SessionStore` 구현 |
| 권한 UX | `PermissionGate` 구현 |

## 작업 에티켓

- 이 레포는 로컬/솔로 작업이라 **main 에 직접 커밋**한다(별도 기능 브랜치 불필요). 커밋/푸시는 사용자가 요청할 때만.
- 되돌리기 어렵거나 외부로 나가는 동작(삭제/덮어쓰기/외부 전송)은 먼저 확인한다.
- 변경은 주변 코드의 관용구·주석 밀도·네이밍에 맞춘다.
- 파일을 지우거나 덮어쓰기 전에 대상을 확인하고, 설명과 어긋나면 진행 대신 보고한다.

## 더 읽기

- 설계 전반: [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md)
- 구현 우선순위/로드맵: [`docs/ROADMAP.md`](./docs/ROADMAP.md)
- 코딩 규칙(전문): [`docs/CODING_RULES.md`](./docs/CODING_RULES.md)
- 세팅/빌드/실행: [`docs/SETUP.md`](./docs/SETUP.md)
