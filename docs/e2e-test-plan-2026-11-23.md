# xfade 端到端全面测试方案（E2E Master Plan，2026-11-23）

目标：对 xfade 的完整功能面做分层、可复现、可量化的端到端验收。分**离线确定性矩阵**（本地 mock sink，不依赖外部服务）与**真实网关链路**（freebuff-proxy / OmniRoute）两部分，另含真实工具端到端。

术语：`PROXY=http://127.0.0.1:24860`，`XF=$HOME/.local/bin/xfade`（或 brew 的 `/opt/homebrew/bin/xfade`），`DB=$HOME/.config/xfade/xfade.db`。

---

## 0. 测试分层总览

| 层 | 主题 | 依赖 | 自动化 |
|---|---|---|---|
| T0 | 环境准备 / 隔离 | 无 | `scripts/e2e-test.sh`（隔离 `XFADE_DATA_DIR`） |
| T1 | 网关直连 | 真实 freebuff / omni | `scripts/gateway-upstream-test.sh` |
| T2 | Provider 管理 | 无（mock） | e2e 脚本 §2 |
| T3 | serve 端点矩阵 | mock | e2e 脚本 §3 |
| T4 | 协议转换（双向） | mock | e2e 脚本 §4 |
| T5 | 路由 / 故障转移 / 熔断 / 模型路由 | mock + 真实 | e2e 脚本 §5 |
| T6 | 会话 / 统计 | mock + 真实 | e2e 脚本 §6 |
| T7 | 真实工具端到端 | claude/codex/opencode/pi/aider | 手工 + 条件自动 |
| T8 | daemon 生命周期 | 无 | 手工为主 |
| T9 | 安全（auth/egress/loopback） | mock | e2e 脚本 §9 |
| T10 | 非功能 / 健壮性 | mock | e2e 脚本 §10 |

隔离约定（离线矩阵必须遵守，避免污染用户真实配置）：
```bash
export XFADE_DATA_DIR="$(mktemp -d)/data"
export XFADE_SECRETS=file        # 无头友好，不碰 macOS 钥匙串
```
真实链路（T1/T7 真工具）才使用 `$HOME/.config/xfade`。

---

## T0 环境准备

| 检查 | 命令 | 期望 |
|---|---|---|
| 二进制 | `xfade --version` | 0.12.3 |
| 依赖 | `command -v jq python3 curl` | 三者齐全 |
| daemon 存活 | `curl -s $PROXY/__xfade/status` | `running:true` |
| 网关容器 | `docker ps --filter name=omniroute --filter name=freebuff-proxy` | healthy ×2（+redis） |
| 构建（若自测） | `cargo build --release -p xfade` | 成功 |

---

## T1 网关直连（真实上游，逐项断言 http code / 耗时 / 关键字段）

已实测结果（详见 `docs/gateway-upstream-testing-2026-11-23.md`）：

| # | 断言 | freebuff 期望 | omni 期望 |
|---|---|---|---|
| health | `/health` / `/api/health` | 200 | 200 |
| models | `GET /v1/models` + Bearer | 200，12 模型 | 200，522 模型 |
| chat 非流式 | `POST /v1/chat/completions` | 200（⚠️ 返回 SSE 格式） | 200，JSON，content 非空 |
| chat 流式 | `stream:true` | 200，≥1 `data:` 行 | 200，SSE+keepalive |
| messages | `POST /v1/messages` | 不支持（仅 OpenAI） | 200，Anthropic 形态 |
| 错 key | 401 | 401 | ⚠️ 400 |
| 假模型 | 400 | 400（本地目录校验） | — |

**关键差异记录**（供后续 failover 判断）：
1. freebuff `stream:false` 也返回 SSE chunk —— 严格 OpenAI 客户端在兜底链路会解析失败
2. omni 错 key 返回 400 而非 401 —— 影响 failover-4xx 语义
3. omni 单凭据 429 冷却指数递增（40s→80s→160s）—— 决定 failover 兜底价值

---

## T2 Provider 管理（离线 mock）

每条断言隔离环境内执行，验证 SQLite `providers` 表与 `secrets.json`：

```bash
# 添加 + 列表
xfade add p1 --tool claude-code --base-url http://127.0.0.1:PORT --key sk-p1
xfade add p2 --tool claude-code --base-url http://127.0.0.1:PORT --key sk-p2 --set '{"api":"chat"}'
xfade ls --tool claude-code
```

| # | 断言 | 命令 | 期望 |
|---|---|---|---|
| 2.1 | add 落库 | `sqlite3 $DB "SELECT id,tool FROM providers"` | p1/p2 存在 |
| 2.2 | key 入 secrets 而非 providers | `sqlite3 $DB "SELECT key_ref"` + 查 secrets | key 在 secrets.json，`key_ref` 指向它 |
| 2.3 | use 切换 active | `xfade use p1 --tool claude-code && xfade current` | claude: p1 |
| 2.4 | ⚠️ use 的 tool_routes 副作用 | `sqlite3 $DB "SELECT routes FROM tool_routes WHERE tool='claude'"` | 记录为 `["p1"]`（自引用坑，见 §11） |
| 2.5 | edit 改 base_url/model | `xfade edit p1 --tool claude-code --base-url ... --set '{"model":"m"}'` | 落库生效 |
| 2.6 | rm 删除 | `xfade rm p2 --tool claude-code && xfade ls` | p2 消失 |
| 2.7 | presets 可枚举 | `xfade presets` | 输出内置 preset |
| 2.8 | import 从工具配置导入 | `xfade import --tool codex` | 无报错，导入现有配置 |
| 2.9 | backup ls / restore | `xfade backup ls --tool claude-code` 后 restore | 往返一致 |

---

## T3 serve 端点矩阵（离线 mock）

端点覆盖（api.md §Proxy endpoints）：

| # | 端点 | 断言 |
|---|---|---|
| 3.1 | `GET /health` | 200，无鉴权 |
| 3.2 | `GET /__xfade/status` | 200，含 routes/target_protocol/models_merge/circuits |
| 3.3 | `POST /v1/chat/completions` 非流式 | 200，JSON |
| 3.4 | `POST /v1/chat/completions` 流式 | 200，SSE，`[DONE]` 结束 |
| 3.5 | `POST /v1/messages` | 200（转换后回 Anthropic 形态） |
| 3.6 | `POST /v1/responses` | 200（responses 事件流 / 或 JSON） |
| 3.7 | `GET /v1/models`（merge off） | 首个成功上游目录透传 |
| 3.8 | `GET /v1/models`（merge on） | 多路由聚合，重名 id 加 `<route>/` 前缀 |
| 3.9 | 未知 `/v1/*` 路径 | 透传或 404（行为记录） |

---

## T4 协议转换（双向，mock 关键）

xfade 核心价值是双向 Anthropic↔OpenAI 转换，必须覆盖两个方向 + responses：

| # | 客户端协议 | 上游协议 | 断言 |
|---|---|---|---|
| 4.1 | `/v1/messages`（Anthropic） | chat（OpenAI） | 200；返回体为 Anthropic `type:message`，`content[].text` 非空 |
| 4.2 | `/v1/chat/completions`（OpenAI） | messages（Anthropic 上游，`target messages`） | 200；返回体为 OpenAI `choices[].message.content` |
| 4.3 | `/v1/responses`（Codex） | chat | 200；`object:response`，`output[].content[].text` 非空 |
| 4.4 | 错误体转换 | 上游返回 OpenAI 错误 | 客户端拿到的错误按 inbound 协议转码（`transcode_error_body`） |
| 4.5 | system/工具消息映射 | 带 tools + system 的请求 | 字段映射不丢（tools→tools，system→system） |

验证点：`target_protocol` 决定上游会话协议，`inbound_proto` 由端点路径决定（forward.rs `endpoint_to_inbound_protocol`）。

---

## T5 路由 / 故障转移 / 熔断 / 模型路由（mock + 真实）

| # | 场景 | 注入方式 | 断言 |
|---|---|---|---|
| 5.1 | 静态顺序（smart off） | 双路由 a,b | a 优先被调用 |
| 5.2 | smart on 轻/重意图 | prompt 分类 | 轻意图 local、重意图 cloud |
| 5.3 | 开关即刻生效 | 运行中 `proxy smart on/off` | 不重启 daemon 立即变路由 |
| 5.4 | transport 错误 failover | a 指向死端口 | 落到 b；`X-Xfade-Failover-Path` 含两个 provider |
| 5.5 | 429/5xx 始终 failover | a 返回 429/500 | 落到 b |
| 5.6 | failover-4xx off：401 透传 | a 返回 401 | 不 failover，客户端 401 |
| 5.7 | failover-4xx on：401/403/model-400 触发 | 同上 | 落到 b |
| 5.8 | 熔断器 trip | a 连续失败 N 次 | `circuit_state` fails≥阈值 → 跳过 a → 直接 b |
| 5.9 | 熔断器恢复 | 等冷却过 + 半开探测成功 | 回 Closed，a 恢复参与 |
| 5.10 | `--route-model` 按 provider 覆写 | 两上游模型目录不相交 | 各收到自己 pin 的 model |
| 5.11 | models-merge 后 POST pin 到 owner | 先 `GET /v1/models` 再 POST 某 id | 命中所属 provider，上游 model 用真实 id（去掉 `<route>/` 前缀） |
| 5.12 | 单路由 429 错误体 + Retry-After 透传 | a 429 + retry-after:17 | 客户端 429、错误体保留、头透传 |
| 5.13 | 真实 failover（omni→freebuff） | omni 指向不可达局域网地址 | freebuff 200，`X-Xfade-Failover-Path:[{omni:502},{freebuff:200}]`（已实测 ✅） |

> 5.4/5.13 死端口注入注意：**不要用 `127.0.0.1:<空端口>`**（OrbStack 会劫持挂死 30s→ttft_timeout），用不可达局域网地址如 `10.72.1.17:8787`。

---

## T6 会话 / 统计

| # | 断言 | 命令/检查 |
|---|---|---|
| 6.1 | 会话 id 注入 | 响应头 `X-Xfade-Session-Id` 非空 |
| 6.2 | 同一会话复用 | 30min 内连续请求 → 同 session id |
| 6.3 | sessions 列表 | `xfade sessions` 显示 tool/provider/model/tokens/status |
| 6.4 | 会话时间线（模型切换回填） | `xfade sessions show <id>` 含 model-switch timeline |
| 6.5 | stats 聚合 | `xfade stats` 按 provider 分组 requests/tokens/cost/errors/avg_ms |
| 6.6 | stats --by session | 每会话 token 聚合 |
| 6.7 | request_logs 落账 | `sqlite3 $DB "SELECT provider_id,status,duration_ms FROM request_logs ORDER BY id DESC LIMIT 5"` |

---

## T7 真实工具端到端（条件执行：本机有对应二进制）

| 工具 | 命令 | 断言 | 状态 |
|---|---|---|---|
| claude | `xfade run local-proxy --tool claude -- -p "reply with exactly: ok"` | 输出 "ok" | ✅ 已实测 |
| codex | `xfade run local-proxy --tool codex -- exec --skip-git-repo-check "reply with exactly: ok"` | 输出 "ok" | ✅ 已实测 |
| codex（注入层，mock sink） | `xfade codex probe-cx -- exec 'say hi'` | sink 流式回复，key 打到 `/v1/responses` | 走 v2-test §8 |
| opencode | `xfade opencode probe-oc -- run 'say hi'` | sink 流式回复，key 打到 `/v1/chat/completions` | 条件 |
| pi | `xfade pi nonexistent` | 拒绝并指引 `use` | 走 v2-test §8 |

**真实链路端到端前置**（已修，见 gateway 文档）：
- claude/codex 的 `tool_routes` 必须指真实上游 `["omni","freebuff"]`，不能是自引用 `["local-proxy"]`
- codex 需 `xfade use local-proxy --tool codex` 重建 client token 并同步 provider secret

---

## T8 daemon 生命周期

| # | 断言 | 命令 |
|---|---|---|
| 8.1 | foreground 启动 | `xfade serve --port PORT` → `/health` 200 |
| 8.2 | macOS daemon 安装 | `xfade serve install` → `xfade serve status` running |
| 8.3 | 停止 | `xfade serve stop` → `/health` 拒绝连接 |
| 8.4 | 崩溃重启 | kill 进程 → 观察 launchd/自启拉起（⚠️ 本机实测自动拉起不可靠，需手动 `xfade serve`） |
| 8.5 | 端口占用冲突 | 二次 `serve --port 同端口` → 报 bind 错误不崩溃 |

---

## T9 安全（mock）

| # | 场景 | 断言 |
|---|---|---|
| 9.1 | auth-token 开启后无 token | `--auth-token T` 启动 → 无 `Authorization` 请求 → 401 |
| 9.2 | auth-token 校验 | 带正确 Bearer → 通过；错误 → 401 |
| 9.3 | `/health` 免鉴权 | 开启 auth 后 `/health` 仍 200 |
| 9.4 | 非 loopback Host → 403（DNS-rebinding） | `curl -H 'Host: evil.com'` | 403 |
| 9.5 | 非 loopback Origin → 403 | `curl -H 'Origin: https://evil.com'` | 403 |
| 9.6 | egress 域名白名单 | provider base_url 指向不在白名单的域 | 403 `xfade_egress_blocked`，且触发 failover |
| 9.7 | 非 loopback `--host` 必须带 token | `serve --host 0.0.0.0` 无 token | 拒绝/要求 token |

---

## T10 非功能 / 健壮性（mock）

| # | 场景 | 断言 |
|---|---|---|
| 10.1 | 上游返回 HTML 200 | 代理转 502（`html_200_from_upstream_returns_actionable_502`） |
| 10.2 | 上游 200 空 body | 代理转 502 |
| 10.3 | 流式尾缓冲 usage 提取 | SSE 最后 usage 帧 → request_logs 有 token 数 |
| 10.4 | 并发压测 | 并行 20 请求 → 无崩溃、计数一致 |
| 10.5 | 429 Retry-After≤2s 同上游重试 | 上游 429 + retry-after:1 → 同 provider 重试成功 |
| 10.6 | 请求体 rectification（400 thinking 参数） | 上游 400 thinking 错误 → 自动去参数重试 |

---

## 11. 已知坑 / 风险清单（测试判定依据）

1. **`xfade use` gateway 模式写自引用 tool_routes**（service.rs:386）→ tool 专属路由覆盖全局，请求回环。判定：e2e 真工具前必须检查 `tool_routes` 表。
2. **127.0.0.1 死端口被 OrbStack 劫持** → 故障注入用不可达局域网地址。
3. **client token 轮换后须同步 provider secret**，否则 run 模式 env 注入旧 key → 401。
4. **freebuff 非流式返 SSE** → 兜底链路严格客户端可能解析失败。
5. **omni 单凭据 429 冷却递增** → 主路由可用性依赖该账号额度。
6. daemon 自动拉起在本机不可靠（kill 后 `xfade run` 报 Connection refused），需手动 `xfade serve`。

---

## 12. 自动化映射与执行顺序

```bash
# 离线确定性矩阵（T0/T2/T3/T4/T5(5.1-5.12)/T6/T9/T10）：无外部依赖
scripts/e2e-test.sh            # 新增，扩展自 v2-test.sh 的 sink 范式

# 真实网关直连 + serve 转换 + 真实 failover（T1 + 部分 T4/T5）
scripts/gateway-upstream-test.sh   # 已有

# 真实工具端到端（T7）：手工或条件自动
xfade run local-proxy --tool claude -- -p "reply with exactly: ok"
xfade run local-proxy --tool codex -- exec --skip-git-repo-check "reply with exactly: ok"

# daemon/安全（T8/T9）按 §T8/T9 命令手工验证
```

执行顺序建议：`e2e-test.sh`（离线全绿）→ `gateway-upstream-test.sh`（真实网关）→ 真实工具手工 → 安全/daemon 手工。

---

## 13. 通过标准（Definition of Done）

- 离线矩阵 e2e-test.sh 全部 PASS（0 FAIL）
- 真实网关 gateway-upstream-test.sh 全部 PASS，failover-path 头含 `freebuff:200`
- claude + codex 真实工具返回 "ok"
- `xfade sessions` / `xfade stats` 有落账
- 安全项：auth/egress/loopback 断言全部命中
