# Agent Switch 本地代理模式（v0.2）设计文档

日期：2026-07-26
状态：已确认
前置：v0.1（core + CLI，MVP）已完成并实战验证

## 1. 背景与目标

v0.1 的配置切换模式有一个根本痛点：切换后需重启工具（除 Claude Code 热加载外）。v0.2 引入本地代理：各工具一次性指向 `http://127.0.0.1:24860`，之后切换在代理层即时生效，无需重启。同时获得 failover 与用量统计两个新能力。

### 核心决策（已与用户确认）

| 决策点 | 结论 |
|--------|------|
| 协议范围 | OpenAI chat/completions + responses + Anthropic messages，三端点**透传**；互转归 v0.3 |
| 运行方式 | `asw serve` 前台子命令 |
| 路由模型 | 独立路由命令 `asw proxy use`，db 记 proxy_state，与工具 active 无关 |
| Failover | v0.2 简化版：主+有序 fallback，429/5xx/连接错误切换，主连续失败 3 次熔断 60s |
| 统计 | 请求日志 + token 统计写 SQLite，`asw stats` 聚合 |

## 2. 架构

```
codex / claude / opencode
   │  配置一次指向 http://127.0.0.1:24860（presets 内置 local-proxy 条目）
   ▼
asw serve（前台进程，axum）
   │  每个请求：① 读 db proxy_state 得路由（主+fallback 列表）
   │           ② keyring 取真实 key 替换 Authorization
   │           ③ 转发到 provider.base_url（SSE 流式透传）
   │           ④ 失败(连接错/429/5xx)按序切 fallback，主连续失败 3 次熔断 60s
   │           ⑤ 提取 usage 写 request_logs
   ▼
真实上游（yy 中转 / 官方 / 任何 provider）
```

### 端点（统一 `/v1` 前缀，覆盖三客户端路径）

| 端点 | 转发到 | 服务 |
|------|--------|------|
| `POST /v1/chat/completions` | `{base}/chat/completions` | OpenAI 系 |
| `POST /v1/responses` | `{base}/responses` | Codex（新版） |
| `POST /v1/messages` | `{base}/messages` | Claude Code |
| `GET /v1/models` | `{base}/models` | 模型列表 |
| `GET /health` | —（本地 200） | 探活 |

三客户端接入路径兼容性：OpenAI SDK 请求 `{base}/chat/completions`（base 含 /v1）；Anthropic SDK 请求 `{base}/v1/messages`（base 不含 /v1）。统一 /v1 前缀后三者均命中。

### 转发原则

- body 与响应原样透传，不解析业务字段；仅替换 `Authorization: Bearer <keyring 取出的真实 key>`
- SSE 流式透传（bytes stream）；统计所需的 usage 提取见 §5
- 上游错误（状态码+body）原样回传客户端；全部路由失败 → 502 + 最后一次错误

## 3. 热切换与路由

- 每次请求现查 SQLite `proxy_state`（亚毫秒，无缓存一致性问题）
- `asw proxy use zz` 后下一个请求即走 zz，代理与工具均无需重启
- 路由与工具配置的 active provider 完全独立（proxy_state 是自己的表）

### proxy_state 与熔断

- `proxy_state.routes`：JSON 数组 `["yy","backup1"]`，首项为主，其余按序 failover
- 熔断：内存状态（进程级）。主 provider 连续失败 3 次 → 冷却 60s，期间直接走 fallback；冷却结束回切主。`proxy status` 可见熔断状态
- 失败定义：连接错误 / 上游 429 / 上游 5xx。4xx（非 429）视为客户端错误，原样回传不触发 failover

## 4. 数据模型

```sql
CREATE TABLE IF NOT EXISTS proxy_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    routes TEXT NOT NULL,           -- JSON 数组，首项为主 provider
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS request_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL,               -- RFC3339
    endpoint TEXT NOT NULL,         -- chat | responses | messages
    model TEXT,                     -- 从请求体提取（透传不改写）
    provider_id TEXT NOT NULL,      -- 实际命中的 provider
    status INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL,
    error TEXT                      -- 失败时的摘要
);
```

迁移并入现有 `Database::migrate`（CREATE TABLE IF NOT EXISTS，向后兼容）。

## 5. 用量统计的 usage 提取

| 场景 | 提取方式 |
|------|---------|
| 非流式（三种端点） | 响应 JSON 的 `usage`（OpenAI: prompt_tokens/completion_tokens；Anthropic: input_tokens/output_tokens，统一映射） |
| chat/completions 流式 | 转发请求前注入 `"stream_options": {"include_usage": true}`（若客户端未带），从末尾 chunk 提取 |
| responses 流式 | `response.completed` 事件的 usage |
| messages 流式 | `message_delta` 事件的 usage |

提取不到记 0，不影响转发主流程。model 从请求体 `model` 字段提取（仅记录，不改写）。

## 6. CLI 设计

```bash
asw serve [--port 24860] [--host 127.0.0.1] [--auth-token <t>]
# 前台运行；每请求一行日志：ts endpoint model provider status tokens ms
# --auth-token 设置后校验入站 Authorization: Bearer <t>，转发时替换为真实 key

asw proxy use <name> [fallback1] [fallback2]   # 设置路由（热生效）
asw proxy status                               # 当前路由 + 各 provider 熔断状态
asw proxy clear                                # 清除路由（serve 时 503）

asw stats [--since 7d] [--by provider|model]   # 聚合：请求数/tokens/错误率/平均耗时
```

presets 每工具新增 `local-proxy` 条目：codex/opencode 为 `http://127.0.0.1:24860/v1`，claude 为 `http://127.0.0.1:24860`。用户 `asw use local-proxy --tool <t>` 一次接入（该条目 key 任意值即可，代理会替换）。

## 7. 技术要点

- **async 边界**：core 新增 `proxy` 模块是唯一 async 部分（tokio / axum / reqwest(rustls, stream) / futures）。core 其余保持同步；tokio runtime 由 CLI `serve` 命令创建（`#[tokio::main]` 仅用于该子命令路径）。reqwest 用 rustls 避免 OpenSSL 系统依赖
- **代理自身的 SecretStore 与 db 复用**：serve 启动时经 `build_core()` 同一环境契约拿 Core（db + secrets）
- **请求体大小**：默认上限 32MB（axum DefaultBodyLimit 调整），SSE 不缓冲
- **测试**：axum 自建 mock 上游（复用 axum 依赖，不加 wiremock）。覆盖：三端点透传 / Authorization 替换 / SSE 逐字节一致 / 热切换（请求间改 proxy_state）/ failover 切换与回切 / 熔断开闭 / 统计落库（含流式 usage）/ auth-token 校验

## 8. 错误处理

- 新增 `CoreError::Proxy(String)`（启动失败、路由缺失等）
- 路由未设置：503 + 提示 `asw proxy use`
- 上游全部失败：502 + 最后错误体
- 请求日志写库失败仅记录 stderr，不影响转发

## 9. 明确不做（v0.2 边界）

- Anthropic ↔ OpenAI 协议互转（v0.3）
- 后台守护 / launchd / systemd 开机自启（后续小迭代）
- 按模型路由（v0.2 路由维度仅为 provider）
- 代理侧改写 model / 负载均衡（随机/加权）
- Web 管理界面、请求重放、云同步、MCP 管理
