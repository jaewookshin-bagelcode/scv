---
name: commit
description: scv 저장소에 git 커밋을 만든다. Conventional Commits 형식(type/scope=크레이트명/72자 헤더/! breaking)과 저장소 규칙 — 요청 시에만 커밋, 기본 브랜치면 기능 브랜치 먼저, 코드 변경이면 같은 커밋에 SSOT 문서 갱신 포함 — 을 따른다. 커밋은 되돌리기 번거롭고 협업자에게 그대로 노출되므로, 사용자가 $commit 으로 명시 호출할 때만 실행한다.
---

# commit

scv 저장소에서 잘 구성된 git 커밋을 만든다.

커밋 메시지는 나중에 읽는 사람(리뷰어, 미래의 나, `git log`/`git blame` 을 보는 누구든)을
위한 것이다. 그래서 **형식이 일정해야 훑어보기 쉽고**, **한 커밋이 한 가지 변경**이어야
되돌리기·체리픽이 안전하다. 커밋은 부작용이 있고 협업자에게 그대로 보이므로 **사용자가
명시적으로 요청할 때만** 만든다(자동 커밋 금지).

## 절차

1. **변경 파악** — `git status`, `git diff`, `git diff --staged` 로 무엇이 왜 바뀌었는지
   이해한다. 무관한 변경이 섞여 있으면 한 가지 논리적 변경으로 좁히거나 커밋을 나눈다.
   "한 커밋 = 한 의도" 가 되도록.
2. **브랜치 확인** — 현재 브랜치가 기본 브랜치(`main`)면 **먼저 기능 브랜치를 판다**
   (`git switch -c <type>/<topic>`). 기본 브랜치 직접 커밋은 협업·롤백을 어렵게 한다.
3. **스테이징** — 의도한 파일만 `git add`. 디버그 코드·임시 파일·`.env`·비밀이 섞였는지
   확인한다(비밀은 히스토리에 남으면 회수 불가).
4. **SSOT 갱신 포함** — 코드 변경이 동작/인터페이스/기본값/로드맵을 바꿨다면, **같은
   커밋에 해당 SSOT 문서 갱신을 넣는다**(`AGENTS.md` § 단일 출처 규칙). 코드와 문서가
   갈라지면 다음 사람이 어느 쪽을 믿어야 할지 모른다. 문서만 바뀌면 `docs:`.
5. **메시지 작성** — 아래 형식. 작성 후 헤더를 검증한다(§ 검증).
6. **커밋** — 멀티라인은 heredoc 으로(§ 커밋 실행).

## 메시지 형식 (Conventional Commits)

```text
<type>(<scope>)<!>: <subject>

<body — 무엇이 아니라 "왜". 한 줄 비우고 시작, ~72자로 줄바꿈>

<footer — 선택: BREAKING CHANGE: ... / Refs: #123>
```

헤더(첫 줄) 규칙: **72자 이내**, 명령형(imperative), 끝에 마침표 없음. 명령형은
"이 커밋을 적용하면 ...하게 된다" 를 완성하는 어조다(예: `add`, `fix`, `remove`).

| type | 쓰임 | / | type | 쓰임 |
|------|------|---|------|------|
| `feat` | 기능 추가 | | `build` | 빌드/의존성(Cargo) |
| `fix` | 버그 수정 | | `ci` | CI 설정 |
| `docs` | 문서만 변경 | | `chore` | 잡일 |
| `refactor` | 동작 불변 구조 개선 | | `revert` | 되돌리기 |
| `perf` | 성능 | | `style` | 포맷/공백 |
| `test` | 테스트 | | | |

`scope`(선택)는 크레이트/영역: `scv-core` `scv-providers` `scv-tools` `scv-skills`
`scv-config` `scv-tui` `scv-cli` `docs` `config` `ci`. 파괴적 변경은 `type`/`scope`
뒤에 `!` 를 붙이거나 footer 에 `BREAKING CHANGE:` 를 적는다.

예시:

```text
feat(scv-providers): add OpenAI chat completions streaming adapter
fix(scv-tools): reject read paths that escape the workspace root
refactor(scv-core)!: drop non-streaming complete(), keep stream() only
```

## 검증 (커밋 전 헤더 확인)

번들 스크립트로 헤더가 규약(형식 + 72자)을 만족하는지 확인한다:

```sh
printf '%s' "<헤더>" | sh .codex/skills/commit/scripts/check-commit-header.sh -
```

원하면 `commit-msg` 훅으로 걸어 로컬에서 자동 검증할 수 있다:

```sh
ln -sf ../../.codex/skills/commit/scripts/check-commit-header.sh .git/hooks/commit-msg
chmod +x .git/hooks/commit-msg
```

## 커밋 실행

멀티라인 메시지는 `-F -` + heredoc 으로 만든다:

```sh
git commit -F - <<'EOF'
feat(scv-core): add session store trait

세션을 JSONL 로 영속화/재개하기 위한 SessionStore 추상을 추가한다.
파일 구현은 합성 루트(scv-cli)에 두어 core 가 저장 위치를 모르게 한다.
EOF
```

> 커밋 트레일러(co-author 등)는 이 스킬에서 **강제하지 않는다**. 트레일러가 필요하면
> 실행 도구/환경의 규약을 따른다. Codex 가 만드는 커밋에 `Co-Authored-By: Claude ...`
> 를 넣지 않는다.

`git push` 는 **사용자가 요청할 때만** 한다.

## 하지 말 것 — 그리고 왜

- **요청 없이 커밋/푸시** — 커밋 시점은 사용자가 정한다.
- **무관한 변경 섞기** — 한 커밋이 여러 의도를 담으면 리뷰·되돌리기가 꼬인다.
- **헤더 검증 생략** — 깨진 형식은 히스토리에 영구히 남는다.
- **코드만 바꾸고 SSOT 문서 누락** — 코드와 문서가 갈라져 다음 사람을 헷갈리게 한다.
- **부정확한 co-author 트레일러** — `Co-Authored-By` 는 실제 기여자를 나타내므로,
  Claude 가 만들었다는 트레일러를 Codex 커밋에 넣지 않는다.
