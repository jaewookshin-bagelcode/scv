#!/usr/bin/env bash
# 캐시 무효화 검증(빠름) + beta-on 타입 수정 재시도.
#   (a) context editing beta ON 을 올바른 edit type 으로 재전송 → 200/applied_edits 확인
#   (b) prefix 를 1토큰 바꾸면 cache_read=0(미스)로 떨어지는지 → "1토큰이라도 바뀌면 깨진다" 검증
set -u
BASE="${AIPROXY_BASE}"
URL="$BASE/v1/messages"; RAW="$(cd "$(dirname "$0")" && pwd)"; MODEL="claude-sonnet-4-6"
H=(-H "content-type: application/json" -H "anthropic-version: 2023-06-01" -H "authorization: Bearer ${CODEB_TOKEN:?CODEB_TOKEN 미설정}")
u(){ python3 -c "import json,sys;d=json.load(open(sys.argv[1]));u=d.get('usage',{});e=(d.get('error') or {}).get('message','-');print('  write=%s read=%s in=%s stop=%s cm=%s err=%s'%(u.get('cache_creation_input_tokens'),u.get('cache_read_input_tokens'),u.get('input_tokens'),d.get('stop_reason'),d.get('context_management'),e))" "$1"; }

echo "── (a) context editing beta ON — 올바른 타입(clear_tool_uses_20250919) ──"
printf '{"model":"%s","max_tokens":16,"context_management":{"edits":[{"type":"clear_tool_uses_20250919"}]},"messages":[{"role":"user","content":"ping"}]}' "$MODEL" > "$RAW/ctxmgmt_req.json"
code=$(curl -sS -o "$RAW/ctxmgmt_beta_on.json" -w "%{http_code}" -X POST "$URL" "${H[@]}" -H "anthropic-beta: context-management-2025-06-27" --data @"$RAW/ctxmgmt_req.json")
echo "  HTTP $code"; u "$RAW/ctxmgmt_beta_on.json"

echo "── (b) prefix 1토큰 변경 → 캐시 미스 검증 ──"
# 원본 system 뒤에 ' x'(≈1토큰) 만 덧붙여 프리픽스를 최소 변경
python3 -c "import json;d=json.load(open('$RAW/cache_body.json'));d['system'][0]['text']+=' x';json.dump(d,open('$RAW/cache_body_1tok.json','w'))"
code=$(curl -sS -o "$RAW/cache_break_1tok.json" -w "%{http_code}" -X POST "$URL" "${H[@]}" --data @"$RAW/cache_body_1tok.json")
echo "  HTTP $code"; u "$RAW/cache_break_1tok.json"
echo "  대조: 동일 prefix 2회차는 read=3403 이었음. 1토큰 바뀐 이건 read=0 이어야 '깨진다'가 증명됨."
