#!/usr/bin/env bash
# report.md 실측 재현 — 빠졌던 두 응답(과제2 캐시 3403, 과제5 beta-off 400)을 파일로 캡처한다.
# 토큰이 있는 셸에서 실행:  CODEB_TOKEN=<aiproxy_xxx> bash replay.sh
# (또는 codeb login 후 export CODEB_TOKEN=... 하고 bash replay.sh)
#
# 이 스크립트는 토큰을 파일에 쓰지 않는다(헤더로만 전송). 응답 로그엔 인증정보가 없다.
set -u

BASE="${AIPROXY_BASE}"   # 코드(scv-config)에 있는 실제 aiproxy base_url
URL="$BASE/v1/messages"
RAW="$(cd "$(dirname "$0")" && pwd)"
LOG="$RAW/replay_run.log"
MODEL="claude-sonnet-4-6"

if [ -z "${CODEB_TOKEN:-}" ]; then
  echo "ERROR: CODEB_TOKEN 미설정. 'codeb login --token aiproxy_xxx' 후 export CODEB_TOKEN=... 하고 다시 실행." >&2
  exit 2
fi

: > "$LOG"
say(){ echo "$@" | tee -a "$LOG"; }
usage(){ python3 -c "import json,sys;d=json.load(open(sys.argv[1]));print('  usage:',json.dumps(d.get('usage',{}),ensure_ascii=False));print('  stop:',d.get('stop_reason'),'| type:',d.get('type'),'| error:',json.dumps(d.get('error'),ensure_ascii=False) if d.get('error') else '-')" "$1" 2>/dev/null || echo "  (json 파싱 실패 — 파일 원문 확인)"; }

post(){ # $1=body파일 $2=out파일 $3=선택 beta헤더값
  local body="$1" out="$2" beta="${3:-}"
  local -a H=(-H "content-type: application/json" -H "anthropic-version: 2023-06-01" -H "authorization: Bearer $CODEB_TOKEN")
  [ -n "$beta" ] && H+=(-H "anthropic-beta: $beta")
  local code
  code=$(curl -sS -o "$out" -w "%{http_code}" -X POST "$URL" "${H[@]}" --data @"$body")
  echo "$code"
}

say "== replay @ $(date -u +%FT%TZ) =="
say "URL: $URL"
say ""

# ── 과제 2: 캐시 write→read (같은 prefix 2회 연속) ──────────────
say "── 과제2 캐싱: cache_body.json 2회 연속 전송 ──"
c1=$(post "$RAW/cache_body.json" "$RAW/cache_resp_1.json")
say "1회차 HTTP $c1"; usage "$RAW/cache_resp_1.json" | tee -a "$LOG"
c2=$(post "$RAW/cache_body.json" "$RAW/cache_resp_2.json")
say "2회차 HTTP $c2"; usage "$RAW/cache_resp_2.json" | tee -a "$LOG"
say "  기대: 1회차 cache_creation>0/cache_read=0, 2회차 cache_read>0(같은 값)"
say ""

# ── 과제 5: context editing beta ON(200) / OFF(400) ────────────
cat > "$RAW/ctxmgmt_req.json" <<JSON
{
  "model": "$MODEL",
  "max_tokens": 16,
  "context_management": { "edits": [ { "type": "clear_tool_uses_20250606" } ] },
  "messages": [ { "role": "user", "content": "ping" } ]
}
JSON
say "── 과제5 context editing: beta ON vs OFF ──"
on=$(post "$RAW/ctxmgmt_req.json" "$RAW/ctxmgmt_beta_on.json" "context-management-2025-06-27")
say "beta ON  HTTP $on (기대 200, applied_edits:[])"; usage "$RAW/ctxmgmt_beta_on.json" | tee -a "$LOG"
off=$(post "$RAW/ctxmgmt_req.json" "$RAW/ctxmgmt_beta_off.json")
say "beta OFF HTTP $off (기대 400, 'context_management: Extra inputs are not permitted')"; usage "$RAW/ctxmgmt_beta_off.json" | tee -a "$LOG"
say ""
say "※ beta ON 이 400 이면 edits.type 스키마 버전 차이일 수 있음(그 경우도 파일은 남음)."
say "※ beta OFF 의 400 은 top-level context_management 필드 자체가 거부되는 것이라 스키마와 무관하게 재현됨 — 이게 보고서 과제5의 그 400."
say "== done. 로그: $LOG =="
