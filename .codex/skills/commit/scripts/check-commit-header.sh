#!/bin/sh
# Conventional Commits 헤더 검증기. 통과=0, 위반=1, 사용오류=2.
#
# scv 의 commit 스킬이 쓰는 결정적 검증기. commit-msg 훅으로도 쓸 수 있다.
# 사용:
#   sh check-commit-header.sh <message-file>   # 파일에서
#   sh check-commit-header.sh -                 # 표준입력에서
#   (인자 없으면 .git/COMMIT_EDITMSG)
set -eu

src="${1:-.git/COMMIT_EDITMSG}"
if [ "$src" = "-" ]; then
  msg="$(cat)"
elif [ -f "$src" ]; then
  msg="$(cat "$src")"
else
  echo "commit-lint: 메시지 파일 없음: $src" >&2
  exit 2
fi

# 헤더 = 첫 번째 (주석 # 아니고 공백 아닌) 줄
header="$(printf '%s\n' "$msg" \
  | grep -v '^[[:space:]]*#' \
  | grep -v '^[[:space:]]*$' \
  | head -n 1)"

if [ -z "$header" ]; then
  echo "commit-lint: 빈 커밋 메시지" >&2
  exit 1
fi

types='feat|fix|docs|refactor|perf|test|build|ci|chore|revert|style'
pattern="^(${types})(\([a-z0-9._-]+\))?!?: .+"

fail=0
printf '%s' "$header" | grep -Eq "$pattern" || { fail=1; bad_format=1; }
: "${bad_format:=0}"

len="$(printf '%s' "$header" | wc -m | tr -d ' ')"
if [ "$len" -gt 72 ]; then fail=1; too_long=1; else too_long=0; fi

if [ "$fail" -ne 0 ]; then
  echo "commit-lint: 헤더가 Conventional Commits 규약을 위반함" >&2
  echo "  헤더: $header" >&2
  [ "$bad_format" -eq 1 ] && echo "  - 형식: <type>(<scope>)!: <subject>  (type: ${types})" >&2
  [ "$too_long" -eq 1 ] && echo "  - 첫 줄 ${len}자 — 72자 이내여야 함" >&2
  exit 1
fi

echo "commit-lint: OK ($header)"
