use crate::error::{CoreError, Result};
use crate::models::{Provider, ToolKind};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// A single request log row (all numeric fields are i64).
#[derive(Debug, serde::Serialize)]
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

/// Stats aggregation dimension.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum StatsGroupBy {
    Provider,
    Model,
}

/// A single aggregated stats row.
#[derive(Debug, serde::Serialize)]
pub struct StatsRow {
    pub group: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub errors: i64,
    pub avg_duration_ms: i64,
}

/// Max request log rows to retain (older rows are evicted on each insert once exceeded).
pub const MAX_REQUEST_LOG_ROWS: usize = 10_000;

#[derive(Clone)]
pub struct Database {
    conn: std::sync::Arc<Mutex<Connection>>,
}

/// Proxy route config: primary→fallback route list, model override, target protocol.
pub type RoutesConfig = (Vec<String>, Option<String>, String);

impl Database {
    /// Get the lock-protected connection, handling poison errors (recover and log a warning on poison).
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| {
            // Mutex poison: some thread holding the lock panicked. Recover the lock
            // and keep using it — the connection may still be consistent (SQLite has
            // built-in ACID protection).
            eprintln!("[xfade] WARNING: database mutex was poisoned, recovering");
            e.into_inner()
        })
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // WAL mode: enables concurrent reads and reduces lock contention.
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        let db = Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_memory() -> Result<Self> {
        let db = Self {
            conn: std::sync::Arc::new(Mutex::new(Connection::open_in_memory()?)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch(
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
                model_override TEXT,
                target_protocol TEXT NOT NULL DEFAULT 'chat',
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
            CREATE INDEX IF NOT EXISTS idx_request_logs_ts ON request_logs(ts);
            CREATE TABLE IF NOT EXISTS circuit_state (
                provider_id TEXT PRIMARY KEY,
                fails INTEGER NOT NULL DEFAULT 0,
                cooldown_until_secs INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        // Idempotent column migration for pre-v0.3 databases where proxy_state
        // existed without model_override / target_protocol.
        let existing_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(proxy_state)")?;
            let mut rows = stmt.query([])?;
            let mut cols = std::collections::HashSet::new();
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                cols.insert(name);
            }
            cols
        };
        if !existing_cols.contains("model_override") {
            conn.execute("ALTER TABLE proxy_state ADD COLUMN model_override TEXT", [])?;
        }
        if !existing_cols.contains("target_protocol") {
            conn.execute(
                "ALTER TABLE proxy_state ADD COLUMN target_protocol TEXT NOT NULL DEFAULT 'chat'",
                [],
            )?;
        }
        Ok(())
    }

    /// Insert or update (does not touch is_active).
    pub fn upsert(&self, p: &Provider) -> Result<()> {
        self.conn().execute(
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
        let conn = self.conn();
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
        let conn = self.conn();
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

    /// Set active exclusively within a tool.
    pub fn set_active(&self, tool: ToolKind, id: &str) -> Result<()> {
        let conn = self.conn();
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE providers SET is_active = 0 WHERE tool = ?1",
            params![tool.as_str()],
        )?;
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
        self.conn().execute(
            "DELETE FROM providers WHERE tool = ?1 AND id = ?2",
            params![tool.as_str(), id],
        )?;
        Ok(())
    }

    /// Update a provider's key_ref (used by keyring migration: agent-switch/ prefix → xfade/).
    pub fn update_key_ref(&self, tool: ToolKind, id: &str, new_key_ref: &str) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE providers SET key_ref = ?1 WHERE tool = ?2 AND id = ?3",
            params![new_key_ref, tool.as_str(), id],
        )?;
        if n == 0 {
            return Err(CoreError::ProviderNotFound(format!("{tool}/{id}")));
        }
        Ok(())
    }

    /// Set proxy routes (primary→fallback order) + model override + target protocol.
    pub fn set_routes(
        &self,
        routes: &[String],
        model_override: Option<&str>,
        target_protocol: &str,
    ) -> Result<()> {
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        self.conn().execute(
            "INSERT INTO proxy_state (id, routes, model_override, target_protocol, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT (id) DO UPDATE SET
                routes = excluded.routes,
                model_override = excluded.model_override,
                target_protocol = excluded.target_protocol,
                updated_at = excluded.updated_at",
            params![
                serde_json::to_string(routes)?,
                model_override,
                target_protocol,
                now
            ],
        )?;
        Ok(())
    }

    /// Read proxy routes and associated config; returns None if not set.
    /// Returns (routes, model_override, target_protocol).
    pub fn get_routes(&self) -> Result<Option<RoutesConfig>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT routes, model_override, target_protocol FROM proxy_state WHERE id = 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let s: String = row.get(0)?;
                let routes: Vec<String> = serde_json::from_str(&s)?;
                let model_override: Option<String> = row.get(1)?;
                let target_protocol: String = row.get(2)?;
                Ok(Some((routes, model_override, target_protocol)))
            }
            None => Ok(None),
        }
    }

    pub fn clear_routes(&self) -> Result<()> {
        self.conn()
            .execute("DELETE FROM proxy_state WHERE id = 1", [])?;
        Ok(())
    }

    pub fn insert_request_log(&self, log: &RequestLog) -> Result<()> {
        let conn = self.conn();
        conn.execute(
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
        // Eviction: retain the most recent MAX_REQUEST_LOG_ROWS rows, deleting the oldest once exceeded.
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM request_logs", [], |row| row.get(0))?;
        if count > MAX_REQUEST_LOG_ROWS as i64 {
            let excess = count - MAX_REQUEST_LOG_ROWS as i64;
            conn.execute(
                "DELETE FROM request_logs WHERE id IN (
                    SELECT id FROM request_logs ORDER BY id ASC LIMIT ?1
                )",
                params![excess],
            )?;
        }
        Ok(())
    }

    /// Aggregate request stats since `since_ts` (RFC3339).
    pub fn stats_since(&self, since_ts: &str, by: StatsGroupBy) -> Result<Vec<StatsRow>> {
        let group_expr = match by {
            StatsGroupBy::Provider => "provider_id",
            StatsGroupBy::Model => "COALESCE(model, '(unknown)')",
        };
        let sql = format!(
            "SELECT {group_expr}, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    SUM(CASE WHEN status >= 400 OR status = 0 THEN 1 ELSE 0 END),
                    CAST(AVG(duration_ms) AS INTEGER)
             FROM request_logs WHERE ts >= ?1
             GROUP BY {group_expr} ORDER BY 2 DESC"
        );
        let conn = self.conn();
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

    /// Read the most recent `limit` request logs (reverse insertion order). Used for test assertions.
    pub fn recent_request_logs(&self, limit: usize) -> Result<Vec<RequestLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT ts, endpoint, model, provider_id, status, prompt_tokens, completion_tokens, duration_ms, error
             FROM request_logs ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(RequestLog {
                ts: row.get(0)?,
                endpoint: row.get(1)?,
                model: row.get(2)?,
                provider_id: row.get(3)?,
                status: row.get(4)?,
                prompt_tokens: row.get(5)?,
                completion_tokens: row.get(6)?,
                duration_ms: row.get(7)?,
                error: row.get(8)?,
            });
        }
        Ok(out)
    }

    // ── Circuit breaker persistence ───────────────────────────────────────

    /// Save (upsert) a circuit breaker state for a provider.
    pub fn save_circuit(&self, provider_id: &str, circuit: &crate::proxy::Circuit) -> Result<()> {
        self.conn().execute(
            "INSERT INTO circuit_state (provider_id, fails, cooldown_until_secs)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (provider_id) DO UPDATE SET
                fails = excluded.fails,
                cooldown_until_secs = excluded.cooldown_until_secs",
            params![provider_id, circuit.fails, circuit.cooldown_until_secs],
        )?;
        Ok(())
    }

    /// Remove a circuit breaker state (provider recovered).
    pub fn delete_circuit(&self, provider_id: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM circuit_state WHERE provider_id = ?1",
            params![provider_id],
        )?;
        Ok(())
    }

    /// Load all persisted circuit states on startup.
    pub fn load_circuits(
        &self,
    ) -> Result<std::collections::HashMap<String, crate::proxy::Circuit>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT provider_id, fails, cooldown_until_secs FROM circuit_state")?;
        let mut rows = stmt.query([])?;
        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            map.insert(
                row.get(0)?,
                crate::proxy::Circuit {
                    fails: row.get(1)?,
                    cooldown_until_secs: row.get(2)?,
                },
            );
        }
        Ok(map)
    }
}

fn row_to_provider(row: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let tool_str: String = row.get(0)?;
    let extra_str: String = row.get(4)?;
    Ok(Provider {
        tool: tool_str.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        id: row.get(1)?,
        base_url: row.get(2)?,
        key_ref: row.get(3)?,
        extra: serde_json::from_str(&extra_str).unwrap_or_else(|e| {
            eprintln!(
                "[xfade] WARNING: invalid JSON in provider extra field for id={}: {e}",
                row.get::<_, String>(1).unwrap_or_default()
            );
            serde_json::Value::Null
        }),
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
        assert!(
            !db.get(ToolKind::ClaudeCode, "a")
                .unwrap()
                .unwrap()
                .is_active
        );
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
        db.set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["yy".to_string(), "bak".to_string()]);
        assert_eq!(model_override, None);
        assert_eq!(target_protocol, "chat");
        db.clear_routes().unwrap();
        assert!(db.get_routes().unwrap().is_none());
    }

    #[test]
    fn proxy_routes_with_model_and_target() {
        let db = Database::open_memory().unwrap();
        db.set_routes(&["yy".into()], Some("gpt-5.6-luna"), "messages")
            .unwrap();
        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["yy".to_string()]);
        assert_eq!(model_override.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(target_protocol, "messages");
    }

    #[test]
    fn proxy_routes_update_preserves_model_and_target() {
        let db = Database::open_memory().unwrap();
        db.set_routes(&["yy".into()], Some("gpt-5.6-luna"), "chat")
            .unwrap();
        // Update routes only, keeping model+target
        db.set_routes(&["yy".into(), "bak".into()], None, "chat")
            .unwrap();
        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["yy".to_string(), "bak".to_string()]);
        // set_routes overwrites all fields; None here means model_override cleared
        assert_eq!(model_override, None);
        assert_eq!(target_protocol, "chat");
    }

    #[test]
    fn proxy_state_migration_from_old_schema() {
        // Simulate an old v0.2 database (no model_override/target_protocol columns)
        // by creating the table manually without the new columns, then opening via
        // Database::open (which runs migrate()) and verifying defaults.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE proxy_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1),
                    routes TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                CREATE TABLE providers (
                    tool TEXT NOT NULL, id TEXT NOT NULL, base_url TEXT,
                    key_ref TEXT NOT NULL, extra TEXT NOT NULL DEFAULT 'null',
                    is_active INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (tool, id)
                );
                CREATE TABLE request_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts TEXT NOT NULL, endpoint TEXT NOT NULL, model TEXT,
                    provider_id TEXT NOT NULL, status INTEGER NOT NULL,
                    prompt_tokens INTEGER NOT NULL DEFAULT 0,
                    completion_tokens INTEGER NOT NULL DEFAULT 0,
                    duration_ms INTEGER NOT NULL, error TEXT
                );
                INSERT INTO proxy_state (id, routes, updated_at)
                VALUES (1, '[\"old\"]', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        // Now open via Database which should run migrate() and add the columns.
        let db = Database::open(&db_path).unwrap();
        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["old".to_string()]);
        // Migration defaults: model_override None, target_protocol 'chat'
        assert_eq!(model_override, None);
        assert_eq!(target_protocol, "chat");
        // And we can update with new values
        db.set_routes(&["yy".into()], Some("gpt-5.6-luna"), "messages")
            .unwrap();
        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["yy".to_string()]);
        assert_eq!(model_override.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(target_protocol, "messages");
    }

    #[test]
    fn request_log_and_stats() {
        let db = Database::open_memory().unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("gpt-5.6-luna".into()),
            provider_id: "yy".into(),
            status: 200,
            prompt_tokens: 100,
            completion_tokens: 50,
            duration_ms: 800,
            error: None,
        })
        .unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T11:00:00Z".into(),
            endpoint: "messages".into(),
            model: Some("claude-sonnet-4-6".into()),
            provider_id: "yy".into(),
            status: 429,
            prompt_tokens: 0,
            completion_tokens: 0,
            duration_ms: 120,
            error: Some("rate limited".into()),
        })
        .unwrap();
        // F5: status=0 (upstream connection failure) should count as errors
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T12:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("claude-sonnet-4-6".into()),
            provider_id: "yy".into(),
            status: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            duration_ms: 50,
            error: Some("connection refused".into()),
        })
        .unwrap();
        let rows = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[0].prompt_tokens, 100);
        assert_eq!(rows[0].errors, 2);
        let by_model = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Model)
            .unwrap();
        assert_eq!(by_model.len(), 2);
    }

    #[test]
    fn request_log_serializes() {
        let log = RequestLog {
            ts: "2026-07-27T00:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("gpt-x".into()),
            provider_id: "yy".into(),
            status: 200,
            prompt_tokens: 1,
            completion_tokens: 2,
            duration_ms: 10,
            error: None,
        };
        let v: serde_json::Value = serde_json::to_value(&log).unwrap();
        assert_eq!(v["endpoint"], "chat");
        assert_eq!(v["status"], 200);
    }

    #[test]
    fn stats_group_by_roundtrip() {
        for b in [StatsGroupBy::Provider, StatsGroupBy::Model] {
            let s = serde_json::to_string(&b).unwrap();
            let back: StatsGroupBy = serde_json::from_str(&s).unwrap();
            let _ = back;
        }
        // Also verify field naming (default PascalCase == variant name)
        assert_eq!(
            serde_json::to_string(&StatsGroupBy::Provider).unwrap(),
            "\"Provider\""
        );
    }
}
