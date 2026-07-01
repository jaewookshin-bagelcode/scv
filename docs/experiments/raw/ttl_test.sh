#!/usr/bin/env bash
# TTL 5분 만료 검증(느림, ~5.5분). 고유 프리픽스를 write 후, 중간 접근 없이 330초 대기하고
# 다시 보내 cache_read 가 0 으로 떨어지는지(만료) 확인한다.
set -u
BASE="${AIPROXY_BASE}"
URL="$BASE/v1/messages"; RAW="$(cd "$(dirname "$0")" && pwd)"; MODEL="claude-sonnet-4-6"
LOG="$RAW/ttl_run.log"; : > "$LOG"
H=(-H "content-type: application/json" -H "anthropic-version: 2023-06-01" -H "authorization: Bearer ${CODEB_TOKEN:?CODEB_TOKEN 미설정}")
u(){ python3 -c "import json,sys;d=json.load(open(sys.argv[1]));x=d.get('usage',{});print('  write=%s read=%s ephem5m=%s'%(x.get('cache_creation_input_tokens'),x.get('cache_read_input_tokens'),x.get('cache_creation',{}).get('ephemeral_5m_input_tokens')))" "$1"; }
say(){ echo "$@" | tee -a "$LOG"; }

# 다른 테스트와 안 겹치도록 고유 마커 프리픽스
python3 -c "import json;d=json.load(open('$RAW/cache_body.json'));d['system'][0]['text']+=' TTL-PROBE-2026-07-01-UNIQUE';json.dump(d,open('$RAW/ttl_body.json','w'))"

say "== TTL test @ $(date -u +%FT%TZ) =="
c1=$(curl -sS -o "$RAW/ttl_1_write.json" -w "%{http_code}" -X POST "$URL" "${H[@]}" --data @"$RAW/ttl_body.json")
say "t0  write HTTP $c1"; u "$RAW/ttl_1_write.json" | tee -a "$LOG"
say "… 330초 대기(중간 접근 없음, TTL 300초 초과) …"
sleep 330
c2=$(curl -sS -o "$RAW/ttl_2_afterexpiry.json" -w "%{http_code}" -X POST "$URL" "${H[@]}" --data @"$RAW/ttl_body.json")
say "t+330  재전송 HTTP $c2"; u "$RAW/ttl_2_afterexpiry.json" | tee -a "$LOG"
say "판정: t+330 에서 read=0/write>0 이면 TTL 만료로 캐시 깨짐 확정. read>0 이면 5분보다 오래 산다는 뜻."
say "== TTL test done =="
