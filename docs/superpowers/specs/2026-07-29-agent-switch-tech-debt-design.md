# Agent Switch 技术债清理设计（v0.6a / Phase A）

日期：2026-07-29
状态：已批准
范围：v0.6 发布硬化的第一个子项目（技术债），聚焦高价值修复。打包（B）与文档（C）为后续子项目。

## 背景

v0.1-v0.5 交付过程中积累若干安全/稳健性技术债：OAuth 劫持、panic 风险、统计漏计、废弃值。本子项目清理 4 项高价值项；F4（歧义 provider）已解决跳过，F6（熔断持久化）/F7（托盘刷新等 GUI 体验）按"聚焦高价值"延后到后续阶段。

## 目标 / 非目标

### 目标
- F2：codex apply 移除 auth.json 的官方 OAuth `tokens`，防 Authorization 被劫持。
- F3：三处 `base_url.unwrap()` 改为安全错误，防 panic。
- F5：stats errors 计入 status=0（连接失败）。
- F1：codex apply 后扫描 config.toml，对残留废弃 `wire_api=chat` 节输出警告。

### 非目标
- F6 熔断持久化、F7 托盘刷新/autostart 开关/Proxy 页改造（后续阶段）。
- 打包、CI、文档（B/C 子项目）。
- CoreError derive Serialize（当前 to_string 够用）。

## F2：OAuth tokens 清理（codex）

**文件**：`crates/core/src/adapters/codex.rs:100-110`

**现状**：apply 第三方 provider 时，`load_auth()` → 设 `OPENAI_API_KEY` → 写回。若 auth.json 含官方 OAuth `tokens` 字段，Codex 仍优先用 tokens 的 access_token 作 Authorization，忽略 OPENAI_API_KEY（劫持）。

**修复**：apply 写 auth.json 前，若 `auth` 含 `tokens` 键 → 删除 `tokens` + 设 `preferred_auth_method = "apikey"`。auth.json 已由 `service.rs::use_provider` 在 apply 前备份（backup.rs 覆盖 auth.json），故无需在此重复备份。

```rust
let mut auth = self.load_auth()?;
if !auth.is_object() { auth = json!({}); }
let obj = auth.as_object_mut().unwrap();
if obj.remove("tokens").is_some() {
    obj.insert("preferred_auth_method".into(), json!("apikey"));
}
obj.insert("OPENAI_API_KEY".into(), json!(api_key.unwrap_or_default()));
```

**测试**：`apply_removes_oauth_tokens` — 预置 auth.json `{"tokens":{...},"OPENAI_API_KEY":"old"}` → apply 第三方 → 结果 auth.json 无 `tokens`、有 `preferred_auth_method="apikey"`、`OPENAI_API_KEY` 为新 key。

**注意（现有测试需适配）**：`codex.rs` 现有测试 `apply_preserves_other_toml_sections`（约 :217）预置 auth.json 含 `{"tokens":{"x":1}}` 并断言 apply 后 `auth.get("tokens").is_some()`。F2 后该断言反转为失败——实现时须将此断言改为 `assert!(auth.get("tokens").is_none())` 并补 `assert_eq!(auth["preferred_auth_method"], "apikey")`。

## F3：base_url unwrap 防御

**文件**：
- `crates/core/src/adapters/codex.rs:73` `provider.base_url.as_deref().unwrap()`
- `crates/core/src/adapters/claude_code.rs:79` 同
- `crates/core/src/adapters/opencode.rs:108` 同

**现状**：第三方 provider 缺 base_url（数据不一致）时 `unwrap()` panic。

**修复**：改为 `provider.base_url.as_deref().ok_or_else(|| CoreError::ConfigParse { path, msg: "non-official provider missing base_url" })?`。`path` 取各 adapter 配置文件路径的 display 字符串——注意 claude_code.rs 对应方法为 `settings_path()`（非 `config_path()`），codex.rs / opencode.rs 为 `config_path()`。返回错误而非 panic。

**测试**：各 adapter 新增 `apply_missing_base_url_returns_error` — `Provider::new(id, tool, None)`（非官方无 base_url）→ apply → 返回 `Err(CoreError::ConfigParse{..})`，不 panic。

## F5：stats errors 计入 status=0

**文件**：`crates/core/src/store/db.rs:298`

**现状**：`SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END)` — status=0（上游连接失败）不计入 errors。

**修复**：改为 `SUM(CASE WHEN status >= 400 OR status = 0 THEN 1 ELSE 0 END)`。

**测试**：扩展 `db.rs` 现有 stats 测试——插入一条 status=0 日志 → `stats_since` 该组 errors 计数包含它。

## F1：apply 废弃 wire_api 警告（codex）

**文件**：`crates/core/src/adapters/codex.rs` apply 末尾（写 config 后）

**现状**：apply 只规范化自身写入的节；config.toml 中**其他** model_providers.* 节若含废弃 `wire_api="chat"`，新版 Codex 全文件校验会拒绝启动。

**修复**：apply 写完 config.toml 后，扫描 `model_providers` 表所有子节的 `wire_api`，若有 `"chat"` → `eprintln!` 警告（不阻塞，提示用户手动改）。返回 `Ok(())`。

```rust
// 写完 config.toml 后
if let Some(provs) = cfg.get("model_providers").and_then(|v| v.as_table()) {
    for (pid, p) in provs {
        if p.get("wire_api").and_then(|v| v.as_str()) == Some("chat") {
            eprintln!("[asw] 警告: config.toml 中 model_providers.{pid} 仍使用废弃的 wire_api=\"chat\"，新版 Codex 将拒绝启动，请改为 \"responses\"");
        }
    }
}
```

**测试**：`apply_warns_on_legacy_wire_api` — 预置 config.toml 含一个 `wire_api="chat"` 的其他 provider 节 → apply 第三方 → 验证警告输出（用 `eprintln` 难直接断言；改为返回/收集警告字符串可测，或测试仅校验不 panic 且 apply 成功，警告行为用文档说明）。**取舍**：为可测，将警告收集到一个返回的 `Vec<String>` 或通过 `log::warn!`；本子项目用 `eprintln!` 并辅以一个"扫描函数返回废弃节列表"的纯函数单测（`find_legacy_wire_api(&cfg) -> Vec<String>`），apply 调用它打印——纯函数可测。

## 测试总览
- F2：1 新测试（codex）+ 适配现有 `apply_preserves_other_toml_sections` 断言
- F3：3 测试（每 adapter 1）
- F5：1 测试（db，扩展现有）
- F1：1 测试（`find_legacy_wire_api` 纯函数）+ apply 不回归
共 +6 新测试；现有 97 中 1 个（apply_preserves_other_toml_sections）断言需适配，其余保持绿。

## 风险
- F2 移除 tokens 后，用户若想用官方 OAuth 需重新登录 Codex；但本工具场景就是切到第三方，可接受，且 auth.json 已备份可恢复。
- F1 `eprintln` 在 GUI daemon 上下文输出到 serve.log（launchd 重定向），用户可见。
