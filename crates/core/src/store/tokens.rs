use std::collections::HashMap;
use std::fmt::Write;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;
use sha2::{Digest, Sha256};

use crate::error::Result;
use crate::models::ToolKind;
use crate::store::Db;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClientTokenEntry {
    pub tool: ToolKind,
    pub token: String,
    pub created_at_secs: u64,
    pub last_used_secs: Option<u64>,
}

/// Constant-time token comparison via SHA-256 digest to prevent timing side-channels.
pub(crate) fn token_matches(expected: &str, presented: &str) -> bool {
    let a = Sha256::digest(expected.as_bytes());
    let b = Sha256::digest(presented.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn generate_random_hex24() -> String {
    let bytes: [u8; 12] = rand::thread_rng().gen();
    let mut s = String::with_capacity(24);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct TokenManager {
    db: Arc<Db>,
    cache: Arc<RwLock<HashMap<String, ToolKind>>>,
}

impl TokenManager {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Retrieve an existing loopback token for the given tool, or generate and persist a new one.
    pub fn get_or_create(&self, tool: ToolKind) -> Result<String> {
        if let Some((existing_token, _, _)) = self.db.get_client_token(tool.as_str())? {
            if let Ok(mut cache) = self.cache.write() {
                cache.insert(existing_token.clone(), tool);
            }
            return Ok(existing_token);
        }

        let hex = generate_random_hex24();
        let token = format!("xfade-local-{}-{}", tool.as_str(), hex);
        let created_at = now_secs();

        self.db
            .upsert_client_token(tool.as_str(), &token, created_at)?;

        if let Ok(mut cache) = self.cache.write() {
            cache.insert(token.clone(), tool);
        }

        Ok(token)
    }

    /// Validate a token presented by a client.
    /// Checks the in-memory cache first; if not found (e.g. cross-process newly created token),
    /// reloads all tokens from SQLite and checks again.
    pub fn validate(&self, token: &str) -> Option<ToolKind> {
        let clean = token.trim();
        if clean.is_empty() {
            return None;
        }

        // 1. Fast path: check memory cache
        if let Ok(cache) = self.cache.read() {
            for (cached_token, tool) in cache.iter() {
                if token_matches(cached_token, clean) {
                    let tool = *tool;
                    drop(cache);
                    let _ = self
                        .db
                        .update_client_token_last_used(tool.as_str(), now_secs());
                    return Some(tool);
                }
            }
        }

        // 2. Slow path: reload from DB on cache miss
        let tokens = self.db.list_client_tokens().ok()?;
        let mut matched = None;

        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
            for (tool_str, tok, _, _) in tokens {
                if let Ok(tool) = ToolKind::from_str(&tool_str) {
                    if token_matches(&tok, clean) {
                        matched = Some(tool);
                    }
                    cache.insert(tok, tool);
                }
            }
        }

        if let Some(tool) = matched {
            let _ = self
                .db
                .update_client_token_last_used(tool.as_str(), now_secs());
        }

        matched
    }

    /// Rotate the token for a specific tool, invalidating the previous token.
    pub fn rotate(&self, tool: ToolKind) -> Result<String> {
        let hex = generate_random_hex24();
        let new_token = format!("xfade-local-{}-{}", tool.as_str(), hex);
        let created_at = now_secs();

        self.db
            .upsert_client_token(tool.as_str(), &new_token, created_at)?;

        if let Ok(mut cache) = self.cache.write() {
            cache.retain(|_, v| *v != tool);
            cache.insert(new_token.clone(), tool);
        }

        Ok(new_token)
    }

    /// List all persisted client tokens.
    pub fn list_tokens(&self) -> Result<Vec<ClientTokenEntry>> {
        let rows = self.db.list_client_tokens()?;
        let mut entries = Vec::with_capacity(rows.len());

        for (tool_str, token, created_at_secs, last_used_secs) in rows {
            if let Ok(tool) = ToolKind::from_str(&tool_str) {
                entries.push(ClientTokenEntry {
                    tool,
                    token,
                    created_at_secs,
                    last_used_secs,
                });
            }
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_token_get_or_create_and_validate() {
        let dir = tempdir().unwrap();
        let db = Arc::new(Db::open_in_dir(dir.path()).unwrap());
        let tm = TokenManager::new(db.clone());

        let t1 = tm.get_or_create(ToolKind::ClaudeCode).unwrap();
        assert!(
            t1.starts_with("xfade-local-claude-"),
            "token should have prefix: {t1}"
        );
        assert_eq!(tm.validate(&t1), Some(ToolKind::ClaudeCode));
        assert_eq!(tm.validate("invalid-token"), None);

        // Repeated call returns same token
        let t2 = tm.get_or_create(ToolKind::ClaudeCode).unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_token_cross_process_cache_miss_reload() {
        let dir = tempdir().unwrap();
        let db = Arc::new(Db::open_in_dir(dir.path()).unwrap());
        let tm1 = TokenManager::new(db.clone());
        let tm2 = TokenManager::new(db.clone());

        // tm1 generates token
        let token = tm1.get_or_create(ToolKind::Codex).unwrap();

        // tm2's memory cache is empty, but validate should query DB on miss
        assert_eq!(tm2.validate(&token), Some(ToolKind::Codex));
    }

    #[test]
    fn test_token_rotation() {
        let dir = tempdir().unwrap();
        let db = Arc::new(Db::open_in_dir(dir.path()).unwrap());
        let tm = TokenManager::new(db);

        let t1 = tm.get_or_create(ToolKind::OpenCode).unwrap();
        let t2 = tm.rotate(ToolKind::OpenCode).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(tm.validate(&t2), Some(ToolKind::OpenCode));
        assert_eq!(tm.validate(&t1), None);
    }

    #[test]
    fn test_token_list_tokens() {
        let dir = tempdir().unwrap();
        let db = Arc::new(Db::open_in_dir(dir.path()).unwrap());
        let tm = TokenManager::new(db);

        let t_claude = tm.get_or_create(ToolKind::ClaudeCode).unwrap();
        let t_codex = tm.get_or_create(ToolKind::Codex).unwrap();

        let list = tm.list_tokens().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list
            .iter()
            .any(|e| e.tool == ToolKind::ClaudeCode && e.token == t_claude));
        assert!(list
            .iter()
            .any(|e| e.tool == ToolKind::Codex && e.token == t_codex));
    }
}
