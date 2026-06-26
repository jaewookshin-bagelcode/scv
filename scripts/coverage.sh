#!/usr/bin/env bash
# scv 커버리지 게이트 — 테스트 티어별 라인 커버리지 임계를 강제한다(blocking).
#
# SSOT: docs/CODING_RULES.md §10. 실행/PR 게이트 안내는 docs/SETUP.md §5.
#
# 티어 분류는 테스트의 위치/파일명 컨벤션으로 정한다:
#   unit         src 내부 `#[cfg(test)] mod tests`   (cargo --lib/--bins)   목표 ≥95%
#   integration  crates/*/tests/*.rs  (단 e2e_*.rs 제외)                    목표 ≥90%
#   e2e          crates/*/tests/e2e_*.rs  (fake Provider 로 루프 종단 구동)  목표 ≥85%
#
# 각 티어는 독립 측정한다: 해당 티어의 테스트만 돌려(`clean` 후 `--no-report` 로 누적)
# 워크스페이스 소스 전체에 대한 라인 커버리지를 `report --fail-under-lines` 로 게이트한다.
# 임계는 환경변수로 덮어쓸 수 있다: SCV_COV_UNIT / SCV_COV_INTEGRATION / SCV_COV_E2E.
#
# 종료코드: 모든 티어 통과 0, 하나라도 미달/데이터없음 1, 전제조건 미충족 2.
set -uo pipefail

UNIT_MIN="${SCV_COV_UNIT:-95}"
INT_MIN="${SCV_COV_INTEGRATION:-90}"
E2E_MIN="${SCV_COV_E2E:-85}"

# 커버 불가/미구현 경로는 분모에서 제외한다(SSOT: docs/CODING_RULES.md §10):
#   - scv-cli/src/main.rs : 부트스트랩/조립(테스트로 실행 불가)
#   - scv-tui/src/        : 인터랙티브 TUI(raw-mode — 단위/통합 테스트 불가)
#   - scv-providers/src/anthropic.rs : Phase 4 미구현 스텁
# 구현·테스트가 가능해지면 해당 항목을 이 정규식에서 뺀다.
EXCLUDE_RE='(scv-cli/src/main\.rs|scv-tui/src/|scv-providers/src/anthropic\.rs)'

# 저장소 루트로 이동(스크립트 위치 기준) + 비-인터랙티브 셸 대비 cargo 환경 로드.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
# shellcheck disable=SC1091
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

if ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "✗ cargo-llvm-cov 가 없습니다. 설치: cargo install cargo-llvm-cov --locked" >&2
  echo "  (컴포넌트 llvm-tools 는 rust-toolchain.toml 에 고정돼 rustup 이 자동 설치)" >&2
  exit 2
fi

# 티어별 (package, test-target) 수집. tests/ 최상위 .rs 만 cargo 통합 테스트 타깃이다.
int_pkgs=(); int_tests=()
e2e_pkgs=(); e2e_tests=()
shopt -s nullglob
for f in crates/*/tests/*.rs; do
  pkg="$(basename "$(dirname "$(dirname "$f")")")"   # crates/<pkg>/tests/<name>.rs
  name="$(basename "$f" .rs)"
  case "$name" in
    e2e_*) e2e_pkgs+=("$pkg"); e2e_tests+=("$name") ;;
    *)     int_pkgs+=("$pkg"); int_tests+=("$name") ;;
  esac
done
shopt -u nullglob

# 워크스페이스 크레이트 목록(티어별 책임 범위 산정용).
all_pkgs="$(for d in crates/*/; do basename "$d"; done)"

fail=0

# 누적된 profraw 를 라인 커버리지로 요약하고 임계와 비교한다. 티어는 **자신이 실제로
# 운동하는 크레이트**에만 책임진다 — `$3`(포함 크레이트, 공백 구분; 빈값=전체) 에 없는
# 크레이트의 src 는 분모에서 뺀다. 이렇게 해야 "e2e 가 안 거치는 providers HTTP 가
# e2e 분모에 남아 영구 미달" 같은 왜곡을 막는다(SSOT: CODING_RULES §10).
gate () {  # $1=label  $2=min  $3=include(공백 구분 크레이트, 빈값=전체)
  local label="$1" min="$2" include="$3"
  local ignore="$EXCLUDE_RE"
  if [ -n "$include" ]; then
    local include_re others
    include_re="$(printf '%s\n' $include | paste -sd'|' -)"
    others="$(printf '%s\n' $all_pkgs | grep -vE "^(${include_re})$" | paste -sd'|' -)"
    [ -n "$others" ] && ignore="${ignore}|crates/(${others})/src/"
  fi
  if cargo llvm-cov report --summary-only --ignore-filename-regex "$ignore" --fail-under-lines "$min"; then
    echo "✓ ${label}: lines ≥ ${min}%"
  else
    echo "✗ ${label}: lines < ${min}% (또는 커버리지 데이터 없음)"
    fail=1
  fi
}

# 한 티어를 격리 측정한다: clean → 해당 타깃만 --no-report 누적 → gate.
# $3=include(책임 크레이트), $4.. = cargo 타깃 선택자(--lib/--bins 또는 -p .. --test ..).
measure () {  # $1=label  $2=min  $3=include  $4...=cargo 타깃 선택자
  local label="$1" min="$2" include="$3"; shift 3
  cargo llvm-cov clean --workspace
  cargo llvm-cov --no-report "$@"
  gate "$label" "$min" "$include"
}

echo "── unit (≥${UNIT_MIN}%) ──────────────────────────────"
# unit 은 워크스페이스 전체 lib/bin 을 책임진다(include 비움).
measure "unit" "$UNIT_MIN" "" --workspace --lib --bins

echo "── integration (≥${INT_MIN}%) ───────────────────────"
if [ "${#int_tests[@]}" -eq 0 ]; then
  echo "✗ integration: tests/*.rs (e2e_ 제외) 타깃이 없습니다 — ≥${INT_MIN}% 게이트 미충족. 통합 테스트를 추가하세요."
  fail=1
else
  sel=(); for i in "${!int_tests[@]}"; do sel+=( -p "${int_pkgs[$i]}" --test "${int_tests[$i]}" ); done
  int_inc="$(printf '%s\n' "${int_pkgs[@]}" | sort -u | paste -sd' ' -)"
  measure "integration" "$INT_MIN" "$int_inc" "${sel[@]}"
fi

echo "── e2e (≥${E2E_MIN}%) ────────────────────────────────"
if [ "${#e2e_tests[@]}" -eq 0 ]; then
  echo "✗ e2e: tests/e2e_*.rs 타깃이 없습니다 — ≥${E2E_MIN}% 게이트 미충족. 종단 테스트를 추가하세요."
  fail=1
else
  sel=(); for i in "${!e2e_tests[@]}"; do sel+=( -p "${e2e_pkgs[$i]}" --test "${e2e_tests[$i]}" ); done
  e2e_inc="$(printf '%s\n' "${e2e_pkgs[@]}" | sort -u | paste -sd' ' -)"
  measure "e2e" "$E2E_MIN" "$e2e_inc" "${sel[@]}"
fi

echo "──────────────────────────────────────────────────────"
if [ "$fail" -ne 0 ]; then
  echo "✗ 커버리지 게이트 실패 (목표 unit≥${UNIT_MIN} integration≥${INT_MIN} e2e≥${E2E_MIN})."
  exit 1
fi
echo "✓ 커버리지 게이트 통과 (unit≥${UNIT_MIN} integration≥${INT_MIN} e2e≥${E2E_MIN})."
