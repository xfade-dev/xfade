# 快速上手（各工具）

本文以三工具为例，演示添加第三方 provider、切换、可选代理模式。统一前置：安装 `asw`（见 README）。

## 通用流程

```bash
asw add <id> --tool <tool> --base-url <url> --key <key> [--set '<extra json>']
asw use <id> --tool <tool>
asw current --tool <tool>
```

`<tool>` ∈ `claude-code | codex | open-code`。切换前自动备份原配置（`asw backup ls --tool <tool>` 查看恢复）。

---

## Claude Code

配置文件：`~/.claude/settings.json`，`env.ANTHROPIC_BASE_URL` + `env.ANTHROPIC_API_KEY`（或 `ANTHROPIC_AUTH_TOKEN`）。

```bash
asw add kimi --tool claude-code \
  --base-url https://api.moonshot.cn/anthropic \
  --key sk-xxx
asw use kimi --tool claude-code
```

切换后 `settings.json` 的 `env` 写入对应值，key 存系统钥匙串（`agent-switch/claude-code/kimi`）。官方 provider：`asw use official --tool claude-code`（清空 base_url，恢复官方）。

## Codex

配置文件：`~/.codex/config.toml`（`model_providers.<id>`）+ `~/.codex/auth.json`（`OPENAI_API_KEY`）。

```bash
asw add kimi --tool codex --base-url https://api.moonshot.cn/v1 --key sk-xxx
asw use kimi --tool codex
```

注意：
- apply 时若 `auth.json` 含官方 OAuth `tokens`，会自动移除并设 `preferred_auth_method=apikey`，防 token 劫持 Authorization（已备份可恢复）。
- apply 后扫描全文件，若其他 provider 节仍用废弃 `wire_api="chat"` 会警告（新版 Codex 全文件校验会拒绝启动，请改 `responses`）。
- 官方 provider：`asw use official --tool codex`。

## OpenCode

配置文件：`~/.config/opencode/opencode.json`（`provider.<id>`）+ `~/.local/share/opencode/auth.json`。

```bash
asw add yy --tool open-code --base-url https://api.example.com/v1 --key sk-xxx
asw use yy --tool open-code
```

## 代理模式（可选）

不走工具直连，而由 `asw serve` 起本地兼容端点，工具指向本地：

```bash
# 1. 起代理（前台或 daemon）
asw serve install --port 24860        # macOS 常驻 daemon
# 或前台：asw serve --port 24860

# 2. 配置路由（主→备）
asw proxy use --routes yy,bak --model-override gpt-5.6-luna --target-protocol chat

# 3. 工具指向本地（以 Codex 为例）
asw add local-proxy --tool codex --base-url http://127.0.0.1:24860/v1 --key any
asw use local-proxy --tool codex

# 4. 查看
asw serve status          # 运行/路由/熔断
asw stats                 # token 统计
curl http://127.0.0.1:24860/__asw/status
```

`target_protocol`：`chat`（OpenAI 风格）/ `messages`（Anthropic 风格），决定协议互转方向。`model_override` 透传或覆盖上游模型名。

## GUI

`npx tauri dev`（或装 .dmg）→ Providers 页操作同上；Proxy 页启停 daemon + 编辑路由；Stats/Logs 查看统计与日志；托盘快速切换。
