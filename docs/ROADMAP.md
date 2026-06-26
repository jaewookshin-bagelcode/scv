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

가장 큰 병목이던 **Phase 0(`stream` 구현)** 은 **완료**됐다 — OpenAI 로 한 턴이
end-to-end 로 흐른다(텍스트·도구 경로 모두, fake Provider 종단 테스트로 검증). 다음
가치 단위는 실파일 작업을 여는 Phase 1b/1c(읽기·쓰기 도구)와 인터랙티브 Phase 2 다.

## 현재 상태 (스캐폴드)

| 영역 | 상태 |
|------|------|
| 도메인 모델(`Message`/`ContentBlock`/`StreamEvent`/`StopReason`/`Usage`) | ✅ 완료 |
| trait 정의(`Provider`/`Tool`/`Skill`/`PermissionGate`/`ContextManager`/`SessionStore`) | ✅ 완료 |
| agentic loop 골격(`Agent::run_turn`) | ✅ text·tool_use 경로 동작(집계 완료) |
| `SystemPromptBuilder` 계층 합성 | ✅ 완료 |
| `read` 도구 | ✅ 완료 |
| 권한 게이트 | ✅ 루프 fail-closed(`Allow` 만 실행) + **TUI 대화형 승인 모달 구현**(`Ask` 도구는 모달 승인 시 실행) + 설정 `[permissions]` 정적 정책 배선. ⚠ 원샷(비-TUI)엔 대화형 경로가 없어 `Ask` 도구는 여전히 거부(명시 `allow` 만 실행). 요구사항·동작: ARCHITECTURE §4.3·§4.5 |
| `SkillRegistry` 로더 | ✅ 완료 |
| `FileSessionStore` | ✅ 구현 + 원샷 루프 연결(`--resume`/저장) |
| 설정 로드(`scv-config`) | ✅ 단일 파일 / ⚠ 다단계 병합 미구현(4d) |
| `Provider::stream` (openai·anthropic) | ✅ openai SSE + HTTP 경로 통합테스트 / ⛔ anthropic(→4a) |
| `to_wire` (요청 변환) | ✅ openai 구현 / ⛔ anthropic 빈 스텁(→4a) |
| `Provider::count_tokens` | ✅ openai 로컬 tiktoken(o200k_base) 추정 / ⛔ anthropic(→4a) |
| `glob`/`grep`/`bash`/`write`/`edit` 도구 | ✅ 5개 모두 구현(+`read`) · write·edit·bash 는 `Ask` |
| `scv-tui::App` 대화 루프 + 권한 모달 + 인터럽트 + 진행 표시 | ✅ 구현(3-소스 select! · 모달 · Ctrl-C · 스피너 · Drop 복원 · 턴별 저장) |
| 취소(`CancellationToken` 실제) + 루프 취소 체크포인트 | ✅ 실제 토큰 + 3 체크포인트 + 부분보존 |
| `AgentEvent` + `Observer` 확장(도구/권한/취소 통지) | ✅ `AgentEvent` + `on_event(&AgentEvent)` |
| `ContextManager` 전략 | ✅ Noop/Clear/Summarizing 구현 + 임계 기반 루프 주입(scv-cli 기본 Summarizing) |
| `ProjectContextLoader`(AGENTS.md 체인) | ✅ 탐색 체인 구현 |
| `web_fetch`/`transcript-search` 도구 | ⛔ 미구현 |
| 세션 격리(per-session worktree) | ⛔ 미구현 |

---

## Phase 0 — "말한다" (한 턴이 실제로 흐른다) ★최우선

목표: `scv "이 repo 설명해줘"` 가 **실제 모델 응답을 stdout 으로 스트리밍**.

- [x] **0a. 요청 wire 변환** — core `Message[]`/`tools[]` → OpenAI 와이어 JSON.
  - `crates/scv-providers/src/openai.rs` `to_wire` ✅ — system→`messages[0]`, tool_use→
    `tool_calls`, tool_result→`role:"tool"`, 입력은 문자열 `arguments`, 추론은
    `reasoning_effort`, `max_completion_tokens`. 순수 함수 + 단위 테스트.
  - Anthropic `to_wire` 는 Phase **4a** 로 이관(아래).
- [x] **0b. `Provider::stream` SSE 파싱 → `StreamEvent`** — OpenAI 어댑터(기본 프로바이더 로컬 Ollama 가 재사용).
  - `crates/scv-providers/src/openai.rs` `ChunkDecoder`/`stream` ✅ — SSE delta →
    `MessageStart`/`TextDelta`/`ToolUseStart`/`ToolUseInputDelta`/`MessageStop`,
    `finish_reason`·`usage` 합성, tool_call `index`→`id` 추적. 순수 디코더 + 단위 테스트.
  - 실 HTTP/SSE 경로(reqwest 전송 + eventsource 파싱 + `drive_stream`)는 로컬 mock 서버
    통합 테스트(`crates/scv-providers/tests/openai_http.rs`)로 검증. 실제 모델 응답 형상은 수동 테스트.

> **첫 구현 프로바이더는 OpenAI(`gpt-5.5`) 어댑터** — Phase 0 의 `stream`/`to_wire` 를
> 여기에 먼저 세웠다. 기본 프로바이더인 **로컬 Ollama(`qwen3.5:9b`)는 이 OpenAI-호환 어댑터를
> 그대로 재사용**(`kind="ollama"`, `base_url` 자동 `localhost:11434/v1`)하므로 추가 어댑터
> 없이 out-of-box 로 동작한다. 두 번째(Anthropic)는 Phase 4 에서 **추상이 새지 않는지
> 실증하는 용도**로 채운다. (text 경로는 `MessageAssembler` 가 이미 처리하므로
> Phase 0 만으로 대화가 흐른다.)

- [x] **0c. `ProjectContextLoader`** — repo `AGENTS.md` 탐색 체인(전역→루트→하위,
  `CLAUDE.md` 폴백) 구현. `crates/scv-cli/src/project_context.rs` `load()` ✅ — `.git` 경계로
  repo 루트를 찾아 루트→cwd 로 내려가며 더 구체적인 문서를 뒤에 덧붙인다(가까운 것 우선).
  시스템 프롬프트 project-context 레이어에 주입(main.rs 배선). 단위 테스트 포함. (§4.1)

## Phase 1 — "행동한다" (agentic loop 가 닫힌다)

목표: 모델이 `grep`→`read`→`edit` 로 실제 파일 작업을 수행.

- [x] **1a. `MessageAssembler` tool_use 집계** — `ToolUseStart`/`ToolUseInputDelta`
  → `ContentBlock::ToolUse`. `crates/scv-core/src/agent.rs`. ✅ 블록 자동-닫기(open-block)
  방식으로 text·thinking·tool_use 를 순서 보존해 집계. 단위 테스트 + 루프 종단 테스트
  (`crates/scv-core/tests/agent_loop.rs`, fake Provider)로 검증.
- [x] **1b. 읽기 도구 `glob`/`grep`** — `Allow`+`parallel_safe` 구현. `glob`(`.gitignore`
  존중 walk + `globset`), `grep`(정규식 + 선택적 파일 glob 필터). 경로는 workdir 제한,
  walk 는 `spawn_blocking`. 단위 테스트 포함. `crates/scv-tools/src/{glob,grep}.rs`.
- [x] **1c. 비가역 도구 `bash`/`write`/`edit`** — `Ask` 클래스 구현. `write`(새/덮어쓰기,
  부모 디렉터리 제한 + 심볼릭 링크 탈출 차단), `edit`(유일 일치 치환 / `replace_all`),
  `bash`(`sh -c`, 타임아웃, `kill_on_drop`, 출력 길이 제한). 단위 테스트 포함. 권한 게이트가
  fail-closed(`Allow` 만 실행)라 **TUI 모달(2a) 전에는 거부**된다 — 안전하지만 실제로 쓰려면
  2a 가 필요하다(또는 설정에서 명시적 `Allow` 오버라이드). 경로 보안 헬퍼는
  `crates/scv-tools/src/path.rs` 가 공유(`read` 도 사용하도록 리팩터).
- [x] (선택) **병렬 도구 실행** — `parallel_safe` 도구(read/glob/grep)를 `join_all` 로 동시
  실행. 권한은 순차 해소(대화형 Ask 프롬프트·거부 abort 보존), 비-parallel(write/edit/bash)은
  순차, 결과는 원래 tool_use 순서로 모은다. `crates/scv-core/src/agent.rs` `execute_tool_calls`.
  단위 테스트(barrier 로 동시성 증명 + 순서 보존 + 거부 abort).

## Phase 2 — "쓸 수 있다" (인터랙티브 + 인터럽트 + 재개)

- [x] **2a₀. (core 선행) 취소 + 통지 기반.** 완료 — ARCHITECTURE §2·§4.5·§6 대로.
  - ✅ 실제 `tokio_util::sync::CancellationToken` 로 교체. `scv-core::tool` 이 재노출하므로
    다른 크레이트는 `tokio-util` 에 직접 의존하지 않는다. placeholder 모듈 제거.
  - ✅ `Agent::run_turn` 협조적 취소 체크포인트 3곳: 이터레이션 진입부 · 스트림 소비를
    `tokio::select!`(biased) · 도구 실행 직전 및 도구 사이. 중단 시 모은 부분 텍스트를 세션 보존.
  - ✅ `Error::Cancelled` 추가(`#[non_exhaustive]`).
  - ✅ `AgentEvent` enum(`scv-core::message`) + `Observer::on_event(&AgentEvent)` 로 확장.
    루프가 `Stream`/`ToolStart`/`ToolEnd`/`PermissionAsked`/`Interrupted` emit.
    `NullObserver`·`scv-tui::StreamObserver` 갱신(StreamObserver 는 토큰마다 stdout flush).
  - ✅ 원샷 모드: `tokio::signal::ctrl_c()` 를 별도 태스크에서 토큰 cancel(같은 토큰·`Cancelled`).
    `run_turn` 을 select! 로 드롭하지 않아 협조적 정리(부분 보존)가 실제로 실행된다.
  - ✅ `bash` 도구도 `ctx.cancel` 을 `select!` 로 관찰해 긴 명령을 즉시 중단(kill_on_drop).
  - ✅ 회귀 테스트(e2e 티어): 미리-cancel → `Cancelled`(스트림 전), 스트림 도중 취소 →
    부분 텍스트 보존 + `Cancelled`.
- [x] **2a. `scv-tui::App` 대화 루프 + 권한 모달 + 인터럽트 + 진행 표시.** 구현 완료.
  - ✅ 3-소스 `select!` 루프(crossterm `EventStream` 입력 / `AgentEvent` mpsc / 렌더 틱 80ms)
    를 한 태스크에서 `run_turn` 미래와 함께 폴링(`crates/scv-tui/src/app.rs`) — ARCHITECTURE §4.5.
    spawn 없이 `&mut session`/`&agent` 빌림을 유지(Send/'static 불필요).
  - ✅ 권한 모달: **`Ask` 도구는 사용자의 명시적 승인을 받아야만 실행된다**(승인 전제,
    ARCHITECTURE §4.3). `InteractivePermissionGate` 가 정적 정책(설정 `[permissions]`)과
    합성 — 설정이 `allow`/`deny` 로 확정하면 안 묻고, `Ask` 면 모달로 y/n 을 받아 `Allow`/`Deny`
    를 돌려준다. UI 채널이 끊기거나 응답이 드롭되면 `Ask`(fail-closed). 모달 단위 테스트 포함.
  - ✅ 진행 phase 상태머신(`phase.rs`, 순수+단위테스트) + 스피너(Braille / ascii 폴백
    `[ui].spinner`, `NO_COLOR` 존중). `scv-config` 에 `UiConfig` 추가.
  - ✅ Ctrl-C: 턴 진행 중 = 현재 턴 중단(턴별 새 `CancellationToken`) / idle = 더블 프레스로
    종료. 터미널 복원은 `RawModeGuard` 의 `Drop`(패닉 포함).
  - ✅ `main.rs` 배선: 턴별 토큰 주입 + 설정 기반 `StaticPermissionGate`(`build_permission_gate`)
    를 App 이 대화형 게이트로 감싼다. 턴마다 세션 저장.
- [x] **2b. `FileSessionStore` 루프 연결 + `--resume`.** 원샷 모드에서 턴 종료 후 세션을
  `<dir>/<id>.jsonl` 로 저장하고 세션 id 를 출력한다. `--resume <id>` 로 로드해 이어간다.
  설정 `[session].dir` 의 선행 `~/` 확장. save→load round-trip 단위 테스트 포함.
  (TUI 의 세션 루프/저장은 2a 에서.)

## Phase 3 — "오래간다" (컨텍스트 수명)

- [x] **3a. `Provider::count_tokens` 어댑터** — OpenAI 는 로컬 tiktoken(`o200k_base`)으로 추정
  구현(`openai.rs` `count_tokens`/`render_for_count`, 단위 테스트). count 엔드포인트가 없는
  OpenAI 경로를 다룬다. **Anthropic `/v1/messages/count_tokens` 는 4a(어댑터 와이어)와 함께
  구현**(현재 `anthropic.rs` 스텁). compaction **트리거 주 신호는 `MessageStop` 의 usage**,
  count_tokens 는 사전 점검 보조.
- [x] **3b. `ContextManager` 두 전략 + 루프 주입** — `ClearToolResultsManager`(tool_result
  비우기) ✅ 구현. `SummarizingContextManager`(LLM 요약) ✅ 구현 — 임계 초과 시 오래된 앞부분을
  주입된 Provider 로 요약(compaction)하고 최근 `keep_recent` 개는 verbatim 유지. **루프 주입**:
  `ContextManager::prepare(messages, last_input_tokens)` 가 직전 응답 usage 의 입력 토큰을 받아
  트리거 신호로 쓰고, `Agent::run_turn` 이 `MessageStop` usage 를 추적해 넘긴다. `scv-cli` 가
  기본으로 `SummarizingContextManager`(임계=`compact_threshold_tokens`, keep_recent=8)를 주입.
  단위 테스트(임계 이하 무동작 / 앞부분 접기 / 접을 것 없을 때 무동작).

## Phase 4 — "완성 / 견고" (폭 + 격리)

- [ ] **4a. 두 번째 프로바이더(Anthropic) `stream`/`to_wire`** — 멀티 프로바이더 추상 실증.
  `anthropic.rs:87`.
- [ ] **4b. `web_fetch` / `transcript-search` 도구** — `Tool` 구현 + 레지스트리 등록만.
- [ ] **4c. 세션 격리** — per-session git worktree(또는 임시 workdir) +
  세션 파일 append-only/락. 다중 세션 시 `SessionManager`. (§4.2 세션 격리)
- [ ] **4d. 설정 다단계 병합** — `Config::load` 는 `~/.config/scv/config.toml` 한 곳만
  읽는다(`SCV_CONFIG` override·cwd 독립 경로는 정리됨). 계획된 순서(내장 기본값 → 사용자
  → 프로젝트 `./.scv/config.toml` → 환경변수 `SCV_*` → CLI)로 병합 구현(figment 등).
- [x] **4e. 인증 타입 일반화** — `api_key | none` 구현 완료. `ProviderConfig.api_key_env` 가
  **선택**(`Option`)이 되어, 생략하면 무인증으로 동작한다(`main.rs` 는 빈 키, OpenAI 어댑터는
  Authorization 헤더 생략). 기본 프로바이더(로컬 Ollama)는 `api_key_env` 가 없어 **키·환경변수
  0 으로 out-of-box**. `base_url` 일반화는 기존 구현. **결정(확정):** scv 는 LLM 을 *모델*로만 쓰고 도구·루프는
  scv 가 소유한다("api 와 가장 유사한 동작"). 도구는 오직 scv `ToolRegistry`. **기존 OpenAI
  API 키 어댑터(`/chat/completions` + Bearer)는 삭제하지 않고 기본 경로로 유지**하고, 인증
  *수단*만 일반화한다(`api_key | none` + `base_url`). 모델 토큰을 받는 길: (1) OpenAI 플랫폼
  **API 키**(가장 직접·공식, 기본) (2) OpenAI-호환 **게이트웨이**(`base_url`, 코드 0 — A 경로,
  먼저 테스트). 구독 OAuth 로 *모델 토큰만* 받는 공식 경로는 없어(OpenAI 는 구독 OAuth 를
  Codex 하네스로만 연다) 구독 재활용은 비공식 백엔드 직접뿐(ToS·안정성 리스크)이라 비권장.
- [ ] **4f. (TODO / 향후) Codex 런타임 — 구독/워크스페이스 권한 경로.** ChatGPT/Codex 구독·
  워크스페이스 권한을 쓰려면 `Provider`(모델 토큰)가 아니라 **별도 `CodexRuntime` 계층**으로
  Codex 를 감싼다. 인증: `chatgpt.com/admin/access-tokens` 의 **Codex access token**(= OpenAI
  API 키 아님 — `/v1/chat/completions` 불가; `codex login --with-access-token` / `CODEX_ACCESS_TOKEN`
  전용) 또는 `codex login` ambient. 통합 수단: `codex exec --json`(서브프로세스, 단순) 또는
  `codex app-server`(JSON-RPC: initialize → thread/start{dynamicTools} → turn/start →
  item/agentMessage/delta·turn/completed). **트레이드오프(확정): 루프·도구·승인을 Codex 가 소유**
  → scv 의 `run_turn`/`ToolRegistry`/`PermissionGate` 미사용(="하네스 직접 구축/도구는 scv 만"
  원칙과 충돌). 도구는 `--sandbox read-only --ask-for-approval never -c web_search=disabled` 로
  *줄일* 뿐 0 으로 끄는 스위치는 없음. 비공식·무거움 → **구독이 자체 하네스보다 우선일 때만** 착수.

---

## 마일스톤 요약

| 단계 | 끝나면 보여줄 수 있는 것 |
|------|------------------------|
| Phase 0 ✅ | 모델과 한 턴 대화(스트리밍 출력) — OpenAI end-to-end |
| Phase 1 (도구 구현 ✅) | 자율 도구 호출 — read/glob/grep 즉시, write/edit/bash 는 2a 모달·명시 Allow 후 |
| Phase 2 | 인터랙티브 TUI + 권한 확인 + 인터럽트(Ctrl-C)·진행 표시 + 세션 재개 |
| Phase 3 | 긴 대화에서 컨텍스트 자동 관리 |
| Phase 4 | 프로바이더 2개 + 프로젝트 컨텍스트 + 격리 |
