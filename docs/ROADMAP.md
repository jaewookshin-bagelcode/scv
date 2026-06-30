# scv 구현 로드맵 / 우선순위

> **이 문서가 "남은 작업 / 구현 순서"의 SSOT다.** 설계(무엇을 만드는가)는
> [`ARCHITECTURE.md`](./ARCHITECTURE.md), 코딩 규칙은 [`CODING_RULES.md`](./CODING_RULES.md).
> 완료 항목은 같은 PR 에서 여기 체크한다(`AGENTS.md` § 단일 출처 규칙).

## 우선순위 원칙

1. **의존성** — 없으면 다음이 안 돌아가는 것을 먼저.
2. **수직 슬라이스** — 폭(여러 프로바이더/도구)보다 하나를 **end-to-end** 로 먼저(데모 가능한 동작에 빠르게 도달).
3. **trait 경계로 분리** — 각 단계는 독립 테스트 가능하게 끊는다.

**Phase 0–4 구현 완료** — OpenAI·Anthropic·로컬 Ollama 로 한 턴이 end-to-end 로 흐르고(텍스트·
도구 경로 모두, fake Provider 종단 테스트로 검증), 도구·권한·인터랙티브 TUI·세션 재개·컨텍스트
압축·세션 격리까지 동작한다. 남은 것은 선택 작업 `4f`(Codex 런타임)와, **Phase 5 — 서버사이드 기능 &
로컬/서버 트레이드오프**(아래)다.

## 현재 상태 (Phase 0–4 완료)

| 영역 | 상태 |
|------|------|
| 도메인 모델 · trait(`Provider`/`Tool`/`Skill`/`PermissionGate`/`ContextManager`/`SessionStore`) | ✅ |
| agentic loop(`Agent::run_turn` — text·tool_use 집계 + 협조적 취소) | ✅ |
| `SystemPromptBuilder` 계층 합성 · `ProjectContextLoader`(AGENTS.md 체인) | ✅ |
| 도구 8종(read/glob/grep/write/edit/bash/web_fetch/transcript_search) | ✅ |
| 권한 게이트(루프 fail-closed + TUI 승인 모달 + 설정 `[permissions]`) | ✅ · ⚠ 원샷엔 대화형 경로 없어 `Ask` 거부 |
| 프로바이더 `stream`/`to_wire`/`count_tokens`(openai · anthropic · ollama 가 openai 어댑터 재사용) | ✅ |
| `scv-tui::App`(대화 루프 · 모달 · Ctrl-C 인터럽트 · 스피너 · 턴별 저장) | ✅ |
| `FileSessionStore`(`--resume`/저장) · `ContextManager`(Noop/Clear/Summarizing) | ✅ |
| 설정 다단계 병합(`scv-config`, figment) · 세션 격리(per-session worktree) | ✅ · ⚠ 세션파일 동시쓰기 락 잔여 |

---

## Phase 0–4 (완료 항목)

상세 구현(파일·함수)은 코드와 [`ARCHITECTURE.md`](./ARCHITECTURE.md) 가 SSOT다 — 여기서는 순서와 완료만 기록한다.

**Phase 0 — "말한다"** (한 턴이 stdout 으로 스트리밍)
- [x] **0a.** 요청 wire 변환 — `Message[]`/`tools[]` → OpenAI 와이어(`openai.rs` `to_wire`, 순수+단위테스트). Anthropic 은 4a.
- [x] **0b.** `Provider::stream` SSE → `StreamEvent` — OpenAI `ChunkDecoder`(로컬 Ollama 가 재사용). 실 HTTP 는 mock 서버 통합테스트.
- [x] **0c.** `ProjectContextLoader` — `AGENTS.md` 탐색 체인(전역→루트→cwd, `CLAUDE.md` 폴백), system 프롬프트 주입.

**Phase 1 — "행동한다"** (agentic loop 가 닫힌다)
- [x] **1a.** `MessageAssembler` tool_use 집계(text·thinking·tool_use 순서 보존) — `agent.rs`, 루프 종단 테스트.
- [x] **1b.** 읽기 도구 `glob`/`grep`(`Allow`+parallel_safe, workdir 제한).
- [x] **1c.** 비가역 도구 `bash`/`write`/`edit`(`Ask`, 경로 보안 헬퍼 `path.rs` 공유).
- [x] (선택) 병렬 도구 실행 — parallel_safe 도구 `join_all`, 권한 순차 해소, 결과 순서 보존.

**Phase 2 — "쓸 수 있다"** (인터랙티브 + 인터럽트 + 재개)
- [x] **2a₀.** (core) 실제 `CancellationToken` + `run_turn` 취소 체크포인트 3곳(부분 보존) + `AgentEvent`/`Observer` 통지.
- [x] **2a.** `scv-tui::App` — 3-소스 `select!` 루프 + 권한 모달(fail-closed) + 진행 phase/스피너 + Ctrl-C.
- [x] **2b.** `FileSessionStore` 연결 + `--resume`(`<id>.jsonl` 저장/로드, round-trip 테스트).

**Phase 3 — "오래간다"** (컨텍스트 수명)
- [x] **3a.** `count_tokens` — OpenAI 로컬 tiktoken, Anthropic 서버 엔드포인트(4a). compaction 주 신호는 `MessageStop` usage.
- [x] **3b.** `ContextManager` — `Clear`/`Summarizing` 구현(임계 초과 시 앞부분 요약·최근 verbatim). scv-cli 기본 Summarizing.

**Phase 4 — "완성 / 견고"** (폭 + 격리)
- [x] **4a.** Anthropic `stream`/`to_wire`/`count_tokens` — 코어 변경 0 으로 붙어 멀티 프로바이더 추상 실증.
- [x] **4b.** `web_fetch`(Ask egress)/`transcript_search`(Allow 세션검색) — `Tool` trait 만으로 추가, 코어 변경 0.
- [x] **4c.** 세션 격리 — `--isolate` per-session git worktree(`~/.scv/worktrees/<id>`, Drop 정리), 비-git 폴백.
- [x] **4d.** 설정 다단계 병합 — figment 레이어(기본값→사용자→프로젝트→`SCV_*`).
- [x] **4e.** 인증 일반화 — `api_key_env` 선택(`Option`)이라 생략 시 무인증(로컬 Ollama out-of-box). OpenAI 키 + `base_url` 게이트웨이 경로 유지.

## Phase 5 — 서버사이드 기능 & 로컬/서버 트레이드오프 (계획)

프로바이더를 좁히고, 그 위에서 **서버사이드 vs 로컬 실행**의 트레이드오프를 기능별로 적용한다.
초점은 폭이 아니라 "각 기능을 서버에 맡길지 로컬에 둘지"의 근거다.

- [ ] **5a. 프로바이더 좁히기** — 로컬 Ollama(무인증 개발/CI) + 클라우드는 aiproxy 경유 Anthropic(Sonnet/Haiku) 하나로 고정. anthropic 어댑터에 Bearer 인증 모드(`auth_style`), `base_url`에 프록시 경로. 이후 단계의 전제(와이어를 Anthropic 하나로 고정해 변수 통제).
- [x] **5b. Prompt caching 실 활성화 + 비용 실측** — `to_wire` 가 `system` 블록에 `cache_control:{type:ephemeral}` 적재(렌더 순서 tools→system→messages 이므로 tools+system 안정 prefix 를 함께 캐시), 디코더가 `cache_creation/read_input_tokens` → `Usage`, observer 가 in/out/cache 토큰 표시. **실측(aiproxy Sonnet, 동일 prefix 2회)**: 1회차 cache_write 4707·read 0 → 2회차 read 4707·write 0(~0.1x). (**서버사이드 기능** — scv 는 마커·측정만.)
- [x] **5c. 서버사이드 도구용 루프 일반화** — `StopReason::PauseTurn` 추가(+anthropic `map_stop_reason`). `run_turn` 이 stop_reason 을 ToolUse(로컬 실행)/PauseTurn(로컬·user 추가 없이 히스토리 재전송 재개, iteration 상한이 무한 pause 방지)/그 외(종료) 3갈래로 분기. 서버 tool_use 블록 보존(ContentBlock 확장)은 5d 에서 와이어와 함께. 5d·5e 의 전제.
- [x] **5d. web_search 서버사이드** — `ProviderConfig.web_search`(anthropic 전용) → `to_wire` 가 native `web_search_20250305` 서버툴을 tools 에 주입. 모델이 **서버에서** 검색 실행, 결과·인용을 같은 응답에 실어 보냄(로컬 도구의 tool_use→tool_result 왕복 없음). **라이브 검증**(aiproxy Sonnet, `--no-tools`로 격리): 실시간 BTC 시세를 다중 출처로 회신. *follow*: citations 구조적 표시·다중검색 `pause_turn` 시 서버블록 보존(ContentBlock 확장)은 미구현.
- [ ] **5e. web_fetch 서버 vs 로컬 — 비교·측정 후 판단** — 서버 위임 vs 로컬 실행을 여러 축(권한 게이트·사내망 접근·감사/투명성·라운드트립·비용)으로 **실제 비교**하고, 어느 쪽을 택하는지 측정·근거와 함께 도출·문서화. *(결론은 미리 박지 않는다 — 측정 후 결정.)*
- [ ] **5f. compaction 서버 vs 로컬 — 비교·측정 후 판단** — 로컬 전략(`Clear`/`Summarizing`) vs Anthropic 서버사이드 context editing/memory 를 토큰·손실·캐시 prefix 상호작용·프로바이더 종속성 축으로 **비교 측정**하고 근거와 함께 판단. 서버 context editing ↔ 5b 캐시 prefix 상호작용 주의. *(결론은 측정 후 도출.)*

순서: 5a → 5b → 5c → 5d → 5e, 5f 병행. 5b 는 5a 직후 착수 가능, 5d·5e 는 5c 선행.

## 남은 작업 / 잔여

- [ ] **4f. (보류) Codex 런타임 — 구독/워크스페이스 권한 경로.** ChatGPT/Codex 구독을 쓰려면
  `Provider`(모델 토큰)가 아니라 별도 `CodexRuntime` 계층으로 Codex 를 감싼다(`codex exec --json`
  또는 `codex app-server` JSON-RPC). **트레이드오프(확정)**: 루프·도구·승인을 Codex 가 소유 →
  scv 의 `run_turn`/`ToolRegistry`/`PermissionGate` 미사용("도구는 scv 만" 원칙과 충돌). 실험적·
  무거움 → **구독이 자체 하네스보다 우선일 때만** 착수.
- ⚠ **세션 파일 동시쓰기 락** — 같은 세션 id 를 두 프로세스가 동시에 `--resume` 할 때의 append-only/
  락은 미구현(저장소 통째 쓰기). 동시 *작업파일* 충돌은 worktree(4c)로 해소됨.
- ⚠ **원샷 `Ask` 도구** — 비-TUI 엔 대화형 승인 경로가 없어 `Ask` 도구는 거부된다(명시 `allow` 만 실행).

## 마일스톤 요약

| 단계 | 보여줄 수 있는 것 |
|------|------------------|
| Phase 0 ✅ | 모델과 한 턴 대화(스트리밍 출력) — OpenAI end-to-end |
| Phase 1 ✅ | 자율 도구 호출 — read/glob/grep 즉시, write/edit/bash 는 모달·명시 Allow 후 |
| Phase 2 ✅ | 인터랙티브 TUI + 권한 확인 + Ctrl-C 인터럽트·진행 표시 + 세션 재개 |
| Phase 3 ✅ | 긴 대화에서 컨텍스트 자동 관리 |
| Phase 4 ✅ | 프로바이더 2개 + 프로젝트 컨텍스트 + 격리 |
| Phase 5 🔜 | aiproxy-Anthropic 고정 + 프롬프트 캐싱 실측 + 서버사이드 web_search + 서버/로컬 트레이드오프 |
