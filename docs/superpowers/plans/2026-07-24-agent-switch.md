# Agent Switch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 构建 Rust workspace：`agent-switch-core` 核心库（供应商管理、密钥钥匙串存储、三工具配置适配、备份）+ `asw` CLI。

**Architecture:** core 为唯一知道"怎么改配置、怎么存密钥"的 crate，同步 API（rusqlite，无 async 传染）；CLI 是薄壳。所有文件写入走"临时文件 + rename"原子写入，写前自动备份。密钥只存系统钥匙串，SQLite 存引用。

**Tech Stack:** Rust 2021 · rusqlite(bundled) · keyring v3(sync) · serde/serde_json/toml · clap4(derive) + clap_complete · dialoguer · 测试：tempfile/assert_cmd/predicates

**Spec:** `docs/superpowers/specs/2026-07-24-agent-switch-design.md`

---

## 文件结构

```
Cargo.toml                       # workspace 根
crates/core/Cargo.toml
crates/core/src/
├── lib.rs                       # 模块导出 + Core 服务层入口
├── error.rs                     # CoreError (thiserror)
├── models.rs                    # ToolKind / Provider
├── service.rs                   # Core：add/use/import/remove/current/list
├── presets.rs                   # 内置预设
├── store/
│   ├── mod.rs
│   ├── db.rs                    # SQLite (rusqlite)
│   └── secrets.rs               # SecretStore trait + KeyringStore + MockStore
├── backup.rs                    # 备份/轮转/恢复
└── adapters/
    ├── mod.rs                   # ToolAdapter trait + adapter_for + atomic_write
    ├── claude_code.rs
    ├── codex.rs
    └── opencode.rs
crates/cli/Cargo.toml
crates/cli/src/main.rs           # clap 定义 + 子命令分发
```

关键约定：**适配器与 Core 一律接收路径参数（home 目录），不直接调用 `dirs::home_dir()`**，测试用 `tempfile` 注入假 home。唯一读取真实 home 的地方是 `Core::for_current_user()` 与 CLI main。

---

### Task 1: Workspace 脚手架

**Files:**
- Create: `Cargo.toml`
- Create: `crates/core/Cargo.toml`
- Create: `crates/cli/Cargo.toml`
- Create: `crates/core/src/lib.rs`
- Create: `crates/cli/src/main.rs`

- [ ] **Step 1: 写 workspace 根 Cargo.toml**

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/cli"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
rusqlite = { version = "0.32", features = ["bundled"] }
keyring = { version = "3", features = ["apple-native", "windows-native", "sync-secret-service"] }
toml = "0.8"
dirs = "6"
tempfile = "3"
```

- [ ] **Step 2: 写 crates/core/Cargo.toml**

```toml
[package]
name = "agent-switch-core"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
rusqlite = { workspace = true }
keyring = { workspace = true }
toml = { workspace = true }
dirs = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 3: 写 crates/cli/Cargo.toml**

```toml
[package]
name = "asw"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "asw"
path = "src/main.rs"

[dependencies]
agent-switch-core = { path = "../core" }
serde = { workspace = true }
serde_json = { workspace = true }
clap = { version = "4", features = ["derive"] }
clap_complete = "4"
dialoguer = "0.11"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
tempfile = { workspace = true }
```

- [ ] **Step 4: 写占位 lib.rs 与 main.rs**

`crates/core/src/lib.rs` 留空；`crates/cli/src/main.rs`：

```rust
fn main() {
    println!("asw placeholder");
}
```

- [ ] **Step 5: 验证编译并提交**

Run: `cargo build && cargo test --workspace`
Expected: 编译成功，0 测试通过

```bash
git add -A && git commit -m "chore: workspace scaffold"
```

---

### Task 2: 错误类型 + 数据模型

**Files:**
- Create: `crates/core/src/error.rs`
- Create: `crates/core/src/models.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**（追加到 `models.rs` 末尾，先只有测试模块和空文件会编译失败——先建空 `models.rs`/`error.rs` 让 lib 编译）

在 `crates/core/src/models.rs` 中写：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_kind_roundtrip() {
        for t in ToolKind::ALL {
            let s = t.as_str();
            assert_eq!(s.parse::<ToolKind>().unwrap(), t);
        }
    }

    #[test]
    fn provider_key_ref_scoped_by_tool_and_id() {
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        assert_eq!(p.key_ref, "agent-switch/codex/kimi");
        assert!(!p.is_active);
        assert!(p.extra.is_null());
    }

    #[test]
    fn provider_serde_roundtrip() {
        let p = Provider::new("a", ToolKind::ClaudeCode, None);
        let s = serde_json::to_string(&p).unwrap();
        let back: Provider = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core`
Expected: FAIL（`ToolKind`/`Provider` 未定义）

- [ ] **Step 3: 实现 error.rs 与 models.rs**

`crates/core/src/error.rs`：

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("config parse failed for {path}: {msg}")]
    ConfigParse { path: String, msg: String },

    #[error("keyring error: {0}")]
    Keyring(String),

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("cannot remove active provider: {0}")]
    ActiveProviderRemoval(String),

    #[error("secret not found: {0}")]
    SecretNotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml error: {0}")]
    Toml(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
```

`crates/core/src/models.rs`（测试模块之上）：

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolKind {
    ClaudeCode,
    Codex,
    OpenCode,
}

impl ToolKind {
    pub const ALL: [ToolKind; 3] = [ToolKind::ClaudeCode, ToolKind::Codex, ToolKind::OpenCode];

    pub fn as_str(&self) -> &'static str {
        match self {
            ToolKind::ClaudeCode => "claude",
            ToolKind::Codex => "codex",
            ToolKind::OpenCode => "opencode",
        }
    }
}

impl fmt::Display for ToolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "claude" | "claude-code" => Ok(ToolKind::ClaudeCode),
            "codex" => Ok(ToolKind::Codex),
            "opencode" => Ok(ToolKind::OpenCode),
            _ => Err(format!("unknown tool: {s} (expected claude|codex|opencode)")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub tool: ToolKind,
    pub base_url: Option<String>,
    pub key_ref: String,
    #[serde(default)]
    pub extra: serde_json::Value,
    #[serde(default)]
    pub is_active: bool,
}

impl Provider {
    pub fn new(id: impl Into<String>, tool: ToolKind, base_url: Option<String>) -> Self {
        let id = id.into();
        Self {
            key_ref: format!("agent-switch/{}/{}", tool.as_str(), id),
            id,
            tool,
            base_url,
            extra: serde_json::Value::Null,
            is_active: false,
        }
    }

    /// base_url 为 None 表示官方登录（清除第三方配置）
    pub fn is_official(&self) -> bool {
        self.base_url.is_none()
    }
}
```

`lib.rs`：

```rust
pub mod error;
pub mod models;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core`
Expected: 3 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): error types and data models"
```

---

### Task 3: SQLite 存储层

**Files:**
- Create: `crates/core/src/store/mod.rs`
- Create: `crates/core/src/store/db.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**（`store/db.rs` 内测试模块）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Provider, ToolKind};

    fn sample(id: &str, tool: ToolKind) -> Provider {
        Provider::new(id, tool, Some("https://api.example.com".into()))
    }

    #[test]
    fn crud_roundtrip() {
        let db = Database::open_memory().unwrap();
        let p = sample("kimi", ToolKind::Codex);
        db.upsert(&p).unwrap();
        let got = db.get(ToolKind::Codex, "kimi").unwrap().unwrap();
        assert_eq!(got.base_url, p.base_url);
        assert_eq!(got.key_ref, p.key_ref);
        assert!(db.get(ToolKind::ClaudeCode, "kimi").unwrap().is_none());
    }

    #[test]
    fn same_id_allowed_across_tools() {
        let db = Database::open_memory().unwrap();
        db.upsert(&sample("kimi", ToolKind::Codex)).unwrap();
        db.upsert(&sample("kimi", ToolKind::ClaudeCode)).unwrap();
        assert_eq!(db.list(None).unwrap().len(), 2);
        assert_eq!(db.list(Some(ToolKind::Codex)).unwrap().len(), 1);
    }

    #[test]
    fn set_active_exclusive_per_tool() {
        let db = Database::open_memory().unwrap();
        db.upsert(&sample("a", ToolKind::Codex)).unwrap();
        db.upsert(&sample("b", ToolKind::Codex)).unwrap();
        db.upsert(&sample("a", ToolKind::ClaudeCode)).unwrap();
        db.set_active(ToolKind::Codex, "a").unwrap();
        db.set_active(ToolKind::Codex, "b").unwrap();
        let list = db.list(Some(ToolKind::Codex)).unwrap();
        assert!(!list.iter().find(|p| p.id == "a").unwrap().is_active);
        assert!(list.iter().find(|p| p.id == "b").unwrap().is_active);
        // 不影响其他工具
        assert!(!db.get(ToolKind::ClaudeCode, "a").unwrap().unwrap().is_active);
    }

    #[test]
    fn upsert_preserves_is_active() {
        let db = Database::open_memory().unwrap();
        db.upsert(&sample("a", ToolKind::Codex)).unwrap();
        db.set_active(ToolKind::Codex, "a").unwrap();
        let mut p = sample("a", ToolKind::Codex);
        p.base_url = Some("https://changed".into());
        db.upsert(&p).unwrap();
        assert!(db.get(ToolKind::Codex, "a").unwrap().unwrap().is_active);
    }

    #[test]
    fn delete_works() {
        let db = Database::open_memory().unwrap();
        db.upsert(&sample("a", ToolKind::Codex)).unwrap();
        db.delete(ToolKind::Codex, "a").unwrap();
        assert!(db.get(ToolKind::Codex, "a").unwrap().is_none());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core store`
Expected: FAIL（`Database` 未定义）

- [ ] **Step 3: 实现**

`crates/core/src/store/mod.rs`：

```rust
pub mod db;
pub mod secrets;
```

`crates/core/src/store/db.rs`（测试模块之上）：

```rust
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use rusqlite::{params, Connection};
use std::path::Path;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Self { conn: Connection::open(path)? };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let db = Self { conn: Connection::open_in_memory()? };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS providers (
                tool TEXT NOT NULL,
                id TEXT NOT NULL,
                base_url TEXT,
                key_ref TEXT NOT NULL,
                extra TEXT NOT NULL DEFAULT 'null',
                is_active INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (tool, id)
            );",
        )?;
        Ok(())
    }

    /// 插入或更新（不触碰 is_active）
    pub fn upsert(&self, p: &Provider) -> Result<()> {
        self.conn.execute(
            "INSERT INTO providers (tool, id, base_url, key_ref, extra, is_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (tool, id) DO UPDATE SET
                base_url = excluded.base_url,
                key_ref = excluded.key_ref,
                extra = excluded.extra",
            params![
                p.tool.as_str(),
                p.id,
                p.base_url,
                p.key_ref,
                serde_json::to_string(&p.extra)?,
                p.is_active as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, tool: ToolKind, id: &str) -> Result<Option<Provider>> {
        let mut stmt = self.conn.prepare(
            "SELECT tool, id, base_url, key_ref, extra, is_active FROM providers
             WHERE tool = ?1 AND id = ?2",
        )?;
        let mut rows = stmt.query(params![tool.as_str(), id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_provider(row)?)),
            None => Ok(None),
        }
    }

    pub fn list(&self, tool: Option<ToolKind>) -> Result<Vec<Provider>> {
        let (sql, param): (&str, Option<String>) = match tool {
            Some(t) => (
                "SELECT tool, id, base_url, key_ref, extra, is_active FROM providers
                 WHERE tool = ?1 ORDER BY id",
                Some(t.as_str().to_string()),
            ),
            None => (
                "SELECT tool, id, base_url, key_ref, extra, is_active FROM providers
                 ORDER BY tool, id",
                None,
            ),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = match param {
            Some(p) => stmt.query(params![p])?,
            None => stmt.query([])?,
        };
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row_to_provider(row)?);
        }
        Ok(out)
    }

    /// 同一工具内排他设置 active
    pub fn set_active(&self, tool: ToolKind, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE providers SET is_active = 0 WHERE tool = ?1", params![tool.as_str()])?;
        let n = tx.execute(
            "UPDATE providers SET is_active = 1 WHERE tool = ?1 AND id = ?2",
            params![tool.as_str(), id],
        )?;
        if n == 0 {
            return Err(CoreError::ProviderNotFound(format!("{tool}/{id}")));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&self, tool: ToolKind, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM providers WHERE tool = ?1 AND id = ?2",
            params![tool.as_str(), id],
        )?;
        Ok(())
    }
}

fn row_to_provider(row: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let tool_str: String = row.get(0)?;
    let extra_str: String = row.get(4)?;
    Ok(Provider {
        tool: tool_str.parse().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}"))),
            )
        })?,
        id: row.get(1)?,
        base_url: row.get(2)?,
        key_ref: row.get(3)?,
        extra: serde_json::from_str(&extra_str).unwrap_or(serde_json::Value::Null),
        is_active: row.get::<_, i64>(5)? != 0,
    })
}
```

`lib.rs` 追加 `pub mod store;`。`store/secrets.rs` 先建空文件（Task 4 填）。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core store`
Expected: 5 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): sqlite storage layer"
```

---

### Task 4: 密钥存储抽象（keyring + mock）

**Files:**
- Modify: `crates/core/src/store/secrets.rs`

- [ ] **Step 1: 写失败测试**

`store/secrets.rs` 测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_store_roundtrip() {
        let s = MockStore::default();
        s.set("a/b", "sk-123").unwrap();
        assert_eq!(s.get("a/b").unwrap(), "sk-123");
        s.set("a/b", "sk-456").unwrap();
        assert_eq!(s.get("a/b").unwrap(), "sk-456");
        s.delete("a/b").unwrap();
        assert!(s.get("a/b").is_err());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core secrets`
Expected: FAIL

- [ ] **Step 3: 实现**（测试模块之上）

```rust
use crate::error::{CoreError, Result};
use std::collections::HashMap;
use std::sync::Mutex;

pub trait SecretStore: Send + Sync {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()>;
    fn get(&self, key_ref: &str) -> Result<String>;
    fn delete(&self, key_ref: &str) -> Result<()>;
}

/// 系统钥匙串实现（macOS Keychain / Windows Credential Manager / Linux Secret Service）
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self { service: "agent-switch".into() }
    }

    fn entry(&self, key_ref: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key_ref).map_err(|e| CoreError::Keyring(e.to_string()))
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        self.entry(key_ref)?
            .set_password(secret)
            .map_err(|e| CoreError::Keyring(e.to_string()))
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        self.entry(key_ref)?
            .get_password()
            .map_err(|e| match e {
                keyring::Error::NoEntry => CoreError::SecretNotFound(key_ref.to_string()),
                other => CoreError::Keyring(other.to_string()),
            })
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        match self.entry(key_ref)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CoreError::Keyring(e.to_string())),
        }
    }
}

/// 测试用内存实现
#[derive(Default)]
pub struct MockStore {
    map: Mutex<HashMap<String, String>>,
}

impl SecretStore for MockStore {
    fn set(&self, key_ref: &str, secret: &str) -> Result<()> {
        self.map.lock().unwrap().insert(key_ref.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, key_ref: &str) -> Result<String> {
        self.map
            .lock()
            .unwrap()
            .get(key_ref)
            .cloned()
            .ok_or_else(|| CoreError::SecretNotFound(key_ref.to_string()))
    }

    fn delete(&self, key_ref: &str) -> Result<()> {
        self.map.lock().unwrap().remove(key_ref);
        Ok(())
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core secrets`
Expected: 1 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): secret store abstraction with keyring and mock"
```

---

### Task 5: 备份模块

**Files:**
- Create: `crates/core/src/backup.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("config.json");
        std::fs::write(&src, b"v1").unwrap();
        let bak_dir = dir.path().join("backups");

        let bak = backup_file(&src, &bak_dir).unwrap().unwrap();
        std::fs::write(&src, b"v2").unwrap();
        restore(&bak, &src).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"v1");
    }

    #[test]
    fn backup_missing_source_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let r = backup_file(&dir.path().join("nope.json"), &dir.path().join("b")).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn rotate_keeps_latest_n() {
        let dir = tempfile::tempdir().unwrap();
        let bak_dir = dir.path().join("b");
        let src = dir.path().join("c.json");
        for i in 0..15 {
            std::fs::write(&src, format!("v{i}")).unwrap();
            backup_file(&src, &bak_dir).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(std::fs::read_dir(&bak_dir).unwrap().count(), 10);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core backup`
Expected: FAIL

- [ ] **Step 3: 实现**（测试模块之上）

```rust
use crate::error::Result;
use std::fs;
use std::path::{Path, PathBuf};

const KEEP: usize = 10;

/// 备份 path 到 backup_dir/<filename>.<millis>；源不存在返回 Ok(None)
pub fn backup_file(path: &Path, backup_dir: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::create_dir_all(backup_dir)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let name = format!("{}.{}", path.file_name().unwrap().to_string_lossy(), millis);
    let dest = backup_dir.join(name);
    fs::copy(path, &dest)?;
    rotate(backup_dir, KEEP)?;
    Ok(Some(dest))
}

/// 按文件名（时间戳后缀）排序，删除最旧的直到剩 keep 份
pub fn rotate(dir: &Path, keep: usize) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    while entries.len() > keep {
        fs::remove_file(entries.remove(0))?;
    }
    Ok(())
}

pub fn restore(backup: &Path, target: &Path) -> Result<()> {
    fs::copy(backup, target)?;
    Ok(())
}

/// 列出备份，新→旧
pub fn list_backups(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    entries.reverse();
    Ok(entries)
}
```

`lib.rs` 追加 `pub mod backup;`

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core backup`
Expected: 3 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): backup module with rotation"
```

---

### Task 6: 适配器 trait + 原子写入

**Files:**
- Create: `crates/core/src/adapters/mod.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**（`adapters/mod.rs` 内）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c.json");
        atomic_write(&target, b"{}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"{}");
    }

    #[test]
    fn atomic_write_overwrites_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("c.json");
        atomic_write(&target, b"1").unwrap();
        atomic_write(&target, b"2").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"2");
        assert!(!dir.path().join("c.tmp-asw").exists());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core adapters`
Expected: FAIL

- [ ] **Step 3: 实现**（测试模块之上）

```rust
pub mod claude_code;
pub mod codex;
pub mod opencode;

use crate::error::Result;
use crate::models::{Provider, ToolKind};
use std::path::{Path, PathBuf};

pub trait ToolAdapter: Send + Sync {
    fn tool(&self) -> ToolKind;
    /// 切换时会被修改的配置文件（service 层据此备份）
    fn config_paths(&self) -> Vec<PathBuf>;
    /// 写入 provider 到工具配置。
    /// `provider.base_url` 为 None 时表示切回官方（清除/恢复快照），api_key 随之忽略。
    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()>;
    /// 读取工具当前生效的配置（首次导入用）；无第三方配置时返回 Ok(None)。
    /// 返回 (provider, api_key)；api_key 可能为 None（读不到密钥时）。
    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>>;
}

pub fn adapter_for(tool: ToolKind, home: &Path) -> Box<dyn ToolAdapter> {
    match tool {
        ToolKind::ClaudeCode => Box::new(claude_code::ClaudeCodeAdapter::new(home)),
        ToolKind::Codex => Box::new(codex::CodexAdapter::new(home)),
        ToolKind::OpenCode => Box::new(opencode::OpenCodeAdapter::new(home)),
    }
}

/// 临时文件 + rename 原子写入；自动创建父目录
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp-asw");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
```

同时创建三个空适配器文件，各含最小骨架使编译通过，例如 `claude_code.rs`：

```rust
use super::ToolAdapter;
use crate::error::Result;
use crate::models::{Provider, ToolKind};
use std::path::{Path, PathBuf};

pub struct ClaudeCodeAdapter {
    home: PathBuf,
}

impl ClaudeCodeAdapter {
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_path_buf() }
    }
}

impl ToolAdapter for ClaudeCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::ClaudeCode
    }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![]
    }
    fn apply(&self, _provider: &Provider, _api_key: Option<&str>) -> Result<()> {
        Ok(())
    }
    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        Ok(None)
    }
}
```

`codex.rs`、`opencode.rs` 同理（结构体名分别为 `CodexAdapter`/`OpenCodeAdapter`，`tool()` 返回对应值）。`lib.rs` 追加 `pub mod adapters;`

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core adapters`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): adapter trait, atomic write, skeleton adapters"
```

---

### Task 7: Claude Code 适配器

**Files:**
- Modify: `crates/core/src/adapters/claude_code.rs`

行为定义：
- 配置路径：`<home>/.claude/settings.json`
- 第三方：设置 `env.ANTHROPIC_BASE_URL` / `env.ANTHROPIC_AUTH_TOKEN`；`extra.model` 存在则设置 `env.ANTHROPIC_MODEL`，否则删除该键
- 官方（base_url=None）：删除上述三个 env 键；`env` 对象为空则保留（不动其他字段）
- `~/.claude.json` 绝不触碰
- `read_current`：env 中无 BASE_URL 且无 TOKEN → `Ok(None)`；否则构造 imported provider

- [ ] **Step 1: 写失败测试**（追加到 claude_code.rs 测试模块）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, ClaudeCodeAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = ClaudeCodeAdapter::new(dir.path());
        (dir, ad)
    }

    #[test]
    fn apply_third_party_sets_env_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"model": "opus", "env": {"OTHER": "keep"}}).to_string(),
        ).unwrap();

        let mut p = Provider::new("kimi", ToolKind::ClaudeCode, Some("https://api.moonshot.cn/anthropic".into()));
        p.extra = json!({"model": "kimi-k2.5"});
        ad.apply(&p, Some("sk-test")).unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        ).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_BASE_URL"], "https://api.moonshot.cn/anthropic");
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test");
        assert_eq!(doc["env"]["ANTHROPIC_MODEL"], "kimi-k2.5");
        assert_eq!(doc["env"]["OTHER"], "keep"); // 无关字段保留
        assert_eq!(doc["model"], "opus");
    }

    #[test]
    fn apply_official_removes_env_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "x", "ANTHROPIC_AUTH_TOKEN": "y", "OTHER": "keep"}}).to_string(),
        ).unwrap();

        let official = Provider::new("official", ToolKind::ClaudeCode, None);
        ad.apply(&official, None).unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        ).unwrap();
        assert!(doc["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(doc["env"].get("ANTHROPIC_AUTH_TOKEN").is_none());
        assert_eq!(doc["env"]["OTHER"], "keep");
    }

    #[test]
    fn apply_creates_settings_when_missing() {
        let (dir, ad) = setup();
        let p = Provider::new("k", ToolKind::ClaudeCode, Some("https://x".into()));
        ad.apply(&p, Some("k1")).unwrap();
        assert!(dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn read_current_none_when_no_third_party() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports_existing() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://relay", "ANTHROPIC_AUTH_TOKEN": "sk-9"}}).to_string(),
        ).unwrap();
        let (p, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(p.base_url.as_deref(), Some("https://relay"));
        assert_eq!(key.as_deref(), Some("sk-9"));
        assert_eq!(p.id, "imported");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core claude`
Expected: FAIL

- [ ] **Step 3: 实现**（替换骨架，测试模块保留）

```rust
use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const ENV_BASE: &str = "ANTHROPIC_BASE_URL";
const ENV_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const ENV_MODEL: &str = "ANTHROPIC_MODEL";

pub struct ClaudeCodeAdapter {
    home: PathBuf,
}

impl ClaudeCodeAdapter {
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_path_buf() }
    }

    fn settings_path(&self) -> PathBuf {
        self.home.join(".claude").join("settings.json")
    }

    fn load(&self) -> Result<Value> {
        match std::fs::read_to_string(self.settings_path()) {
            Ok(s) => {
                let v: Value = serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: self.settings_path().display().to_string(),
                    msg: e.to_string(),
                })?;
                if !v.is_object() {
                    return Err(CoreError::ConfigParse {
                        path: self.settings_path().display().to_string(),
                        msg: "top-level value is not an object".into(),
                    });
                }
                Ok(v)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, doc: &Value) -> Result<()> {
        atomic_write(
            &self.settings_path(),
            format!("{}\n", serde_json::to_string_pretty(doc)?).as_bytes(),
        )
    }
}

impl ToolAdapter for ClaudeCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::ClaudeCode
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.settings_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load()?;
        let root = doc.as_object_mut().expect("load guarantees object");
        let env = root.entry("env").or_insert_with(|| json!({}));
        if !env.is_object() {
            *env = json!({});
        }
        let env = env.as_object_mut().unwrap();

        if provider.is_official() {
            env.remove(ENV_BASE);
            env.remove(ENV_TOKEN);
            env.remove(ENV_MODEL);
        } else {
            env.insert(ENV_BASE.into(), json!(provider.base_url.as_deref().unwrap()));
            env.insert(ENV_TOKEN.into(), json!(api_key.unwrap_or_default()));
            match provider.extra.get("model").and_then(|m| m.as_str()) {
                Some(model) => {
                    env.insert(ENV_MODEL.into(), json!(model));
                }
                None => {
                    env.remove(ENV_MODEL);
                }
            }
        }
        self.save(&doc)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load()?;
        let Some(env) = doc.get("env") else { return Ok(None) };
        let base = env.get(ENV_BASE).and_then(|v| v.as_str());
        let token = env.get(ENV_TOKEN).and_then(|v| v.as_str());
        if base.is_none() && token.is_none() {
            return Ok(None);
        }
        let mut p = Provider::new("imported", ToolKind::ClaudeCode, base.map(String::from));
        if let Some(m) = env.get(ENV_MODEL).and_then(|v| v.as_str()) {
            p.extra = json!({ "model": m });
        }
        Ok(Some((p, token.map(String::from))))
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core claude`
Expected: 5 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): claude code adapter"
```

---

### Task 8: Codex 适配器

**Files:**
- Modify: `crates/core/src/adapters/codex.rs`

行为定义：
- 配置路径：`<home>/.codex/config.toml` 与 `<home>/.codex/auth.json`
- 第三方：config.toml 顶层 `model_provider = "<id>"`，`[model_providers.<id>]` 写 `name`/`base_url`/`wire_api`（extra.wire_api，默认 `"chat"`）/`env_key`（`<ID 大写，- 换 _>_API_KEY`）；auth.json 写 `OPENAI_API_KEY`（auth.json 其他键保留）
- 官方：移除 config.toml 顶层 `model_provider`（不动 `[model_providers]` 节，保留自定义节供再切换）；auth.json 不动
- `read_current`：有顶层 `model_provider` 且能在 `[model_providers]` 找到 → imported；key 从 auth.json `OPENAI_API_KEY` 读取

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, CodexAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = CodexAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_config(dir: &tempfile::TempDir) -> toml::Value {
        let s = std::fs::read_to_string(dir.path().join(".codex/config.toml")).unwrap();
        toml::from_str(&s).unwrap()
    }

    #[test]
    fn apply_third_party_writes_toml_and_auth() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://api.moonshot.cn/v1".into()));
        ad.apply(&p, Some("sk-k")).unwrap();

        let cfg = read_config(&dir);
        assert_eq!(cfg["model_provider"].as_str().unwrap(), "kimi");
        let prov = &cfg["model_providers"]["kimi"];
        assert_eq!(prov["base_url"].as_str().unwrap(), "https://api.moonshot.cn/v1");
        assert_eq!(prov["wire_api"].as_str().unwrap(), "chat");
        assert_eq!(prov["env_key"].as_str().unwrap(), "KIMI_API_KEY");

        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap(),
        ).unwrap();
        assert_eq!(auth["OPENAI_API_KEY"], "sk-k");
    }

    #[test]
    fn apply_preserves_other_toml_sections() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".codex")).unwrap();
        std::fs::write(
            dir.path().join(".codex/config.toml"),
            "model = \"gpt-5\"\napproval_policy = \"never\"\n",
        ).unwrap();
        std::fs::write(
            dir.path().join(".codex/auth.json"),
            json!({"tokens": {"x": 1}}).to_string(),
        ).unwrap();

        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-1")).unwrap();

        let cfg = read_config(&dir);
        assert_eq!(cfg["model"].as_str().unwrap(), "gpt-5");
        assert_eq!(cfg["approval_policy"].as_str().unwrap(), "never");
        let auth: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".codex/auth.json")).unwrap(),
        ).unwrap();
        assert!(auth.get("tokens").is_some());
        assert_eq!(auth["OPENAI_API_KEY"], "sk-1");
    }

    #[test]
    fn apply_official_removes_model_provider_only() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-1")).unwrap();
        let official = Provider::new("official", ToolKind::Codex, None);
        ad.apply(&official, None).unwrap();

        let cfg = read_config(&dir);
        assert!(cfg.get("model_provider").is_none());
        assert!(cfg["model_providers"].get("kimi").is_some()); // 自定义节保留
    }

    #[test]
    fn wire_api_from_extra() {
        let (dir, ad) = setup();
        let mut p = Provider::new("oa", ToolKind::Codex, Some("https://x".into()));
        p.extra = json!({"wire_api": "responses"});
        ad.apply(&p, Some("k")).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(cfg["model_providers"]["oa"]["wire_api"].as_str().unwrap(), "responses");
    }

    #[test]
    fn read_current_none_without_model_provider() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::Codex, Some("https://x".into()));
        ad.apply(&p, Some("sk-9")).unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.id, "imported");
        assert_eq!(got.base_url.as_deref(), Some("https://x"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core codex`
Expected: FAIL

- [ ] **Step 3: 实现**（替换骨架）

```rust
use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct CodexAdapter {
    home: PathBuf,
}

impl CodexAdapter {
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_path_buf() }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".codex").join("config.toml")
    }

    fn auth_path(&self) -> PathBuf {
        self.home.join(".codex").join("auth.json")
    }

    fn load_toml(&self) -> Result<toml::Value> {
        match std::fs::read_to_string(self.config_path()) {
            Ok(s) => toml::from_str(&s).map_err(|e| CoreError::ConfigParse {
                path: self.config_path().display().to_string(),
                msg: e.to_string(),
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Value::Table(Default::default())),
            Err(e) => Err(e.into()),
        }
    }

    fn load_auth(&self) -> Result<Value> {
        match std::fs::read_to_string(self.auth_path()) {
            Ok(s) => Ok(serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                path: self.auth_path().display().to_string(),
                msg: e.to_string(),
            })?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }
}

impl ToolAdapter for CodexAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Codex
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path(), self.auth_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut cfg = self.load_toml()?;
        let root = cfg.as_table_mut().ok_or_else(|| CoreError::ConfigParse {
            path: self.config_path().display().to_string(),
            msg: "top-level is not a table".into(),
        })?;

        if provider.is_official() {
            root.remove("model_provider");
        } else {
            let id = &provider.id;
            let url = provider.base_url.as_deref().unwrap();
            let wire_api = provider
                .extra
                .get("wire_api")
                .and_then(|v| v.as_str())
                .unwrap_or("chat");
            let env_key = format!("{}_API_KEY", id.to_uppercase().replace('-', "_"));

            let mut prov = toml::map::Map::new();
            prov.insert("name".into(), toml::Value::String(id.clone()));
            prov.insert("base_url".into(), toml::Value::String(url.to_string()));
            prov.insert("wire_api".into(), toml::Value::String(wire_api.to_string()));
            prov.insert("env_key".into(), toml::Value::String(env_key));

            let providers = root
                .entry("model_providers")
                .or_insert_with(|| toml::Value::Table(Default::default()));
            if !providers.is_table() {
                *providers = toml::Value::Table(Default::default());
            }
            providers.as_table_mut().unwrap().insert(id.clone(), toml::Value::Table(prov));
            root.insert("model_provider".into(), toml::Value::String(id.clone()));

            // key 写 auth.json（保留其他键）
            let mut auth = self.load_auth()?;
            if !auth.is_object() {
                auth = json!({});
            }
            auth.as_object_mut().unwrap().insert(
                "OPENAI_API_KEY".into(),
                json!(api_key.unwrap_or_default()),
            );
            atomic_write(
                &self.auth_path(),
                format!("{}\n", serde_json::to_string_pretty(&auth)?).as_bytes(),
            )?;
        }

        atomic_write(
            &self.config_path(),
            toml::to_string_pretty(&cfg)
                .map_err(|e| CoreError::Toml(e.to_string()))?
                .as_bytes(),
        )
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let cfg = self.load_toml()?;
        let Some(active_id) = cfg.get("model_provider").and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        let prov = cfg.get("model_providers").and_then(|m| m.get(active_id));
        let base_url = prov
            .and_then(|p| p.get("base_url"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let mut p = Provider::new("imported", ToolKind::Codex, base_url);
        if let Some(w) = prov.and_then(|p| p.get("wire_api")).and_then(|v| v.as_str()) {
            p.extra = json!({ "wire_api": w });
        }
        let auth = self.load_auth()?;
        let key = auth
            .get("OPENAI_API_KEY")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(Some((p, key)))
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core codex`
Expected: 6 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): codex adapter"
```

---

### Task 9: OpenCode 适配器

**Files:**
- Modify: `crates/core/src/adapters/opencode.rs`

行为定义：
- 配置路径：`<home>/.config/opencode/opencode.json` 与 `<home>/.local/share/opencode/auth.json`
- 第三方：`provider.<id>` 写 `{ npm: "@ai-sdk/openai-compatible", name: <id>, options: { baseURL }, models }`（models 来自 extra.models，缺省 `{}`）；auth.json 写 `<id>: { type: "api", key }`
- 官方：删除 `provider.<id>`（指当前 active 的自定义 provider；由 service 层传入 active provider 的 id）与 auth.json 中同名条目
- `read_current`：扫描 `provider` 节第一个 `npm == "@ai-sdk/openai-compatible"` 的自定义 provider → imported

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, OpenCodeAdapter) {
        let dir = tempfile::tempdir().unwrap();
        let ad = OpenCodeAdapter::new(dir.path());
        (dir, ad)
    }

    fn read_doc(dir: &tempfile::TempDir) -> serde_json::Value {
        let s = std::fs::read_to_string(
            dir.path().join(".config/opencode/opencode.json"),
        ).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    fn read_auth(dir: &tempfile::TempDir) -> serde_json::Value {
        let s = std::fs::read_to_string(
            dir.path().join(".local/share/opencode/auth.json"),
        ).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn apply_third_party_writes_provider_and_auth() {
        let (dir, ad) = setup();
        let mut p = Provider::new("kimi", ToolKind::OpenCode, Some("https://api.moonshot.cn/v1".into()));
        p.extra = json!({"models": {"kimi-k2.5": {}}});
        ad.apply(&p, Some("sk-k")).unwrap();

        let doc = read_doc(&dir);
        let prov = &doc["provider"]["kimi"];
        assert_eq!(prov["npm"], "@ai-sdk/openai-compatible");
        assert_eq!(prov["options"]["baseURL"], "https://api.moonshot.cn/v1");
        assert!(prov["models"].get("kimi-k2.5").is_some());

        let auth = read_auth(&dir);
        assert_eq!(auth["kimi"]["type"], "api");
        assert_eq!(auth["kimi"]["key"], "sk-k");
    }

    #[test]
    fn apply_official_removes_custom_provider() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("k")).unwrap();

        let mut official = Provider::new("official", ToolKind::OpenCode, None);
        // 官方 provider 需要知道要移除哪个自定义条目：extra.remove_provider
        official.extra = json!({"remove_provider": "kimi"});
        ad.apply(&official, None).unwrap();

        let doc = read_doc(&dir);
        assert!(doc["provider"].get("kimi").is_none());
        let auth = read_auth(&dir);
        assert!(auth.get("kimi").is_none());
    }

    #[test]
    fn apply_preserves_unrelated_keys() {
        let (dir, ad) = setup();
        std::fs::create_dir_all(dir.path().join(".config/opencode")).unwrap();
        std::fs::write(
            dir.path().join(".config/opencode/opencode.json"),
            json!({"theme": "dark", "provider": {"builtin-x": {"npm": "@ai-sdk/anthropic"}}}).to_string(),
        ).unwrap();

        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("k")).unwrap();

        let doc = read_doc(&dir);
        assert_eq!(doc["theme"], "dark");
        assert!(doc["provider"].get("builtin-x").is_some());
    }

    #[test]
    fn read_current_none_when_no_custom() {
        let (_dir, ad) = setup();
        assert!(ad.read_current().unwrap().is_none());
    }

    #[test]
    fn read_current_imports_first_custom() {
        let (dir, ad) = setup();
        let p = Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into()));
        ad.apply(&p, Some("sk-9")).unwrap();
        let (got, key) = ad.read_current().unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("https://x"));
        assert_eq!(key.as_deref(), Some("sk-9"));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core opencode`
Expected: FAIL

- [ ] **Step 3: 实现**（替换骨架）

```rust
use super::{atomic_write, ToolAdapter};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const OPENAI_COMPAT: &str = "@ai-sdk/openai-compatible";

pub struct OpenCodeAdapter {
    home: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new(home: &Path) -> Self {
        Self { home: home.to_path_buf() }
    }

    fn config_path(&self) -> PathBuf {
        self.home.join(".config").join("opencode").join("opencode.json")
    }

    fn auth_path(&self) -> PathBuf {
        self.home.join(".local").join("share").join("opencode").join("auth.json")
    }

    fn load_json(&self, path: &Path) -> Result<Value> {
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let v: Value = serde_json::from_str(&s).map_err(|e| CoreError::ConfigParse {
                    path: path.display().to_string(),
                    msg: e.to_string(),
                })?;
                if !v.is_object() {
                    return Err(CoreError::ConfigParse {
                        path: path.display().to_string(),
                        msg: "top-level is not an object".into(),
                    });
                }
                Ok(v)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(e) => Err(e.into()),
        }
    }

    fn save_json(&self, path: &Path, doc: &Value) -> Result<()> {
        atomic_write(path, format!("{}\n", serde_json::to_string_pretty(doc)?).as_bytes())
    }
}

impl ToolAdapter for OpenCodeAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::OpenCode
    }

    fn config_paths(&self) -> Vec<PathBuf> {
        vec![self.config_path(), self.auth_path()]
    }

    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        let mut doc = self.load_json(&self.config_path())?;
        let mut auth = self.load_json(&self.auth_path())?;

        let providers = doc
            .as_object_mut()
            .unwrap()
            .entry("provider")
            .or_insert_with(|| json!({}));
        if !providers.is_object() {
            *providers = json!({});
        }
        let providers = providers.as_object_mut().unwrap();
        let auth_obj = auth.as_object_mut().unwrap();

        if provider.is_official() {
            // extra.remove_provider 指定要清理的自定义 provider
            if let Some(id) = provider.extra.get("remove_provider").and_then(|v| v.as_str()) {
                providers.remove(id);
                auth_obj.remove(id);
            }
        } else {
            let id = &provider.id;
            let models = provider
                .extra
                .get("models")
                .cloned()
                .unwrap_or_else(|| json!({}));
            providers.insert(
                id.clone(),
                json!({
                    "npm": OPENAI_COMPAT,
                    "name": id,
                    "options": { "baseURL": provider.base_url.as_deref().unwrap() },
                    "models": models,
                }),
            );
            auth_obj.insert(
                id.clone(),
                json!({ "type": "api", "key": api_key.unwrap_or_default() }),
            );
        }

        self.save_json(&self.config_path(), &doc)?;
        self.save_json(&self.auth_path(), &auth)
    }

    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        let doc = self.load_json(&self.config_path())?;
        let Some(providers) = doc.get("provider").and_then(|v| v.as_object()) else {
            return Ok(None);
        };
        for (id, prov) in providers {
            if prov.get("npm").and_then(|v| v.as_str()) != Some(OPENAI_COMPAT) {
                continue;
            }
            let base_url = prov
                .get("options")
                .and_then(|o| o.get("baseURL"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let mut p = Provider::new("imported", ToolKind::OpenCode, base_url);
            if let Some(models) = prov.get("models") {
                p.extra = json!({ "models": models.clone() });
            }
            let auth = self.load_json(&self.auth_path())?;
            let key = auth
                .get(id)
                .and_then(|a| a.get("key"))
                .and_then(|v| v.as_str())
                .map(String::from);
            return Ok(Some((p, key)));
        }
        Ok(None)
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core opencode`
Expected: 5 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): opencode adapter"
```

---

### Task 10: 内置预设

**Files:**
- Create: `crates/core/src/presets.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_official_preset() {
        for t in ToolKind::ALL {
            assert!(presets_for(t).iter().any(|p| p.is_official()));
        }
    }

    #[test]
    fn preset_ids_unique_per_tool() {
        for t in ToolKind::ALL {
            let list = presets_for(t);
            let mut ids: Vec<_> = list.iter().map(|p| p.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), list.len());
        }
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core presets`
Expected: FAIL

- [ ] **Step 3: 实现**（测试模块之上）

```rust
use crate::models::ToolKind;

#[derive(Debug, Clone)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: Option<&'static str>,
    pub extra: serde_json::Value,
}

impl Preset {
    pub fn is_official(&self) -> bool {
        self.base_url.is_none()
    }
}

/// 各工具的内置预设。第三方端点均为各厂商公开的兼容端点，新增前需核对。
pub fn presets_for(tool: ToolKind) -> Vec<Preset> {
    let official = Preset { id: "official", label: "官方登录", base_url: None, extra: serde_json::Value::Null };
    let mut v = vec![official];
    match tool {
        ToolKind::ClaudeCode => {
            v.extend([
                Preset { id: "kimi", label: "Kimi (Moonshot)", base_url: Some("https://api.moonshot.cn/anthropic"), extra: serde_json::Value::Null },
                Preset { id: "glm", label: "GLM (智谱)", base_url: Some("https://open.bigmodel.cn/api/anthropic"), extra: serde_json::Value::Null },
                Preset { id: "deepseek", label: "DeepSeek", base_url: Some("https://api.deepseek.com/anthropic"), extra: serde_json::Value::Null },
            ]);
        }
        ToolKind::Codex => {
            v.extend([
                Preset { id: "openrouter", label: "OpenRouter", base_url: Some("https://openrouter.ai/api/v1"), extra: serde_json::json!({"wire_api": "chat"}) },
                Preset { id: "kimi", label: "Kimi (Moonshot)", base_url: Some("https://api.moonshot.cn/v1"), extra: serde_json::json!({"wire_api": "chat"}) },
            ]);
        }
        ToolKind::OpenCode => {
            v.extend([
                Preset { id: "openrouter", label: "OpenRouter", base_url: Some("https://openrouter.ai/api/v1"), extra: serde_json::Value::Null },
                Preset { id: "kimi", label: "Kimi (Moonshot)", base_url: Some("https://api.moonshot.cn/v1"), extra: serde_json::Value::Null },
            ]);
        }
    }
    v
}
```

`lib.rs` 追加 `pub mod presets;`

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core presets`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): built-in provider presets"
```

---

### Task 11: Core 服务层

**Files:**
- Create: `crates/core/src/service.rs`
- Modify: `crates/core/src/lib.rs`

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Provider, ToolKind};
    use crate::store::secrets::MockStore;
    use serde_json::json;

    fn setup() -> (tempfile::TempDir, Core) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        let core = Core::with_paths(&home, &data, Box::new(MockStore::default())).unwrap();
        (dir, core)
    }

    #[test]
    fn add_and_use_claude_provider() {
        let (dir, core) = setup();
        let p = Provider::new("kimi", ToolKind::ClaudeCode, Some("https://api.moonshot.cn/anthropic".into()));
        core.add_provider(p, Some("sk-test")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();

        let s = std::fs::read_to_string(
            dir.path().join("home/.claude/settings.json"),
        ).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test");

        let cur = core.current(ToolKind::ClaudeCode).unwrap().unwrap();
        assert_eq!(cur.id, "kimi");
    }

    #[test]
    fn first_use_auto_imports_current_config() {
        let (dir, core) = setup();
        // 预置一份"用户正在用"的 claude 配置
        std::fs::create_dir_all(dir.path().join("home/.claude")).unwrap();
        std::fs::write(
            dir.path().join("home/.claude/settings.json"),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://relay", "ANTHROPIC_AUTH_TOKEN": "sk-old"}}).to_string(),
        ).unwrap();

        let p = Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into()));
        core.add_provider(p, Some("sk-new")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();

        // 自动创建了 imported，且旧 key 进了（mock）keyring
        let imported = core.list(Some(ToolKind::ClaudeCode)).unwrap()
            .into_iter().find(|p| p.id == "imported").unwrap();
        assert_eq!(imported.base_url.as_deref(), Some("https://relay"));

        // 切回 imported 能恢复旧配置
        core.use_provider(ToolKind::ClaudeCode, "imported").unwrap();
        let s = std::fs::read_to_string(dir.path().join("home/.claude/settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-old");
    }

    #[test]
    fn use_official_clears_claude_env() {
        let (dir, core) = setup();
        core.add_provider(Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into())), Some("k")).unwrap();
        core.add_provider(Provider::new("official", ToolKind::ClaudeCode, None), None).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();
        core.use_provider(ToolKind::ClaudeCode, "official").unwrap();

        let s = std::fs::read_to_string(dir.path().join("home/.claude/settings.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert!(doc["env"].get("ANTHROPIC_BASE_URL").is_none());
    }

    #[test]
    fn remove_active_rejected() {
        let (_dir, core) = setup();
        core.add_provider(Provider::new("kimi", ToolKind::Codex, Some("https://x".into())), Some("k")).unwrap();
        core.use_provider(ToolKind::Codex, "kimi").unwrap();
        let err = core.remove(ToolKind::Codex, "kimi").unwrap_err();
        assert!(matches!(err, CoreError::ActiveProviderRemoval(_)));
    }

    #[test]
    fn use_creates_backup() {
        let (dir, core) = setup();
        std::fs::create_dir_all(dir.path().join("home/.claude")).unwrap();
        std::fs::write(dir.path().join("home/.claude/settings.json"), "{}").unwrap();
        core.add_provider(Provider::new("kimi", ToolKind::ClaudeCode, Some("https://x".into())), Some("k")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi").unwrap();
        let backups = std::fs::read_dir(dir.path().join("data/backups/claude")).unwrap();
        assert_eq!(backups.count(), 1);
    }

    #[test]
    fn use_unknown_provider_errors() {
        let (_dir, core) = setup();
        assert!(core.use_provider(ToolKind::Codex, "nope").is_err());
    }

    #[test]
    fn use_opencode_official_removes_previous_custom() {
        let (dir, core) = setup();
        core.add_provider(Provider::new("kimi", ToolKind::OpenCode, Some("https://x".into())), Some("k")).unwrap();
        core.add_provider(Provider::new("official", ToolKind::OpenCode, None), None).unwrap();
        core.use_provider(ToolKind::OpenCode, "kimi").unwrap();
        core.use_provider(ToolKind::OpenCode, "official").unwrap();

        let doc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("home/.config/opencode/opencode.json")).unwrap(),
        ).unwrap();
        assert!(doc["provider"].get("kimi").is_none());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p agent-switch-core service`
Expected: FAIL（`Core` 未定义）

- [ ] **Step 3: 实现**（测试模块之上）

```rust
use crate::adapters::{adapter_for, ToolAdapter};
use crate::backup::{backup_file, list_backups, restore};
use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use crate::store::db::Database;
use crate::store::secrets::{KeyringStore, SecretStore};
use std::path::{Path, PathBuf};

pub struct Core {
    db: Database,
    secrets: Box<dyn SecretStore>,
    home: PathBuf,
    data_dir: PathBuf,
}

impl Core {
    /// 测试与嵌入用：显式指定 home（工具配置根）与 data_dir（自身数据根）
    pub fn with_paths(home: &Path, data_dir: &Path, secrets: Box<dyn SecretStore>) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Self {
            db: Database::open(&data_dir.join("agent-switch.db"))?,
            secrets,
            home: home.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        })
    }

    /// 真实环境：home = 用户主目录，data_dir = ~/.config/agent-switch，系统钥匙串
    pub fn for_current_user() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| CoreError::Keyring("cannot locate home dir".into()))?;
        let data = home.join(".config").join("agent-switch");
        Self::with_paths(&home, &data, Box::new(KeyringStore::new()))
    }

    fn adapter(&self, tool: ToolKind) -> Box<dyn ToolAdapter> {
        adapter_for(tool, &self.home)
    }

    pub fn add_provider(&self, mut provider: Provider, api_key: Option<&str>) -> Result<()> {
        if let Some(key) = api_key {
            self.secrets.set(&provider.key_ref, key)?;
        }
        provider.is_active = false;
        self.db.upsert(&provider)
    }

    /// 编辑 provider（key 传 Some 时同时更换钥匙串中的密钥）
    pub fn update_provider(&self, provider: &Provider, api_key: Option<&str>) -> Result<()> {
        if self.db.get(provider.tool, &provider.id)?.is_none() {
            return Err(CoreError::ProviderNotFound(format!("{}/{}", provider.tool, provider.id)));
        }
        if let Some(key) = api_key {
            self.secrets.set(&provider.key_ref, key)?;
        }
        self.db.upsert(provider)
    }

    pub fn list(&self, tool: Option<ToolKind>) -> Result<Vec<Provider>> {
        self.db.list(tool)
    }

    pub fn current(&self, tool: ToolKind) -> Result<Option<Provider>> {
        Ok(self.db.list(Some(tool))?.into_iter().find(|p| p.is_active))
    }

    pub fn remove(&self, tool: ToolKind, id: &str) -> Result<()> {
        let p = self.db.get(tool, id)?.ok_or_else(|| CoreError::ProviderNotFound(format!("{tool}/{id}")))?;
        if p.is_active {
            return Err(CoreError::ActiveProviderRemoval(format!("{tool}/{id}")));
        }
        self.secrets.delete(&p.key_ref)?;
        self.db.delete(tool, id)
    }

    /// 切换：备份 → 写入工具配置 → 更新 active
    pub fn use_provider(&self, tool: ToolKind, id: &str) -> Result<()> {
        self.ensure_imported(tool)?;
        let p = self
            .db
            .get(tool, id)?
            .ok_or_else(|| CoreError::ProviderNotFound(format!("{tool}/{id}")))?;

        let adapter = self.adapter(tool);
        let backup_dir = self.data_dir.join("backups").join(tool.as_str());
        for path in adapter.config_paths() {
            backup_file(&path, &backup_dir)?;
        }

        let key = if p.is_official() {
            None
        } else {
            Some(self.secrets.get(&p.key_ref)?)
        };

        // OpenCode 官方切换需要知道清理哪个自定义 provider
        let p = self.patch_official_removal(tool, p)?;
        adapter.apply(&p, key.as_deref())?;
        self.db.set_active(tool, id)
    }

    /// OpenCode 切官方时，把当前 active 的自定义 provider id 写入 extra.remove_provider
    fn patch_official_removal(&self, tool: ToolKind, mut p: Provider) -> Result<Provider> {
        if tool == ToolKind::OpenCode && p.is_official() {
            if let Some(cur) = self.current(tool)? {
                if !cur.is_official() {
                    p.extra = serde_json::json!({ "remove_provider": cur.id });
                }
            }
        }
        Ok(p)
    }

    /// 首次切换前：该工具若无 imported，则把当前实况存为 imported 快照
    pub fn ensure_imported(&self, tool: ToolKind) -> Result<()> {
        if self.db.get(tool, "imported")?.is_some() {
            return Ok(());
        }
        if let Some((p, key)) = self.adapter(tool).read_current()? {
            if let Some(k) = key.as_deref() {
                self.secrets.set(&p.key_ref, k)?;
            }
            self.db.upsert(&p)?;
        }
        Ok(())
    }

    /// 手动导入：覆盖更新 imported 快照
    pub fn import(&self, tool: ToolKind) -> Result<Option<Provider>> {
        match self.adapter(tool).read_current()? {
            Some((p, key)) => {
                if let Some(k) = key.as_deref() {
                    self.secrets.set(&p.key_ref, k)?;
                }
                self.db.upsert(&p)?;
                Ok(Some(p))
            }
            None => Ok(None),
        }
    }

    pub fn backups(&self, tool: ToolKind) -> Result<Vec<PathBuf>> {
        list_backups(&self.data_dir.join("backups").join(tool.as_str()))
    }

    pub fn restore_backup(&self, tool: ToolKind, backup: &Path) -> Result<()> {
        // 备份文件名形如 <原文件名>.<时间戳>；恢复到对应 config_paths 中同名文件
        let file_name = backup
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| CoreError::ConfigParse { path: backup.display().to_string(), msg: "bad backup name".into() })?;
        let original = file_name.rsplit_once('.').map(|(n, _)| n).unwrap_or(file_name);
        let adapter = self.adapter(tool);
        let target = adapter
            .config_paths()
            .into_iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(original))
            .ok_or_else(|| CoreError::ConfigParse { path: backup.display().to_string(), msg: format!("no config file named {original}") })?;
        restore(backup, &target)
    }
}
```

`lib.rs` 完整内容：

```rust
pub mod adapters;
pub mod backup;
pub mod error;
pub mod models;
pub mod presets;
pub mod service;
pub mod store;

pub use error::{CoreError, Result};
pub use models::{Provider, ToolKind};
pub use service::Core;
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p agent-switch-core`
Expected: 全部通过（含此前任务的测试）

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(core): service layer with use/import/remove"
```

---

### Task 12: CLI 非交互命令

**Files:**
- Modify: `crates/cli/src/main.rs`
- Create: `crates/cli/tests/cli.rs`

CLI 结构（clap derive）：

```rust
use agent_switch_core::{Core, Provider, ToolKind};
use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(name = "asw", version, about = "Switch API providers for AI coding tools")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 添加 provider（无参数进入交互模式）
    Add {
        #[arg(long)] tool: Option<ToolKind>,
        #[arg(long)] name: Option<String>,
        #[arg(long)] base_url: Option<String>,
        #[arg(long)] key: Option<String>,
        /// 工具特有配置，可重复：--set model=kimi-k2.5
        #[arg(long = "set", value_parser = parse_kv)]
        sets: Vec<(String, String)>,
    },
    /// 列出 providers
    Ls { #[arg(long)] tool: Option<ToolKind> },
    /// 切换 provider
    Use { name: String, #[arg(long)] tool: Option<ToolKind> },
    /// 显示各工具当前生效的 provider
    Current,
    /// 修改 provider
    Edit {
        name: String,
        #[arg(long)] tool: Option<ToolKind>,
        #[arg(long)] base_url: Option<String>,
        #[arg(long)] key: Option<String>,
        #[arg(long = "set", value_parser = parse_kv)]
        sets: Vec<(String, String)>,
    },
    /// 删除 provider（active 不可删）
    Rm { name: String, #[arg(long)] tool: Option<ToolKind> },
    /// 列出内置预设
    Presets,
    /// 手动导入工具当前配置为 imported 快照
    Import { #[arg(long)] tool: Option<ToolKind> },
    /// 备份管理
    Backup { #[command(subcommand)] cmd: BackupCmd },
    /// 生成 shell 补全
    Completion { shell: Shell },
}

#[derive(Subcommand)]
enum BackupCmd {
    Ls { #[arg(long)] tool: ToolKind },
    Restore { #[arg(long)] tool: ToolKind, file: std::path::PathBuf },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s}"))
}
```

ToolKind 需实现 `clap::ValueEnum` 或走 `FromStr`：clap4 derive 对实现了 `FromStr<Err: Display>` 的类型可直接用作参数（`value_parser` 自动）。在 `models.rs` 中 `impl FromStr` 已满足；无需 ValueEnum。

- [ ] **Step 1: 写失败测试**（`crates/cli/tests/cli.rs`）

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// 以隔离 HOME 运行 asw
fn asw(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("asw").unwrap();
    cmd.env("HOME", home)
        .env("ASW_DATA_DIR", home.join(".asw-data")) // 测试注入数据目录
        .env("ASW_MOCK_SECRETS", "1");             // 测试用内存密钥库
    cmd
}

#[test]
fn add_ls_use_current_flow() {
    let home = tempfile::tempdir().unwrap();

    asw(home.path())
        .args(["add", "--tool", "claude", "--name", "kimi",
               "--base-url", "https://api.moonshot.cn/anthropic", "--key", "sk-1",
               "--set", "model=kimi-k2.5"])
        .assert()
        .success();

    asw(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));

    asw(home.path()).args(["use", "kimi", "--tool", "claude"]).assert().success();

    let settings = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(settings.contains("api.moonshot.cn"));

    asw(home.path())
        .args(["current"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));
}

#[test]
fn rm_active_fails() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "k", "--base-url", "https://x", "--key", "1"])
        .assert()
        .success();
    asw(home.path()).args(["use", "k", "--tool", "codex"]).assert().success();
    asw(home.path())
        .args(["rm", "k", "--tool", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("active"));
}

#[test]
fn presets_lists_all_tools() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["presets"])
        .assert()
        .success()
        .stdout(predicate::str::contains("official").and(predicate::str::contains("kimi")));
}
```

测试隔离需要两个环境变量注入点，在 main.rs 实现：
- `ASW_DATA_DIR`：替代默认 `~/.config/agent-switch`
- `ASW_MOCK_SECRETS=1`：用内存 SecretStore（仅测试）

为此 core 需暴露 `MockStore`：`lib.rs` 中 `pub use store::secrets::{KeyringStore, MockStore, SecretStore};`

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p asw`
Expected: FAIL（main.rs 还是占位）

- [ ] **Step 3: 实现 main.rs**

```rust
use agent_switch_core::store::secrets::{KeyringStore, MockStore};
use agent_switch_core::{presets::presets_for, Core, CoreError, Provider, ToolKind};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::path::PathBuf;

// ... Cmd / BackupCmd / parse_kv 定义如上述结构 ...

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// 环境契约（与 tests/cli.rs 的 asw() 辅助函数严格一致）：
/// - `HOME`：工具配置根。测试注入 tempdir；真实环境即用户主目录。
/// - `ASW_DATA_DIR`：自身数据目录（db/backups），缺省 `$HOME/.config/agent-switch`。
/// - `ASW_MOCK_SECRETS=1`：用内存 MockStore 替代系统钥匙串（仅测试/冒烟）。
fn build_core() -> Result<Core, CoreError> {
    let use_mock = std::env::var("ASW_MOCK_SECRETS").ok().as_deref() == Some("1");
    if let Ok(home) = std::env::var("HOME") {
        let data = std::env::var_os("ASW_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&home).join(".config").join("agent-switch"));
        let secrets: Box<dyn agent_switch_core::store::secrets::SecretStore> = if use_mock {
            Box::new(MockStore::default())
        } else {
            Box::new(KeyringStore::new())
        };
        return Core::with_paths(std::path::Path::new(&home), &data, secrets);
    }
    Core::for_current_user() // 无 HOME 的极端环境（如部分 Windows 服务）兜底
}
```

注意：CLI 不依赖 `dirs` crate——`HOME` 环境变量覆盖目标平台（macOS/Linux），无 HOME 时兜底走 `Core::for_current_user()`（core 内部用 `dirs`）。CLI 的 Cargo.toml 无需改动。

剩余分发逻辑（含 Task 12 版 cmd_add——仅非交互路径，`name` 缺失时报错提示，交互路径 Task 13 补全）：

```rust
/// Task 12 版：仅支持参数齐全的非交互调用；交互模式在 Task 13 实现
fn cmd_add(
    tool: Option<ToolKind>,
    name: Option<String>,
    base_url: Option<String>,
    key: Option<String>,
    sets: Vec<(String, String)>,
) -> Result<(), CoreError> {
    let (Some(tool), Some(name)) = (tool, name) else {
        return Err(CoreError::ConfigParse {
            path: String::new(),
            msg: "interactive mode not yet implemented; pass --tool and --name".into(),
        });
    };
    let mut map = serde_json::Map::new();
    for (k, v) in sets {
        map.insert(k, serde_json::Value::String(v));
    }
    let mut p = Provider::new(&name, tool, base_url);
    if !map.is_empty() {
        p.extra = serde_json::Value::Object(map);
    }
    let core = build_core()?;
    core.add_provider(p, key.as_deref())?;
    println!("added {tool}/{name}");
    Ok(())
}

fn run(cli: Cli) -> Result<(), CoreError> {
    match cli.cmd {
        Cmd::Add { tool, name, base_url, key, sets } => cmd_add(tool, name, base_url, key, sets),
        Cmd::Ls { tool } => {
            let core = build_core()?;
            let list = core.list(tool)?;
            print_table(&list);
            Ok(())
        }
        Cmd::Use { name, tool } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            core.use_provider(tool, &name)?;
            println!("switched {} -> {}", tool, name);
            Ok(())
        }
        Cmd::Current => {
            let core = build_core()?;
            for t in ToolKind::ALL {
                match core.current(t)? {
                    Some(p) => println!("{t}: {} ({})", p.id, p.base_url.as_deref().unwrap_or("official")),
                    None => println!("{t}: (none)"),
                }
            }
            Ok(())
        }
        Cmd::Rm { name, tool } => {
            let core = build_core()?;
            let tool = resolve_tool(&core, &name, tool)?;
            core.remove(tool, &name)?; // active 时 CoreError::ActiveProviderRemoval，消息含 "active"
            println!("removed {tool}/{name}");
            Ok(())
        }
        Cmd::Presets => {
            for t in ToolKind::ALL {
                println!("[{t}]");
                for p in presets_for(t) {
                    println!("  {:<12} {:<20} {}", p.id, p.label, p.base_url.unwrap_or("(官方)"));
                }
            }
            Ok(())
        }
        // Edit / Import / Backup / Completion 在 Task 13 实现
        _ => todo!("Task 13"),
    }
}

/// use/rm/edit 缺省 --tool 时：全库查同名 provider；
/// 唯一 → 用之；多个 → 报错提示加 --tool；零 → ProviderNotFound
/// 注：spec 第 6 节原表述为"多个时交互选择"，此处有意偏离为报错——
/// CLI 脚本化场景下确定性优于便捷性，交互选择留给未来 GUI。
fn resolve_tool(core: &Core, name: &str, tool: Option<ToolKind>) -> Result<ToolKind, CoreError> {
    if let Some(t) = tool {
        return Ok(t);
    }
    let matches: Vec<ToolKind> = core
        .list(None)?
        .into_iter()
        .filter(|p| p.id == name)
        .map(|p| p.tool)
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(CoreError::ProviderNotFound(name.into())),
        _ => Err(CoreError::ConfigParse {
            path: String::new(),
            msg: format!("'{name}' exists in multiple tools {matches:?}; pass --tool"),
        }),
    }
}
```

`print_table`：简单格式化输出（`{tool}  {id}  {base_url or "official"}  {active 标记 *}`），无需外部表格库。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p asw`
Expected: 集成测试通过

- [ ] **Step 5: 提交**

```bash
git add -A && git commit -m "feat(cli): non-interactive commands (add/ls/use/current/rm/presets)"
```

---

### Task 13: CLI 交互模式 + 剩余命令 + 端到端

**Files:**
- Modify: `crates/cli/src/main.rs`
- Modify: `crates/cli/tests/cli.rs`

`cmd_add` 交互路径（任一必需参数缺失时触发）：

```rust
fn cmd_add(
    tool: Option<ToolKind>,
    name: Option<String>,
    base_url: Option<String>,
    key: Option<String>,
    sets: Vec<(String, String)>,
) -> Result<(), CoreError> {
    let core = build_core()?;

    let tool = match tool {
        Some(t) => t,
        None => {
            let items: Vec<String> = ToolKind::ALL.iter().map(|t| t.to_string()).collect();
            let idx = dialoguer::Select::new()
                .with_prompt("选择工具")
                .items(&items)
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
            ToolKind::ALL[idx]
        }
    };

    // 非交互判定：只要 --name 已提供就不再进交互。
    // --name 有 + --base-url 无 => 官方 provider（base_url=None），无需 key；
    // --name 缺失 => 交互：选预设或自定义。
    let (name, base_url, mut extra) = match name {
        Some(n) => (n, base_url, serde_json::Value::Null),
        None => {
            let presets = presets_for(tool);
            let labels: Vec<String> = presets.iter().map(|p| p.label.to_string()).chain(["自定义".into()]).collect();
            let idx = dialoguer::Select::new()
                .with_prompt("选择供应商")
                .items(&labels)
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?;
            if idx == presets.len() {
                let n = dialoguer::Input::<String>::new()
                    .with_prompt("名称")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                let u = dialoguer::Input::<String>::new()
                    .with_prompt("Base URL")
                    .interact_text()
                    .map_err(|e| CoreError::Keyring(e.to_string()))?;
                (n, Some(u), serde_json::Value::Null)
            } else {
                let p = &presets[idx];
                (p.id.to_string(), p.base_url.map(String::from), p.extra.clone())
            }
        }
    };

    // 官方无需 key
    let key = if base_url.is_none() {
        None
    } else {
        Some(match key {
            Some(k) => k,
            None => dialoguer::Password::new()
                .with_prompt("API Key")
                .interact()
                .map_err(|e| CoreError::Keyring(e.to_string()))?,
        })
    };

    // --set 合并进 extra
    let mut map = extra.as_object().cloned().unwrap_or_default();
    for (k, v) in sets {
        map.insert(k, serde_json::Value::String(v));
    }
    if !map.is_empty() {
        extra = serde_json::Value::Object(map);
    }

    let mut p = Provider::new(&name, tool, base_url);
    p.extra = extra;
    let official = p.is_official();
    core.add_provider(p, key.as_deref())?;
    println!("added {tool}/{name}");
    if !official {
        println!("run `asw use {name} --tool {tool}` to switch");
    }
    Ok(())
}
```

Edit 实现：取出旧 provider → 应用 `base_url`/`sets` 覆盖 → `core.update_provider(&p, key.as_deref())`。

Backup 子命令：

```rust
BackupCmd::Ls { tool } => {
    let core = build_core()?;
    for b in core.backups(tool)? {
        println!("{}", b.display());
    }
    Ok(())
}
BackupCmd::Restore { tool, file } => {
    let core = build_core()?;
    core.restore_backup(tool, &file)?;
    println!("restored {}", file.display());
    Ok(())
}
```

Completion：

```rust
Cmd::Completion { shell } => {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
```

- [ ] **Step 1: 补端到端测试**（追加 `crates/cli/tests/cli.rs`）

```rust
#[test]
fn switch_back_to_imported_snapshot() {
    let home = tempfile::tempdir().unwrap();
    // 预置"正在使用"的中转配置
    fs::create_dir_all(home.path().join(".claude")).unwrap();
    fs::write(
        home.path().join(".claude/settings.json"),
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://relay","ANTHROPIC_AUTH_TOKEN":"sk-old"}}"#,
    ).unwrap();

    asw(home.path())
        .args(["add", "--tool", "claude", "--name", "kimi", "--base-url", "https://x", "--key", "sk-new"])
        .assert().success();
    asw(home.path()).args(["use", "kimi", "--tool", "claude"]).assert().success();
    asw(home.path()).args(["use", "imported", "--tool", "claude"]).assert().success();

    let s = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("https://relay"));
    assert!(s.contains("sk-old"));
}

#[test]
fn codex_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "kimi", "--base-url", "https://api.moonshot.cn/v1", "--key", "sk-1"])
        .assert().success();
    asw(home.path()).args(["use", "kimi", "--tool", "codex"]).assert().success();

    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(cfg.contains("model_provider = \"kimi\""));
    let auth = fs::read_to_string(home.path().join(".codex/auth.json")).unwrap();
    assert!(auth.contains("sk-1"));

    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "official"])
        .assert().success(); // 无 --base-url => 官方
    asw(home.path()).args(["use", "official", "--tool", "codex"]).assert().success();
    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(!cfg.contains("model_provider ="));
}

#[test]
fn completion_generates() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["completion", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef asw"));
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p asw`
Expected: FAIL（交互路径/剩余子命令未实现）

- [ ] **Step 3: 实现上述 cmd_add / Edit / Backup / Completion**

- [ ] **Step 4: 全量测试**

Run: `cargo test --workspace`
Expected: 全部通过

- [ ] **Step 5: 手动冒烟（真实 home 之外的临时 HOME）**

```bash
HOME=/tmp/asw-smoke ASW_DATA_DIR=/tmp/asw-smoke/.data ASW_MOCK_SECRETS=1 cargo run -p asw -- add --tool claude --name kimi --base-url https://api.moonshot.cn/anthropic --key sk-x
HOME=/tmp/asw-smoke ASW_DATA_DIR=/tmp/asw-smoke/.data ASW_MOCK_SECRETS=1 cargo run -p asw -- use kimi --tool claude
cat /tmp/asw-smoke/.claude/settings.json   # 应含 moonshot URL
rm -rf /tmp/asw-smoke
```

- [ ] **Step 6: 提交**

```bash
git add -A && git commit -m "feat(cli): interactive add, edit, backup, completion"
```

---

## 完成标准

- `cargo test --workspace` 全绿（core 单测 + CLI 集成测试）
- `cargo clippy --workspace -- -D warnings` 无警告
- `cargo fmt --check` 通过
- 三工具端到端切换（含切回 imported / official）在隔离 HOME 下验证通过
