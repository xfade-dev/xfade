# xfade

[English](README.md) | 简体中文

**xfade** —— 面向 AI 编码工具（Claude Code、Codex、OpenCode、Aider、Cline、Hermes、OpenClaw 等）的中立、零裸 Key 本地可信网关。

> **Switch models without dropping the beat** —— 换模型，不掉拍，上游真实 Key 永不落入客户端配置。一流的协议转译、精细 Failover、动态成本核算；零遥测、零返佣中转商、零归因头。

[![CI](https://github.com/xfade-dev/xfade/actions/workflows/ci.yml/badge.svg)](https://github.com/xfade-dev/xfade/actions)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Security Policy](https://img.shields.io/badge/security-SLA%2048h%2F7d-green.svg)](SECURITY.md)

---

## 为什么选择 xfade？

| 核心维度 | 传统 CLI 配置方式 | 常见桌面切换器 / 中转代理 | xfade 本地可信网关 |
| :--- | :--- | :--- | :--- |
| **磁盘密钥安全** | 明文落盘在 `settings.json` / `config.toml` | 明文落盘或未加密 SQLite | **零裸 Key 落盘**（客户端仅持虚拟凭证 `xf-tok-*`，真实密钥安全锁定在系统钥匙串） |
| **Failover 粒度** | 报错中断退出会话 | 任意 4xx 盲目重试打遍所有上游 | **首字节前精细换路**（TTFT 超时、200 假成功错误体兜底、429 退避，首字节后绝不重放） |
| **上游协议** | 强绑定客户端自身协议 | 全局单一转译规则 | **每个上游独立协议**（Chat、Messages、Responses 独立配置与双向流式转译） |
| **额外转发延迟 (F10)**| 0 ms | 5 ms – 50 ms（Node/Python 运行时） | **~0.32 ms**（Rust Axum 极速内核，实测往返仅 0.35 ms） |
| **模型成本核算** | 无 / 静态估算 | 过时或不可控硬编码 | **三层动态定价**（用户自定义 > OpenRouter 实时同步 > 2026 最新离线底册，微美元精度） |
| **中立与可验证性** | 无 | 内置中转商推广、遥测、返佣 | **100% 中立可信**：零遥测、零返佣、Sigstore keyless 签名发布、CycloneDX SBOM |

---

## 安装与升级指南

### 1. Homebrew 安装与升级（macOS / Linux）

```bash
# 首次安装
brew install xfade-dev/xfade/xfade

# 升级至最新版本
brew update && brew upgrade xfade-dev/xfade/xfade
```

### 2. 一键安装 / 升级脚本 (`xfade.sh`)

```bash
# 默认安装至 ~/.local/bin
curl -fsSL https://xfade.sh | sh

# 或指定覆盖安装至系统目录：
curl -fsSL https://xfade.sh | XFADE_INSTALL_DIR=/usr/local/bin sh
```

### 3. CLI 内置自更新

```bash
xfade self-update
# 若安装在系统受限目录（如 /usr/local/bin），请使用：
# sudo xfade self-update
```

### 4. 源码编译构建

```bash
cargo build --release -p xfade   # 产物 target/release/xfade
```

### 发布产物与签名校验

每个 Release 均包含 Sigstore 无长期私钥签名与 GitHub 构建来源证明（Build Provenance）。可在运行前独立校验：

```bash
scripts/verify-release.sh v0.12.0
```

> [!TIP]
> **版本冲突排查**：若重新安装或升级后执行 `xfade --version` 仍显示老版本，请使用 `which -a xfade` 排查系统中是否存在多个冲突路径（如 `/usr/local/bin` 或 `/opt/homebrew/bin` 优先于 `~/.local/bin`），并在终端执行 `rehash`（Zsh）或 `hash -r`（Bash）刷新 Shell 命令缓存。

---

## 快速上手：零裸 Key 凭据隔离

### 1. 添加 Provider 并安全存储密钥

真实 API 密钥安全存储在 OS 系统钥匙串（或加密存储）中：

```bash
# 添加 Anthropic 官方 Messages 协议节点
xfade add anthropic --tool claude --protocol messages \
  --base-url https://api.anthropic.com --key sk-ant-api03-...

# 添加 DeepSeek 或 Kimi 等 OpenAI Chat 兼容节点
xfade add kimi --tool claude --protocol chat \
  --base-url https://api.moonshot.cn/v1 --key sk-xxx
```

### 2. 默认网关切换（凭据隔离）

执行 `xfade use` 会自动签发本地虚拟 Token（`xf-tok-*`），并将目标客户端配置指向本地网关：

```bash
xfade use kimi --tool claude
```

此时 Claude Code 的配置文件（`~/.claude/settings.json`）仅包含本地回环地址与虚拟 Token，**任何真实的外部 API Key 都不会写入磁盘**。

*(如遇特殊网络或需客户端直连上游，可显式启用逃生舱：`xfade use kimi --tool claude --direct`，直连状态将被安全审计与体检标记)*。

### 3. 可信体检与一键修复

随时运行安全诊断命令检查虚拟凭据、文件权限及配置泄露：

```bash
xfade doctor --security
```

若发现配置被外部软件改动或权限过宽，一键自动修复：

```bash
xfade doctor --security --fix
```

---

## 核心特性

### 1. 首字节前 Failover 稳健性 (F6)
- **语义提交前安全换路**：当上游返回 HTTP 200 但内容为错误 JSON 体，或首字节响应（TTFT）超时时，网关透明切换至备用上游。
- **429 智能退避**：自动解析上游返回的 `Retry-After` 头（≤2s 时原地单次重试），避免盲目换路打垮备用节点。
- **首字节后坚决不重放**：一旦向客户端发出首个有效语义事件，**绝不重放请求**（防止重复扣费与重复执行工具），流中断直接合成标准 `xfade_stream_interrupted` 事件并优雅关闭流。

### 2. 显式模型映射 (F5)
支持客户端逻辑模型名到各上游真实模型 ID 的精准改写：

```bash
# 将客户端别名 sonnet 映射为 openrouter 节点的真实全称
xfade model-map set openrouter sonnet anthropic/claude-3.7-sonnet

# 查看生效的模型映射表
xfade model-map ls
```

### 3. 三层动态模型定价与成本核算 (F8 & F8-Ext)
支持 micro-dollar（1 美元 = 1,000,000 微美元）整数精度的费用核算，原生支持缓存命中与写入折扣：
- **第一层（Custom 自定义）**：用户手动设定的优先级最高覆盖；
- **第二层（Synced 实时同步）**：从 OpenRouter API 实时同步的全量主流模型价格库；
- **第三层（Built-in 2026 底册）**：内置 2026 年最新旗舰模型离线定价兜底。

```bash
xfade pricing list            # 查看各模型价格及来源（custom/synced/builtin）
xfade pricing sync            # 联网拉取最新模型价目表
xfade stats --by model        # 按模型查看 Token 吞吐与估算费用
```

### 4. 出站审计轨迹与白名单防护 (F7)
- **SSRF 与数据外发防护**：出站请求严格校验域名白名单，仅放行已登记 Provider 的合法域名。
- **结构化审计导出**：记录每次请求的客户端来源、路由路径、Failover 轨迹与 Token 消耗（绝不记录敏感密钥与提示词正文）：
```bash
xfade audit export --format json
```
- **三态信任灯**：TUI、CLI 与桌面托盘实时显示信任状态：
  - `GUARDED`：处于受保护的零裸 Key 本地网关接管状态；
  - `DIRECT`：处于直连逃生舱模式（磁盘存在明文 Key）；
  - `EXPOSED`：检测到 Key 泄漏或文件权限异常。

### 5. 可复现的高性能基准实测 (F10)
Criterion 自动化微基准实测数据（本地回环环境）：
- **网关代理转发额外延迟**：**~0.32 ms**（直接上游 0.032 ms vs 网关全链路 0.353 ms）
- **模型路由映射匹配延迟**：**~26 ns**
- **微美元价格检索计算耗时**：**~30 ns – 88 ns**
- **SSE 流式数据块语义解析**：**< 1 µs**

---

## 常用命令一览

```bash
# Provider 管理
xfade add <id> --tool <t> [--protocol chat|messages|responses] [--base-url URL] [--key KEY]
xfade use <id> --tool <t> [--direct]      # 切换 provider（默认启用零裸 Key 网关模式）
xfade ls [--tool <t>]                     # 查看已配置列表及当前激活状态
xfade current                             # 显示各工具当前激活的 Provider

# 安全诊断、审计与状态
xfade doctor --security [--fix]           # 检查 Token 隔离与文件权限，支持自动加固
xfade audit [export]                      # 查看与导出结构化请求审计轨迹
xfade status                              # 查看网关常驻状态、当前路由、熔断器与信任灯

# 模型映射与动态定价
xfade model-map [set|ls|rm|clear]         # 配置客户端模型到上游真实模型的映射关系
xfade pricing [list|sync|set|rm|reset]    # 查询与管理三层动态定价库

# 代理与系统服务
xfade serve [--port 24860] [--host 127.0.0.1]
xfade proxy use <p1> <p2> [--model M]     # 设置多上游故障转移级联链
xfade proxy smart on|off                  # Smart Fader 任务意图自适应路由
xfade stats [--by provider|model|session] # 查看 Token 统计与美元支出
xfade serve install|uninstall             # 安装/卸载 launchd 或 systemd 开机守护进程
xfade serve restart                        # 重启守护进程（升级后让新二进制生效）
```

---

## 中立与隐私承诺

1. **零裸 Key 落盘**：客户端配置文件与运行时日志中绝不出现未经加密的外部真实 API Key。
2. **零遥测外发**：除用户明确配置的上游 API 及显式执行的更新/同步命令外，网关没有任何未经许可的外部通信。
3. **零中转赞助与零返佣**：不设中转商推广位，不加归因头，不参与任何模型调用分成。
4. **全链路可独立核验**：所有发布二进制均经过 Sigstore 签名，并提供 CycloneDX 软件物料清单（SBOM）。

---

## 安全披露

如发现安全漏洞，请通过 GitHub Security Advisories 提交私密报告，或直接发送邮件至 **`security@xfade.dev`**。
详细响应时间请参阅我们的 [安全策略与 SLA](SECURITY.md)。

## 开源协议

本项目采用 [Apache License, Version 2.0](LICENSE-APACHE) 或 [MIT License](LICENSE-MIT) 双重授权。
