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
    /// Tool the provider belongs to (provider ids can collide across tools).
    pub tool: String,
    pub status: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    /// Session this request belongs to (see `crate::sessions`); NULL for
    /// pre-V2 rows and non-conversation endpoints (model listings).
    pub session_id: Option<String>,
}

/// A session row — the aggregated, eviction-isolated counterpart of
/// `request_logs` (which caps at `MAX_REQUEST_LOG_ROWS`): sessions survive
/// request-log eviction so the unified session view keeps long history.
#[derive(Debug, serde::Serialize)]
pub struct SessionRow {
    pub id: String,
    pub tool: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    /// running | ok | error
    pub status: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Cost is computed later (V2.5 per-model pricing); the column is
    /// forward-compatible and stays 0 for now.
    pub cost_usd_micros: i64,
    /// Privacy-bounded preview of the conversation opener (truncated by the
    /// writer). Full prompt text is never stored.
    pub first_prompt: Option<String>,
}

/// A model/route switch recorded on the session timeline. `session_id` is
/// NULL at switch time (a config-level switch has no request context) and is
/// backfilled by the forward pipeline from the next matching request.
#[derive(Debug, serde::Serialize)]
pub struct HandoverRow {
    pub id: i64,
    pub tool: String,
    pub session_id: Option<String>,
    pub from_model: Option<String>,
    pub to_model: Option<String>,
    pub ts: String,
}

/// Stats aggregation dimension.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub enum StatsGroupBy {
    Provider,
    Model,
    /// Group request logs by their session (`(detached)` when NULL).
    Session,
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

/// Sessions older than this are evicted when a new session is inserted.
pub const SESSION_RETENTION_DAYS: i64 = 90;
/// Hard cap on session rows (oldest evicted beyond the cap).
pub const MAX_SESSION_ROWS: i64 = 50_000;

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
        // busy_timeout: the daemon (serve) and CLI (use/sessions) are separate
        // processes writing to the same SQLite file — without a wait, writers
        // collide with immediate SQLITE_BUSY errors.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )?;
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
                duration_ms INTEGER NOT NULL, error TEXT,
                tool TEXT NOT NULL DEFAULT ''
            );
            CREATE INDEX IF NOT EXISTS idx_request_logs_ts ON request_logs(ts);
            CREATE TABLE IF NOT EXISTS circuit_state (
                provider_id TEXT PRIMARY KEY,
                fails INTEGER NOT NULL DEFAULT 0,
                cooldown_until_secs INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                tool TEXT NOT NULL DEFAULT '',
                fingerprint TEXT NOT NULL,
                provider_id TEXT,
                model TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                prompt_tokens INTEGER NOT NULL DEFAULT 0,
                completion_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd_micros INTEGER NOT NULL DEFAULT 0,
                first_prompt TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_fingerprint
                ON sessions(fingerprint, started_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at);
            CREATE TABLE IF NOT EXISTS handovers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tool TEXT NOT NULL DEFAULT '',
                session_id TEXT,
                from_model TEXT,
                to_model TEXT,
                ts TEXT NOT NULL
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
        if !existing_cols.contains("smart_enabled") {
            conn.execute(
                "ALTER TABLE proxy_state ADD COLUMN smart_enabled INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        // Same pattern for request_logs: pre-tool-column databases get a
        // default of '' (those rows only show up in unfiltered stats views).
        let mut stmt = conn.prepare("PRAGMA table_info(request_logs)")?;
        let mut rows = stmt.query([])?;
        let mut log_cols = std::collections::HashSet::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            log_cols.insert(name);
        }
        drop(rows);
        drop(stmt);
        if !log_cols.contains("tool") {
            conn.execute(
                "ALTER TABLE request_logs ADD COLUMN tool TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        if !log_cols.contains("session_id") {
            conn.execute("ALTER TABLE request_logs ADD COLUMN session_id TEXT", [])?;
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS repo_maps (
                root_path TEXT PRIMARY KEY,
                fingerprint TEXT NOT NULL,
                map_json TEXT NOT NULL,
                generated_at TEXT NOT NULL
            );",
            [],
        )?;
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
    /// An empty route set (post-`clear_routes`) also reads as None: "no
    /// usable routes" is a single representation for all consumers.
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
                if routes.is_empty() {
                    return Ok(None);
                }
                let model_override: Option<String> = row.get(1)?;
                let target_protocol: String = row.get(2)?;
                Ok(Some((routes, model_override, target_protocol)))
            }
            None => Ok(None),
        }
    }

    /// Clear the route set without touching the independent Smart Fader
    /// toggle (`smart_enabled`).
    pub fn clear_routes(&self) -> Result<()> {
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        self.conn().execute(
            "UPDATE proxy_state
             SET routes = '[]', model_override = NULL, updated_at = ?1
             WHERE id = 1",
            params![now],
        )?;
        Ok(())
    }

    /// Whether the Smart Fader (intent-based tiered routing) is enabled.
    /// Read on every request so `xfade proxy smart on/off` takes effect on
    /// a running daemon immediately.
    pub fn smart_enabled(&self) -> Result<bool> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT smart_enabled FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, i64>(0)? != 0),
            None => Ok(false),
        }
    }

    pub fn set_smart_enabled(&self, on: bool) -> Result<()> {
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|e| CoreError::ConfigParse {
                path: "time".into(),
                msg: e.to_string(),
            })?;
        // Full upsert: proxy_state has NOT NULL columns without defaults
        // (routes, updated_at), so a partial INSERT OR IGNORE would be
        // silently swallowed by the conflict clause.
        self.conn().execute(
            "INSERT INTO proxy_state (id, routes, model_override, target_protocol, updated_at, smart_enabled)
             VALUES (1, '[]', NULL, 'chat', ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET smart_enabled = excluded.smart_enabled",
            params![now, on as i64],
        )?;
        Ok(())
    }

    /// Cache a repo map (analyzer fingerprint + payload) keyed by root path.
    pub fn save_repo_map(&self, root_path: &str, fingerprint: &str, map_json: &str) -> Result<()> {
        let now = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|e| CoreError::ConfigParse {
                path: "time".into(),
                msg: e.to_string(),
            })?;
        self.conn().execute(
            "INSERT INTO repo_maps (root_path, fingerprint, map_json, generated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root_path) DO UPDATE SET
                fingerprint = excluded.fingerprint,
                map_json = excluded.map_json,
                generated_at = excluded.generated_at",
            params![root_path, fingerprint, map_json, now],
        )?;
        Ok(())
    }

    /// Fetch a cached repo map: (fingerprint, map_json).
    pub fn get_repo_map(&self, root_path: &str) -> Result<Option<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT fingerprint, map_json FROM repo_maps WHERE root_path = ?1")?;
        let mut rows = stmt.query(params![root_path])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    pub fn insert_request_log(&self, log: &RequestLog) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO request_logs (ts, endpoint, model, provider_id, tool, status, prompt_tokens, completion_tokens, duration_ms, error, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                log.ts,
                log.endpoint,
                log.model,
                log.provider_id,
                log.tool,
                log.status,
                log.prompt_tokens,
                log.completion_tokens,
                log.duration_ms,
                log.error,
                log.session_id,
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

    /// Aggregate request stats since `since_ts` (RFC3339). When `tool` is
    /// given, only rows logged for that tool's providers are counted —
    /// provider ids can collide across tools, so scoped views (TUI) must
    /// filter by tool to avoid bleeding another tool's numbers in.
    pub fn stats_since(
        &self,
        since_ts: &str,
        by: StatsGroupBy,
        tool: Option<&ToolKind>,
    ) -> Result<Vec<StatsRow>> {
        let group_expr = match by {
            StatsGroupBy::Provider => "provider_id",
            StatsGroupBy::Model => "COALESCE(model, '(unknown)')",
            StatsGroupBy::Session => "COALESCE(session_id, '(detached)')",
        };
        let tool_filter = if tool.is_some() { " AND tool = ?2" } else { "" };
        let sql = format!(
            "SELECT {group_expr}, COUNT(*),
                    COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0),
                    SUM(CASE WHEN status >= 400 OR status = 0 THEN 1 ELSE 0 END),
                    CAST(AVG(duration_ms) AS INTEGER)
             FROM request_logs WHERE ts >= ?1{tool_filter}
             GROUP BY {group_expr} ORDER BY 2 DESC"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match tool {
            Some(t) => stmt.query(params![since_ts, t.as_str()])?,
            None => stmt.query(params![since_ts])?,
        };
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
            "SELECT ts, endpoint, model, provider_id, tool, status, prompt_tokens, completion_tokens, duration_ms, error, session_id
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
                tool: row.get(4)?,
                status: row.get(5)?,
                prompt_tokens: row.get(6)?,
                completion_tokens: row.get(7)?,
                duration_ms: row.get(8)?,
                error: row.get(9)?,
                session_id: row.get(10)?,
            });
        }
        Ok(out)
    }

    // ── Sessions & handovers ──────────────────────────────────────────────

    /// Find the most recent session whose conversation-opener fingerprint
    /// matches and that started at/after `cutoff_ts` (RFC3339). Consecutive
    /// turns of one conversation reuse the session id this way.
    pub fn find_recent_session_id(
        &self,
        fingerprint: &str,
        cutoff_ts: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM sessions WHERE fingerprint = ?1 AND started_at >= ?2
             ORDER BY started_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![fingerprint, cutoff_ts])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Insert a new session row, evicting sessions past the retention window
    /// (and beyond the row cap) — new-session inserts are once per
    /// conversation, so the cleanup cost is amortized there.
    pub fn insert_session(
        &self,
        id: &str,
        fingerprint: &str,
        started_at: &str,
        first_prompt: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (id, fingerprint, started_at, first_prompt)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, fingerprint, started_at, first_prompt],
        )?;
        let retention_cutoff = (OffsetDateTime::now_utc()
            - time::Duration::days(SESSION_RETENTION_DAYS))
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());
        conn.execute(
            "DELETE FROM sessions WHERE started_at < ?1",
            params![retention_cutoff],
        )?;
        conn.execute(
            "DELETE FROM sessions WHERE id IN (
                SELECT id FROM sessions ORDER BY started_at DESC, id DESC
                LIMIT -1 OFFSET ?1
            )",
            params![MAX_SESSION_ROWS],
        )?;
        Ok(())
    }

    /// Fold one request's outcome into its session: accumulate tokens, refresh
    /// provider/model/tool (first writer wins for empty fields), set status
    /// and ended_at. Best-effort caller — errors are the caller's choice.
    #[allow(clippy::too_many_arguments)] // one flat record per logging site
    pub fn update_session_progress(
        &self,
        session_id: &str,
        tool: &str,
        provider_id: &str,
        model: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        status: &str,
        ended_at: &str,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET
                tool = CASE WHEN ?2 != '' THEN ?2 ELSE tool END,
                provider_id = CASE WHEN ?3 != '' THEN ?3 ELSE provider_id END,
                model = COALESCE(?4, model),
                prompt_tokens = prompt_tokens + ?5,
                completion_tokens = completion_tokens + ?6,
                status = ?7,
                ended_at = ?8
             WHERE id = ?1",
            params![
                session_id,
                tool,
                provider_id,
                model,
                prompt_tokens,
                completion_tokens,
                status,
                ended_at
            ],
        )?;
        Ok(())
    }

    /// Record a model/route switch on the session timeline. `session_id` is
    /// NULL at switch time (config-level switches have no request context);
    /// the forward pipeline backfills it from the next matching request.
    pub fn insert_handover(
        &self,
        tool: &str,
        from_model: Option<&str>,
        to_model: Option<&str>,
        ts: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO handovers (tool, session_id, from_model, to_model, ts)
             VALUES (?1, NULL, ?2, ?3, ?4)",
            params![tool, from_model, to_model, ts],
        )?;
        Ok(())
    }

    /// Attach `session_id` to the most recent not-yet-attributed handover
    /// within the time window (RFC3339 `cutoff_ts`).
    pub fn backfill_handover_session(&self, session_id: &str, cutoff_ts: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE handovers SET session_id = ?1
             WHERE session_id IS NULL AND ts >= ?2
               AND id = (SELECT id FROM handovers
                         WHERE session_id IS NULL AND ts >= ?2
                         ORDER BY id DESC LIMIT 1)",
            params![session_id, cutoff_ts],
        )?;
        Ok(())
    }

    /// List sessions, newest first.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, tool, provider_id, model, started_at, ended_at, status,
                    prompt_tokens, completion_tokens, cost_usd_micros, first_prompt
             FROM sessions ORDER BY started_at DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(SessionRow {
                id: row.get(0)?,
                tool: row.get(1)?,
                provider_id: row.get(2)?,
                model: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                status: row.get(6)?,
                prompt_tokens: row.get(7)?,
                completion_tokens: row.get(8)?,
                cost_usd_micros: row.get(9)?,
                first_prompt: row.get(10)?,
            });
        }
        Ok(out)
    }

    /// Fetch one session by id.
    pub fn get_session(&self, id: &str) -> Result<Option<SessionRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, tool, provider_id, model, started_at, ended_at, status,
                    prompt_tokens, completion_tokens, cost_usd_micros, first_prompt
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(SessionRow {
                id: row.get(0)?,
                tool: row.get(1)?,
                provider_id: row.get(2)?,
                model: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                status: row.get(6)?,
                prompt_tokens: row.get(7)?,
                completion_tokens: row.get(8)?,
                cost_usd_micros: row.get(9)?,
                first_prompt: row.get(10)?,
            })),
            None => Ok(None),
        }
    }

    /// Handovers attributed to one session, oldest first.
    pub fn list_handovers(&self, session_id: &str) -> Result<Vec<HandoverRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, tool, session_id, from_model, to_model, ts
             FROM handovers WHERE session_id = ?1 ORDER BY id ASC",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(HandoverRow {
                id: row.get(0)?,
                tool: row.get(1)?,
                session_id: row.get(2)?,
                from_model: row.get(3)?,
                to_model: row.get(4)?,
                ts: row.get(5)?,
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
    fn smart_enabled_defaults_off_and_roundtrips() {
        let db = Database::open_memory().unwrap();
        assert!(!db.smart_enabled().unwrap());
        db.set_smart_enabled(true).unwrap();
        assert!(db.smart_enabled().unwrap());
        db.set_smart_enabled(false).unwrap();
        assert!(!db.smart_enabled().unwrap());
    }

    #[test]
    fn clear_routes_keeps_smart_enabled() {
        // `xfade proxy clear` clears the route set; it must not silently
        // reset the independent Smart Fader toggle.
        let db = Database::open_memory().unwrap();
        db.set_routes(&["p1".to_string()], None, "chat").unwrap();
        db.set_smart_enabled(true).unwrap();

        db.clear_routes().unwrap();

        assert!(db.smart_enabled().unwrap());
        // No routes left to serve.
        assert!(db.get_routes().unwrap().is_none());
    }

    #[test]
    fn repo_maps_upsert_and_get() {
        let db = Database::open_memory().unwrap();
        assert!(db.get_repo_map("/proj").unwrap().is_none());

        db.save_repo_map("/proj", "fp1", r#"{"lang":"rust"}"#)
            .unwrap();
        let (fp, map) = db.get_repo_map("/proj").unwrap().unwrap();
        assert_eq!(fp, "fp1");
        assert_eq!(map, r#"{"lang":"rust"}"#);

        // Same root overwrites (cache semantics).
        db.save_repo_map("/proj", "fp2", r#"{"lang":"go"}"#)
            .unwrap();
        let (fp, _) = db.get_repo_map("/proj").unwrap().unwrap();
        assert_eq!(fp, "fp2");

        // Distinct roots coexist.
        db.save_repo_map("/other", "fp3", "{}").unwrap();
        assert!(db.get_repo_map("/other").unwrap().is_some());
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
        // The old request_logs schema had no tool column; migrate() must add it.
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:00:00Z".into(),
            endpoint: "chat".into(),
            model: None,
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 1,
            completion_tokens: 1,
            duration_ms: 5,
            error: None,
            session_id: None,
        })
        .unwrap();
        let rows = db
            .stats_since(
                "2026-01-01T00:00:00Z",
                StatsGroupBy::Provider,
                Some(&ToolKind::ClaudeCode),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 1);
    }

    #[test]
    fn request_log_and_stats() {
        let db = Database::open_memory().unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("gpt-5.6-luna".into()),
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 100,
            completion_tokens: 50,
            duration_ms: 800,
            error: None,
            session_id: None,
        })
        .unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T11:00:00Z".into(),
            endpoint: "messages".into(),
            model: Some("claude-sonnet-4-6".into()),
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 429,
            prompt_tokens: 0,
            completion_tokens: 0,
            duration_ms: 120,
            error: Some("rate limited".into()),
            session_id: None,
        })
        .unwrap();
        // F5: status=0 (upstream connection failure) should count as errors
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T12:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("claude-sonnet-4-6".into()),
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            duration_ms: 50,
            error: Some("connection refused".into()),
            session_id: None,
        })
        .unwrap();
        let rows = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[0].prompt_tokens, 100);
        assert_eq!(rows[0].errors, 2);
        let by_model = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Model, None)
            .unwrap();
        assert_eq!(by_model.len(), 2);
    }

    #[test]
    fn stats_tool_filter_isolates_same_id_providers_across_tools() {
        let db = Database::open_memory().unwrap();
        let log = |tool: &str| {
            db.insert_request_log(&RequestLog {
                ts: "2026-07-26T10:00:00Z".into(),
                endpoint: "chat".into(),
                model: None,
                provider_id: "dup".into(),
                tool: tool.into(),
                status: 200,
                prompt_tokens: 10,
                completion_tokens: 0,
                duration_ms: 100,
                error: None,
                session_id: None,
            })
            .unwrap();
        };
        log("claude");
        log("claude");
        log("codex");
        // Unfiltered: rows merge under the shared id.
        let all = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider, None)
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].requests, 3);
        // Tool-scoped: only that tool's rows count.
        let claude = db
            .stats_since(
                "2026-07-25T00:00:00Z",
                StatsGroupBy::Provider,
                Some(&ToolKind::ClaudeCode),
            )
            .unwrap();
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].requests, 2);
        let codex = db
            .stats_since(
                "2026-07-25T00:00:00Z",
                StatsGroupBy::Provider,
                Some(&ToolKind::Codex),
            )
            .unwrap();
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].requests, 1);
        // A tool with no logs returns nothing.
        assert!(db
            .stats_since(
                "2026-07-25T00:00:00Z",
                StatsGroupBy::Provider,
                Some(&ToolKind::Aider)
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn request_log_serializes() {
        let log = RequestLog {
            ts: "2026-07-27T00:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("gpt-x".into()),
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 1,
            completion_tokens: 2,
            duration_ms: 10,
            error: None,
            session_id: None,
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

    // ── sessions & handovers ─────────────────────────────────────────────

    #[test]
    fn session_sticky_lookup_and_progress() {
        let db = Database::open_memory().unwrap();
        // Nothing yet.
        assert!(db
            .find_recent_session_id("fp1", "2026-10-09T10:00:00Z")
            .unwrap()
            .is_none());
        db.insert_session("s1", "fp1", "2026-10-09T10:00:00Z", Some("hello world"))
            .unwrap();
        // Lookup takes cutoff = now - 30min: a session started at 10:00 is
        // visible while "now" <= 10:30.
        assert_eq!(
            db.find_recent_session_id("fp1", "2026-10-09T09:45:00Z")
                .unwrap()
                .as_deref(),
            Some("s1")
        );
        assert_eq!(
            db.find_recent_session_id("fp1", "2026-10-09T10:00:00Z")
                .unwrap()
                .as_deref(),
            Some("s1")
        );
        // …outside the window it does not (a fresh session is created).
        assert!(db
            .find_recent_session_id("fp1", "2026-10-09T10:00:01Z")
            .unwrap()
            .is_none());
        // Different fingerprint never matches.
        assert!(db
            .find_recent_session_id("fp2", "2026-10-09T10:30:00Z")
            .unwrap()
            .is_none());

        // Progress folding: accumulate tokens, first writer wins fields.
        db.update_session_progress(
            "s1",
            "claude",
            "kimi",
            Some("kimi-k2.5"),
            100,
            50,
            "ok",
            "2026-10-09T10:01:00Z",
        )
        .unwrap();
        db.update_session_progress(
            "s1",
            "claude",
            "kimi",
            None,
            10,
            5,
            "error",
            "2026-10-09T10:02:00Z",
        )
        .unwrap();
        let s = db.get_session("s1").unwrap().unwrap();
        assert_eq!(s.tool, "claude");
        assert_eq!(s.provider_id.as_deref(), Some("kimi"));
        assert_eq!(s.model.as_deref(), Some("kimi-k2.5")); // COALESCE keeps it
        assert_eq!(s.prompt_tokens, 110);
        assert_eq!(s.completion_tokens, 55);
        assert_eq!(s.status, "error");
        assert_eq!(s.first_prompt.as_deref(), Some("hello world"));

        // Empty tool/provider must not clobber filled fields.
        db.update_session_progress("s1", "", "", None, 0, 0, "ok", "2026-10-09T10:03:00Z")
            .unwrap();
        let s = db.get_session("s1").unwrap().unwrap();
        assert_eq!(s.tool, "claude");
        assert_eq!(s.provider_id.as_deref(), Some("kimi"));

        // Listing is newest first.
        db.insert_session("s2", "fp9", "2026-10-09T11:00:00Z", None)
            .unwrap();
        let list = db.list_sessions(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "s2");
    }

    #[test]
    fn handover_record_and_backfill() {
        let db = Database::open_memory().unwrap();
        db.insert_handover("claude", Some("kimi"), Some("glm"), "2026-10-09T10:00:00Z")
            .unwrap();
        // Backfill attaches the most recent unattributed handover in window.
        db.backfill_handover_session("s9", "2026-10-09T09:59:00Z")
            .unwrap();
        let hs = db.list_handovers("s9").unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].from_model.as_deref(), Some("kimi"));
        assert_eq!(hs[0].to_model.as_deref(), Some("glm"));
        assert_eq!(hs[0].session_id.as_deref(), Some("s9"));

        // A second handover is attributed to the next session, not the old one.
        db.insert_handover(
            "claude",
            Some("glm"),
            Some("deepseek"),
            "2026-10-09T10:05:00Z",
        )
        .unwrap();
        db.backfill_handover_session("s10", "2026-10-09T10:04:00Z")
            .unwrap();
        assert_eq!(db.list_handovers("s9").unwrap().len(), 1);
        let hs = db.list_handovers("s10").unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].to_model.as_deref(), Some("deepseek"));

        // Backfill with a cutoff after the handover leaves it unattributed.
        db.insert_handover("claude", Some("a"), Some("b"), "2026-10-09T12:00:00Z")
            .unwrap();
        db.backfill_handover_session("s11", "2026-10-09T13:00:00Z")
            .unwrap();
        assert!(db.list_handovers("s11").unwrap().is_empty());
    }

    #[test]
    fn request_log_carries_session_id() {
        let db = Database::open_memory().unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-10-09T10:00:00Z".into(),
            endpoint: "chat".into(),
            model: None,
            provider_id: "yy".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 1,
            completion_tokens: 1,
            duration_ms: 7,
            error: None,
            session_id: Some("s1".into()),
        })
        .unwrap();
        let logs = db.recent_request_logs(5).unwrap();
        assert_eq!(logs[0].session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn stats_group_by_session() {
        let db = Database::open_memory().unwrap();
        for (i, sid) in ["s1", "s1", "s2", "detached-none"].iter().enumerate() {
            let session_id = if *sid == "detached-none" {
                None
            } else {
                Some(sid.to_string())
            };
            db.insert_request_log(&RequestLog {
                ts: "2026-10-09T10:00:00Z".into(),
                endpoint: "chat".into(),
                model: None,
                provider_id: "yy".into(),
                tool: "claude".into(),
                status: 200,
                prompt_tokens: i as i64 + 1,
                completion_tokens: 0,
                duration_ms: 10,
                error: None,
                session_id,
            })
            .unwrap();
        }
        let rows = db
            .stats_since("2026-01-01T00:00:00Z", StatsGroupBy::Session, None)
            .unwrap();
        // s1 (2 requests), s2, (detached)
        assert_eq!(rows.len(), 3);
        let s1 = rows.iter().find(|r| r.group == "s1").unwrap();
        assert_eq!(s1.requests, 2);
        assert_eq!(s1.prompt_tokens, 3); // 1 + 2
        assert!(rows.iter().any(|r| r.group == "(detached)"));
    }
}
