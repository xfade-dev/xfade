# Agent Switch 设计文档

日期：2026-07-24
状态：已确认

## 1. 背景与目标

开发者日常使用多个 AI coding 工具（Claude Code、Codex、OpenCode），每个工具有自己的配置格式，切换 API 供应商需要手动编辑 JSON/TOML/认证文件。现有方案：cc-switch（GUI 配置切换，密钥明文存 SQLite）与 aivo（CLI 启动注入，无 GUI）。

本项目目标：构建一个 Rust 实现的供应商切换工具，核心逻辑独立成库，先交付 CLI，后续可复用同一核心增加 Tauri GUI 与本地代理模式。

### 核心决策（已与用户确认）

| 决策点 | 结论 |
|--------|------|
| 产品形态 | CLI + GUI 双形态，核心逻辑独立 crate |
| 支持工具 | Claude Code、Codex、OpenCode |
| 技术栈 | Rust |
| 核心机制 | 改写配置文件为主，架构预留本地代理接口（二期） |
| MVP 范围 | 核心库 + CLI；GUI 后置 |
| 密钥存储 | 系统钥匙串（keyring crate），SQLite 只存引用 |

## 2. 架构

```
agent-switch/
├── Cargo.toml                   # workspace 根
├── crates/
│   ├── core/                    # 核心库：不依赖 CLI/GUI，二期代理与 GUI 复用
│   │   └── src/
│   │       ├── models.rs        # Provider、ToolKind 等数据模型
│   │       ├── store/
│   │       │   ├── db.rs        # SQLite（sqlx）：provider 元数据
│   │       │   └── secrets.rs   # keyring：API key 存取
│   │       ├── adapters/        # 工具配置适配层
│   │       │   ├── mod.rs       # trait ToolAdapter
│   │       │   ├── claude_code.rs
│   │       │   ├── codex.rs
│   │       │   └── opencode.rs
│   │       ├── presets.rs       # 内置供应商预设模板
│   │       ├── backup.rs        # 写入前自动备份 + 回滚
│   │       └── error.rs         # thiserror 统一错误
│   └── cli/                     # CLI 前端（clap + dialoguer）
│       └── src/main.rs
└── apps/
    └── gui/                     # （二期）Tauri，仅依赖 core
```

原则：core 是唯一知道"怎么改配置、怎么存密钥"的 crate；CLI（及未来的 GUI、代理）只是调用 core 的薄壳。

## 3. 数据模型

```rust
struct Provider {
    id: String,               // 用户命名，如 "kimi"、"official"
    tool: ToolKind,           // ClaudeCode | Codex | OpenCode
    base_url: Option<String>, // None = 官方默认
    key_ref: String,          // keyring 引用，如 "agent-switch/kimi"
    extra: serde_json::Value, // 工具特有配置（model、wire_api 等）
    is_active: bool,          // 每个工具恰好一个 active
}
```

唯一性约束：`(tool, id)` 复合唯一——同一工具内不允许同名，不同工具允许同名（如 claude 和 codex 下都可以有 "kimi"）。

密钥流程：添加时 key 写入系统钥匙串，SQLite 只存 `key_ref`；切换时从 keyring 取出明文写入工具配置文件。主库永不落明文。

存储位置：

- 数据库：`~/.config/agent-switch/agent-switch.db`
- 备份：`~/.config/agent-switch/backups/<tool>/`（每工具保留最近 10 份）

## 4. 写入安全

所有配置文件修改遵循统一流程：

1. 读取现有配置
2. 合并新值（保留无关字段原样）
3. 写入临时文件
4. 备份原文件到 backups 目录
5. rename 原子替换

首次运行（该工具下尚无任何 provider 时）自动把当前配置导入为名为 `imported` 的 provider，保证可随时切回。手动执行 `asw import` 时若 `imported` 已存在，则覆盖更新快照（导入语义就是"把当前实况存为快照"）。

## 5. 适配层细节

### 5.1 Claude Code

只修改 `~/.claude/settings.json` 的 `env` 节：

```json
{ "env": { "ANTHROPIC_BASE_URL": "...", "ANTHROPIC_AUTH_TOKEN": "..." } }
```

- 切回官方 = 恢复到 `imported` 快照的状态；对无快照场景等价于删除这两个 env 键，保留 settings.json 其他内容
- `~/.claude.json`（登录态、用户数据）绝不触碰
- 可选写入 `ANTHROPIC_MODEL`（存于 extra）

### 5.2 Codex

双文件写入。`~/.codex/config.toml`：

```toml
model_provider = "kimi"
[model_providers.kimi]
name = "kimi"
base_url = "https://api.moonshot.cn/v1"
wire_api = "chat"          # 多数第三方为 chat 协议；官方为 responses
env_key = "KIMI_API_KEY"
```

key 写入 `~/.codex/auth.json`（`{"OPENAI_API_KEY": "..."}`）。说明（已经真实环境验证）：Codex 对配置了 `env_key` 的 provider **强制**从该环境变量读 key 并完全忽略 auth.json；**省略 `env_key`** 时才会回退到 auth.json 的 `OPENAI_API_KEY`。因此适配器不写 `env_key`，统一走 auth.json 路径（与 cc-switch 一致）。另：新版 Codex 已废弃 `wire_api = "chat"`，默认写 `"responses"`。切回官方 = 移除顶层 `model_provider`，恢复 `imported` 快照的 config.toml 与 auth.json。

### 5.3 OpenCode

配置与认证分离。`~/.config/opencode/opencode.json`：

```json
{ "provider": { "kimi": {
    "npm": "@ai-sdk/openai-compatible",
    "options": { "baseURL": "..." },
    "models": {}
} } }
```

key 写入 `~/.local/share/opencode/auth.json`：

```json
{ "kimi": { "type": "api", "key": "sk-..." } }
```

## 6. CLI 设计

二进制名暂定 `asw`（正式名待定）。

```bash
asw add                          # 交互式：选工具 → 选预设/自定义 → 填 key
asw add --tool codex --name kimi \
    --base-url https://api.moonshot.cn/v1 --key sk-xxx   # 非交互

asw ls [--tool <tool>]           # 列表，active 高亮
asw use <name> [--tool <tool>]   # 切换
asw current                      # 各工具当前生效 provider
asw edit <name> [--set key=val]  # 修改 base_url / extra；--key 可更换密钥（重新写入 keyring）
asw rm <name>                    # 删除（active 不可删）
asw presets                      # 内置预设列表
asw backup ls                    # 备份列表
asw backup restore <file>        # 恢复备份
asw completion <shell>           # shell 补全
```

取舍：`use` 不带 `--tool` 时若同名 provider 跨工具存在则交互选择；工具特有配置统一经 `--set key=value` 传入 extra。

## 7. 错误处理

`thiserror` 定义 `CoreError`：

- 配置文件解析失败
- keyring 不可用（如无桌面环境的 Linux）
- 文件权限不足
- 目标文件损坏（提示从备份恢复）

## 8. 测试策略

- core 层：用 `tempfile` 构造隔离的假 `$HOME`，对每个适配器做 round-trip 测试（写入 → 读回 → 断言）
- CLI 层：`assert_cmd` 命令级集成测试
- 密钥相关测试 mock keyring（`secrets` 模块抽象为 trait）

## 9. 非目标（YAGNI）

以下明确不属于 MVP：

- 本地代理模式 / failover / 用量统计（二期）
- Tauri GUI（二期）
- MCP / Skills / Prompts 管理
- 端点测速、云同步、会话历史浏览
- 支持 Claude Code / Codex / OpenCode 之外的工具
