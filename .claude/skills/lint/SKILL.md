---
name: lint
description: scv 저장소의 코드와 문서를 검증한다 — cargo fmt --check · clippy -D warnings · test 게이트와, 변경이 동작/기본값/규칙을 바꿨을 때 SSOT 문서(ARCHITECTURE/SETUP/CODING_RULES/config)가 함께 갱신됐는지, 그리고 보안 위생을 점검한다. 작업을 끝냈다고 말하기 전, PR 직전, 또는 사용자가 "lint"/"검증"/"확인해줘"/"끝났어?"/"머지해도 돼?" 라고 하거나 코드·문서를 변경한 직후에는 반드시 이 스킬로 통과 여부를 확인하고 정직하게 보고한다.
allowed-tools: Bash(cargo *) Bash(git *) Read Grep Glob
---

# lint

scv 의 변경을 "끝났다"고 부르기 전에 통과해야 할 검증. **여기서 정의하는 '끝남' 은
fmt·clippy·test 가 통과하고, 바뀐 동작이 문서(SSOT)에 반영됐고, 비밀이 새지 않은 상태**다.

검증은 **실제로 실행**한다(짐작 금지). 실패하면 실패한 명령과 출력을 그대로 보고한다 —
통과한 척하면 다음 사람이 깨진 채로 받게 되므로, 정직한 실패 보고가 거짓 통과보다 낫다.

## 1. 코드 게이트 (반드시 통과)

```sh
cargo fmt --all -- --check                                 # 포맷이 적용돼 있는가
cargo clippy --all-targets --all-features -- -D warnings   # 경고를 에러로 취급
cargo test --workspace                                     # 테스트
```

- 셋 다 종료코드 0 이어야 한다. `-D warnings` 라서 clippy 경고 하나도 실패다.
- 포맷이 어긋나면 `cargo fmt --all` 로 적용한 뒤 다시 확인한다.
- 빠른 반복 중에는 `cargo check --workspace` 로 타입만 먼저 봐도 된다.
- 일부 함수가 스캐폴드(`todo!()`/빈 스트림)임을 감안한다 — 빌드·clippy·test 는 통과해야
  하고, 새로 채운 부분은 테스트를 동반해야 한다.

## 2. 문서 / SSOT 일관성

코드는 진실의 한 면일 뿐이다. 변경이 아래를 바꿨다면, 해당 **SSOT 문서가 같은 변경
안에서 갱신됐는지** 확인한다(`AGENTS.md` § 단일 출처 규칙). 미갱신이면 lint 실패로 본다 —
코드와 문서가 갈라지면 문서를 믿은 사람이 틀리게 된다.

- 동작/인터페이스/공개 API → `docs/ARCHITECTURE.md` (+ 해당 항목 doc 주석)
- 기본값/설정 키 → `config/config.example.toml`, `docs/SETUP.md`
- 규칙/컨벤션 → `docs/CODING_RULES.md`
- 스캐폴드 `todo!()` 를 채웠다 → `docs/ROADMAP.md` 에서 그 항목 체크
- 같은 사실이 두 문서에 중복돼 충돌하지 않는가(사실은 한 곳 + 나머지는 링크)

빠른 점검:

```sh
git status --short                                          # 무엇이 바뀌었나
# 깨진 상대 링크 후보(대상 파일이 실제로 있는지 사람이 확인)
grep -rEo '\]\(\.{0,2}/[^)]+\)' docs README.md AGENTS.md CLAUDE.md 2>/dev/null
```

## 3. 보안 / 위생

- 비밀(API 키/토큰)이 코드·로그·설정·커밋 diff 에 섞이지 않았는가? `.env` 는 커밋 대상
  아님. 비밀은 히스토리에 남으면 회수 불가다.
- 새 의존성은 루트 `[workspace.dependencies]` 단일 버전인가(`dep.workspace = true`)?
- 도구 경로 입력 제한 등 보안 규칙(`docs/CODING_RULES.md` §8)을 어기지 않았는가?

## 보고 형식

게이트 결과를 한눈에 보이게 요약하고, 실패는 재현 명령과 함께 적는다.

```
✓ fmt(--check)   ✓ clippy(0 warnings)   ✓ test(NN passed)
✓ SSOT: ROADMAP 에서 'OpenAiProvider::stream' 항목 체크됨
⚠ docs/SETUP.md 모델 id 가 config.example.toml 과 불일치 → 동기화 필요
✗ clippy: scv-tools/src/read.rs:42  unused variable `ctx`  (재현: cargo clippy -p scv-tools)
```

통과면 통과라고 분명히, 실패면 무엇이/어디서/왜 실패했는지 정확히 — 그래야 바로 고친다.
