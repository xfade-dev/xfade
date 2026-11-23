#!/usr/bin/env bash
# gateway-upstream-test.sh — freebuff-proxy / OmniRoute 作为 xfade 上游网关的可重复测试
# 结果追加写入 /tmp/xfade-gateway-test-report.md
# 用法: bash scripts/gateway-upstream-test.sh
set -u

FB_URL="${FB_URL:-http://127.0.0.1:8787}"
OM_URL="${OM_URL:-http://127.0.0.1:20138/api/v1}"
XF_URL="${XF_URL:-http://127.0.0.1:24860}"
FB_KEY="${FB_KEY:-$(python3 -c "import json;print(json.load(open('$HOME/.config/xfade/secrets.json')).get('xfade/claude/freebuff',''))")}"
OM_KEY="${OM_KEY:-$(python3 -c "import json;print(json.load(open('$HOME/.config/xfade/secrets.json')).get('xfade/codex/omni',''))")}"
FB_MODEL="${FB_MODEL:-deepseek/deepseek-v4-flash}"
OM_MODEL="${OM_MODEL:-codebuddy-cn/glm-5.3-flash}"
REPORT="${REPORT:-/tmp/xfade-gateway-test-report.md}"

pass=0; fail=0
chk() { # chk <name> <expected> <actual>
  if [ "$2" = "$3" ]; then printf "  PASS  %-34s (%s)\n" "$1" "$3"; pass=$((pass+1));
  else printf "  FAIL  %-34s expected=%s actual=%s\n" "$1" "$2" "$3"; fail=$((fail+1)); fi
}
code() { curl -s -m "${2:-90}" -o /tmp/gwt-body -w "%{http_code}" "${@:3}"; }

{
echo ""
echo "# xfade 上游网关测试 $(date '+%F %T')"
} >> "$REPORT"

# —— 1. 网关直连 ——
echo "## 网关直连" 
chk "freebuff health"      200 "$(code x 8 "$FB_URL/health")"
chk "freebuff models(auth)" 200 "$(code x 15 -H "Authorization: Bearer $FB_KEY" "$FB_URL/v1/models")"
chk "freebuff wrong-key"    401 "$(code x 8 -X POST "$FB_URL/v1/chat/completions" -H "Authorization: Bearer sk-wrong" -H 'Content-Type: application/json' -d '{"model":"x","messages":[]}')"
chk "freebuff fake-model"   400 "$(code x 15 -X POST "$FB_URL/v1/chat/completions" -H "Authorization: Bearer $FB_KEY" -H 'Content-Type: application/json' -d '{"model":"no-such-model-xyz","messages":[{"role":"user","content":"hi"}],"max_tokens":8}')"
chk "omni health"          200 "$(code x 8 "${OM_URL%/api/v1}/api/health")"
chk "omni models(auth)"    200 "$(code x 15 -H "Authorization: Bearer $OM_KEY" "$OM_URL/models")"
chk "omni chat 非流式"      200 "$(code x 90 -X POST "$OM_URL/chat/completions" -H "Authorization: Bearer $OM_KEY" -H 'Content-Type: application/json' -d "{\"model\":\"$OM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"reply with exactly: ok\"}],\"max_tokens\":512}")"
chk "omni messages(anthropic)" 200 "$(code x 90 -X POST "$OM_URL/messages" -H "Authorization: Bearer $OM_KEY" -H 'Content-Type: application/json' -d "{\"model\":\"$OM_MODEL\",\"max_tokens\":512,\"messages\":[{\"role\":\"user\",\"content\":\"reply with exactly: ok\"}]}")"

# —— 2. xfade serve 层 ——
echo "## xfade serve ($XF_URL)" | tee -a "$REPORT"
chk "proxy models(merge)"  200 "$(code x 20 "$XF_URL/v1/models")"
chk "proxy chat(omni 主)"  200 "$(code x 90 -X POST "$XF_URL/v1/chat/completions" -H 'Content-Type: application/json' -d "{\"model\":\"$OM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"reply with exactly: ok\"}],\"max_tokens\":512}")"
chk "proxy /v1/messages 转换" 200 "$(code x 90 -X POST "$XF_URL/v1/messages" -H 'Content-Type: application/json' -H 'anthropic-version: 2023-06-01' -d "{\"model\":\"$OM_MODEL\",\"max_tokens\":512,\"messages\":[{\"role\":\"user\",\"content\":\"reply with exactly: ok\"}]}")"
chk "proxy /v1/responses(codex)" 200 "$(code x 90 -X POST "$XF_URL/v1/responses" -H 'Content-Type: application/json' -d "{\"model\":\"$OM_MODEL\",\"input\":\"reply with exactly: ok\",\"max_output_tokens\":512}")"

# —— 3. 故障转移（omni 指向不可达地址，观察 fallback）——
# 说明:需要临时改 omni base_url;测试后恢复。xfade DB 即时生效。
ORIG_URL=$(sqlite3 "$HOME/.config/xfade/xfade.db" "SELECT base_url FROM providers WHERE tool='codex' AND id='omni';")
xfade edit omni --tool codex --base-url http://10.72.1.17:8787/v1 >/dev/null 2>&1
FO_HDR=$(curl -s -m 90 -D - -o /dev/null -X POST "$XF_URL/v1/chat/completions" -H 'Content-Type: application/json' \
  -d "{\"model\":\"$OM_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"reply with exactly: ok\"}],\"max_tokens\":512}" | grep -i "x-xfade-failover-path" | tr -d '\r')
xfade edit omni --tool codex --base-url "$ORIG_URL" >/dev/null 2>&1
echo "- failover-path: ${FO_HDR:-<无>}" | tee -a "$REPORT"
if echo "$FO_HDR" | grep -q '"provider":"freebuff","status":200'; then
  chk "failover omni→freebuff" ok ok; else chk "failover omni→freebuff" ok "见 $REPORT"; fi

# —— 4. 端到端（真实工具）——
echo "## 端到端" | tee -a "$REPORT"
CL=$(cd /tmp && xfade run local-proxy --tool claude -- -p "reply with exactly: ok" --model claude-sonnet-4-5 2>&1 | tail -1)
chk "claude e2e" "ok" "$CL"
CX=$(cd /tmp && xfade run local-proxy --tool codex -- exec --skip-git-repo-check "reply with exactly: ok" 2>&1 | tail -1)
chk "codex e2e" "ok" "$CX"

echo ""
echo "==== 汇总: PASS=$pass FAIL=$fail (详情追加于 $REPORT) ===="
exit $([ "$fail" -eq 0 ] && echo 0 || echo 1)
