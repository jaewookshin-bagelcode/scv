# scv 구현 로드맵 / 우선순위

> **이 문서가 "남은 작업 / 구현 순서"의 SSOT다.** 설계(무엇을 만드는가)는
> [`ARCHITECTURE.md`](./ARCHITECTURE.md), 코딩 규칙은 [`CODING_RULES.md`](./CODING_RULES.md).
> 여기서는 **무엇을 어떤 순서로 채우는가**만 다룬다.
>
> 스캐폴드의 `todo!()`/빈 스트림을 채우면 **같은 PR 에서 이 문서의 해당 항목을 체크**한다
> (`AGENTS.md` § 단일 출처 규칙).

## 우선순위 원칙

1. **의존성** — 없으면 다음이 안 돌아가는 것을 먼저.
2. **수직 슬라이스(vertical slice)** — "데모 가능한 동작"에 가장 빨리 도달하는 순서.
   폭(여러 프로바이더/도구)보다 **하나를 end-to-end** 로 먼저 세운다.
3. **trait 경계로 분리** — 각 단계는 독립 테스트 가능하게 끊는다.

가장 큰 병목은 **Phase 0(`stream` 구현)** 이다. 그 전엔 어떤 것도 end-to-end 로
돌릴 수 없다.

## 현재 상태 (스캐폴드)

| 영역 | 상태 |
|------|------|
| 도메인 모델(`Message`/`ContentBlock`/`StreamEvent`/`StopReason`/`Usage`) | ✅ 완료 |
| trait 정의(`Provider`/`Tool`/`Skill`/`PermissionGate`/`ContextManager`/`SessionStore`) | ✅ 완료 |
| agentic loop 골격(`Agent::run_turn`) | ✅ text 경로 동작 / ⚠ tool_use 집계 미완 |
| `SystemPromptBuilder` 계층 합성 | ✅ 완료 |
| `read` 도구 | ✅ 완료 |
| 권한 게이트 | ✅ 루프 fail-closed(`Allow` 만 실행) / ⚠ 대화형 모달 미구현 |
| `SkillRegistry` 로더 | ✅ 완료 |
| `FileSessionStore` | ✅ 구현 존재 / ⚠ 루프 미연결 |
| 설정 로드(`scv-config`) | ✅ 단일 파일 / ⚠ 다단계 병합 미구현(4d) |
| `Provider::stream` (openai·anthropic) | ⛔ 빈 스트림 스텁 |
| `to_wire` (요청 변환) | ⛔ `messages:[]` 빈 스텁 |
| `Provider::count_tokens` | ⛔ `Ok(0)` 스텁 |
| `glob`/`grep`/`bash`/`write`/`edit` 도구 | ⛔ 미구현 |
| `scv-tui::App` 대화 루프 + 권한 모달 + 인터럽트 + 진행 표시 | ⛔ 스캐폴드 |
| 취소(`CancellationToken` 실제) + 루프 취소 체크포인트 | ⛔ placeholder(`is_cancelled()==false`)만 |
| `AgentEvent` + `Observer` 확장(도구/권한/취소 통지) | ⛔ `Observer` 가 `StreamEvent` 만 봄 |
| `ContextManager` 전략 | ⛔ `NoopContextManager` 만 |
| `ProjectContextLoader`(AGENTS.md 체인) | ⛔ `load()` → `None` 스텁 |
| `web_fetch`/`transcript-search` 도구 | ⛔ 미구현 |
| 세션 격리(per-session worktree) | ⛔ 미구현 |

---

## Phase 0 — "말한다" (한 턴이 실제로 흐른다) ★최우선

목표: `scv "이 repo 설명해줘"` 가 **실제 모델 응답을 stdout 으로 스트리밍**.

- [ ] **0a. 요청 wire 변환** — core `Message[]`/`tools[]` ↔ 와이어 JSON 양방향.
  - `crates/scv-providers/src/openai.rs` (신규 `to_wire`)
  - `crates/scv-providers/src/anthropic.rs:57` `to_wire` — 현재 `messages:[]` 빈 스텁
- [ ] **0b. `Provider::stream` SSE 파싱 → `StreamEvent`** — 프로바이더 **하나만**.
  - `crates/scv-providers/src/openai.rs:56`(기본 프로바이더부터)
  - SSE delta → `TextDelta`/`ToolUseStart`/`ToolUseInputDelta`/`MessageStop` 매핑.

> **첫 프로바이더는 OpenAI(`gpt-5.5`)** — 기본 프로바이더라 예시 설정 그대로
> out-of-box 로 동작한다. 두 번째(Anthropic)는 Phase 4 에서 **추상이 새지 않는지
> 실증하는 용도**로 채운다. (text 경로는 `MessageAssembler` 가 이미 처리하므로
> Phase 0 만으로 대화가 흐른다.)

- [ ] **0c. `ProjectContextLoader`** — repo `AGENTS.md` 탐색 체인(루트→하위→전역,
  `CLAUDE.md` 폴백)을 읽어 시스템 프롬프트 project-context 레이어에 주입.
  `crates/scv-cli/src/project_context.rs:24` 가 항상 `None` 이라, 채우기 전엔 첫 실호출이
  repo 맥락 없이 나간다. stream 에 의존하지 않고 cheap 하며 첫 대화 품질을 좌우하므로
  provider 실호출이 유효해지는 Phase 0 에 둔다. (§4.1)

## Phase 1 — "행동한다" (agentic loop 가 닫힌다)

목표: 모델이 `grep`→`read`→`edit` 로 실제 파일 작업을 수행.

- [ ] **1a. `MessageAssembler` tool_use 집계** — `ToolUseStart`/`ToolUseInputDelta`
  → `ContentBlock::ToolUse`. `crates/scv-core/src/agent.rs:181`.
  없으면 `stop_reason==ToolUse` 인데 tool_use 블록이 비어 루프가 도구를 못 부른다.
- [ ] **1b. 읽기 도구 `glob`/`grep`** — `Allow`+`parallel_safe`. 위험 없는 루프 폐쇄 검증용.
- [ ] **1c. 비가역 도구 `bash`/`write`/`edit`** — `Ask` 클래스. 권한 게이트가
  fail-closed(`Allow` 만 실행)라 **TUI 모달(2a) 전에는 거부**된다 — 안전하지만 실제로
  쓰려면 2a 가 필요하다(또는 설정에서 명시적 `Allow` 오버라이드). 코딩 에이전트의 핵심 가치.
- [ ] (선택) **병렬 도구 실행** — `parallel_safe` 도구를 `join_all` 로.
  `crates/scv-core/src/agent.rs:135`(현재 순차).

## Phase 2 — "쓸 수 있다" (인터랙티브 + 인터럽트 + 재개)

- [ ] **2a₀. (core 선행) 취소 + 통지 기반.** TUI 인터럽트·진행 표시가 의존하는 core 변경
  (원샷 모드도 함께 이득). 설계는 ARCHITECTURE §2(협조적 취소)·§4.5·§6(`AgentEvent`).
  - 실제 `tokio_util::sync::CancellationToken` 로 교체 — `crates/scv-core/src/tool.rs:147`
    placeholder(`is_cancelled()==false`) 제거.
  - `Agent::run_turn` 협조적 취소 체크포인트 3곳: 이터레이션 진입부(`agent.rs:87`),
    스트림 소비를 `tokio::select!` 로(`agent.rs:106`), 도구 실행 전후(`agent.rs:132`).
    중단 시 모은 부분 텍스트를 세션에 보존.
  - `Error::Cancelled` 추가 — `crates/scv-core/src/error.rs`(`#[non_exhaustive]`).
  - `AgentEvent` enum + `Observer::on_event(&AgentEvent)` 확장(`scv-core::message`/`agent`).
    루프가 도구/권한/취소 시점에 emit. `NullObserver`·`scv-tui::StreamObserver` 갱신.
  - 원샷 모드: `tokio::signal::ctrl_c()` 를 `run_turn` 과 `select!`(같은 토큰·`Error::Cancelled`).
- [ ] **2a. `scv-tui::App` 대화 루프 + 권한 모달 + 인터럽트 + 진행 표시.**
  - 3-소스 `select!` 루프(crossterm 입력 / `AgentEvent` mpsc / 렌더 틱) — ARCHITECTURE §4.5.
  - 권한 모달: fail-closed 라 `Ask` 도구는 모달이 동의를 받아 `Allow` 를 돌려줘야 실행
    (`bash`/`edit` 인터랙티브 사용의 필수 조건).
  - 진행 phase 상태머신 + 스피너(Braille / ascii 폴백 `[ui].spinner`, `NO_COLOR` 존중).
  - Ctrl-C: 턴 진행 중 = 현재 턴 중단(턴별 새 토큰) / idle = 더블 프레스로 종료.
    터미널 복원은 `Drop` 가드(패닉 포함).
  - `main.rs` 배선: 턴별 `CancellationToken` 주입 + 대화형 `PermissionGate` 주입
    (현재 `crates/scv-cli/src/main.rs:69` 는 `StaticPermissionGate(Ask)` 라 모달이 없으면 전부 거부).
- [ ] **2b. `FileSessionStore` 루프 연결 + `--resume`.**
  `crates/scv-cli/src/main.rs:113` TODO 해소.

## Phase 3 — "오래간다" (컨텍스트 수명)

- [ ] **3a. `Provider::count_tokens` 어댑터** — OpenAI tiktoken(o200k_base) /
  Anthropic `/v1/messages/count_tokens`. `openai.rs:63`, `anthropic.rs:105`.
  단 compaction **트리거 주 신호는 `MessageStop` 의 usage**, count_tokens 는 사전 점검 보조.
- [ ] **3b. `ContextManager` 두 전략** — `SummarizingContextManager`(요약) /
  `ClearToolResultsManager`(tool_result 비우기). 루프에 주입(현재 `NoopContextManager`).

## Phase 4 — "완성 / 견고" (폭 + 격리)

- [ ] **4a. 두 번째 프로바이더(Anthropic) `stream`/`to_wire`** — 멀티 프로바이더 추상 실증.
  `anthropic.rs:87`.
- [ ] **4b. `web_fetch` / `transcript-search` 도구** — `Tool` 구현 + 레지스트리 등록만.
- [ ] **4c. 세션 격리** — per-session git worktree(또는 임시 workdir) +
  세션 파일 append-only/락. 다중 세션 시 `SessionManager`. (§4.2 세션 격리)
- [ ] **4d. 설정 다단계 병합** — 현재 `Config::load` 는 `~/.config/scv/config.toml`
  한 곳만 읽는다(`crates/scv-config/src/lib.rs:95`). 계획된 순서(내장 기본값 → 사용자
  → 프로젝트 `./.scv/config.toml` → 환경변수 `SCV_*` → CLI)로 병합 구현(figment 등).

---

## 마일스톤 요약

| 단계 | 끝나면 보여줄 수 있는 것 |
|------|------------------------|
| Phase 0 | 모델과 한 턴 대화(스트리밍 출력) |
| Phase 1 | 자율적으로 도구 호출해 파일 읽고 고침 |
| Phase 2 | 인터랙티브 TUI + 권한 확인 + 인터럽트(Ctrl-C)·진행 표시 + 세션 재개 |
| Phase 3 | 긴 대화에서 컨텍스트 자동 관리 |
| Phase 4 | 프로바이더 2개 + 프로젝트 컨텍스트 + 격리 |
