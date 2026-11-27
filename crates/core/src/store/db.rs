use crate::error::{CoreError, Result};
use crate::models::{ModelMapProfile, Provider, ToolKind};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// A single request log row (all numeric fields are i64).
#[derive(Debug, Default, serde::Serialize)]
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
    pub cached_prompt_tokens: i64,
    pub cache_write_tokens: i64,
    pub duration_ms: i64,
    pub error: Option<String>,
    /// Session this request belongs to (see `crate::sessions`); NULL for
    /// pre-V2 rows and non-conversation endpoints (model listings).
    pub session_id: Option<String>,
    /// Estimated cost in USD micro-dollars ($1.00 = 1,000,000 micros).
    pub cost_usd_micros: i64,
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct AuditLog {
    pub id: i64,
    pub ts: u64,
    pub client_tool: String,
    pub client_token_id: String,
    pub provider_id: String,
    pub upstream_domain: String,
    pub upstream_model: String,
    pub key_ref: String,
    pub status_code: u16,
    pub duration_ms: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub failover_path: Option<String>,
}

/// A model/route switch recorded on the session timeline. `session_id` is
/// NULL at switch time (a config-level switch has no request context) and is
/// backfilled by the forward pipeline from the next matching request.
#[derive(Debug, serde::Serialize, Clone, PartialEq, Eq)]
pub struct HandoverRow {
    pub id: i64,
    pub tool: String,
    pub session_id: Option<String>,
    pub from_model: Option<String>,
    pub to_model: Option<String>,
    pub ts: String,
    pub injected: bool,
    pub injected_at: Option<String>,
}

/// A dynamic pricing entry loaded from OpenRouter sync or configured by the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DynamicPriceRow {
    pub model_pattern: String,
    pub input_micros_per_m: i64,
    pub output_micros_per_m: i64,
    pub cache_read_micros_per_m: i64,
    pub cache_write_micros_per_m: i64,
    pub source: String, // "synced" or "custom"
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EgressPolicy {
    Registered,
    Unrestricted,
}

impl std::fmt::Display for EgressPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Registered => write!(f, "registered"),
            Self::Unrestricted => write!(f, "unrestricted"),
        }
    }
}

impl std::str::FromStr for EgressPolicy {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "registered" => Ok(EgressPolicy::Registered),
            "unrestricted" | "all" => Ok(EgressPolicy::Unrestricted),
            other => Err(format!(
                "unknown egress policy '{other}', expected 'registered' or 'unrestricted'"
            )),
        }
    }
}

/// Normalizes domain name by stripping protocol, path, port, and trailing slashes.
pub fn normalize_domain(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }
    let url_candidate = if !s.starts_with("http://") && !s.starts_with("https://") {
        format!("http://{s}")
    } else {
        s.to_string()
    };
    if let Ok(url) = reqwest::Url::parse(&url_candidate) {
        if let Some(host) = url.host_str() {
            return host.trim_matches(|c| c == '[' || c == ']').to_lowercase();
        }
    }
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let s = s.split('/').next().unwrap_or(s);
    if s.starts_with('[') {
        if let Some(end) = s.find(']') {
            return s[1..end].trim().to_lowercase();
        }
    }
    let s = s.split(':').next().unwrap_or(s);
    s.trim().to_lowercase()
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
#[derive(Debug, Default, serde::Serialize)]
pub struct StatsRow {
    pub group: String,
    pub requests: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_prompt_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd_micros: i64,
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

/// Where a model exposed by the aggregated `/v1/models` catalog actually lives.
/// `upstream_model` is stored explicitly so a client-selected id never needs to
/// be guessed back from a `<route>/` prefix (a raw id may legitimately start
/// with `<route>/`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelRoute {
    pub route_id: String,
    pub upstream_model: String,
}

/// Exposed model id → owning route. Rebuilt on each aggregated `/v1/models`.
pub type ModelRoutes = BTreeMap<String, ModelRoute>;

pub type ClientTokenRow = (String, String, u64, Option<u64>);

pub use crate::models::{ModelMappings, ProviderModelMap};

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

    pub fn open_in_dir(dir: &Path) -> Result<Self> {
        Self::open(&dir.join("xfade.db"))
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
                protocol TEXT NOT NULL DEFAULT 'chat',
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
                cached_prompt_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER NOT NULL, error TEXT,
                tool TEXT NOT NULL DEFAULT '',
                session_id TEXT,
                cost_usd_micros INTEGER NOT NULL DEFAULT 0
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
                ts TEXT NOT NULL,
                injected INTEGER NOT NULL DEFAULT 0,
                injected_at TEXT
            );
            CREATE TABLE IF NOT EXISTS client_tokens (
                tool TEXT PRIMARY KEY,
                token TEXT NOT NULL,
                created_at_secs INTEGER NOT NULL,
                last_used_secs INTEGER
            );
            CREATE TABLE IF NOT EXISTS tool_routes (
                tool TEXT PRIMARY KEY,
                routes TEXT NOT NULL,
                model_override TEXT,
                target_protocol TEXT NOT NULL DEFAULT 'chat',
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS audit_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts INTEGER NOT NULL,
                client_tool TEXT NOT NULL,
                client_token_id TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                upstream_domain TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                key_ref TEXT NOT NULL,
                status_code INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                prompt_tokens INTEGER DEFAULT 0,
                completion_tokens INTEGER DEFAULT 0,
                failover_path TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_audit_logs_ts ON audit_logs(ts);
            CREATE TABLE IF NOT EXISTS egress_whitelist (
                domain TEXT PRIMARY KEY,
                created_at_secs INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS egress_policy (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                policy TEXT NOT NULL DEFAULT 'registered'
            );
            INSERT OR IGNORE INTO egress_policy (id, policy) VALUES (1, 'registered');
            CREATE TABLE IF NOT EXISTS model_mappings (
                client_model TEXT NOT NULL,
                provider_id TEXT NOT NULL,
                upstream_model TEXT NOT NULL,
                PRIMARY KEY (client_model, provider_id)
            );
            CREATE TABLE IF NOT EXISTS model_map_profiles (
                name TEXT PRIMARY KEY,
                description TEXT NOT NULL DEFAULT '',
                mappings_json TEXT NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS model_pricing (
                model_pattern TEXT NOT NULL,
                input_micros_per_m INTEGER NOT NULL,
                output_micros_per_m INTEGER NOT NULL,
                cache_read_micros_per_m INTEGER NOT NULL DEFAULT 0,
                cache_write_micros_per_m INTEGER NOT NULL DEFAULT 0,
                source TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (model_pattern, source)
            );
            CREATE INDEX IF NOT EXISTS idx_model_pricing_source ON model_pricing(source);",
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
        if !existing_cols.contains("route_models") {
            conn.execute("ALTER TABLE proxy_state ADD COLUMN route_models TEXT", [])?;
        }
        if !existing_cols.contains("failover_4xx") {
            conn.execute(
                "ALTER TABLE proxy_state ADD COLUMN failover_4xx INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !existing_cols.contains("models_merge") {
            conn.execute(
                "ALTER TABLE proxy_state ADD COLUMN models_merge INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !existing_cols.contains("model_routes") {
            conn.execute("ALTER TABLE proxy_state ADD COLUMN model_routes TEXT", [])?;
        }
        if !existing_cols.contains("handover_auto_inject") {
            conn.execute(
                "ALTER TABLE proxy_state ADD COLUMN handover_auto_inject INTEGER NOT NULL DEFAULT 0",
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
        if !log_cols.contains("cost_usd_micros") {
            conn.execute(
                "ALTER TABLE request_logs ADD COLUMN cost_usd_micros INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !log_cols.contains("cached_prompt_tokens") {
            conn.execute(
                "ALTER TABLE request_logs ADD COLUMN cached_prompt_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !log_cols.contains("cache_write_tokens") {
            conn.execute(
                "ALTER TABLE request_logs ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let session_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let mut rows = stmt.query([])?;
            let mut cols = std::collections::HashSet::new();
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                cols.insert(name);
            }
            cols
        };
        if !session_cols.contains("cost_usd_micros") {
            conn.execute(
                "ALTER TABLE sessions ADD COLUMN cost_usd_micros INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        let existing_handover_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(handovers)")?;
            let mut rows = stmt.query([])?;
            let mut cols = std::collections::HashSet::new();
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                cols.insert(name);
            }
            cols
        };
        if !existing_handover_cols.contains("injected") {
            conn.execute(
                "ALTER TABLE handovers ADD COLUMN injected INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !existing_handover_cols.contains("injected_at") {
            conn.execute("ALTER TABLE handovers ADD COLUMN injected_at TEXT", [])?;
        }
        let provider_cols: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(providers)")?;
            let mut rows = stmt.query([])?;
            let mut cols = std::collections::HashSet::new();
            while let Some(row) = rows.next()? {
                let name: String = row.get(1)?;
                cols.insert(name);
            }
            cols
        };
        if !provider_cols.contains("protocol") {
            conn.execute(
                "ALTER TABLE providers ADD COLUMN protocol TEXT NOT NULL DEFAULT 'chat'",
                [],
            )?;
            conn.execute(
                "UPDATE providers SET protocol = 'messages' 
                 WHERE (tool = 'claude' AND base_url IS NULL) 
                    OR base_url LIKE '%anthropic.com%'",
                [],
            )?;
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
        // Idempotent migration for model_pricing: ensure compound primary key (model_pattern, source)
        let has_compound_pk: bool = {
            let mut stmt = conn.prepare("PRAGMA table_info(model_pricing)")?;
            let mut rows = stmt.query([])?;
            let mut pk_count = 0;
            while let Some(row) = rows.next()? {
                let pk: i64 = row.get(5)?;
                if pk > 0 {
                    pk_count += 1;
                }
            }
            pk_count >= 2
        };
        if !has_compound_pk {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS model_pricing_v2 (
                    model_pattern TEXT NOT NULL,
                    input_micros_per_m INTEGER NOT NULL,
                    output_micros_per_m INTEGER NOT NULL,
                    cache_read_micros_per_m INTEGER NOT NULL DEFAULT 0,
                    cache_write_micros_per_m INTEGER NOT NULL DEFAULT 0,
                    source TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (model_pattern, source)
                );
                INSERT OR IGNORE INTO model_pricing_v2 SELECT * FROM model_pricing;
                DROP TABLE model_pricing;
                ALTER TABLE model_pricing_v2 RENAME TO model_pricing;
                CREATE INDEX IF NOT EXISTS idx_model_pricing_source ON model_pricing(source);",
            )?;
        }
        Ok(())
    }

    /// Insert or update (does not touch is_active).
    pub fn upsert(&self, p: &Provider) -> Result<()> {
        self.conn().execute(
            "INSERT INTO providers (tool, id, base_url, key_ref, protocol, extra, is_active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (tool, id) DO UPDATE SET
                base_url = excluded.base_url,
                key_ref = excluded.key_ref,
                protocol = excluded.protocol,
                extra = excluded.extra",
            params![
                p.tool.as_str(),
                p.id,
                p.base_url,
                p.key_ref,
                p.protocol.as_str(),
                serde_json::to_string(&p.extra)?,
                p.is_active as i64,
            ],
        )?;
        Ok(())
    }

    pub fn get(&self, tool: ToolKind, id: &str) -> Result<Option<Provider>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT tool, id, base_url, key_ref, extra, is_active, protocol FROM providers
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
                "SELECT tool, id, base_url, key_ref, extra, is_active, protocol FROM providers
                 WHERE tool = ?1 ORDER BY id",
                Some(t.as_str().to_string()),
            ),
            None => (
                "SELECT tool, id, base_url, key_ref, extra, is_active, protocol FROM providers
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
    /// A changed route set invalidates the cached aggregated model→route map,
    /// so the conflict clause also resets `model_routes`; the independent
    /// toggles (`smart_enabled`, `failover_4xx`, `models_merge`,
    /// `handover_auto_inject`) are preserved.
    /// Kept as the legacy three-arg writer (GUI + tests): its column-scoped
    /// upsert never touches those toggles.
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
                model_routes = '{}',
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

    /// Set proxy routes together with a per-provider model map
    /// (`--route-model id=model`). Kept separate from the legacy three-arg
    /// `set_routes` (GUI + tests). Both route writers clear the cached
    /// `model_routes` map because a changed route set makes it stale; both
    /// leave the independent toggles (`smart_enabled`, `failover_4xx`,
    /// `models_merge`, `handover_auto_inject`) intact, so `proxy use`
    /// never resets them.
    pub fn set_routes_with_models(
        &self,
        routes: &[String],
        model_override: Option<&str>,
        target_protocol: &str,
        route_models: &BTreeMap<String, String>,
    ) -> Result<()> {
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        self.conn().execute(
            "INSERT INTO proxy_state (id, routes, model_override, target_protocol, route_models, model_routes, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, '{}', ?5)
             ON CONFLICT (id) DO UPDATE SET
                routes = excluded.routes,
                model_override = excluded.model_override,
                target_protocol = excluded.target_protocol,
                route_models = excluded.route_models,
                model_routes = excluded.model_routes,
                updated_at = excluded.updated_at",
            params![
                serde_json::to_string(routes)?,
                model_override,
                target_protocol,
                serde_json::to_string(route_models)?,
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

    pub fn set_tool_routes(
        &self,
        tool: &str,
        routes: &[String],
        model_override: Option<&str>,
        target_protocol: &str,
    ) -> Result<()> {
        let conn = self.conn();
        let routes_json = serde_json::to_string(routes)?;
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        conn.execute(
            "INSERT INTO tool_routes (tool, routes, model_override, target_protocol, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(tool) DO UPDATE SET
                routes = excluded.routes,
                model_override = excluded.model_override,
                target_protocol = excluded.target_protocol,
                updated_at = excluded.updated_at",
            params![tool, routes_json, model_override, target_protocol, now],
        )?;
        Ok(())
    }

    pub fn get_tool_routes(&self, tool: &str) -> Result<Option<RoutesConfig>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT routes, model_override, target_protocol FROM tool_routes WHERE tool = ?1",
        )?;
        let mut rows = stmt.query(params![tool])?;
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

    pub fn is_tool_direct(&self, tool: &str) -> Result<bool> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT routes FROM tool_routes WHERE tool = ?1")?;
        let mut rows = stmt.query(params![tool])?;
        match rows.next()? {
            Some(row) => {
                let s: String = row.get(0)?;
                let routes: Vec<String> = serde_json::from_str(&s)?;
                Ok(routes.is_empty())
            }
            None => Ok(false),
        }
    }

    pub fn upsert_client_token(&self, tool: &str, token: &str, created_at_secs: u64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO client_tokens (tool, token, created_at_secs, last_used_secs)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(tool) DO UPDATE SET token = excluded.token, created_at_secs = excluded.created_at_secs",
            params![tool, token, created_at_secs as i64],
        )?;
        Ok(())
    }

    pub fn get_client_token(&self, tool: &str) -> Result<Option<(String, u64, Option<u64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT token, created_at_secs, last_used_secs FROM client_tokens WHERE tool = ?1",
        )?;
        let mut rows = stmt.query(params![tool])?;
        if let Some(row) = rows.next()? {
            let token: String = row.get(0)?;
            let created_at: i64 = row.get(1)?;
            let last_used: Option<i64> = row.get(2)?;
            Ok(Some((
                token,
                created_at as u64,
                last_used.map(|x| x as u64),
            )))
        } else {
            Ok(None)
        }
    }

    pub fn list_client_tokens(&self) -> Result<Vec<ClientTokenRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT tool, token, created_at_secs, last_used_secs FROM client_tokens ORDER BY tool",
        )?;
        let rows = stmt.query_map([], |row| {
            let tool: String = row.get(0)?;
            let token: String = row.get(1)?;
            let created_at: i64 = row.get(2)?;
            let last_used: Option<i64> = row.get(3)?;
            Ok((tool, token, created_at as u64, last_used.map(|x| x as u64)))
        })?;
        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    pub fn update_client_token_last_used(&self, tool: &str, last_used_secs: u64) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE client_tokens SET last_used_secs = ?1 WHERE tool = ?2",
            params![last_used_secs as i64, tool],
        )?;
        Ok(())
    }

    /// Clear the route set and the cached aggregated model→route map without
    /// touching the independent toggles (`smart_enabled`, `failover_4xx`,
    /// `models_merge`, `handover_auto_inject`).
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
             SET routes = '[]', model_override = NULL, route_models = '{}',
                 model_routes = '{}', updated_at = ?1
             WHERE id = 1",
            params![now],
        )?;
        Ok(())
    }

    /// Per-provider model overrides (`--route-model id=model`). NULL/empty/
    /// malformed JSON reads as an empty map (pre-migration rows included).
    pub fn get_route_models(&self) -> Result<BTreeMap<String, String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT route_models FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let s: Option<String> = row.get(0)?;
                Ok(match s {
                    Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                    _ => BTreeMap::new(),
                })
            }
            None => Ok(BTreeMap::new()),
        }
    }

    /// Whether qualifying 4xx responses should trigger failover. Default off.
    /// Read on every request so `xfade proxy failover-4xx on/off` takes
    /// effect on a running daemon immediately.
    pub fn get_failover_4xx(&self) -> Result<bool> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT failover_4xx FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, i64>(0)? != 0),
            None => Ok(false),
        }
    }

    /// Full upsert into `proxy_state` that updates only `column` on conflict.
    /// `routes`/`updated_at` are NOT NULL without defaults, so a partial
    /// INSERT OR IGNORE would be silently swallowed by the conflict clause;
    /// the literal defaults satisfy them while the conflict clause touches
    /// only the one column, leaving routes and the other toggles intact.
    /// `column` is always an internal constant, never user input.
    fn upsert_proxy_state_column<V: rusqlite::ToSql>(
        &self,
        column: &'static str,
        value: V,
    ) -> Result<()> {
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        let sql = format!(
            "INSERT INTO proxy_state (id, routes, model_override, target_protocol, updated_at, {column})
             VALUES (1, '[]', NULL, 'chat', ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET {column} = excluded.{column}"
        );
        self.conn().execute(&sql, params![now, value])?;
        Ok(())
    }

    pub fn set_failover_4xx(&self, on: bool) -> Result<()> {
        self.upsert_proxy_state_column("failover_4xx", on as i64)
    }

    /// Whether the Smart Fader (intent-based tiered routing) is enabled.
    /// Read on every request so `xfade proxy smart on/off` takes effect on
    /// a running daemon immediately.
    pub fn smart_enabled(&self) -> Result<bool> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT smart_enabled FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, i64>(0)? != 0),
            None => Ok(false),
        }
    }

    pub fn set_smart_enabled(&self, on: bool) -> Result<()> {
        self.upsert_proxy_state_column("smart_enabled", on as i64)
    }

    /// Whether `/v1/models` aggregates every configured route's catalog and
    /// POSTs are pinned by a client-selected model id. Default off; read on
    /// every request so `xfade proxy models-merge on/off` takes effect on a
    /// running daemon immediately.
    pub fn models_merge_enabled(&self) -> Result<bool> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT models_merge FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, i64>(0)? != 0),
            None => Ok(false),
        }
    }

    pub fn set_models_merge_enabled(&self, on: bool) -> Result<()> {
        self.upsert_proxy_state_column("models_merge", on as i64)
    }

    /// Whether the proxy transparently injects pending session-handover
    /// context into the next upstream request (L3). Default off; read on
    /// every request so `xfade proxy handover-inject on/off` takes effect
    /// on a running daemon immediately.
    pub fn handover_auto_inject_enabled(&self) -> Result<bool> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT handover_auto_inject FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, i64>(0)? != 0),
            None => Ok(false),
        }
    }

    pub fn set_handover_auto_inject_enabled(&self, on: bool) -> Result<()> {
        self.upsert_proxy_state_column("handover_auto_inject", on as i64)
    }

    /// Cached `model → owning route` map built by the aggregated `/v1/models`.
    /// NULL/empty/malformed JSON reads as an empty map (pre-migration rows
    /// included), so an unbuilt map simply means "no pinning available".
    pub fn get_model_routes(&self) -> Result<ModelRoutes> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT model_routes FROM proxy_state WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => {
                let s: Option<String> = row.get(0)?;
                Ok(match s {
                    Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
                    _ => ModelRoutes::new(),
                })
            }
            None => Ok(ModelRoutes::new()),
        }
    }

    /// Reverse-look-up a client-selected model id in the cached aggregated
    /// catalog map, returning the owning route + real upstream model id.
    pub fn model_route_for(&self, model: &str) -> Result<Option<ModelRoute>> {
        Ok(self.get_model_routes()?.get(model).cloned())
    }

    pub fn set_model_routes(&self, map: &ModelRoutes) -> Result<()> {
        self.upsert_proxy_state_column("model_routes", serde_json::to_string(map)?)
    }

    /// Retrieve all configured client model aliases and their per-provider mappings.
    pub fn get_model_mappings(&self) -> Result<ModelMappings> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT client_model, provider_id, upstream_model FROM model_mappings ORDER BY client_model, provider_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = ModelMappings::new();
        while let Some(row) = rows.next()? {
            let client_model: String = row.get(0)?;
            let provider_id: String = row.get(1)?;
            let upstream_model: String = row.get(2)?;
            out.entry(client_model)
                .or_default()
                .insert(provider_id, upstream_model);
        }
        Ok(out)
    }

    /// Retrieve per-provider mappings for a specific client model alias.
    pub fn get_mappings_for_client_model(&self, client_model: &str) -> Result<ProviderModelMap> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT provider_id, upstream_model FROM model_mappings WHERE client_model = ?1 ORDER BY provider_id",
        )?;
        let mut rows = stmt.query(params![client_model.trim()])?;
        let mut out = ProviderModelMap::new();
        while let Some(row) = rows.next()? {
            let provider_id: String = row.get(0)?;
            let upstream_model: String = row.get(1)?;
            out.insert(provider_id, upstream_model);
        }
        Ok(out)
    }

    /// Add or update a single model mapping entry: (client_model, provider_id) -> upstream_model.
    pub fn set_model_mapping(
        &self,
        client_model: &str,
        provider_id: &str,
        upstream_model: &str,
    ) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO model_mappings (client_model, provider_id, upstream_model)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(client_model, provider_id) DO UPDATE SET upstream_model = excluded.upstream_model",
            params![client_model.trim(), provider_id.trim(), upstream_model.trim()],
        )?;
        Ok(())
    }

    /// Add or update multiple model mapping entries for a client model alias in a transaction.
    pub fn set_model_mappings_batch(
        &self,
        client_model: &str,
        mappings: &[(String, String)],
    ) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_mappings (client_model, provider_id, upstream_model)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(client_model, provider_id) DO UPDATE SET upstream_model = excluded.upstream_model",
            )?;
            for (provider_id, upstream_model) in mappings {
                stmt.execute(params![
                    client_model.trim(),
                    provider_id.trim(),
                    upstream_model.trim()
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a mapping for a client model alias.
    /// If `provider_id` is Some, deletes only that provider mapping.
    /// If `provider_id` is None, deletes all provider mappings under that alias.
    /// Returns true if at least one row was deleted.
    pub fn remove_model_mapping(
        &self,
        client_model: &str,
        provider_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn();
        let affected = match provider_id {
            Some(pid) => conn.execute(
                "DELETE FROM model_mappings WHERE client_model = ?1 AND provider_id = ?2",
                params![client_model.trim(), pid.trim()],
            )?,
            None => conn.execute(
                "DELETE FROM model_mappings WHERE client_model = ?1",
                params![client_model.trim()],
            )?,
        };
        Ok(affected > 0)
    }

    /// Clear all model mappings across all client model aliases.
    pub fn clear_model_mappings(&self) -> Result<()> {
        self.conn().execute("DELETE FROM model_mappings", [])?;
        Ok(())
    }

    /// Apply a batch of `(client_model, provider_id, upstream_model)` rules in a single transaction.
    /// When `replace` is true, existing rules are cleared first.
    pub fn apply_model_mappings_batch(
        &self,
        entries: &[(String, String, String)],
        replace: bool,
    ) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if replace {
            tx.execute("DELETE FROM model_mappings", [])?;
        }
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_mappings (client_model, provider_id, upstream_model)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(client_model, provider_id) DO UPDATE SET upstream_model = excluded.upstream_model",
            )?;
            for (alias, provider_id, upstream_model) in entries {
                let a = alias.trim();
                let p = provider_id.trim();
                let u = upstream_model.trim();
                if a.is_empty() || p.is_empty() || u.is_empty() {
                    continue;
                }
                stmt.execute(params![a, p, u])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// List all saved model map profiles ordered by name.
    pub fn list_model_map_profiles(&self) -> Result<Vec<ModelMapProfile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, description, mappings_json, is_active, updated_at
             FROM model_map_profiles ORDER BY name",
        )?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mappings_json: String = row.get(2)?;
            let mappings: ModelMappings = serde_json::from_str(&mappings_json)?;
            out.push(ModelMapProfile {
                name: row.get(0)?,
                description: row.get(1)?,
                mappings,
                is_active: row.get::<_, i64>(3)? != 0,
                updated_at: row.get(4)?,
            });
        }
        Ok(out)
    }

    /// Fetch a single model map profile by name.
    pub fn get_model_map_profile(&self, name: &str) -> Result<Option<ModelMapProfile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT name, description, mappings_json, is_active, updated_at
             FROM model_map_profiles WHERE name = ?1",
        )?;
        let mut rows = stmt.query(params![name.trim()])?;
        match rows.next()? {
            Some(row) => {
                let mappings_json: String = row.get(2)?;
                let mappings: ModelMappings = serde_json::from_str(&mappings_json)?;
                Ok(Some(ModelMapProfile {
                    name: row.get(0)?,
                    description: row.get(1)?,
                    mappings,
                    is_active: row.get::<_, i64>(3)? != 0,
                    updated_at: row.get(4)?,
                }))
            }
            None => Ok(None),
        }
    }

    /// Save (or overwrite) a named model map profile.
    /// When `mappings` is `None`, snapshots the current `model_mappings` table.
    pub fn save_model_map_profile(
        &self,
        name: &str,
        description: &str,
        mappings: Option<&ModelMappings>,
    ) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(CoreError::ConfigParse {
                path: "model_map_profile".into(),
                msg: "profile name cannot be empty".into(),
            });
        }
        let snapshot = match mappings {
            Some(m) => m.clone(),
            None => self.get_model_mappings()?,
        };
        let json = serde_json::to_string(&snapshot)?;
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        self.conn().execute(
            "INSERT INTO model_map_profiles (name, description, mappings_json, is_active, updated_at)
             VALUES (?1, ?2, ?3, 0, ?4)
             ON CONFLICT(name) DO UPDATE SET
                description = excluded.description,
                mappings_json = excluded.mappings_json,
                updated_at = excluded.updated_at",
            params![trimmed, description.trim(), json, now],
        )?;
        Ok(())
    }

    /// Switch to a saved model map profile, applying its rules to `model_mappings`
    /// and marking it active.
    pub fn switch_model_map_profile(&self, name: &str, replace: bool) -> Result<usize> {
        let trimmed = name.trim();
        let profile =
            self.get_model_map_profile(trimmed)?
                .ok_or_else(|| CoreError::ConfigParse {
                    path: "model_map_profile".into(),
                    msg: format!("profile '{trimmed}' not found"),
                })?;
        let mut entries = Vec::new();
        for (alias, prov_map) in &profile.mappings {
            for (prov, upstream) in prov_map {
                entries.push((alias.clone(), prov.clone(), upstream.clone()));
            }
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if replace {
            tx.execute("DELETE FROM model_mappings", [])?;
        }
        let mut count = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_mappings (client_model, provider_id, upstream_model)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(client_model, provider_id) DO UPDATE SET upstream_model = excluded.upstream_model",
            )?;
            for (a, p, u) in &entries {
                stmt.execute(params![a, p, u])?;
                count += 1;
            }
        }
        tx.execute("UPDATE model_map_profiles SET is_active = 0", [])?;
        tx.execute(
            "UPDATE model_map_profiles SET is_active = 1 WHERE name = ?1",
            params![trimmed],
        )?;
        tx.commit()?;
        Ok(count)
    }

    /// Delete a saved model map profile by name.
    pub fn delete_model_map_profile(&self, name: &str) -> Result<bool> {
        let affected = self.conn().execute(
            "DELETE FROM model_map_profiles WHERE name = ?1",
            params![name.trim()],
        )?;
        Ok(affected > 0)
    }

    /// Cache a repo map (analyzer fingerprint + payload) keyed by root path.
    pub fn save_repo_map(&self, root_path: &str, fingerprint: &str, map_json: &str) -> Result<()> {
        let now =
            OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .map_err(|e| CoreError::ConfigParse {
                    path: "time".into(),
                    msg: e.to_string(),
                })?;
        self.conn().execute(
            "INSERT OR REPLACE INTO repo_maps (root_path, fingerprint, map_json, generated_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![root_path, fingerprint, map_json, now],
        )?;
        Ok(())
    }

    /// Fetch a cached repo map: (fingerprint, map_json).
    pub fn get_repo_map(&self, root_path: &str) -> Result<Option<(String, String)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT fingerprint, map_json FROM repo_maps WHERE root_path = ?1")?;
        let mut rows = stmt.query(params![root_path])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    /// The most recently *written* repo map: (root_path, map_json). Ordered
    /// by rowid (INSERT OR REPLACE re-allocates it), so re-saving a root
    /// makes it the latest regardless of timestamp granularity.
    pub fn latest_repo_map(&self) -> Result<Option<(String, String)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT root_path, map_json FROM repo_maps ORDER BY rowid DESC LIMIT 1")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }

    pub fn insert_request_log(&self, log: &RequestLog) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO request_logs (ts, endpoint, model, provider_id, tool, status, prompt_tokens, completion_tokens, cached_prompt_tokens, cache_write_tokens, duration_ms, error, session_id, cost_usd_micros)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                log.ts,
                log.endpoint,
                log.model,
                log.provider_id,
                log.tool,
                log.status,
                log.prompt_tokens,
                log.completion_tokens,
                log.cached_prompt_tokens,
                log.cache_write_tokens,
                log.duration_ms,
                log.error,
                log.session_id,
                log.cost_usd_micros,
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
                    COALESCE(SUM(cached_prompt_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_usd_micros), 0),
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
                cached_prompt_tokens: row.get(4)?,
                cache_write_tokens: row.get(5)?,
                cost_usd_micros: row.get(6)?,
                errors: row.get(7)?,
                avg_duration_ms: row.get(8)?,
            });
        }
        Ok(out)
    }

    /// Read the most recent `limit` request logs (reverse insertion order). Used for test assertions.
    pub fn recent_request_logs(&self, limit: usize) -> Result<Vec<RequestLog>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT ts, endpoint, model, provider_id, tool, status, prompt_tokens, completion_tokens, cached_prompt_tokens, cache_write_tokens, duration_ms, error, session_id, cost_usd_micros
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
                cached_prompt_tokens: row.get(8)?,
                cache_write_tokens: row.get(9)?,
                duration_ms: row.get(10)?,
                error: row.get(11)?,
                session_id: row.get(12)?,
                cost_usd_micros: row.get(13)?,
            });
        }
        Ok(out)
    }

    /// Insert an immutable audit record for an outgoing proxied request.
    pub fn insert_audit_log(&self, log: &AuditLog) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO audit_logs (ts, client_tool, client_token_id, provider_id, upstream_domain, upstream_model, key_ref, status_code, duration_ms, prompt_tokens, completion_tokens, failover_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                log.ts as i64,
                log.client_tool,
                log.client_token_id,
                log.provider_id,
                log.upstream_domain,
                log.upstream_model,
                log.key_ref,
                log.status_code as i64,
                log.duration_ms as i64,
                log.prompt_tokens as i64,
                log.completion_tokens as i64,
                log.failover_path,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Read the most recent `limit` audit logs, optionally filtered by `tool`.
    pub fn recent_audit_logs(&self, limit: usize, tool: Option<&str>) -> Result<Vec<AuditLog>> {
        let conn = self.conn();
        let (sql, param) = match tool {
            Some(t) => (
                "SELECT id, ts, client_tool, client_token_id, provider_id, upstream_domain, upstream_model, key_ref, status_code, duration_ms, prompt_tokens, completion_tokens, failover_path
                 FROM audit_logs WHERE client_tool = ?1 ORDER BY id DESC LIMIT ?2",
                Some(t.to_string()),
            ),
            None => (
                "SELECT id, ts, client_tool, client_token_id, provider_id, upstream_domain, upstream_model, key_ref, status_code, duration_ms, prompt_tokens, completion_tokens, failover_path
                 FROM audit_logs ORDER BY id DESC LIMIT ?1",
                None,
            ),
        };

        let mut stmt = conn.prepare(sql)?;
        let mut rows = match param {
            Some(t) => stmt.query(params![t, limit as i64])?,
            None => stmt.query(params![limit as i64])?,
        };

        let mut res = Vec::new();
        while let Some(row) = rows.next()? {
            res.push(AuditLog {
                id: row.get(0)?,
                ts: row.get::<_, i64>(1)? as u64,
                client_tool: row.get(2)?,
                client_token_id: row.get(3)?,
                provider_id: row.get(4)?,
                upstream_domain: row.get(5)?,
                upstream_model: row.get(6)?,
                key_ref: row.get(7)?,
                status_code: row.get::<_, i64>(8)? as u16,
                duration_ms: row.get::<_, i64>(9)? as u64,
                prompt_tokens: row.get::<_, i64>(10)? as u64,
                completion_tokens: row.get::<_, i64>(11)? as u64,
                failover_path: row.get(12)?,
            });
        }
        Ok(res)
    }

    /// Export audit logs optionally starting from a timestamp.
    pub fn export_audit_logs(&self, since_ts: Option<u64>) -> Result<Vec<AuditLog>> {
        let conn = self.conn();
        let sql = match since_ts {
            Some(_) => "SELECT id, ts, client_tool, client_token_id, provider_id, upstream_domain, upstream_model, key_ref, status_code, duration_ms, prompt_tokens, completion_tokens, failover_path
                        FROM audit_logs WHERE ts >= ?1 ORDER BY id ASC",
            None => "SELECT id, ts, client_tool, client_token_id, provider_id, upstream_domain, upstream_model, key_ref, status_code, duration_ms, prompt_tokens, completion_tokens, failover_path
                     FROM audit_logs ORDER BY id ASC",
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = match since_ts {
            Some(ts) => stmt.query(params![ts as i64])?,
            None => stmt.query([])?,
        };
        let mut res = Vec::new();
        while let Some(row) = rows.next()? {
            res.push(AuditLog {
                id: row.get(0)?,
                ts: row.get::<_, i64>(1)? as u64,
                client_tool: row.get(2)?,
                client_token_id: row.get(3)?,
                provider_id: row.get(4)?,
                upstream_domain: row.get(5)?,
                upstream_model: row.get(6)?,
                key_ref: row.get(7)?,
                status_code: row.get::<_, i64>(8)? as u16,
                duration_ms: row.get::<_, i64>(9)? as u64,
                prompt_tokens: row.get::<_, i64>(10)? as u64,
                completion_tokens: row.get::<_, i64>(11)? as u64,
                failover_path: row.get(12)?,
            });
        }
        Ok(res)
    }

    // ── Egress whitelist & policy ─────────────────────────────────────────

    pub fn get_egress_policy(&self) -> Result<EgressPolicy> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT policy FROM egress_policy WHERE id = 1")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let s: String = row.get(0)?;
            Ok(s.parse().unwrap_or(EgressPolicy::Registered))
        } else {
            Ok(EgressPolicy::Registered)
        }
    }

    pub fn set_egress_policy(&self, policy: EgressPolicy) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO egress_policy (id, policy) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET policy = excluded.policy",
            params![policy.to_string()],
        )?;
        Ok(())
    }

    pub fn add_egress_domain(&self, domain: &str) -> Result<()> {
        let norm = normalize_domain(domain);
        if norm.is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO egress_whitelist (domain, created_at_secs) VALUES (?1, ?2)",
            params![norm, now as i64],
        )?;
        Ok(())
    }

    pub fn remove_egress_domain(&self, domain: &str) -> Result<bool> {
        let norm = normalize_domain(domain);
        let conn = self.conn();
        let changed = conn.execute(
            "DELETE FROM egress_whitelist WHERE domain = ?1",
            params![norm],
        )?;
        Ok(changed > 0)
    }

    pub fn list_egress_domains(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT domain FROM egress_whitelist ORDER BY domain")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row.get(0)?);
        }
        Ok(out)
    }

    pub fn is_domain_allowed(&self, domain: &str) -> Result<bool> {
        if self.get_egress_policy()? == EgressPolicy::Unrestricted {
            return Ok(true);
        }
        let norm = normalize_domain(domain);
        if norm.is_empty()
            || norm == "127.0.0.1"
            || norm == "localhost"
            || norm == "::1"
            || norm.starts_with("127.")
        {
            return Ok(true);
        }

        let conn = self.conn();
        // 1. Check custom whitelist table
        let mut stmt = conn.prepare("SELECT 1 FROM egress_whitelist WHERE domain = ?1")?;
        let mut rows = stmt.query(params![norm])?;
        if rows.next()?.is_some() {
            return Ok(true);
        }

        // 2. Check registered provider domains
        let mut stmt = conn.prepare("SELECT base_url FROM providers WHERE base_url IS NOT NULL")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let url_str: String = row.get(0)?;
            let p_domain = normalize_domain(&url_str);
            if p_domain == norm {
                return Ok(true);
            }
        }

        Ok(false)
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
        cost_usd_micros: i64,
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
                cost_usd_micros = cost_usd_micros + ?7,
                status = ?8,
                ended_at = ?9
             WHERE id = ?1",
            params![
                session_id,
                tool,
                provider_id,
                model,
                prompt_tokens,
                completion_tokens,
                cost_usd_micros,
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
            "INSERT INTO handovers (tool, session_id, from_model, to_model, ts, injected, injected_at)
             VALUES (?1, NULL, ?2, ?3, ?4, 0, NULL)",
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
            "SELECT id, tool, session_id, from_model, to_model, ts, injected, injected_at
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
                injected: row.get::<_, i64>(6)? != 0,
                injected_at: row.get(7)?,
            });
        }
        Ok(out)
    }

    /// Find the most recent pending handover that has not been injected yet.
    /// Prefers a handover already attributed to `session_id`; falls back to the
    /// newest unattributed row (`session_id IS NULL`) within `cutoff_ts`.
    /// Returns `None` when no session id is available — auto-inject must not
    /// claim a pending handover for an unidentified conversation.
    pub fn find_pending_handover(
        &self,
        session_id: Option<&str>,
        cutoff_ts: &str,
    ) -> Result<Option<HandoverRow>> {
        let Some(sid) = session_id else {
            return Ok(None);
        };
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, tool, session_id, from_model, to_model, ts, injected, injected_at
             FROM handovers
             WHERE (session_id = ?1 OR session_id IS NULL)
               AND ts >= ?2
               AND injected = 0
             ORDER BY CASE WHEN session_id = ?1 THEN 0 ELSE 1 END, id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![sid, cutoff_ts])?;
        match rows.next()? {
            Some(row) => Ok(Some(HandoverRow {
                id: row.get(0)?,
                tool: row.get(1)?,
                session_id: row.get(2)?,
                from_model: row.get(3)?,
                to_model: row.get(4)?,
                ts: row.get(5)?,
                injected: row.get::<_, i64>(6)? != 0,
                injected_at: row.get(7)?,
            })),
            None => Ok(None),
        }
    }

    /// Mark a handover as injected, attributing it to `session_id` if currently NULL.
    pub fn mark_handover_injected(
        &self,
        id: i64,
        session_id: &str,
        injected_at: &str,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE handovers
             SET injected = 1,
                 injected_at = ?1,
                 session_id = CASE WHEN (session_id IS NULL OR session_id = '') AND ?2 != '' THEN ?2 ELSE session_id END
             WHERE id = ?3",
            params![injected_at, session_id, id],
        )?;
        Ok(())
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

    /// Insert or update a model pricing entry.
    pub fn upsert_model_pricing(&self, row: &DynamicPriceRow) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO model_pricing (
                model_pattern, input_micros_per_m, output_micros_per_m,
                cache_read_micros_per_m, cache_write_micros_per_m, source, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(model_pattern, source) DO UPDATE SET
                input_micros_per_m = excluded.input_micros_per_m,
                output_micros_per_m = excluded.output_micros_per_m,
                cache_read_micros_per_m = excluded.cache_read_micros_per_m,
                cache_write_micros_per_m = excluded.cache_write_micros_per_m,
                updated_at = excluded.updated_at",
            rusqlite::params![
                row.model_pattern,
                row.input_micros_per_m,
                row.output_micros_per_m,
                row.cache_read_micros_per_m,
                row.cache_write_micros_per_m,
                row.source,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Atomically insert or update multiple model pricing entries in a transaction.
    pub fn batch_upsert_model_pricing(&self, rows: &[DynamicPriceRow]) -> Result<usize> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO model_pricing (
                    model_pattern, input_micros_per_m, output_micros_per_m,
                    cache_read_micros_per_m, cache_write_micros_per_m, source, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(model_pattern, source) DO UPDATE SET
                    input_micros_per_m = excluded.input_micros_per_m,
                    output_micros_per_m = excluded.output_micros_per_m,
                    cache_read_micros_per_m = excluded.cache_read_micros_per_m,
                    cache_write_micros_per_m = excluded.cache_write_micros_per_m,
                    updated_at = excluded.updated_at",
            )?;
            for row in rows {
                stmt.execute(rusqlite::params![
                    row.model_pattern,
                    row.input_micros_per_m,
                    row.output_micros_per_m,
                    row.cache_read_micros_per_m,
                    row.cache_write_micros_per_m,
                    row.source,
                    row.updated_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }

    /// Delete a single custom model pricing entry by pattern.
    pub fn delete_model_pricing(&self, pattern: &str) -> Result<bool> {
        let conn = self.conn();
        let rows = conn.execute(
            "DELETE FROM model_pricing WHERE model_pattern = ?1 AND source = 'custom'",
            rusqlite::params![pattern],
        )?;
        Ok(rows > 0)
    }

    /// Remove all synced pricing entries, preserving custom user overrides.
    pub fn clear_synced_model_pricing(&self) -> Result<usize> {
        let conn = self.conn();
        let rows = conn.execute("DELETE FROM model_pricing WHERE source = 'synced'", [])?;
        Ok(rows)
    }

    /// List all dynamic model pricing entries ordered alphabetically.
    pub fn list_model_pricing(&self) -> Result<Vec<DynamicPriceRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT model_pattern, input_micros_per_m, output_micros_per_m,
                    cache_read_micros_per_m, cache_write_micros_per_m, source, updated_at
             FROM model_pricing
             ORDER BY model_pattern ASC, CASE source WHEN 'synced' THEN 0 ELSE 1 END ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DynamicPriceRow {
                model_pattern: row.get(0)?,
                input_micros_per_m: row.get(1)?,
                output_micros_per_m: row.get(2)?,
                cache_read_micros_per_m: row.get(3)?,
                cache_write_micros_per_m: row.get(4)?,
                source: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        let mut list = Vec::new();
        for r in rows {
            list.push(r?);
        }
        Ok(list)
    }

    /// Resolve dynamic pricing for a model name.
    ///
    /// Checks both the model name and an optional canonical name against `model_pattern`.
    /// Resolution precedence:
    /// 1. Fast exact match index seek (`model_pattern IN (?1, ?2)`).
    /// 2. Delimiter-checked prefix match (`LENGTH(model_pattern) DESC`).
    /// 3. Custom overrides (`source = 'custom'`) over synced entries (`source = 'synced'`).
    pub fn find_dynamic_model_price(
        &self,
        model: &str,
        canonical: Option<&str>,
    ) -> Result<Option<DynamicPriceRow>> {
        let model_trimmed = model.trim().to_lowercase();
        if model_trimmed.is_empty() {
            return Ok(None);
        }
        let canonical_str = canonical.unwrap_or("").trim().to_lowercase();
        let conn = self.conn();

        // Stage 1: Fast exact-match lookup using PRIMARY KEY index
        let mut stmt_exact = conn.prepare(
            "SELECT model_pattern, input_micros_per_m, output_micros_per_m,
                    cache_read_micros_per_m, cache_write_micros_per_m, source, updated_at
             FROM model_pricing
             WHERE model_pattern = ?1 OR (?2 != '' AND model_pattern = ?2)
             ORDER BY
                 CASE source WHEN 'custom' THEN 0 ELSE 1 END ASC
             LIMIT 1",
        )?;
        let mut exact_rows = stmt_exact.query(rusqlite::params![model_trimmed, canonical_str])?;
        if let Some(row) = exact_rows.next()? {
            return Ok(Some(DynamicPriceRow {
                model_pattern: row.get(0)?,
                input_micros_per_m: row.get(1)?,
                output_micros_per_m: row.get(2)?,
                cache_read_micros_per_m: row.get(3)?,
                cache_write_micros_per_m: row.get(4)?,
                source: row.get(5)?,
                updated_at: row.get(6)?,
            }));
        }
        drop(exact_rows);
        drop(stmt_exact);

        // Stage 2: Prefix lookup with delimiter check to avoid collisions (e.g. 'gpt-4' vs 'gpt-4o')
        let mut stmt_prefix = conn.prepare(
            "SELECT model_pattern, input_micros_per_m, output_micros_per_m,
                    cache_read_micros_per_m, cache_write_micros_per_m, source, updated_at
             FROM model_pricing
             WHERE model_pattern != ''
               AND (
                   (LENGTH(?1) > LENGTH(model_pattern)
                    AND SUBSTR(?1, 1, LENGTH(model_pattern)) = model_pattern
                    AND SUBSTR(?1, LENGTH(model_pattern) + 1, 1) IN ('-', ':', '.', '/'))
                OR (?2 != ''
                    AND LENGTH(?2) > LENGTH(model_pattern)
                    AND SUBSTR(?2, 1, LENGTH(model_pattern)) = model_pattern
                    AND SUBSTR(?2, LENGTH(model_pattern) + 1, 1) IN ('-', ':', '.', '/'))
               )
             ORDER BY
                 CASE source WHEN 'custom' THEN 0 ELSE 1 END ASC,
                 LENGTH(model_pattern) DESC
             LIMIT 1",
        )?;
        let mut rows = stmt_prefix.query(rusqlite::params![model_trimmed, canonical_str])?;
        if let Some(row) = rows.next()? {
            Ok(Some(DynamicPriceRow {
                model_pattern: row.get(0)?,
                input_micros_per_m: row.get(1)?,
                output_micros_per_m: row.get(2)?,
                cache_read_micros_per_m: row.get(3)?,
                cache_write_micros_per_m: row.get(4)?,
                source: row.get(5)?,
                updated_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }
}

fn row_to_provider(row: &rusqlite::Row) -> rusqlite::Result<Provider> {
    let tool_str: String = row.get(0)?;
    let extra_str: String = row.get(4)?;
    let protocol_str: Option<String> = row.get(6).ok();
    let protocol = protocol_str
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(crate::models::ProviderProtocol::Chat);
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
        protocol,
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
    use crate::models::{Provider, ProviderProtocol, ToolKind};

    #[test]
    fn test_migration_and_backfill_provider_protocol() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        // Simulate v0.12 schema without protocol column
        conn.execute(
            "CREATE TABLE providers (
                tool TEXT NOT NULL,
                id TEXT NOT NULL,
                base_url TEXT,
                key_ref TEXT NOT NULL,
                extra TEXT NOT NULL DEFAULT 'null',
                is_active INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (tool, id)
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (tool, id, base_url, key_ref, extra, is_active)
             VALUES ('claude', 'official', NULL, 'ref1', '{}', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (tool, id, base_url, key_ref, extra, is_active)
             VALUES ('claude', 'thirdparty', 'https://api.deepseek.com', 'ref2', '{}', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO providers (tool, id, base_url, key_ref, extra, is_active)
             VALUES ('claude', 'anthropic_proxy', 'https://api.anthropic.com/v1', 'ref3', '{}', 0)",
            [],
        )
        .unwrap();
        drop(conn);

        let db = Database::open(&db_path).unwrap();
        let p_off = db.get(ToolKind::ClaudeCode, "official").unwrap().unwrap();
        assert_eq!(p_off.protocol, ProviderProtocol::Messages);

        let p_tp = db.get(ToolKind::ClaudeCode, "thirdparty").unwrap().unwrap();
        assert_eq!(p_tp.protocol, ProviderProtocol::Chat);

        let p_ap = db
            .get(ToolKind::ClaudeCode, "anthropic_proxy")
            .unwrap()
            .unwrap();
        assert_eq!(p_ap.protocol, ProviderProtocol::Messages);
    }

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
    fn set_routes_with_models_roundtrips() {
        let db = Database::open_memory().unwrap();
        let mut models = BTreeMap::new();
        models.insert("freebuff".to_string(), "z-ai/glm-5.3-flash".to_string());
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        db.set_routes_with_models(
            &["freebuff".into(), "omni".into()],
            Some("global-fallback"),
            "chat",
            &models,
        )
        .unwrap();

        let (routes, model_override, target_protocol) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["freebuff".to_string(), "omni".to_string()]);
        assert_eq!(model_override.as_deref(), Some("global-fallback"));
        assert_eq!(target_protocol, "chat");
        assert_eq!(db.get_route_models().unwrap(), models);
    }

    #[test]
    fn get_route_models_defaults_to_empty_map() {
        let db = Database::open_memory().unwrap();
        assert!(db.get_route_models().unwrap().is_empty());
        // Legacy set_routes never writes the column → still empty.
        db.set_routes(&["yy".into()], None, "chat").unwrap();
        assert!(db.get_route_models().unwrap().is_empty());
    }

    #[test]
    fn legacy_set_routes_preserves_new_columns() {
        // GUI + test callers use the three-arg set_routes. Its column-scoped
        // upsert must not wipe a CLI-configured model map or the 4xx toggle.
        let db = Database::open_memory().unwrap();
        let mut models = BTreeMap::new();
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        db.set_routes_with_models(&["omni".into()], None, "chat", &models)
            .unwrap();
        db.set_failover_4xx(true).unwrap();
        db.set_smart_enabled(true).unwrap();

        db.set_routes(&["omni".into(), "bak".into()], None, "chat")
            .unwrap();

        assert_eq!(db.get_route_models().unwrap(), models);
        assert!(db.get_failover_4xx().unwrap());
        assert!(db.smart_enabled().unwrap());
        let (routes, _, _) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["omni".to_string(), "bak".to_string()]);
    }

    #[test]
    fn set_failover_4xx_defaults_off_and_roundtrips() {
        let db = Database::open_memory().unwrap();
        assert!(!db.get_failover_4xx().unwrap());
        db.set_failover_4xx(true).unwrap();
        assert!(db.get_failover_4xx().unwrap());
        db.set_failover_4xx(false).unwrap();
        assert!(!db.get_failover_4xx().unwrap());
    }

    #[test]
    fn failover_4xx_toggle_keeps_routes_and_models() {
        let db = Database::open_memory().unwrap();
        let mut models = BTreeMap::new();
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        db.set_routes_with_models(&["omni".into()], None, "chat", &models)
            .unwrap();

        db.set_failover_4xx(true).unwrap();

        let (routes, _, _) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["omni".to_string()]);
        assert_eq!(db.get_route_models().unwrap(), models);
        assert!(db.get_failover_4xx().unwrap());
    }

    #[test]
    fn clear_routes_resets_models_but_keeps_toggles() {
        let db = Database::open_memory().unwrap();
        let mut models = BTreeMap::new();
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        db.set_routes_with_models(&["omni".into()], None, "chat", &models)
            .unwrap();
        db.set_failover_4xx(true).unwrap();
        db.set_smart_enabled(true).unwrap();

        db.clear_routes().unwrap();

        assert!(db.get_route_models().unwrap().is_empty());
        // Independent toggles survive a route clear.
        assert!(db.get_failover_4xx().unwrap());
        assert!(db.smart_enabled().unwrap());
        assert!(db.get_routes().unwrap().is_none());
    }

    #[test]
    fn proxy_use_does_not_reset_toggles() {
        // `xfade proxy use` goes through set_routes_with_models; it must not
        // flip the Smart Fader or the 4xx failover switch.
        let db = Database::open_memory().unwrap();
        db.set_smart_enabled(true).unwrap();
        db.set_failover_4xx(true).unwrap();

        db.set_routes_with_models(&["yy".into()], None, "chat", &BTreeMap::new())
            .unwrap();

        assert!(db.smart_enabled().unwrap());
        assert!(db.get_failover_4xx().unwrap());
    }

    fn sample_model_routes() -> ModelRoutes {
        let mut map = ModelRoutes::new();
        map.insert(
            "auto/best-fast".to_string(),
            ModelRoute {
                route_id: "omni".to_string(),
                upstream_model: "auto/best-fast".to_string(),
            },
        );
        map.insert(
            "omni/gpt-x".to_string(),
            ModelRoute {
                route_id: "omni".to_string(),
                upstream_model: "gpt-x".to_string(),
            },
        );
        map
    }

    #[test]
    fn models_merge_enabled_defaults_off_and_roundtrips() {
        let db = Database::open_memory().unwrap();
        assert!(!db.models_merge_enabled().unwrap());
        db.set_models_merge_enabled(true).unwrap();
        assert!(db.models_merge_enabled().unwrap());
        db.set_models_merge_enabled(false).unwrap();
        assert!(!db.models_merge_enabled().unwrap());
    }

    #[test]
    fn handover_auto_inject_defaults_off_and_roundtrips() {
        let db = Database::open_memory().unwrap();
        assert!(!db.handover_auto_inject_enabled().unwrap());
        db.set_handover_auto_inject_enabled(true).unwrap();
        assert!(db.handover_auto_inject_enabled().unwrap());
        db.set_handover_auto_inject_enabled(false).unwrap();
        assert!(!db.handover_auto_inject_enabled().unwrap());
    }

    #[test]
    fn models_merge_toggle_keeps_routes_and_models() {
        let db = Database::open_memory().unwrap();
        let mut models = BTreeMap::new();
        models.insert("omni".to_string(), "auto/best-fast".to_string());
        db.set_routes_with_models(&["omni".into()], None, "chat", &models)
            .unwrap();
        db.set_model_routes(&sample_model_routes()).unwrap();
        db.set_failover_4xx(true).unwrap();
        db.set_smart_enabled(true).unwrap();

        db.set_models_merge_enabled(true).unwrap();

        let (routes, _, _) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["omni".to_string()]);
        assert_eq!(db.get_route_models().unwrap(), models);
        assert_eq!(db.get_model_routes().unwrap(), sample_model_routes());
        assert!(db.get_failover_4xx().unwrap());
        assert!(db.smart_enabled().unwrap());
        assert!(db.models_merge_enabled().unwrap());
    }

    #[test]
    fn get_model_routes_defaults_to_empty_map() {
        let db = Database::open_memory().unwrap();
        assert!(db.get_model_routes().unwrap().is_empty());
        db.set_routes(&["yy".into()], None, "chat").unwrap();
        assert!(db.get_model_routes().unwrap().is_empty());
    }

    #[test]
    fn set_model_routes_roundtrips_and_preserves_toggles() {
        let db = Database::open_memory().unwrap();
        db.set_routes_with_models(&["omni".into()], None, "chat", &BTreeMap::new())
            .unwrap();
        db.set_failover_4xx(true).unwrap();
        db.set_smart_enabled(true).unwrap();
        db.set_models_merge_enabled(true).unwrap();

        let map = sample_model_routes();
        db.set_model_routes(&map).unwrap();

        assert_eq!(db.get_model_routes().unwrap(), map);
        let (routes, _, _) = db.get_routes().unwrap().unwrap();
        assert_eq!(routes, vec!["omni".to_string()]);
        assert!(db.get_failover_4xx().unwrap());
        assert!(db.smart_enabled().unwrap());
        assert!(db.models_merge_enabled().unwrap());
    }

    #[test]
    fn clear_routes_clears_model_routes_map_but_keeps_toggles() {
        let db = Database::open_memory().unwrap();
        db.set_routes_with_models(&["omni".into()], None, "chat", &BTreeMap::new())
            .unwrap();
        db.set_model_routes(&sample_model_routes()).unwrap();
        db.set_failover_4xx(true).unwrap();
        db.set_smart_enabled(true).unwrap();
        db.set_models_merge_enabled(true).unwrap();

        db.clear_routes().unwrap();

        assert!(db.get_model_routes().unwrap().is_empty());
        assert!(db.get_failover_4xx().unwrap());
        assert!(db.smart_enabled().unwrap());
        assert!(db.models_merge_enabled().unwrap());
        assert!(db.get_routes().unwrap().is_none());
    }

    #[test]
    fn proxy_use_clears_stale_model_routes_map() {
        let db = Database::open_memory().unwrap();
        db.set_model_routes(&sample_model_routes()).unwrap();
        db.set_models_merge_enabled(true).unwrap();

        db.set_routes_with_models(&["yy".into()], None, "chat", &BTreeMap::new())
            .unwrap();

        assert!(db.get_model_routes().unwrap().is_empty());
        assert!(db.models_merge_enabled().unwrap());
    }

    #[test]
    fn legacy_set_routes_clears_model_routes_map() {
        let db = Database::open_memory().unwrap();
        db.set_model_routes(&sample_model_routes()).unwrap();
        db.set_models_merge_enabled(true).unwrap();
        db.set_failover_4xx(true).unwrap();

        db.set_routes(&["yy".into()], None, "chat").unwrap();

        assert!(db.get_model_routes().unwrap().is_empty());
        assert!(db.models_merge_enabled().unwrap());
        assert!(db.get_failover_4xx().unwrap());
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
    fn latest_repo_map_returns_most_recently_written() {
        let db = Database::open_memory().unwrap();
        assert!(db.latest_repo_map().unwrap().is_none());
        db.save_repo_map("/a", "fp1", r#"{"lang":"rust"}"#).unwrap();
        db.save_repo_map("/b", "fp2", r#"{"lang":"go"}"#).unwrap();
        let (root, _) = db.latest_repo_map().unwrap().unwrap();
        assert_eq!(root, "/b");
        // Re-saving /a makes it the latest again (rowid ordering, immune to
        // same-second timestamp ties).
        db.save_repo_map("/a", "fp3", r#"{"lang":"rust"}"#).unwrap();
        let (root, map) = db.latest_repo_map().unwrap().unwrap();
        assert_eq!(root, "/a");
        assert_eq!(map, r#"{"lang":"rust"}"#);
    }

    #[test]
    fn test_model_mappings_crud_and_preservation() {
        let db = Database::open_memory().unwrap();
        assert!(db.get_model_mappings().unwrap().is_empty());
        assert!(db
            .get_mappings_for_client_model("sonnet")
            .unwrap()
            .is_empty());

        // Single set
        db.set_model_mapping("sonnet", "anthropic", "claude-3-7-sonnet-20250219")
            .unwrap();
        let map = db.get_mappings_for_client_model("sonnet").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("anthropic").unwrap(), "claude-3-7-sonnet-20250219");

        // Additive batch set
        db.set_model_mappings_batch(
            "sonnet",
            &[
                ("deepseek".into(), "deepseek-chat".into()),
                ("*".into(), "claude-3-7-sonnet".into()),
            ],
        )
        .unwrap();
        let map2 = db.get_mappings_for_client_model("sonnet").unwrap();
        assert_eq!(map2.len(), 3);
        assert_eq!(map2.get("anthropic").unwrap(), "claude-3-7-sonnet-20250219");
        assert_eq!(map2.get("deepseek").unwrap(), "deepseek-chat");
        assert_eq!(map2.get("*").unwrap(), "claude-3-7-sonnet");

        // Verify whitespace trimming on lookup
        let map_trimmed = db.get_mappings_for_client_model("  sonnet  ").unwrap();
        assert_eq!(map_trimmed.len(), 3);

        // Verify full dictionary
        let all = db.get_model_mappings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all.get("sonnet").unwrap().len(), 3);

        // Verify clear_routes does not erase model_mappings
        db.clear_routes().unwrap();
        assert_eq!(db.get_mappings_for_client_model("sonnet").unwrap().len(), 3);

        // Remove single provider mapping
        assert!(db.remove_model_mapping("sonnet", Some("deepseek")).unwrap());
        assert_eq!(db.get_mappings_for_client_model("sonnet").unwrap().len(), 2);
        assert!(!db
            .remove_model_mapping("sonnet", Some("nonexistent"))
            .unwrap());

        // Remove whole alias
        assert!(db.remove_model_mapping("sonnet", None).unwrap());
        assert!(db
            .get_mappings_for_client_model("sonnet")
            .unwrap()
            .is_empty());

        // Clear all
        db.set_model_mapping("haiku", "anthropic", "claude-3-5-haiku")
            .unwrap();
        db.clear_model_mappings().unwrap();
        assert!(db.get_model_mappings().unwrap().is_empty());
    }

    #[test]
    fn test_model_map_profiles_and_batch_apply() {
        let db = Database::open_memory().unwrap();
        let applied = db
            .apply_model_mappings_batch(
                &[
                    ("sonnet".into(), "ds".into(), "deepseek-chat".into()),
                    ("opus".into(), "ds".into(), "deepseek-reasoner".into()),
                ],
                true,
            )
            .unwrap();
        assert_eq!(applied, 2);
        db.save_model_map_profile("daily-ds", "DeepSeek daily", None)
            .unwrap();

        // Replace with another mapping set and save second profile
        db.apply_model_mappings_batch(&[("sonnet".into(), "glm".into(), "glm-4.5".into())], true)
            .unwrap();
        db.save_model_map_profile("glm-only", "GLM fallback", None)
            .unwrap();

        // Switch back to daily-ds with replace=true
        let switched = db.switch_model_map_profile("daily-ds", true).unwrap();
        assert_eq!(switched, 2);
        let active_maps = db.get_model_mappings().unwrap();
        assert_eq!(active_maps.len(), 2);
        assert_eq!(active_maps["sonnet"]["ds"], "deepseek-chat");

        let profiles = db.list_model_map_profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        assert!(
            profiles
                .iter()
                .find(|p| p.name == "daily-ds")
                .unwrap()
                .is_active
        );
        assert!(
            !profiles
                .iter()
                .find(|p| p.name == "glm-only")
                .unwrap()
                .is_active
        );

        // Merge second profile on top of daily-ds
        db.switch_model_map_profile("glm-only", false).unwrap();
        let merged = db.get_model_mappings().unwrap();
        assert_eq!(merged["sonnet"]["ds"], "deepseek-chat");
        assert_eq!(merged["sonnet"]["glm"], "glm-4.5");

        assert!(db.delete_model_map_profile("glm-only").unwrap());
        assert!(!db.delete_model_map_profile("glm-only").unwrap());
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
        // The newer columns also get safe defaults on a pre-existing row.
        assert!(db.get_route_models().unwrap().is_empty());
        assert!(!db.get_failover_4xx().unwrap());
        assert!(!db.models_merge_enabled().unwrap());
        assert!(db.get_model_routes().unwrap().is_empty());
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
            cost_usd_micros: 0,
            ..Default::default()
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
            cost_usd_micros: 0,
            ..Default::default()
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
            cost_usd_micros: 0,
            ..Default::default()
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
            cost_usd_micros: 0,
            ..Default::default()
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
    fn test_request_log_with_cost_usd_micros_and_stats_aggregation() {
        let db = Database::open_memory().unwrap();
        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:00:00Z".into(),
            endpoint: "chat".into(),
            model: Some("claude-3-7-sonnet".into()),
            provider_id: "prov1".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 10_000,
            completion_tokens: 2_000,
            duration_ms: 500,
            error: None,
            session_id: Some("s1".into()),
            cost_usd_micros: 60_000, // 60,000 micros = $0.06
            ..Default::default()
        })
        .unwrap();

        db.insert_request_log(&RequestLog {
            ts: "2026-07-26T10:05:00Z".into(),
            endpoint: "chat".into(),
            model: Some("claude-3-7-sonnet".into()),
            provider_id: "prov1".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 5_000,
            completion_tokens: 1_000,
            duration_ms: 300,
            error: None,
            session_id: Some("s1".into()),
            cost_usd_micros: 30_000, // 30,000 micros = $0.03
            ..Default::default()
        })
        .unwrap();

        // 1. Verify recent request logs have cost_usd_micros
        let recent = db.recent_request_logs(2).unwrap();
        assert_eq!(recent[0].cost_usd_micros, 30_000);
        assert_eq!(recent[1].cost_usd_micros, 60_000);

        // 2. Verify stats_since aggregates cost_usd_micros
        let rows = db
            .stats_since("2026-07-25T00:00:00Z", StatsGroupBy::Provider, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 2);
        assert_eq!(rows[0].cost_usd_micros, 90_000);
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
                cost_usd_micros: 0,
                ..Default::default()
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
            cost_usd_micros: 15,
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&log).unwrap();
        assert_eq!(v["endpoint"], "chat");
        assert_eq!(v["status"], 200);
        assert_eq!(v["cost_usd_micros"], 15);
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
            500,
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
            50,
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
        assert_eq!(s.cost_usd_micros, 550);
        assert_eq!(s.status, "error");
        assert_eq!(s.first_prompt.as_deref(), Some("hello world"));

        // Empty tool/provider must not clobber filled fields.
        db.update_session_progress("s1", "", "", None, 0, 0, 0, "ok", "2026-10-09T10:03:00Z")
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
    fn handover_pending_and_injection_lifecycle() {
        let db = Database::open_memory().unwrap();
        // Insert an unattributed handover
        db.insert_handover(
            "claude",
            Some("opus"),
            Some("sonnet"),
            "2026-10-09T10:00:00Z",
        )
        .unwrap();

        // No session → refuse to claim an unattributed handover.
        assert!(db
            .find_pending_handover(None, "2026-10-09T09:50:00Z")
            .unwrap()
            .is_none());

        // Query pending handover within window
        let pending = db
            .find_pending_handover(Some("s1"), "2026-10-09T09:50:00Z")
            .unwrap()
            .expect("should find pending handover");
        assert_eq!(pending.from_model.as_deref(), Some("opus"));
        assert_eq!(pending.to_model.as_deref(), Some("sonnet"));
        assert!(!pending.injected);

        // Mark it as injected
        db.mark_handover_injected(pending.id, "s1", "2026-10-09T10:01:00Z")
            .unwrap();

        // Should no longer be pending
        let pending_after = db
            .find_pending_handover(Some("s1"), "2026-10-09T09:50:00Z")
            .unwrap();
        assert!(pending_after.is_none());

        // But list_handovers shows it as injected
        let hs = db.list_handovers("s1").unwrap();
        assert_eq!(hs.len(), 1);
        assert!(hs[0].injected);
        assert_eq!(hs[0].injected_at.as_deref(), Some("2026-10-09T10:01:00Z"));
    }

    #[test]
    fn find_pending_handover_prefers_session_attributed() {
        let db = Database::open_memory().unwrap();
        db.insert_handover("claude", Some("a"), Some("b"), "2026-10-09T10:00:00Z")
            .unwrap();
        // Attribute the older row to s1 while leaving injected=0.
        db.backfill_handover_session("s1", "2026-10-09T09:00:00Z")
            .unwrap();
        // Newer unattributed switch — must not steal priority from s1's row.
        db.insert_handover("claude", Some("c"), Some("d"), "2026-10-09T10:01:00Z")
            .unwrap();
        let pending = db
            .find_pending_handover(Some("s1"), "2026-10-09T09:00:00Z")
            .unwrap()
            .expect("should find pending handover");
        assert_eq!(pending.session_id.as_deref(), Some("s1"));
        assert_eq!(pending.to_model.as_deref(), Some("b"));
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
            cost_usd_micros: 0,
            ..Default::default()
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
                cost_usd_micros: 0,
                ..Default::default()
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

    #[test]
    fn test_audit_logs_crud_and_query() {
        let db = Database::open_memory().unwrap();
        let log1 = AuditLog {
            id: 0,
            ts: 1000,
            client_tool: "claude".into(),
            client_token_id: "xfade-local-claude-test".into(),
            provider_id: "kimi".into(),
            upstream_domain: "api.moonshot.cn".into(),
            upstream_model: "kimi-k2.5".into(),
            key_ref: "provider:kimi#sha256:abcd".into(),
            status_code: 200,
            duration_ms: 150,
            prompt_tokens: 50,
            completion_tokens: 20,
            failover_path: None,
        };
        let log2 = AuditLog {
            id: 0,
            ts: 2000,
            client_tool: "codex".into(),
            client_token_id: "xfade-local-codex-test".into(),
            provider_id: "deepseek".into(),
            upstream_domain: "api.deepseek.com".into(),
            upstream_model: "deepseek-coder".into(),
            key_ref: "provider:deepseek#sha256:ef01".into(),
            status_code: 502,
            duration_ms: 300,
            prompt_tokens: 100,
            completion_tokens: 0,
            failover_path: Some(r#"[{"provider":"deepseek","status":502}]"#.into()),
        };

        db.insert_audit_log(&log1).unwrap();
        db.insert_audit_log(&log2).unwrap();

        let recent = db.recent_audit_logs(10, None).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].provider_id, "deepseek");
        assert_eq!(recent[1].provider_id, "kimi");

        let claude_only = db.recent_audit_logs(10, Some("claude")).unwrap();
        assert_eq!(claude_only.len(), 1);
        assert_eq!(claude_only[0].provider_id, "kimi");

        let exported = db.export_audit_logs(Some(1500)).unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].provider_id, "deepseek");
    }

    #[test]
    fn test_egress_whitelist_and_policy() {
        let db = Database::open_memory().unwrap();

        // 1. Default policy is Registered
        assert_eq!(db.get_egress_policy().unwrap(), EgressPolicy::Registered);

        // 2. Localhost is always allowed
        assert!(db.is_domain_allowed("localhost").unwrap());
        assert!(db.is_domain_allowed("127.0.0.1").unwrap());
        assert!(db.is_domain_allowed("127.0.0.1:8080").unwrap());
        assert!(db.is_domain_allowed("http://[::1]:8080/v1").unwrap());
        assert!(db.is_domain_allowed("[::1]").unwrap());
        assert!(db.is_domain_allowed("::1").unwrap());

        // 3. Unregistered domain is blocked in Registered mode
        assert!(!db.is_domain_allowed("api.openai.com").unwrap());
        assert!(!db.is_domain_allowed("evil.com").unwrap());

        // 4. Register provider and verify its domain is allowed
        let prov = Provider::new(
            "openai",
            ToolKind::ClaudeCode,
            Some("https://api.openai.com/v1".into()),
        );
        db.upsert(&prov).unwrap();
        assert!(db.is_domain_allowed("api.openai.com").unwrap());
        assert!(db
            .is_domain_allowed("https://api.openai.com/v1/chat/completions")
            .unwrap());
        assert!(!db.is_domain_allowed("evil.com").unwrap());

        // 5. Add custom egress domain
        db.add_egress_domain("custom.ai").unwrap();
        let list = db.list_egress_domains().unwrap();
        assert_eq!(list, vec!["custom.ai".to_string()]);
        assert!(db.is_domain_allowed("custom.ai").unwrap());
        assert!(db.is_domain_allowed("https://custom.ai:443/test").unwrap());

        // 6. Remove custom egress domain
        assert!(db.remove_egress_domain("custom.ai").unwrap());
        assert!(!db.is_domain_allowed("custom.ai").unwrap());

        // 7. Unrestricted policy allows all domains
        db.set_egress_policy(EgressPolicy::Unrestricted).unwrap();
        assert_eq!(db.get_egress_policy().unwrap(), EgressPolicy::Unrestricted);
        assert!(db.is_domain_allowed("evil.com").unwrap());
        assert!(db.is_domain_allowed("any-random-domain.org").unwrap());

        // Revert back
        db.set_egress_policy(EgressPolicy::Registered).unwrap();
        assert!(!db.is_domain_allowed("evil.com").unwrap());
    }

    #[test]
    fn test_model_pricing_crud_and_precedence() {
        let db = Database::open_memory().unwrap();

        // Initially empty
        assert!(db.list_model_pricing().unwrap().is_empty());
        assert!(db
            .find_dynamic_model_price("claude-sonnet-5.5", None)
            .unwrap()
            .is_none());

        // 1. Upsert a synced row
        let row1 = DynamicPriceRow {
            model_pattern: "anthropic/claude-sonnet-5.5".to_string(),
            input_micros_per_m: 2_000_000,
            output_micros_per_m: 10_000_000,
            cache_read_micros_per_m: 100_000,
            cache_write_micros_per_m: 2_500_000,
            source: "synced".to_string(),
            updated_at: "2026-11-20T20:00:00Z".to_string(),
        };
        db.upsert_model_pricing(&row1).unwrap();

        // Find by full model name
        let found = db
            .find_dynamic_model_price("anthropic/claude-sonnet-5.5", None)
            .unwrap()
            .expect("should find row1");
        assert_eq!(found.input_micros_per_m, 2_000_000);
        assert_eq!(found.source, "synced");

        // Prefix match
        let found_prefix = db
            .find_dynamic_model_price("anthropic/claude-sonnet-5.5-20261101", None)
            .unwrap()
            .expect("should match prefix");
        assert_eq!(found_prefix.model_pattern, "anthropic/claude-sonnet-5.5");

        // Canonical alias match
        let found_canonical = db
            .find_dynamic_model_price("sonnet", Some("anthropic/claude-sonnet-5.5"))
            .unwrap()
            .expect("should match canonical");
        assert_eq!(found_canonical.model_pattern, "anthropic/claude-sonnet-5.5");

        // 2. Custom override wins over synced
        let row_custom = DynamicPriceRow {
            model_pattern: "claude-sonnet-5.5".to_string(),
            input_micros_per_m: 1_500_000,
            output_micros_per_m: 8_000_000,
            cache_read_micros_per_m: 50_000,
            cache_write_micros_per_m: 1_000_000,
            source: "custom".to_string(),
            updated_at: "2026-11-20T20:05:00Z".to_string(),
        };
        db.upsert_model_pricing(&row_custom).unwrap();

        // When queried with model="claude-sonnet-5.5", custom row must be returned
        let found_custom = db
            .find_dynamic_model_price("claude-sonnet-5.5", None)
            .unwrap()
            .expect("custom");
        assert_eq!(found_custom.input_micros_per_m, 1_500_000);
        assert_eq!(found_custom.source, "custom");

        // 3. Batch upsert
        let batch = vec![
            DynamicPriceRow {
                model_pattern: "gpt-6.2-astra".to_string(),
                input_micros_per_m: 8_000_000,
                output_micros_per_m: 40_000_000,
                cache_read_micros_per_m: 400_000,
                cache_write_micros_per_m: 10_000_000,
                source: "synced".to_string(),
                updated_at: "2026-11-20T20:00:00Z".to_string(),
            },
            DynamicPriceRow {
                model_pattern: "deepseek-v4.1-flash".to_string(),
                input_micros_per_m: 36_000,
                output_micros_per_m: 500_000,
                cache_read_micros_per_m: 10_000,
                cache_write_micros_per_m: 36_000,
                source: "synced".to_string(),
                updated_at: "2026-11-20T20:00:00Z".to_string(),
            },
        ];
        let inserted = db.batch_upsert_model_pricing(&batch).unwrap();
        assert_eq!(inserted, 2);
        assert_eq!(db.list_model_pricing().unwrap().len(), 4);

        // 4. Delete single custom pricing
        assert!(db.delete_model_pricing("claude-sonnet-5.5").unwrap());
        assert!(!db.delete_model_pricing("non-existent").unwrap());

        // 5. Clear all synced
        let cleared = db.clear_synced_model_pricing().unwrap();
        assert_eq!(cleared, 3);
        assert!(db.list_model_pricing().unwrap().is_empty());

        // 6. Delimiter collision check: pattern 'gpt-4' must NOT match 'gpt-4o'
        let row_gpt4 = DynamicPriceRow {
            model_pattern: "gpt-4".to_string(),
            input_micros_per_m: 30_000_000,
            output_micros_per_m: 60_000_000,
            cache_read_micros_per_m: 0,
            cache_write_micros_per_m: 0,
            source: "synced".to_string(),
            updated_at: "2026-11-20T20:00:00Z".to_string(),
        };
        db.upsert_model_pricing(&row_gpt4).unwrap();

        // Exact match
        assert!(db
            .find_dynamic_model_price("gpt-4", None)
            .unwrap()
            .is_some());
        // Hyphenated suffix match
        assert!(db
            .find_dynamic_model_price("gpt-4-turbo", None)
            .unwrap()
            .is_some());
        // Non-delimiter suffix must NOT match!
        assert!(db
            .find_dynamic_model_price("gpt-4o", None)
            .unwrap()
            .is_none());
        assert!(db
            .find_dynamic_model_price("gpt-4o-mini", None)
            .unwrap()
            .is_none());

        // Empty pattern lookup
        assert!(db.find_dynamic_model_price("", None).unwrap().is_none());
        assert!(db.find_dynamic_model_price("   ", None).unwrap().is_none());
    }

    #[test]
    fn test_model_pricing_table_migration_from_single_pk() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Simulate pre-existing database with single-column PK
        conn.execute_batch(
            "CREATE TABLE model_pricing (
                model_pattern TEXT PRIMARY KEY,
                input_micros_per_m INTEGER NOT NULL,
                output_micros_per_m INTEGER NOT NULL,
                cache_read_micros_per_m INTEGER NOT NULL DEFAULT 0,
                cache_write_micros_per_m INTEGER NOT NULL DEFAULT 0,
                source TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO model_pricing VALUES ('old-model', 100, 200, 10, 20, 'synced', '2026-01-01');",
        ).unwrap();

        let db = Database {
            conn: std::sync::Arc::new(std::sync::Mutex::new(conn)),
        };
        db.migrate().unwrap();

        // Check compound primary key is now active
        let row_custom = DynamicPriceRow {
            model_pattern: "old-model".to_string(),
            input_micros_per_m: 50,
            output_micros_per_m: 100,
            cache_read_micros_per_m: 5,
            cache_write_micros_per_m: 10,
            source: "custom".to_string(),
            updated_at: "2026-11-20".to_string(),
        };
        db.upsert_model_pricing(&row_custom).unwrap();

        // Both rows should coexist in table
        let list = db.list_model_pricing().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_request_logs_cache_tokens_roundtrip_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("xfade.db")).unwrap();

        db.insert_request_log(&RequestLog {
            ts: "2026-11-26T10:00:00Z".into(),
            endpoint: "messages".into(),
            model: Some("claude-sonnet-5-5".into()),
            provider_id: "devops".into(),
            tool: "claude".into(),
            status: 200,
            prompt_tokens: 1000,
            completion_tokens: 200,
            cached_prompt_tokens: 7719,
            cache_write_tokens: 500,
            duration_ms: 1200,
            ..Default::default()
        })
        .unwrap();

        let logs = db.recent_request_logs(1).unwrap();
        assert_eq!(logs[0].cached_prompt_tokens, 7719);
        assert_eq!(logs[0].cache_write_tokens, 500);

        let stats = db
            .stats_since("2026-11-26T00:00:00Z", StatsGroupBy::Provider, None)
            .unwrap();
        assert_eq!(stats[0].cached_prompt_tokens, 7719);
        assert_eq!(stats[0].cache_write_tokens, 500);
    }
}
