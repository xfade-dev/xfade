use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 单条请求日志（数值均为 i64）
pub struct RequestLog {
    pub ts: String,
    pub endpoint: String,
    pub model: Option<String>,
    pub provider_id: String,
    pub status: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
}

/// 统计聚合维度
pub enum StatsGroupBy {
    Provider,
    Model,
}

/// 一行聚合统计
pub struct StatsRow {
    pub group: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub errors: i64,
    pub avg_duration_ms: i64,
}

#[derive(Clone)]
pub struct Database {
    conn: std::sync::Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Self { conn: std::sync::Arc::new(Mutex::new(Connection::open(path)?)) };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let db = Self { conn: std::sync::Arc::new(Mutex::new(Connection::open_in_memory()?)) };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.lock().unwrap().execute_batch(
            "CREATE TABLE IF NOT EXISTS providers (
                tool TEXT NOT NULL,
                id TEXT NOT NULL,
                base_url TEXT,
                key_ref TEXT NOT NULL,
                extra TEXT NOT NULL DEFAULT 'null',
                is_active INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (tool, id)
            );
            CREATE TABLE IF NOT EXISTS proxy_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                routes TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS request_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL, endpoint TEXT NOT NULL, model TEXT,
                provider_id TEXT NOT NULL, status INTEGER NOT NULL,
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL, error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_request_logs_ts ON request_logs(ts);",
        )?;
        Ok(())
    }

    /// 插入或更新（不触碰 is_active）
    pub fn upsert(&self, p: &Provider) -> Result<()> {
        self.conn.lock().unwrap().execute(
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
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
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;
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
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
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
        self.conn.lock().unwrap().execute(
            "DELETE FROM providers WHERE tool = ?1 AND id = ?2",
            params![tool.as_str(), id],
        )?;
        Ok(())
    }

    /// 设置代理路由（主→备 顺序）
    pub fn set_routes(&self, routes: &[String]) -> Result<()> {
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|e| CoreError::ConfigParse { path: "time".into(), msg: e.to_string() })?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO proxy_state (id, routes, updated_at) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET routes = excluded.routes, updated_at = excluded.updated_at",
            params![serde_json::to_string(routes)?, now],
        )?;
        Ok(())
    }

    /// 读取代理路由；未设置返回 None
    pub fn get_routes(&self) -> Result<Option<Vec<String>>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT routes FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let s: String = row.get(0)?;
                Ok(Some(serde_json::from_str(&s)?))
            }
            None => Ok(None),
        }
    }

    pub fn clear_routes(&self) -> Result<()> {
        self.conn.lock().unwrap().execute("DELETE FROM proxy_state WHERE id = 1", [])?;
        Ok(())
    }

    pub fn insert_request_log(&self, log: &RequestLog) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO request_logs (ts, endpoint, model, provider_id, status, prompt_tokens, completion_tokens, duration_ms, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                log.ts,
                log.endpoint,
                log.model,
                log.provider_id,
                log.status,
                log.prompt_tokens,
                log.completion_tokens,
                log.duration_ms,
                log.error,
            ],
        )?;
        Ok(())
    }

    /// 聚合 since_ts（RFC3339）之后的请求统计
    pub fn stats_since(&self, since_ts: &str, by: StatsGroupBy) -> Result<Vec<StatsRow>> {
        let group_expr = match by {
            StatsGroupBy::Provider => "provider_id",
            StatsGroupBy::Model => "COALESCE(model, '(unknown)')",
        };
        let sql = format!(
            "SELECT {group_expr}, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    SUM(CASE WHEN status >= 400 THEN 1 ELSE 0 END),
                    CAST(AVG(duration_ms) AS INTEGER)
             FROM request_logs WHERE ts >= ?1
             GROUP BY {group_expr} ORDER BY 2 DESC"
        );
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params![since_ts])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(StatsRow {
                group: row.get(0)?,
                requests: row.get(1)?,
                prompt_tokens: row.get(2)?,
                completion_tokens: row.get(3)?,
                errors: row.get(4)?,
                avg_duration_ms: row.get(5)?,
            });
        }
        Ok(out)
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

    #[test]
    fn proxy_routes_crud() {
        let db = Database::open_memory().unwrap();
        assert!(db.get_routes().unwrap().is_none());
        db.set_routes(&["yy".into(), "bak".into()]).unwrap();
        assert_eq!(db.get_routes().unwrap().unwrap(), vec!["yy".to_string(), "bak".to_string()]);
        db.clear_routes().unwrap();
        assert!(db.get_routes().unwrap().is_none());
    }

    #[test]
    fn request_log_and_stats() {
        let db = Database::open_memory().unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:00:00Z".into(), endpoint: "chat".into(),
            model: Some("gpt-5.6-luna".into()), provider_id: "yy".into(),
            status: 200, prompt_tokens: 100, completion_tokens: 50, duration_ms: 800, error: None,
        }).unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T11:00:00Z".into(), endpoint: "messages".into(),
            model: Some("claude-sonnet-4-6".into()), provider_id: "yy".into(),
            status: 429, prompt_tokens: 0, completion_tokens: 0, duration_ms: 120,
            error: Some("rate limited".into()),
        }).unwrap();
        let rows = db.stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 2);
        assert_eq!(rows[0].prompt_tokens, 100);
        assert_eq!(rows[0].errors, 1);
        let by_model = db.stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Model).unwrap();
        assert_eq!(by_model.len(), 2);
    }
}
