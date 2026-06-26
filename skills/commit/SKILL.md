---
name: commit
description: 변경을 의미 단위로 묶어 Conventional Commits 규약의 git 커밋을 만든다.
when_to_use: 사용자가 "커밋"을 요청하거나 작업한 변경을 저장하려 할 때.
---
# commit

git 변경을 **한 커밋 = 한 의도**로 묶어 Conventional Commits 형식으로 커밋한다.

## 절차
1. `git status` 와 `git diff`(+ `git diff --staged`)로 무엇이 왜 바뀌었는지 파악한다.
   무관한 변경이 섞였으면 한 의도로 좁히거나 커밋을 나눈다.
2. 기본 브랜치(main/master)면 먼저 기능 브랜치를 판다: `git switch -c <type>/<topic>`.
3. 의도한 파일만 `git add`. 비밀(.env·키·토큰)·디버그 잔여물이 섞이지 않았는지 확인.
4. 메시지: `<type>(<scope>): <subject>` (헤더 ≤72자, 명령형, 끝 마침표 없음). 본문은 "왜"를
   한 줄 비우고 ~72자 줄바꿈. type: feat|fix|docs|refactor|perf|test|build|ci|chore.
5. 멀티라인은 `git commit -F -` + heredoc 으로 커밋한다.

## 규칙
- **요청 없이 push 하지 않는다.** 커밋도 사용자가 요청할 때만.
- 코드 변경이 동작/인터페이스를 바꿨으면 관련 문서도 같은 커밋에 넣는다.
- 변경 내용을 정직하게 요약한다 — 추측한 효과를 단정하지 않는다.
