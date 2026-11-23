#!/usr/bin/env bash
# e2e-test.sh — xfade 离线确定性 E2E 矩阵（本地 mock sink，零外部依赖）
#
# 覆盖（对应 docs/e2e-test-plan-2026-11-23.md）：
#   T3  serve 端点矩阵（health/status/chat 流式·非流式/messages/responses）
#   T4  协议转换双向（messages→chat、chat→messages target、responses→chat）
#   T5  路由/故障转移（静态序、429/500 failover、failover-4xx 开关、route-model 覆写、单路由 429 透传）
#   T6  会话（X-Xfade-Session-Id 注入 + 同会话复用）
#   T9  安全（auth-token、/health 免鉴权、非 loopback Host/Origin 403）
#   T10 健壮性（上游 HTML 200→502、空 200→502）
#
# 双 sink 隔离：provider s1→sinkA、s2→sinkB，各自独立 mode/log，故障注入互不串扰。
# 熔断阈值 3 次连续失败即 Open、冷却 60s —— 故障注入段之后用 reset() 清熔断再继续。
#
# 用法：scripts/e2e-test.sh
set -euo pipefail
cd "$(dirname "$0")/.."

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
sect() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3"; else bad "$3 (expected=$2 actual=$1)"; fi; }
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'; }

XFADE_BIN="${XFADE_BIN:-target/debug/xfade}"
[ -x "$XFADE_BIN" ] || cargo build -p xfade >&2
command -v jq >/dev/null || { echo "需要 jq"; exit 1; }

# ---------- 隔离环境 ----------
WORK=$(mktemp -d /tmp/xfade-e2e.XXXXXX)
export XFADE_DATA_DIR="$WORK/data"
export XFADE_SECRETS=file
PORT=$(free_port); PORT_AUTH=$(free_port); PORT_A=$(free_port); PORT_B=$(free_port)
PIDS=(); DAEMON_PID=""
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; [ "$FAIL" -eq 0 ] && rm -rf "$WORK" || echo "工作目录保留: $WORK"; }
trap cleanup EXIT

# ---------- 测试 sink（按路径形态应答；mode 文件控制 200/401/403/429/500/html/empty） ----------
cat > "$WORK/sink.py" <<'PY'
import json, time, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port, log, modef = int(sys.argv[1]), sys.argv[2], sys.argv[3]

def chat_chunks(model):
    return [
        {"id":"c1","object":"chat.completion.chunk","created":int(time.time()),"model":model,
         "choices":[{"index":0,"delta":{"role":"assistant","content":"sink-ok"},"finish_reason":None}]},
        {"id":"c1","object":"chat.completion.chunk","created":int(time.time()),"model":model,
         "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
         "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}},
    ]

class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n).decode("utf-8","replace")
        open(log,"a").write(json.dumps({"path":self.path,"auth":self.headers.get("authorization",""),"body":body})+"\n")
        try: mode = open(modef).read().strip()
        except FileNotFoundError: mode = "200"
        if mode == "html":
            p=b"<html>oops</html>"; self.send_response(200); self.send_header("content-type","text/html")
            self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p); return
        if mode == "empty":
            self.send_response(200); self.send_header("content-length","0"); self.end_headers(); return
        if mode != "200":
            p=json.dumps({"error":{"message":f"sink {mode}"}}).encode()
            self.send_response(int(mode)); self.send_header("content-type","application/json")
            if mode=="429": self.send_header("retry-after","17")
            self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p); return
        try: req=json.loads(body)
        except Exception: req={}
        model=req.get("model","sink-model")
        if "/messages" in self.path:
            p=json.dumps({"id":"msg-1","type":"message","role":"assistant","model":model,
                "content":[{"type":"text","text":"sink-ok"}],"stop_reason":"end_turn",
                "usage":{"input_tokens":1,"output_tokens":1}}).encode()
            self.send_response(200); self.send_header("content-type","application/json")
            self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p); return
        if "/responses" in self.path:
            rid="resp-1"; item={"id":"msg-1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"sink-ok"}]}
            p=b"".join(("event: %s\ndata: %s\n\n"%(ev,json.dumps(d))).encode() for ev,d in [
                ("response.created",{"type":"response.created","response":{"id":rid,"object":"response","status":"in_progress","model":model,"output":[]}}),
                ("response.output_item.done",{"type":"response.output_item.done","output_index":0,"item":item}),
                ("response.completed",{"type":"response.completed","response":{"id":rid,"object":"response","status":"completed","model":model,"output":[item]}}),
            ])
            self.send_response(200); self.send_header("content-type","text/event-stream")
            self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p); return
        if req.get("stream"):
            p=b"".join(b"data: "+json.dumps(c).encode()+b"\n\n" for c in chat_chunks(model))+b"data: [DONE]\n\n"
            self.send_response(200); self.send_header("content-type","text/event-stream")
            self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p); return
        p=json.dumps({"id":"sink-1","object":"chat.completion",
            "choices":[{"index":0,"message":{"role":"assistant","content":"sink-ok"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":1,"completion_tokens":1}}).encode()
        self.send_response(200); self.send_header("content-type","application/json")
        self.send_header("content-length",str(len(p))); self.end_headers(); self.wfile.write(p)

HTTPServer(("127.0.0.1",port),H).serve_forever()
PY

LOGA="$WORK/a.log"; LOGB="$WORK/b.log"; MODEA="$WORK/a.mode"; MODEB="$WORK/b.mode"
echo 200 > "$MODEA"; echo 200 > "$MODEB"; : > "$LOGA"; : > "$LOGB"
python3 "$WORK/sink.py" "$PORT_A" "$LOGA" "$MODEA" >"$WORK/sinkA.out" 2>&1 & PIDS+=($!)
python3 "$WORK/sink.py" "$PORT_B" "$LOGB" "$MODEB" >"$WORK/sinkB.out" 2>&1 & PIDS+=($!)
for p in "$PORT_A" "$PORT_B"; do for i in $(seq 1 50); do python3 -c "import socket;s=socket.create_connection(('127.0.0.1',$p),0.2);s.close()" 2>/dev/null && break; sleep 0.1; done; done

n_a() { wc -l < "$LOGA" | tr -d ' '; }
n_b() { wc -l < "$LOGB" | tr -d ' '; }
last_a_model() { tail -1 "$LOGA" | jq -r '.body|fromjson|.model//""'; }
last_b_model() { tail -1 "$LOGB" | jq -r '.body|fromjson|.model//""'; }

# 重启 daemon 并清熔断（failure_threshold=3，故障注入段之后必须清，否则污染后续断言）
reset_circuits() {
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
  sqlite3 "$XFADE_DATA_DIR/xfade.db" "DELETE FROM circuit_state;" 2>/dev/null || true
  "$XFADE_BIN" serve --port "$PORT" >"$WORK/serve.log" 2>&1 & DAEMON_PID=$!
  for i in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break; sleep 0.1; done
}

post() { # post <path> <body> → http status（body 存 $WORK/resp.json）
  local path="$1" body="$2"; shift 2
  curl -s -o "$WORK/resp.json" -w '%{http_code}' -H 'Content-Type: application/json' \
    -H 'Authorization: Bearer client-tok' -d "$body" "http://127.0.0.1:$PORT$path"
}
LIGHT='{"model":"client-model","messages":[{"role":"user","content":"hi"}]}'

# ---------- 准备 ----------
sect "T0 准备（隔离 env, sinkA=$PORT_A sinkB=$PORT_B proxy=${PORT}）"
"$XFADE_BIN" add s1 --tool codex --base-url "http://127.0.0.1:$PORT_A" --key k-a >/dev/null
"$XFADE_BIN" add s2 --tool codex --base-url "http://127.0.0.1:$PORT_B" --key k-b >/dev/null
"$XFADE_BIN" proxy use s1 s2 --target chat >/dev/null
"$XFADE_BIN" serve --port "$PORT" >"$WORK/serve.log" 2>&1 & DAEMON_PID=$!
for i in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && break; sleep 0.1; done
curl -sf "http://127.0.0.1:$PORT/health" >/dev/null || { echo "daemon 未就绪"; cat "$WORK/serve.log"; exit 1; }
ok "daemon 就绪"

# ---------- T3 端点矩阵 ----------
sect "T3 serve 端点矩阵"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/health)" 200 "GET /health 200"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/__xfade/status)" 200 "GET /__xfade/status 200"
assert_eq "$(post /v1/chat/completions "$LIGHT")" 200 "chat 非流式 200"
assert_eq "$(jq -r '.choices[0].message.content' "$WORK/resp.json")" "sink-ok" "chat 非流式 content"
assert_eq "$(post /v1/chat/completions '{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true}')" 200 "chat 流式 200"
assert_eq "$(post /v1/messages '{"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}')" 200 "/v1/messages 200"
assert_eq "$(post /v1/responses '{"model":"m","input":"hi"}')" 200 "/v1/responses 200"

# ---------- T4 协议转换 ----------
sect "T4 协议转换"
st=$(post /v1/messages '{"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}')
assert_eq "$st" 200 "messages 客户端→chat 上游 200"
assert_eq "$(jq -r '.type' "$WORK/resp.json")" "message" "转回 Anthropic type=message"
assert_eq "$(jq -r '.content[0].text' "$WORK/resp.json")" "sink-ok" "Anthropic content.text 透出"
st=$(post /v1/responses '{"model":"m","input":"hi"}')
assert_eq "$st" 200 "responses 客户端→chat 上游 200"
assert_eq "$(jq -r '.object' "$WORK/resp.json")" "response" "转回 response 对象"
assert_eq "$(jq -r '.output[0].content[0].text' "$WORK/resp.json")" "sink-ok" "response 文本透出"
# chat 客户端 → anthropic 上游（target messages）
"$XFADE_BIN" proxy use s1 --target messages >/dev/null
st=$(post /v1/chat/completions "$LIGHT")
assert_eq "$st" 200 "chat 客户端→messages 上游 200"
assert_eq "$(tail -1 "$LOGA" | jq -r '.path')" "/v1/messages" "上游打到 /v1/messages"
assert_eq "$(jq -r '.choices[0].message.content' "$WORK/resp.json")" "sink-ok" "转回 OpenAI content"
"$XFADE_BIN" proxy use s1 s2 --target chat >/dev/null

# ---------- T5 路由/故障转移 ----------
sect "T5 故障转移（双 sink，s1→A s2→B）"
echo 500 > "$MODEA"; ba=$(n_a); bb=$(n_b)
assert_eq "$(post /v1/chat/completions "$LIGHT")" 200 "s1 500 → s2 兜底 200"
assert_eq "$(n_a)" "$((ba+1))" "s1 被尝试 1 次"
assert_eq "$(n_b)" "$((bb+1))" "s2 兜底 1 次"
echo 200 > "$MODEA"

echo 429 > "$MODEA"; ba=$(n_a); bb=$(n_b)
assert_eq "$(post /v1/chat/completions "$LIGHT")" 200 "s1 429 → 恒 failover 到 s2"
assert_eq "$(n_a)" "$((ba+1))" "429 走 failover（s1 1 次）"
assert_eq "$(n_b)" "$((bb+1))" "s2 兜底 1 次"
echo 200 > "$MODEA"

# failover-4xx off：401 透传不 failover
echo 401 > "$MODEA"; ba=$(n_a); bb=$(n_b)
assert_eq "$(post /v1/chat/completions "$LIGHT")" 401 "failover-4xx off：401 透传"
assert_eq "$(n_a)" "$((ba+1))" "401 不 failover（s1 仅 1 次）"
assert_eq "$(n_b)" "$bb" "s2 未被兜底"
echo 200 > "$MODEA"

# failover-4xx on：401 触发 failover（先清熔断，避免前序 500/429/401 把 s1 打开）
reset_circuits
"$XFADE_BIN" proxy failover-4xx on >/dev/null
echo 401 > "$MODEA"; ba=$(n_a); bb=$(n_b)
assert_eq "$(post /v1/chat/completions "$LIGHT")" 200 "failover-4xx on：401 → 兜底 200"
assert_eq "$(n_a)" "$((ba+1))" "401 触发 failover（s1 1 次）"
assert_eq "$(n_b)" "$((bb+1))" "s2 兜底 1 次"
echo 200 > "$MODEA"; "$XFADE_BIN" proxy failover-4xx off >/dev/null

# ---------- route-model 覆写（清熔断后） ----------
sect "T5b route-model 覆写"
reset_circuits
"$XFADE_BIN" proxy use s1 s2 --route-model s1=m-a --route-model s2=m-b --target chat >/dev/null
post /v1/chat/completions "$LIGHT" >/dev/null
assert_eq "$(last_a_model)" "m-a" "route-model 覆写 s1 的 model"
echo 500 > "$MODEA"
post /v1/chat/completions "$LIGHT" >/dev/null
assert_eq "$(last_b_model)" "m-b" "failover 后 s2 用自己的 route-model"
echo 200 > "$MODEA"
"$XFADE_BIN" proxy use s1 s2 --target chat >/dev/null

# ---------- 单路由 429 错误体 + Retry-After 透传 ----------
sect "T5c 单路由 429 透传"
reset_circuits
"$XFADE_BIN" proxy use s1 --target chat >/dev/null
echo 429 > "$MODEA"
st=$(curl -s -D "$WORK/h" -o "$WORK/resp.json" -w '%{http_code}' -H 'Content-Type: application/json' -d "$LIGHT" "http://127.0.0.1:$PORT/v1/chat/completions")
assert_eq "$st" 429 "单路由 429 客户端收到 429"
grep -q 'sink 429' "$WORK/resp.json" && ok "错误体保留" || bad "错误体丢失"
grep -qi '^retry-after: 17' "$WORK/h" && ok "Retry-After 透传" || bad "Retry-After 缺失"
echo 200 > "$MODEA"; "$XFADE_BIN" proxy use s1 s2 --target chat >/dev/null

# ---------- T6 会话 ----------
sect "T6 会话"
reset_circuits
curl -s -D "$WORK/h1" -o /dev/null -H 'Content-Type: application/json' -d "$LIGHT" "http://127.0.0.1:$PORT/v1/chat/completions"
SID1=$(grep -i '^x-xfade-session-id' "$WORK/h1" | tr -d '\r' | awk '{print $2}')
[ -n "$SID1" ] && ok "响应头 X-Xfade-Session-Id 非空 ($SID1)" || bad "缺 session id"
curl -s -D "$WORK/h2" -o /dev/null -H 'Content-Type: application/json' -d "$LIGHT" "http://127.0.0.1:$PORT/v1/chat/completions"
SID2=$(grep -i '^x-xfade-session-id' "$WORK/h2" | tr -d '\r' | awk '{print $2}')
assert_eq "$SID2" "$SID1" "30min 内连续请求同会话复用"

# ---------- T9 安全 ----------
sect "T9 安全"
"$XFADE_BIN" serve --port "$PORT_AUTH" --auth-token secret-tok >"$WORK/serve-auth.log" 2>&1 & AUTH_PID=$!; PIDS+=($AUTH_PID)
for i in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT_AUTH/health" >/dev/null 2>&1 && break; sleep 0.1; done
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT_AUTH/health)" 200 "auth 下 /health 仍免鉴权"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT_AUTH/__xfade/status)" 401 "auth 下 status 无 token 401"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -H 'Authorization: Bearer secret-tok' http://127.0.0.1:$PORT_AUTH/__xfade/status)" 200 "auth 下正确 token 200"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -H 'Host: evil.com' http://127.0.0.1:$PORT/__xfade/status)" 403 "非 loopback Host → 403"
assert_eq "$(curl -s -o /dev/null -w '%{http_code}' -H 'Origin: https://evil.com' http://127.0.0.1:$PORT/__xfade/status)" 403 "非 loopback Origin → 403"

# ---------- T10 健壮性 ----------
sect "T10 健壮性"
reset_circuits
"$XFADE_BIN" proxy use s1 --target chat >/dev/null
echo html > "$MODEA"
assert_eq "$(post /v1/chat/completions "$LIGHT")" 502 "上游 HTML 200 → 502"
echo empty > "$MODEA"
assert_eq "$(post /v1/chat/completions "$LIGHT")" 502 "上游空 200 → 502"
echo 200 > "$MODEA"

sect "汇总"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
