# API 参考

## `GET /__asw/status`

查询代理 daemon 运行状态、路由配置与各 provider 熔断状态。

- **鉴权**：若 daemon 设置了 `auth_token`，需 `Authorization: Bearer <token>`；未设置则放行。`/health` 免鉴权。
- **响应** `200`：

```json
{
  "running": true,
  "host": "127.0.0.1",
  "port": 24860,
  "auth_enabled": true,
  "routes": ["yy", "bak"],
  "model_override": "gpt-5.6-luna",
  "target_protocol": "chat",
  "circuits": [
    { "provider_id": "yy", "fails": 2, "cooldown_remaining_secs": null },
    { "provider_id": "bak", "fails": 3, "cooldown_remaining_secs": 58 }
  ]
}
```

字段：
- `running`：恒 true（端点活着即运行）。
- `circuits[].cooldown_remaining_secs`：剩余冷却秒；`null` 表示未熔断或已过期。
- `routes`：主→备顺序；`model_override` 为 null 表示透传。

```bash
curl http://127.0.0.1:24860/__asw/status
curl -H "Authorization: Bearer <token>" http://127.0.0.1:24860/__asw/status
```

## 代理端点

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/health` | 存活探针（免鉴权），返回 `ok` |
| POST | `/v1/chat/completions` | OpenAI Chat 风格 |
| POST | `/v1/responses` | OpenAI Responses 风格 |
| POST | `/v1/messages` | Anthropic Messages 风格 |
| GET | `/v1/models` | 模型列表透传 |
| GET | `/__asw/status` | 管理端点（见上） |

`/v1/*` 经 `auth_guard`（若设 auth_token）+ 路由 failover + 协议互转（按 `target_protocol`）。流式响应 SSE 透传，尾部 8KB 缓冲提取 usage 记日志。

## CLI 命令参考

```
asw add <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
asw ls [--tool <t>]
asw use <id> --tool <t>
asw current --tool <t>
asw edit <id> --tool <t> [--base-url URL] [--key KEY] [--set 'json']
asw rm <id> --tool <t>
asw presets --tool <t>
asw import --tool <t>
asw backup ls --tool <t>
asw backup restore --tool <t> --path <file>
asw completion <shell>

asw serve [--port 24860] [--host 127.0.0.1] [--auth-token T]            # 前台
asw serve install [--port --host --auth-token]                          # macOS daemon 安装
asw serve uninstall | stop                                              # 卸载
asw serve status                                                        # 查询状态

asw proxy use --routes a,b [--model-override M] [--target-protocol chat|messages]
asw proxy status
asw proxy clear
asw stats
```

`--set '<json>'`：合并到 provider 的 `extra` 字段（如 Codex 的 `wire_api`、OpenCode 的 `models`）。

## GUI invoke 命令

前端经 `@tauri-apps/api` `invoke` 调用（Rust `tauri::command`）：

| command | 参数 |
|---|---|
| `list_providers` | `tool?` |
| `add_provider` / `update_provider` | `input { id, tool, base_url, key, extra }` |
| `remove_provider` / `use_provider` / `current_provider` | `tool, id?` |
| `list_presets` | `tool` |
| `import_provider` | `tool` |
| `list_backups` / `restore_backup` | `tool, path?` |
| `proxy_start` | `host, port, authToken` |
| `proxy_stop` / `proxy_status` | — |
| `get_routes` / `set_routes` / `clear_routes` | routes/modelOverride/targetProtocol |
| `stats` | `since, by` |
| `recent_logs` | `limit` |
