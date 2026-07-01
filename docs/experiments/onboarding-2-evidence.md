# scv 온보딩 2단계 — 실측 근거 (실험 조건 + 결과)

온보딩 2단계 보고서가 인용하는 **라이브 호출 근거**다. 코드만으로는 대조할 수 없는 값
(캐시 토큰·200/400 응답·서버 컨텍스트 관리 동작 등)을 실제 호출로 확인한 원자료와 요약을 담는다.

- **환경**: aiproxy(사내 게이트웨이) 경유 Anthropic, 모델 `claude-sonnet-4-6`. 엔드포인트 `${AIPROXY_BASE}/v1/messages`.
- **인증**: `Authorization: Bearer $CODEB_TOKEN` — **헤더로만 전송하며 어떤 파일에도 토큰 값은 기록하지 않는다.**
  사내 게이트웨이 호스트는 이 저장소에선 `${AIPROXY_BASE}` / `<AIPROXY_HOST>` 플레이스홀더로 치환했다.
- **원자료**: [`raw/`](./raw) — 요청 본문(`*_body.json`) = 조건, 응답 본문(`*_resp.json`·`*_on/off.json` 등) = 결과.
  재현 스크립트는 `raw/*.sh`(base URL·토큰을 환경변수로 채워 실행).
- 코드 게이트(fmt·clippy·test·coverage)는 저장소 `scripts/coverage.sh`로 재현한다(라이브 호출과 무관).

> 아래 200 응답 중 `applied_edits:[]`처럼 "적용 0"으로 보이는 것은 요청이 작아 실제 축소가
> 아직 발동하지 않은 것으로, **기능/조합의 *수용*(200 vs 400)을 확인한 결과**다. 토큰 수·비용 등
> 수치는 그날의 라이브 응답값이라 재실행 시 소폭 달라질 수 있다.

---

## 과제 1 — 프로바이더 제한 (scv end-to-end)

- **조건**: 설정 파일 없이(`SCV_CONFIG=/nonexistent.toml`) `CODEB_TOKEN`만으로 scv 원샷 실행 — `scv "Answer with exactly one word: scv"` (중립 작업 디렉터리)
- **결과**: `stop=EndTurn, in=331, out=5, cache_read=0, cache_write=1254` — `raw/task1_scv_e2e.txt`
- scv **바이너리 종단 실행**(raw 호출 아님) — aiproxy 경유 Anthropic로 end-to-end 동작 + tools+system prefix 캐시 write(1,254토큰) 확인. (`cache_write` 값은 그 시점 prefix 크기라 코드 변화에 따라 소폭 달라진다.)

---

## 과제 2 — Prompt caching

| 실험 | 조건 | 결과 | 원자료 |
|---|---|---|---|
| write→read 적중 | 동일 prefix(~3,403토큰) 2회 연속 전송 | 1회차 `cache_creation_input_tokens=3403, cache_read=0` / 2회차 `creation=0, read=3403` | `raw/cache_body.json`, `raw/cache_resp_1.json`, `raw/cache_resp_2.json` |
| 1토큰 변경 무효화 | prefix에 1토큰 추가 후 전송 | `creation=3404, read=0`(캐시 미스) | `raw/cache_break_1tok.json` |
| TTL(5분) 만료 | write 후 330s(>300s) 무접근 재전송 | 재전송에서 `read=0`·`write` 재발생 → 만료 확정 | `raw/ttl_1_write.json`, `raw/ttl_2_afterexpiry.json`, `raw/ttl_run.log` |
| 1시간 티어 | `cache_control.ttl="1h"`, beta 유/무 | 둘 다 `ephemeral_1h_input_tokens=3412` (beta 없이도 동작) | `raw/1h_A_beta.json`, `raw/1h_B_nobeta.json` |

비용표($) 수치는 위 토큰값에 Anthropic 공식 단가(Sonnet input $3 / write $3.75 / read $0.30 per 1M)를 적용한 산출이다.

---

## 과제 3 — web_search (서버사이드)

- **조건**: `tools:[{type:"web_search_20250305", name:"web_search"}]`, 질의 "latest stable Rust compiler version" — `raw/websearch_body.json`
- **결과**: `usage.server_tool_use.web_search_requests=1`, 결과 10건(first=`releases.rs`), 구조적 citation 3건(`web_search_result_location`), `stop_reason=end_turn` — `raw/websearch_resp.json`
- (참고) 최신 `web_search_20260318`은 code_execution 기반 동적 필터링이 개입 — `raw/websearch_body_20260318.json`, `raw/websearch_resp_20260318.json`

---

## 과제 4 — web_fetch (서버 위임 가능성)

- **조건**: `tools:[{type:"web_fetch_20250910", name:"web_fetch"}]`, "fetch https://www.rust-lang.org/" — `raw/webfetch_body.json`
- **결과**: `web_fetch_requests=1`, `web_fetch_tool_result`(url=rust-lang.org), `stop_reason=end_turn`(클라이언트 왕복 0) — `raw/webfetch_resp.json`
- (참고) 최신 `web_fetch_20260318`은 code_execution 동적 필터링 개입 — `raw/webfetch_body_20260318.json`, `raw/webfetch_resp_20260318.json`
- scv 자체는 **로컬 web_fetch만** 쓰며, 위는 서버 위임이 프록시 너머로 가능한지를 raw 호출로 확인한 것이다.

---

## 과제 5 — context management / compaction (서버)

| 실험 | 조건 | 결과 | 원자료 |
|---|---|---|---|
| context editing(clear) 수용 | `context_management.edits:[{type:"clear_tool_uses_20250919"}]` | beta `context-management-2025-06-27` ON→**200** `applied_edits:[]`, OFF→**400** | `raw/ctxmgmt_beta_on.json`, `raw/ctxmgmt_beta_off.json` |
| server compaction(요약) 수용 | `edits:[{type:"compact_20260112", trigger:...}]` | beta `compact-2026-01-12` ON→**200**, OFF→400, 잘못된 beta→400 | `raw/compact_on.json`, `raw/compact_off.json`, `raw/compact_wrongbeta.json` |
| clear+compact 조합 수용 | `edits:[clear_tool_uses_20250919, compact_20260112]` | `compact-2026-01-12` beta로 **200**(둘 다 수용), `context-management-2025-06-27` beta만→**400** | `raw/compact_clear_combo_compactbeta.json`, `raw/compact_clear_combo_cmbeta_400.json` |

→ 서버는 **비우기(clear)와 요약(compaction)을 한 `edits` 배열에 함께** 받는다(계층 구성 가능). scv는 이 계층을
로컬 `LayeredContextManager`(clear 1차 → 넘치면 요약 2차)로 구현했고, 서버 위임은 가능성만 확인했다(미탑재).

---

## 재현

```sh
export AIPROXY_BASE="https://<사내 aiproxy 게이트웨이 호스트>/anthropic"   # 플레이스홀더 → 실제 값
export CODEB_TOKEN="<발급받은 프록시 토큰>"                                # 헤더로만 전송, 파일에 미기록
bash raw/replay.sh        # 과제2 캐시 write→read + 과제5 context editing 200/400
bash raw/ttl_test.sh      # 과제2 TTL(5분) 만료
bash raw/cache_break.sh   # 과제2 1토큰 변경 무효화
```
