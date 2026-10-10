//! Session engine (V2): give proxy requests a stable conversation identity.
//!
//! Clients (Claude Code, Codex, …) resend the full conversation history with
//! every request, so the *opener* — system prompt plus first user turn — is
//! stable across the turns of one conversation and cheap to extract (no
//! full-body canonicalization of MB-sized histories). The opener is hashed
//! into a fingerprint; consecutive requests whose fingerprint matches a
//! session started within the sticky window reuse that session's id. This
//! gives the unified session view (and later the Handover engine) a
//! request↔session correlation without ever storing prompt text.
//!
//! Known limits, accepted for V2.0:
//! - A client that rewrites its opener (e.g. Claude Code history compaction)
//!   starts a new session — compaction is arguably a new context anyway.
//! - Two conversations with an identical opener inside the sticky window
//!   share one session row (observability-grade identity, not a guarantee).

use crate::error::Result;
use crate::store::db::Database;
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Requests whose opener fingerprint matches a session started within this
/// window reuse that session (30 minutes of inactivity ends a session).
pub const SESSION_STICKY_WINDOW_SECS: i64 = 30 * 60;

/// Privacy bound for the stored `first_prompt` preview. Full prompt text is
/// never persisted.
pub const FIRST_PROMPT_PREVIEW_CHARS: usize = 200;

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn cutoff_rfc3339(window_secs: i64) -> String {
    (OffsetDateTime::now_utc() - time::Duration::seconds(window_secs))
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Extract the conversation opener (system + first user turn) from a parsed
/// request body, per endpoint wire format:
/// - `chat`: system-role message + first user message (`messages[]`)
/// - `messages`: `system` field + first user message (Anthropic)
/// - `responses`: `instructions` + first user input item (OpenAI Responses)
///
/// Non-conversation endpoints (model listings) yield an empty opener.
pub fn opener_text(body: &serde_json::Value, endpoint: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    match endpoint {
        "chat" => {
            if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
                if let Some(sys) = msgs
                    .iter()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
                {
                    if let Some(c) = sys.get("content") {
                        parts.push(content_text(c));
                    }
                }
                if let Some(user) = msgs
                    .iter()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                {
                    if let Some(c) = user.get("content") {
                        parts.push(content_text(c));
                    }
                }
            }
        }
        "messages" => {
            if let Some(sys) = body.get("system") {
                parts.push(content_text(sys));
            }
            if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
                if let Some(user) = msgs
                    .iter()
                    .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
                {
                    if let Some(c) = user.get("content") {
                        parts.push(content_text(c));
                    }
                }
            }
        }
        "responses" => {
            if let Some(instr) = body.get("instructions").and_then(|i| i.as_str()) {
                parts.push(instr.to_string());
            }
            if let Some(items) = body.get("input").and_then(|i| i.as_array()) {
                for item in items {
                    if item.get("role").and_then(|r| r.as_str()) == Some("user") {
                        if let Some(c) = item.get("content") {
                            parts.push(content_text(c));
                        }
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    parts.join("\n")
}

/// Content can be a plain string or an array of typed blocks
/// (`[{"type":"text","text":"…"}]`); both forms collapse to their text.
pub(crate) fn content_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Hash the opener (+ endpoint) into a stable fingerprint.
pub fn fingerprint(opener: &str, endpoint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(endpoint.as_bytes());
    hasher.update([0]);
    hasher.update(opener.as_bytes());
    let out = hasher.finalize();
    format!("{out:x}")
}

/// Resolve (lookup-or-create) the session for a request. Best-effort by
/// design: the proxy must serve the request even when the DB hiccups, so
/// errors degrade to a detached (`None`) session.
pub fn resolve_session(db: &Database, fp: &str, opener: &str) -> Option<String> {
    let cutoff = cutoff_rfc3339(SESSION_STICKY_WINDOW_SECS);
    if let Ok(Some(id)) = db.find_recent_session_id(fp, &cutoff) {
        return Some(id);
    }
    let now = OffsetDateTime::now_utc();
    let started = now.format(&Rfc3339).ok()?;
    let id = new_session_id(fp, &now);
    let preview = truncate_chars(opener, FIRST_PROMPT_PREVIEW_CHARS);
    db.insert_session(&id, fp, &started, Some(&preview)).ok()?;
    Some(id)
}

fn new_session_id(fp: &str, now: &OffsetDateTime) -> String {
    // Nanosecond wall clock + fingerprint prefix: unique under SQLite's
    // serialized writes without pulling in a uuid dependency.
    let prefix: String = fp.chars().take(12).collect();
    format!("s{}-{prefix}", now.unix_timestamp_nanos())
}

/// Fold one request outcome into its session (token accumulation, status,
/// provider/model/tool fill-in) and attribute the latest unattributed
/// handover — the "model switched mid-session" timeline entry. Best-effort.
#[allow(clippy::too_many_arguments)] // one flat record per logging site
pub fn record_outcome(
    db: &Database,
    session_id: Option<&str>,
    tool: &str,
    provider_id: &str,
    model: Option<&str>,
    prompt_tokens: i64,
    completion_tokens: i64,
    status_ok: bool,
) {
    let Some(sid) = session_id else { return };
    let status = if status_ok { "ok" } else { "error" };
    let _ = db.update_session_progress(
        sid,
        tool,
        provider_id,
        model,
        prompt_tokens,
        completion_tokens,
        status,
        &now_rfc3339(),
    );
    let cutoff = cutoff_rfc3339(SESSION_STICKY_WINDOW_SECS);
    let _ = db.backfill_handover_session(sid, &cutoff);
}

/// Record a config-level model/route switch on the session timeline
/// (session_id NULL — backfilled by the next matching request).
pub fn record_handover(
    db: &Database,
    tool: &str,
    from_model: Option<&str>,
    to_model: Option<&str>,
) -> Result<()> {
    db.insert_handover(tool, from_model, to_model, &now_rfc3339())
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chat_body(system: Option<&str>, turns: &[(&str, &str)]) -> serde_json::Value {
        let mut messages = Vec::new();
        if let Some(s) = system {
            messages.push(json!({"role": "system", "content": s}));
        }
        for (role, content) in turns {
            messages.push(json!({"role": role, "content": content}));
        }
        json!({"model": "m", "messages": messages})
    }

    #[test]
    fn opener_is_stable_across_turns() {
        // Turn 1 and turn 2 of the same conversation share the opener.
        let t1 = chat_body(Some("be terse"), &[("user", "fix the bug in main.rs")]);
        let t2 = chat_body(
            Some("be terse"),
            &[
                ("user", "fix the bug in main.rs"),
                ("assistant", "done"),
                ("user", "add a test too"),
            ],
        );
        assert_eq!(
            opener_text(&t1, "chat"),
            opener_text(&t2, "chat"),
            "opener must not change as the conversation grows"
        );
    }

    #[test]
    fn opener_per_protocol() {
        // Anthropic messages: system field + first user.
        let m = json!({
            "system": "sys",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": "yo"}
            ]
        });
        assert_eq!(opener_text(&m, "messages"), "sys\nhi");
        // Responses: instructions + first user input item.
        let r = json!({
            "instructions": "sys",
            "input": [
                {"role": "user", "content": [{"type": "text", "text": "hello"}]},
                {"role": "assistant", "content": "hi"}
            ]
        });
        assert_eq!(opener_text(&r, "responses"), "sys\nhello");
        // Non-conversation endpoints have no opener.
        assert_eq!(opener_text(&json!({}), "models"), "");
    }

    #[test]
    fn fingerprint_differs_by_opener_and_endpoint() {
        let a = fingerprint("opener-a", "chat");
        let b = fingerprint("opener-b", "chat");
        let c = fingerprint("opener-a", "messages");
        assert_ne!(a, b);
        assert_ne!(a, c);
        // Deterministic across calls.
        assert_eq!(a, fingerprint("opener-a", "chat"));
    }

    #[test]
    fn resolve_session_reuses_within_window() {
        let db = Database::open_memory().unwrap();
        let fp = fingerprint("opener-x", "chat");
        let s1 = resolve_session(&db, &fp, "opener-x").unwrap();
        let s2 = resolve_session(&db, &fp, "opener-x").unwrap();
        assert_eq!(s1, s2, "same fingerprint within the window reuses the id");
        let other = resolve_session(&db, &fingerprint("opener-y", "chat"), "opener-y").unwrap();
        assert_ne!(s1, other);
        // The preview is stored truncated.
        let row = db.get_session(&s1).unwrap().unwrap();
        assert_eq!(row.first_prompt.as_deref(), Some("opener-x"));
    }

    #[test]
    fn first_prompt_preview_truncated() {
        let db = Database::open_memory().unwrap();
        let long = "x".repeat(1000);
        let fp = fingerprint(&long, "chat");
        let sid = resolve_session(&db, &fp, &long).unwrap();
        let row = db.get_session(&sid).unwrap().unwrap();
        assert_eq!(
            row.first_prompt.unwrap().chars().count(),
            FIRST_PROMPT_PREVIEW_CHARS
        );
    }

    #[test]
    fn record_outcome_accumulates_and_backfills_handover() {
        let db = Database::open_memory().unwrap();
        let fp = fingerprint("opener-z", "chat");
        let sid = resolve_session(&db, &fp, "opener-z").unwrap();
        // A mid-session switch recorded by `use`/`proxy use`.
        record_handover(&db, "claude", Some("kimi"), Some("glm")).unwrap();
        record_outcome(
            &db,
            Some(&sid),
            "claude",
            "glm",
            Some("glm-5-2"),
            100,
            40,
            true,
        );
        record_outcome(&db, Some(&sid), "claude", "glm", None, 10, 5, false);
        let row = db.get_session(&sid).unwrap().unwrap();
        assert_eq!(row.prompt_tokens, 110);
        assert_eq!(row.completion_tokens, 45);
        assert_eq!(row.status, "error");
        assert_eq!(row.model.as_deref(), Some("glm-5-2"));
        // The handover got attributed to this session.
        let hs = db.list_handovers(&sid).unwrap();
        assert_eq!(hs.len(), 1);
        assert_eq!(hs[0].to_model.as_deref(), Some("glm"));
    }

    #[test]
    fn record_outcome_none_session_is_noop() {
        let db = Database::open_memory().unwrap();
        record_outcome(&db, None, "claude", "kimi", None, 1, 1, true);
        assert!(db.list_sessions(10).unwrap().is_empty());
    }
}
