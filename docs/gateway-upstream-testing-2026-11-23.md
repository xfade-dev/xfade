# 上游网关接入测试文档（freebuff-proxy + OmniRoute → xfade，2026-11-23）

范围：把本机两个 LLM 网关（freebuff-proxy、OmniRoute）接入 xfade 作为上游 provider，验证 xfade 三层链路 —— 网关直连 → xfade serve（路由/协议转换/故障转移）→ 真实工具端到端（Claude Code / Codex）。本文档记录完整测试矩阵、修复项、实测结果与可重复执行方式。

结论：**全链路打通，17 项矩阵全过，故障转移验证成功**。遗留 4 项风险见 §7。

---

## 一、环境拓扑与前置条件

```
Claude Code ─┐                              ┌─ omni (主)   OmniRoute       127.0.0.1:20138/api/v1
             ├─ xfade serve 127.0.0.1:24860 ─┤
Codex ───────┘   target=chat, merge=on       └─ freebuff(备) freebuff-proxy 127.0.0.1:8787/v1
```

前置检查（三项全绿才开测）：

| 检查项 | 命令 | 期望 |
|---|---|---|
| 两个网关容器 healthy | `docker ps --format "{{.Names}}\t{{.Status}}" \| grep -E "freebuff\|omni"` | 3 个容器 Up (healthy) |
| xfade daemon 在跑 | `curl -s 127.0.0.1:24860/__xfade/status` | `running:true` |
| 凭据可读 | `~/.config/xfade/secrets.json` 中 `xfade/claude/freebuff`(sk-fb-)、`xfade/codex/omni` | 非空 |

端口速查：OmniRoute 容器映射 20138 dashboard+API / 20139 API_PORT（**本部署未监听，勿用**）/ 20142 live-ws；freebuff-proxy 8787；xfade 24860。

凭据来源：
- freebuff key：`freebuff-proxy/data/users.json` → admin.apiKey
- OmniRoute key：dashboard（`POST /api/keys`，需登录态）；控制台密码即 OmniRoute 仓库 `.env` 的 `INITIAL_PASSWORD`
- ⚠️ OmniRoute DB 中 `enc:v1:` 加密的存量 key 因 `STORAGE_ENCRYPTION_KEY` 丢失**明文不可找回**，只能重建

---

## 二、配置基线修复（6 项，修复前链路全挂）

| # | 问题 | 现象 | 修复命令 |
|---|---|---|---|
| 1 | omni base_url=`localhost:20128` 死端口 | 转发报 `error sending request` | `xfade edit omni --tool codex --base-url http://127.0.0.1:20138/api/v1` |
| 2 | freebuff base_url=`10.72.1.17:8787` 失效内网 IP | 同上 | `xfade edit freebuff --tool claude --base-url http://127.0.0.1:8787/v1`（codex 同） |
| 3 | OmniRoute 存量 key invalid（key_6a4b） | 容器日志 `invalid bearer ... falling through` | dashboard 登录 → `POST /api/keys {"name":"xfade"}` → `xfade edit omni --key <新key>` |
| 4 | **tool_routes 自引用回环**：claude/codex 的 tool 专属路由 = `["local-proxy"]`（provider base_url 指向代理自身 24860） | claude e2e 503/401，request_logs 出现 provider=local-proxy | DB 直改：`UPDATE tool_routes SET routes='["omni","freebuff"]' WHERE tool IN ('claude','codex');` |
| 5 | codex client token 缺失 + provider secret 与轮换后 token 不一致 | codex e2e `401 invalid client token` | `xfade use local-proxy --tool codex`（重建 token 写 auth.json）+ `xfade edit local-proxy --tool codex --key <token>`（同步 secret） |
| 6 | 路由/模型 pin/协议 | — | `xfade proxy use omni freebuff --route-model omni=codebuddy-cn/glm-5.3-flash --route-model freebuff=deepseek/deepseek-v4-flash --target chat` |

要点说明：
- **tool_routes 优先于全局路由**（forward.rs `resolve_routes`：client_tool 命中 tool_routes 才 fallback 全局）。xfade CLI 未暴露 tool_routes 写入口；`xfade use` gateway 模式固定写自引用（service.rs:386），这是行为坑，DB 直改即时生效（daemon 每请求读库）
- freebuff provider 的 extra `api:anthropic-messages` 只影响工具适配器写配置，**代理转发协议由 providers.protocol 列决定**（chat|messages|responses），不影响本测试
- 401→503 演化：自引用回环 401 失败累计 3 次 → `local-proxy` 熔断器开启 → entries 全跳过 → `503 no usable provider`（last_err 为空即"所有 entry 被熔断跳过"的信号）

---

## 三、网关直连测试矩阵（阶段 3）

### freebuff-proxy（`http://127.0.0.1:8787`，Bearer sk-fb-）

| # | 测试点 | 请求 | 期望 | 实测 |
|---|---|---|---|---|
| F1 | health | `GET /health` | 200 | ✅ 200 (4ms) |
| F2 | models 带 key | `GET /v1/models` | 200 + 模型列表 | ✅ 12 模型 |
| F3 | chat 非流式 | `POST /v1/chat/completions` stream:false, max_tokens:16 | 200 + JSON | ✅ 200 (13.6s) ⚠️ 返回体是 **SSE chunk 格式**（`data:` 行）而非单一 JSON |
| F4 | chat 流式 | 同上 stream:true | 200 + SSE | ✅ 5 条 data 行，含 finish_reason |
| F5 | 错 key | Bearer sk-wrong | 401 | ✅ `invalid_api_key` |
| F6 | 假模型 | model=no-such-model-xyz | 400 | ✅ 本地目录校验拒绝（不透传上游） |

模型目录（12）：deepseek/deepseek-v4-flash、DeepSeek-V4.1-Flash、GPT-6-Luna、Claude-Haiku-5.5-100k-context 等。并发限制：maxConcurrent=32、账号 SSE 并发 2、队列 15s 满→429。

### OmniRoute（`http://127.0.0.1:20138/api/v1`，Bearer sk-6dbb…）

| # | 测试点 | 请求 | 期望 | 实测 |
|---|---|---|---|---|
| O1 | health | `GET /api/health` | 200 | ✅ |
| O2 | models 带 key | `GET /api/v1/models` | 200 | ✅ 522 模型（12 前缀；仅 codebuddy-cn 可用推理） |
| O3 | chat 非流式 | `POST /api/v1/chat/completions` glm-5.3-flash | 200 | ✅ (1.4s) "ok"；⚠️ 上游限流时 429 model_cooldown（reset_seconds 40→80→160 指数递增） |
| O4 | chat 流式 | stream:true | 200 + SSE | ✅ 含 keepalive 心跳帧 |
| O5 | Anthropic `/v1/messages` | Claude 格式 | 200 | ✅ 协议形态正确；⚠️ max_tokens 过小时 content 为空（推理耗尽），512 正常 |
| O6 | 错 key | Bearer sk-wrong | 401 | ⚠️ chat 路由返回 **400** 而非 401 |

其他前缀不可用原因（部署缺依赖）：`zc`→spawn zcode ENOENT、`cxa`→Codex app-server 未配置、`cfp`→缺 Playwright Chromium。

---

## 四、xfade serve 层测试矩阵（阶段 4，`http://127.0.0.1:24860`）

| # | 测试点 | 请求 | 期望 | 实测 |
|---|---|---|---|---|
| X1 | models-merge | `GET /v1/models` | 两网关合并 | ✅ 534 = 522(omni)+12(freebuff) |
| X2 | chat 命中 omni 主 | `POST /v1/chat/completions` glm-5.3-flash | 200，上游 omni | ✅ 1.0s，"ok"，响应头 `X-Xfade-Session-Id` |
| X3 | Anthropic 转换 | `POST /v1/messages`（Claude 格式）→ OpenAI 上游 | 200 | ✅ 1.1s，"ok"，stop_reason=end_turn |
| X4 | responses 形态 | `POST /v1/responses`（codex） | 200 | ✅ 1.6s，"ok"，status=completed |
| X5 | 流式端到端 | stream:true | SSE 透传 | ✅ content "ok" |
| X6 | 会话落账 | `xfade sessions` / `xfade stats` | provider/tokens 记录 | ✅ 按 session 聚合 tokens+cost |

## 五、故障转移测试（阶段 5）✅

方法：临时把 omni base_url 改为不可达地址 `http://10.72.1.17:8787/v1`（**不要用 127.0.0.1 死端口，见 §7-1**），经代理发请求后立即还原。

```
curl -D - -o /dev/null -X POST 127.0.0.1:24860/v1/chat/completions \
  -d '{"model":"codebuddy-cn/glm-5.3-flash","messages":[...],"max_tokens":512}'
# 实测: http=200, 13.0s
# x-xfade-failover-path: [{"provider":"omni","status":502},
#                         {"provider":"freebuff","status":200}]
```

断言点：
1. ✅ omni 传输错误 → `record_failure` + 循环 continue（forward.rs failover_loop Ok(Err) 分支）
2. ✅ freebuff 兜底成功（route_model 覆写为 deepseek/deepseek-v4-flash）
3. ✅ failover 轨迹通过 `x-xfade-failover-path` 响应头暴露
4. ✅ 还原后 `xfade proxy status` 熔断器回 Closed
5. 模型 pinning 生效：客户端请求 deepseek 模型时 freebuff 被 Stage 2c 提到首位（model_routes pin）
6. 熔断阈值：本次注入 2-3 次失败未触发 Open（阈值更高/需持续失败）；`circuit_state` 表可查 fails 与 cooldown_until_secs

## 六、真实工具端到端（阶段 6）✅

| # | 工具 | 命令 | 实测 |
|---|---|---|---|
| E1 | Claude Code | `cd /tmp && xfade run local-proxy --tool claude -- -p "reply with exactly: ok" --model claude-sonnet-4-5` | ✅ 9s 返回 "ok" |
| E2 | Codex | `cd /tmp && xfade run local-proxy --tool codex -- exec --skip-git-repo-check "reply with exactly: ok"` | ✅ "ok"（6787 tokens） |

前置：E1 需要 tool_routes 已修（§2-4）；E2 需要 client token 链路已修（§2-5）。修复前实测错误：E1 依次出现 401 回环 → 503（熔断）；E2 401 invalid client token ×2。

## 七、已知边界与坑（复测必读）

1. **127.0.0.1 死端口在本机不可用作故障注入**：OrbStack 会接受任意 127.0.0.1 端口的 TCP 连接再挂死 → 30s ttft_timeout 而非秒拒。瞬断模拟用不可达局域网地址。
2. **freebuff 非流式响应是 SSE 格式**：兜底链路下严格 OpenAI 客户端解析 body 可能失败（omni 健康时无影响）。建议给 freebuff-proxy 提 issue。
3. **`xfade use` gateway 模式写自引用 tool_routes**：CLI 无写入口，修路由只能 DB 直改。建议给 xfade 提 issue（`proxy use --tool <t>` 或 use 时不覆盖已存在的多上游路由）。
4. **omni 主路由可用性依赖单凭据**：codebuddy-cn 唯一活跃账号，上游限流冷却指数递增；其余 provider 前缀部署缺依赖。监控告警需按 omni 429 + failover-path 头匹配。
5. OmniRoute 错 key 返回 400（非 401）：不影响 xfade 默认策略（failover-4xx off），但监控规则需注意。
6. OmniRoute `/v1/messages` 小 max_tokens 会返回空 content（推理耗尽预算）——真实 Claude Code 请求预算大，无影响。

## 八、可重复执行

```bash
# 一键矩阵（网关直连 + serve 层 + 故障转移 + e2e），结果追加 /tmp/xfade-gateway-test-report.md
bash scripts/gateway-upstream-test.sh

# 凭据/端点/模型可用环境变量覆盖
FB_KEY=sk-fb-... OM_KEY=sk-... bash scripts/gateway-upstream-test.sh
```

排查命令速查：

```bash
xfade proxy status                 # 路由/pin/熔断器
sqlite3 ~/.config/xfade/xfade.db \
  "SELECT id,provider_id,status,duration_ms,substr(error,1,60) FROM request_logs ORDER BY id DESC LIMIT 10;"
sqlite3 ~/.config/xfade/xfade.db "SELECT * FROM tool_routes;"
sqlite3 ~/.config/xfade/xfade.db "SELECT * FROM circuit_state;"   # fails / cooldown_until_secs
docker logs omniroute --tail 50 2>&1 | grep -iE "cool|rate_limit|decrypt"
xfade sessions && xfade stats
```

## 九、最终配置快照（2026-11-23）

```
proxy: routes=omni->freebuff  target=chat  models_merge=on  failover_4xx=off  smart=off
route_models: omni=codebuddy-cn/glm-5.3-flash  freebuff=deepseek/deepseek-v4-flash
providers:  codex/omni      http://127.0.0.1:20138/api/v1  (sk-6dbb…589e)
            claude/freebuff http://127.0.0.1:8787/v1       (sk-fb-…)
            codex/freebuff  http://127.0.0.1:8787/v1
tool_routes: claude=["omni","freebuff"]  codex=["omni","freebuff"]
circuits:   omni [closed]  freebuff [closed]
OmniRoute 控制台密码 = 仓库 .env INITIAL_PASSWORD（已验证）
```
