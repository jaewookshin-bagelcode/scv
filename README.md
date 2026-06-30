# scv

터미널에서 동작하는 멀티 프로바이더 코딩 에이전트(Claude Code / Codex 류). Rust + Tokio.
LLM 은 **모델(토큰)** 로만 쓰고 루프·도구·승인·세션은 scv 가 소유한다 — raw API 에 가장 가까운 투명한 동작.

- 🔌 **프로바이더** — 기본 aiproxy 경유 Anthropic(`claude-sonnet-4-6`, `CODEB_TOKEN` 만 있으면 동작) · 로컬 Ollama(`qwen3.5:9b`, 키·네트워크 불필요)는 `--provider ollama`
- 🛠 **내장 도구 8종** — `read`/`glob`/`grep`/`write`/`edit`/`bash`/`web_fetch`/`transcript_search`
- 🔒 **권한 게이트(fail-closed)** — 부작용 도구는 실행 전 승인(human-in-the-loop)
- 🧩 **스킬** — `SKILL.md` 파일을 `/<name>` 으로 발동(progressive disclosure)
- 💾 **세션·컨텍스트** — JSONL 영속·재개 + 임계 초과 시 자동 요약(compaction)
- 🖥 **인터랙티브 TUI** — 대화 루프 + 승인 모달 + 진행 표시 + 인터럽트

## 빠른 시작

```bash
# 1) 설치 — release 빌드 + PATH 에 심볼릭 링크(코드 수정 후 cargo build --release 만 다시)
sh scripts/scv-link.sh install

# 2) 토큰 주입(기본 프로바이더 aiproxy — 사내 게이트웨이 경유 Anthropic)
codeb login --token aiproxy_xxx && export CODEB_TOKEN="aiproxy_xxx"
#    또는 로컬 모델: ollama serve && ollama pull qwen3.5:9b  (이후 --provider ollama)

# 3) 실행 — 작업할 프로젝트 디렉터리에서
cd ~/work/my-project
scv                          # 인터랙티브 TUI
scv "이 저장소 구조를 설명해줘"   # 원샷(한 번 묻고 끝)
```

> ⚠ scv 는 **자기 소스 레포 안에서는 실행을 거부**한다(자기 코드를 작업 대상으로 삼는 사고 방지).
> 다른 프로젝트 디렉터리에서 쓰자. 개발 중 강제로 돌리려면 `SCV_ALLOW_IN_REPO=1`.

클라우드 프로바이더 전환, 설정 병합, 세션 격리, 스킬 작성 등 상세는 **[`docs/SETUP.md`](./docs/SETUP.md)** 참고.

## 문서

| 문서 | 내용 |
|------|------|
| [`docs/SETUP.md`](./docs/SETUP.md) | 설치·설정·프로바이더 전환·수동 테스트 |
| [`docs/DEVELOPMENT.md`](./docs/DEVELOPMENT.md) | 소스 빌드/실행, lint 게이트, 크레이트 구조, 기여 규약 |
| [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) | 설계 — agentic loop, 4대 기능, 멀티 프로바이더, TUI |
| [`docs/ROADMAP.md`](./docs/ROADMAP.md) | 구현 우선순위/진행 상태 |
| [`docs/CODING_RULES.md`](./docs/CODING_RULES.md) | Rust 컨벤션, 에러/async/보안, LLM 연동 |
| [`AGENTS.md`](./AGENTS.md) | 기여자 작업 규약 — 단일 출처 규칙 |
