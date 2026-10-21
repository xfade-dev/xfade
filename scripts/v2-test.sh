#!/usr/bin/env bash
# v2-test.sh — xfade V2 发版验收脚本（真二进制 + 真 daemon + 本地 sink）
#
# 覆盖：
#   1. 静态顺序路由（smart off，配置顺序优先）
#   2. Smart Fader：轻意图走 local、重意图走 cloud
#   3. `proxy smart on|off` 每请求读 db —— 不重启 daemon 即刻生效
#   4. failover：local 500 时落到 cloud，请求仍 200
#   5. 4xx 不触发 failover
#   6. passthrough model 改写（proxy use --model）
#   7. auth 替换（sink 收到 provider 的 key，不是客户端的）
#   8. proxy clear → 503；且 clear 不重置 smart 开关（db.rs 修复的 CLI 级验证）
#   9. 即时语法注入层：codex/opencode 经 -c / OPENCODE_CONFIG_CONTENT 真可用
#      （本机无二进制则 skip）；pi 拒绝并指引 `use`
#  10. 真实上游双链路（可选）：XFADE_TEST_BASEURL + XFADE_TEST_KEY 设置后启用
#
# 凭据一律走环境变量，不进本脚本、不进 git。
#
# 用法：
#   scripts/v2-test.sh                 # 本地 sink 矩阵（1–9）
#   XFADE_TEST_BASEURL=http://x:3000 \
#   XFADE_TEST_KEY=sk-... \
#   XFADE_TEST_MODEL=glm-5-2-260617 \
#     scripts/v2-test.sh               # 追加真实上游双链路（10）
#
# 环境开关：
#   XFADE_BIN    二进制路径（默认 target/debug/xfade，不存在则自动 cargo build）
#   SKIP_BUILD=1 跳过自动构建
set -euo pipefail

cd "$(dirname "$0")/.."

# ---------- 小工具 ----------
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
sect() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
# assert_eq <actual> <expected> <label>
assert_eq() {
  if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (expected=$2 actual=$1)"; fi
}
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }

# ---------- 构建 ----------
XFADE_BIN="${XFADE_BIN:-target/debug/xfade}"
if [ ! -x "$XFADE_BIN" ]; then
  if [ "${SKIP_BUILD:-0}" = "1" ]; then echo "XFADE_BIN $XFADE_BIN 不存在且 SKIP_BUILD=1"; exit 1; fi
  echo "building xfade ..."
  cargo build -p xfade >&2
fi
command -v jq >/dev/null || { echo "需要 jq"; exit 1; }
command -v python3 >/dev/null || { echo "需要 python3"; exit 1; }

# ---------- 隔离环境 ----------
WORK=$(mktemp -d /tmp/xfade-v2test.XXXXXX)
export XFADE_DATA_DIR="$WORK/data"
export XFADE_SECRETS=file          # 无头/CI 友好：不碰钥匙串
PORT_PROXY=$(free_port)
PORT_LOCAL=$(free_port)
PORT_CLOUD=$(free_port)
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  for p in "${PIDS[@]:-}"; do wait "$p" 2>/dev/null || true; done
  if [ "$FAIL" -eq 0 ]; then rm -rf "$WORK"; else echo "工作目录保留供排查: $WORK"; fi
}
trap cleanup EXIT

# ---------- 测试 sink ----------
# 每个请求把 {path, auth, body} 追加为 JSON 行到日志；响应模式由 mode 文件
# 内容决定（200/400/500），改文件即可动态注入故障，无需重启 sink。
# 200 模式下按请求形态应答：/responses 路径或非空 stream 标志 → SSE 流
# （真实客户端 codex/opencode 都是流式，非 SSE 响应会让它们静默无限重试）；
# 否则返回普通 JSON（curl 探针路径）。
cat > "$WORK/sink.py" <<'PY'
import json, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer

port, log, modef = int(sys.argv[1]), sys.argv[2], sys.argv[3]

def chat_chunks(model):
    return [
        {"id":"chatcmpl-s1","object":"chat.completion.chunk","created":int(time.time()),"model":model,
         "choices":[{"index":0,"delta":{"role":"assistant","content":"sink-ok"},"finish_reason":None}]},
        {"id":"chatcmpl-s1","object":"chat.completion.chunk","created":int(time.time()),"model":model,
         "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
         "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}},
    ]

def resp_events(model):
    rid = "resp-s1"
    item = {"id":"msg-s1","type":"message","status":"completed","role":"assistant",
            "content":[{"type":"output_text","text":"sink-ok"}]}
    resp = {"id":rid,"object":"response","created_at":int(time.time()),"status":"completed",
            "model":model,"output":[item],
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}
    return [
        ("response.created", {"type":"response.created","response":{"id":rid,"object":"response","created_at":int(time.time()),"status":"in_progress","model":model,"output":[]}}),
        ("response.output_item.added", {"type":"response.output_item.added","output_index":0,"item":{"id":"msg-s1","type":"message","status":"in_progress","role":"assistant","content":[]}}),
        ("response.output_text.delta", {"type":"response.output_text.delta","item_id":"msg-s1","output_index":0,"content_index":0,"delta":"sink-ok"}),
        ("response.output_item.done", {"type":"response.output_item.done","output_index":0,"item":item}),
        ("response.completed", {"type":"response.completed","response":resp}),
    ]

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n).decode("utf-8", "replace")
        with open(log, "a") as f:
            f.write(json.dumps({"path": self.path,
                                "auth": self.headers.get("authorization", ""),
                                "body": body}) + "\n")
        try:
            mode = open(modef).read().strip()
        except FileNotFoundError:
            mode = "200"
        if mode == "200":
            try:
                req = json.loads(body)
            except Exception:
                req = {}
            model = req.get("model", "sink-model")
            if "/responses" in self.path:
                payload = b"".join(("event: %s\ndata: %s\n\n" % (ev, json.dumps(d))).encode()
                                   for ev, d in resp_events(model))
                ctype = "text/event-stream"
            elif req.get("stream"):
                payload = b"".join(b"data: " + json.dumps(c).encode() + b"\n\n"
                                   for c in chat_chunks(model)) + b"data: [DONE]\n\n"
                ctype = "text/event-stream"
            else:
                payload = json.dumps({
                    "id": "sink-1", "object": "chat.completion",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "sink-ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                }).encode()
                ctype = "application/json"
            self.send_response(200)
            self.send_header("content-type", ctype)
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        else:
            payload = json.dumps({"error": {"message": f"sink injected {mode}"}}).encode()
            self.send_response(int(mode))
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

HTTPServer(("127.0.0.1", port), H).serve_forever()
PY

LOG_LOCAL="$WORK/local.log"; LOG_CLOUD="$WORK/cloud.log"
MODE_LOCAL="$WORK/mode.local"; MODE_CLOUD="$WORK/mode.cloud"
echo 200 > "$MODE_LOCAL"; echo 200 > "$MODE_CLOUD"
: > "$LOG_LOCAL"; : > "$LOG_CLOUD"

python3 "$WORK/sink.py" "$PORT_LOCAL" "$LOG_LOCAL" "$MODE_LOCAL" >"$WORK/sink-local.out" 2>&1 & PIDS+=($!)
python3 "$WORK/sink.py" "$PORT_CLOUD" "$LOG_CLOUD" "$MODE_CLOUD" >"$WORK/sink-cloud.out" 2>&1 & PIDS+=($!)

# sink 就绪等待：首个请求 502 的竞态曾在此暴露（sink 未 listen 时 daemon 直连失败）。
# 用裸 TCP 连接探测——POST 探测会被 sink 记录、污染请求计数。
for port in "$PORT_LOCAL" "$PORT_CLOUD"; do
  for i in $(seq 1 50); do
    python3 -c "import socket; s=socket.create_connection(('127.0.0.1',$port),0.2); s.close()" 2>/dev/null && break
    sleep 0.1
  done
done

# sink 请求计数 / 最后一条 body 的 model 字段
n_local() { wc -l < "$LOG_LOCAL" | tr -d ' '; }
n_cloud() { wc -l < "$LOG_CLOUD" | tr -d ' '; }
last_cloud_model() { tail -1 "$LOG_CLOUD" | jq -r '.body | fromjson | .model // ""'; }
last_cloud_auth()  { tail -1 "$LOG_CLOUD" | jq -r '.auth'; }

LIGHT='{"model":"client-model","messages":[{"role":"user","content":"why does this fail to compile?"}]}'
HEAVY='{"model":"client-model","messages":[{"role":"user","content":"restructure the whole proxy module into layers"}]}'

post() { # post <body> → 打印 http status
  curl -s -o "$WORK/resp.json" -w '%{http_code}' \
    -H 'Content-Type: application/json' -H 'Authorization: Bearer client-token' \
    -d "$1" "http://127.0.0.1:$PORT_PROXY/v1/chat/completions"
}

# ---------- 配置 provider 与路由 ----------
sect "0. 准备（隔离 XFADE_DATA_DIR=$WORK/data, secrets=file）"
"$XFADE_BIN" add lcl --tool codex --base-url "http://127.0.0.1:$PORT_LOCAL" --key k-local >/dev/null
"$XFADE_BIN" add cld --tool codex --base-url "http://127.0.0.1:$PORT_CLOUD" --key k-cloud --set tier=cloud >/dev/null
"$XFADE_BIN" proxy use cld lcl >/dev/null   # 配置顺序：cloud 在前

"$XFADE_BIN" serve --port "$PORT_PROXY" > "$WORK/serve.log" 2>&1 & PIDS+=($!)
for i in $(seq 1 50); do
  curl -sf "http://127.0.0.1:$PORT_PROXY/health" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -sf "http://127.0.0.1:$PORT_PROXY/health" >/dev/null || { echo "daemon 未就绪"; cat "$WORK/serve.log"; exit 1; }
ok "daemon 就绪 (port $PORT_PROXY)"

# ---------- 1. 静态顺序 ----------
sect "1. smart off：配置顺序优先（cloud 先）"
st=$(post "$LIGHT")
assert_eq "$st" 200 "轻意图请求 200"
assert_eq "$(n_cloud)" 1 "cloud 被调用 1 次"
assert_eq "$(n_local)" 0 "local 未被调用"
assert_eq "$(last_cloud_auth)" "Bearer k-cloud" "auth 被替换为 provider 的 key"

# ---------- 2. Smart Fader 路由 ----------
sect "2. smart on：轻意图 local 优先、重意图 cloud 优先"
"$XFADE_BIN" proxy smart on >/dev/null
st=$(post "$LIGHT")
assert_eq "$st" 200 "轻意图请求 200"
assert_eq "$(n_local)" 1 "轻意图落到 local"
assert_eq "$(n_cloud)" 1 "cloud 无新增调用"
st=$(post "$HEAVY")
assert_eq "$st" 200 "重意图请求 200"
assert_eq "$(n_cloud)" 2 "重意图落到 cloud"

# ---------- 3. 开关即刻生效（不重启 daemon） ----------
sect "3. proxy smart off：不重启 daemon 即刻恢复静态顺序"
"$XFADE_BIN" proxy smart off >/dev/null
st=$(post "$LIGHT")
assert_eq "$st" 200 "请求 200"
assert_eq "$(n_cloud)" 3 "轻意图回到 cloud"
assert_eq "$(n_local)" 1 "local 无新增调用"

# ---------- 4. failover ----------
sect "4. smart on + local 500：failover 到 cloud，请求仍 200"
"$XFADE_BIN" proxy smart on >/dev/null
echo 500 > "$MODE_LOCAL"
st=$(post "$LIGHT")
assert_eq "$st" 200 "local 故障时请求仍 200"
assert_eq "$(n_local)" 2 "local 被尝试"
assert_eq "$(n_cloud)" 4 "cloud 兜底"
echo 200 > "$MODE_LOCAL"

# ---------- 5. 4xx 不 failover ----------
sect "5. cloud 400：不重试不 failover"
echo 400 > "$MODE_CLOUD"
"$XFADE_BIN" proxy smart off >/dev/null
before_local=$(n_local)
st=$(post "$LIGHT")
assert_eq "$st" 400 "上游 400 原样透传"
assert_eq "$(n_local)" "$before_local" "local 未被兜底调用"
echo 200 > "$MODE_CLOUD"

# ---------- 6. passthrough model 改写 ----------
sect "6. proxy use --model：passthrough body 的 model 字段被改写"
"$XFADE_BIN" proxy use cld --model glm-5-2-260617 >/dev/null
st=$(post "$LIGHT")
assert_eq "$st" 200 "请求 200"
assert_eq "$(last_cloud_model)" "glm-5-2-260617" "model 字段被改写为 override"
"$XFADE_BIN" proxy use cld lcl >/dev/null
st=$(post "$LIGHT")
assert_eq "$(last_cloud_model)" "client-model" "去掉 override 后透传客户端 model"

# ---------- 7. clear → 503；clear 不重置 smart ----------
sect "7. proxy clear：503；smart 开关不被 clear 重置"
"$XFADE_BIN" proxy smart on >/dev/null   # 先置 ON，验证 clear 不重置它
"$XFADE_BIN" proxy clear >/dev/null
st=$(post "$LIGHT")
assert_eq "$st" 503 "无路由返回 503"
"$XFADE_BIN" proxy use cld lcl >/dev/null
before_local=$(n_local); before_cloud=$(n_cloud)
st=$(post "$LIGHT")
assert_eq "$st" 200 "重配路由后请求 200"
if [ "$(n_local)" = "$((before_local+1))" ] && [ "$(n_cloud)" = "$before_cloud" ]; then
  ok "smart on 在 clear 后仍然生效（轻意图走 local）"
else
  bad "smart 开关被 clear 重置（local=$(n_local)/cloud=$(n_cloud)）"
fi
"$XFADE_BIN" proxy smart off >/dev/null

# ---------- 8. 即时语法注入层 ----------
sect "8. 即时语法：codex/opencode 真可用（注入层），pi 拒绝并指引 use"
out=$("$XFADE_BIN" pi nonexistent-provider 2>&1) && rc=0 || rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qi 'use'; then
  ok "pi instant launch 被拒绝且指引 use"
else
  bad "pi instant launch 行为异常 (rc=$rc out=$out)"
fi

# codex/opencode 注入层 e2e（条件：本机装有对应二进制）。
# 注入只碰 env/-c 覆盖，真实配置目录零写入；sink 走 SSE 才算真链路。
"$XFADE_BIN" add probe-cx --tool codex --base-url "http://127.0.0.1:$PORT_CLOUD" \
  --key k-cloud --set model=test-model >/dev/null
"$XFADE_BIN" add probe-oc --tool opencode --base-url "http://127.0.0.1:$PORT_CLOUD" \
  --key k-cloud --set model=test-model >/dev/null

if command -v codex >/dev/null; then
  before=$(n_cloud)
  out=$("$XFADE_BIN" codex probe-cx -- exec 'say hi' 2>&1) && rc=0 || rc=$?
  assert_eq "$rc" 0 "xfade codex instant 退出码 0"
  printf '%s' "$out" | grep -q 'sink-ok' && ok "codex 收到 sink 流式回复" || bad "codex 输出异常: $out"
  resp_auth=$(grep '/v1/responses' "$LOG_CLOUD" | tail -1 | jq -r '.auth')
  assert_eq "$resp_auth" "Bearer k-cloud" "codex 请求带 provider key 打到 /v1/responses"
  [ "$(n_cloud)" -gt "$before" ] && ok "codex 请求落到了注入的端点" || bad "codex 未请求注入端点"
else
  echo "  skip: 本机无 codex 二进制"
fi

if command -v opencode >/dev/null; then
  out=$("$XFADE_BIN" opencode probe-oc -- run 'say hi' 2>&1) && rc=0 || rc=$?
  assert_eq "$rc" 0 "xfade opencode instant 退出码 0"
  printf '%s' "$out" | grep -q 'sink-ok' && ok "opencode 收到 sink 流式回复" || bad "opencode 输出异常: $out"
  oc_auth=$(grep '/v1/chat/completions' "$LOG_CLOUD" | tail -1 | jq -r '.auth')
  assert_eq "$oc_auth" "Bearer k-cloud" "opencode 请求带 provider key 打到 /v1/chat/completions"
else
  echo "  skip: 本机无 opencode 二进制"
fi

# ---------- 9. 真实上游双链路（可选） ----------
if [ -n "${XFADE_TEST_BASEURL:-}" ] && [ -n "${XFADE_TEST_KEY:-}" ]; then
  sect "9. 真实上游双链路对比 ($XFADE_TEST_BASEURL)"
  MODEL="${XFADE_TEST_MODEL:?真实链路需要 XFADE_TEST_MODEL}"
  REAL_BODY="{\"model\":\"$MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"say hi in one word\"}]}"

  direct_st=$(curl -s -o "$WORK/direct.json" -w '%{http_code}' \
    -H 'Content-Type: application/json' -H "Authorization: Bearer $XFADE_TEST_KEY" \
    -d "$REAL_BODY" "$XFADE_TEST_BASEURL/v1/chat/completions")
  assert_eq "$direct_st" 200 "直连上游 200"

  "$XFADE_BIN" add realup --tool codex --base-url "$XFADE_TEST_BASEURL" --key "$XFADE_TEST_KEY" >/dev/null
  "$XFADE_BIN" proxy use realup >/dev/null
  proxied_st=$(post "$REAL_BODY")
  cp "$WORK/resp.json" "$WORK/proxied.json"
  assert_eq "$proxied_st" 200 "过代理 200"

  d_content=$(jq -r '.choices[0].message.content // empty' "$WORK/direct.json")
  p_content=$(jq -r '.choices[0].message.content // empty' "$WORK/proxied.json")
  [ -n "$d_content" ] && ok "直连有 content: ${d_content:0:40}" || bad "直连无 content"
  [ -n "$p_content" ] && ok "代理有 content: ${p_content:0:40}" || bad "代理无 content"
  d_fin=$(jq -r '.choices[0].finish_reason // empty' "$WORK/direct.json")
  p_fin=$(jq -r '.choices[0].finish_reason // empty' "$WORK/proxied.json")
  assert_eq "$p_fin" "$d_fin" "finish_reason 一致 ($d_fin)"
else
  sect "9. 真实上游双链路：跳过（设 XFADE_TEST_BASEURL/XFADE_TEST_KEY/XFADE_TEST_MODEL 启用）"
fi

# ---------- 汇总 ----------
sect "汇总"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
