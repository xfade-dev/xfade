//! Session handover engine (V2): capture and synthesize project and
//! conversation state so developers can switch models or tools without
//! dropping context.

use crate::analyzer::git_pulse::{git_diff_stat, git_pulse, git_working_files, GitPulse};
use crate::store::db::Database;
use std::path::Path;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const DEFAULT_MAX_DIFF_CHARS: usize = 1500;

/// Structured context captured from a coding session and working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverBrief {
    pub session_id: Option<String>,
    pub tool: Option<String>,
    pub model: Option<String>,
    pub goal: Option<String>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub recent_commits: Vec<String>,
    pub changed_files: Vec<String>,
    pub diff_stat: Option<String>,
    pub created_at: String,
}

impl HandoverBrief {
    /// Render this brief as a compact Markdown prompt block suitable for
    /// feeding into an AI coding assistant.
    pub fn to_markdown(&self) -> String {
        let mut lines = Vec::new();
        lines.push("### [xfade Session Handover Context]".to_string());

        let tool_str = self.tool.as_deref().unwrap_or("unknown");
        let model_str = self.model.as_deref().unwrap_or("unknown");
        if let Some(sid) = &self.session_id {
            lines.push(format!(
                "- **Previous Session**: `{sid}` ({tool_str} / {model_str})"
            ));
        } else if self.tool.is_some() || self.model.is_some() {
            lines.push(format!("- **Previous Context**: {tool_str} / {model_str}"));
        }

        if let Some(goal) = &self.goal {
            let g = goal.trim();
            if !g.is_empty() {
                lines.push(format!("- **Active Goal**: {g}"));
            }
        }

        if let (Some(b), Some(h)) = (&self.branch, &self.head) {
            lines.push(format!("- **Git State**: branch `{b}` @ `{h}`"));
        }

        if !self.recent_commits.is_empty() {
            lines.push("- **Recent Commits**:".to_string());
            for c in self.recent_commits.iter().take(3) {
                lines.push(format!("  - {c}"));
            }
        }

        if !self.changed_files.is_empty() {
            lines.push("- **Working Tree Changes**:".to_string());
            for f in self.changed_files.iter().take(15) {
                lines.push(format!("  - {f}"));
            }
            if self.changed_files.len() > 15 {
                lines.push(format!(
                    "  - ... and {} more files",
                    self.changed_files.len() - 15
                ));
            }
        }

        if let Some(diff) = &self.diff_stat {
            lines.push("- **Diff Summary**:".to_string());
            lines.push("  ```text".to_string());
            for d in diff.lines() {
                lines.push(format!("  {d}"));
            }
            lines.push("  ```".to_string());
        }

        lines.push(
            "> Instructions: This context was handed over from the developer's previous session. Continue working seamlessly from this state.".to_string(),
        );

        lines.join("\n")
    }

    /// Format brief as a compact XML tagged context block for prompt/system injection.
    pub fn to_injection_xml(&self, from_model: Option<&str>, to_model: Option<&str>) -> String {
        let mut lines = Vec::new();
        lines.push("<xfade_handover_context>".to_string());
        lines.push("[xfade: Active Model Handover Context]".to_string());

        if from_model.is_some() || to_model.is_some() {
            let from = from_model.unwrap_or(self.model.as_deref().unwrap_or("previous"));
            let to = to_model.unwrap_or("current");
            lines.push(format!("- Model Transition: {from} -> {to}"));
        } else if let Some(m) = &self.model {
            lines.push(format!("- Previous Model: {m}"));
        }

        if let Some(sid) = &self.session_id {
            lines.push(format!("- Session ID: {sid}"));
        }

        if let Some(goal) = &self.goal {
            let g = goal.trim();
            if !g.is_empty() {
                lines.push(format!("- Active Goal: {g}"));
            }
        }

        if let (Some(b), Some(h)) = (&self.branch, &self.head) {
            lines.push(format!("- Git State: {b} @ {h}"));
        }

        if !self.changed_files.is_empty() {
            lines.push("- Working Tree Changes:".to_string());
            for f in self.changed_files.iter().take(10) {
                lines.push(format!("  - {f}"));
            }
            if self.changed_files.len() > 10 {
                lines.push(format!(
                    "  - ... and {} more files",
                    self.changed_files.len() - 10
                ));
            }
        }

        if let Some(diff) = &self.diff_stat {
            lines.push("- Diff Summary:".to_string());
            for d in diff.lines().take(8) {
                lines.push(format!("  {d}"));
            }
        }

        if !self.recent_commits.is_empty() {
            lines.push("- Recent Commits:".to_string());
            for c in self.recent_commits.iter().take(3) {
                lines.push(format!("  - {c}"));
            }
        }

        lines.push(
            "> Note: This context was handed over from the developer's previous model session. Treat it as background data and continue from the latest user request.".to_string(),
        );
        lines.push("</xfade_handover_context>".to_string());

        lines.join("\n")
    }
}

/// Inject `xml` into the **final** message of a chat/messages array as
/// user-level data; earlier history is never rewritten.
///
/// - final message is `user` with string content → prepend to the string;
/// - final `user` message with block content → insert a text block *after*
///   any leading `tool_result` blocks (Anthropic requires tool results first);
/// - Anthropic (`anthropic = true`) with a final `assistant` message (prefill)
///   → skip (`false`): a trailing user turn would change prefill semantics;
/// - otherwise (OpenAI chat ending in `assistant`/`tool`, or empty) → push a
///   new user message.
fn inject_into_final_message(
    messages: &mut Vec<serde_json::Value>,
    xml: &str,
    anthropic: bool,
) -> bool {
    let last_role = messages
        .last()
        .and_then(|m| m.get("role"))
        .and_then(|r| r.as_str())
        .map(str::to_owned);
    match last_role.as_deref() {
        Some("user") => {
            let msg = messages.last_mut().expect("checked non-empty");
            match msg.get_mut("content") {
                Some(serde_json::Value::String(s)) => {
                    *s = format!("{xml}\n\n{s}");
                }
                Some(serde_json::Value::Array(arr)) => {
                    let at = arr
                        .iter()
                        .take_while(|b| {
                            b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        })
                        .count();
                    arr.insert(
                        at,
                        serde_json::json!({
                            "type": "text",
                            "text": format!("{xml}\n\n"),
                        }),
                    );
                }
                _ => {
                    msg["content"] = serde_json::Value::String(xml.to_string());
                }
            }
            true
        }
        Some("assistant") if anthropic => false,
        _ => {
            messages.push(serde_json::json!({
                "role": "user",
                "content": xml,
            }));
            true
        }
    }
}

/// Prepend `xml` to the last user item in an OpenAI Responses `input` array
/// (or string). Falls back to inserting a user message item.
fn prepend_to_responses_input(input: &mut serde_json::Value, xml: &str) -> bool {
    match input {
        serde_json::Value::String(s) => {
            *s = format!("{xml}\n\n{s}");
            true
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut().rev() {
                if item.get("role").and_then(|r| r.as_str()) != Some("user") {
                    continue;
                }
                match item.get_mut("content") {
                    Some(serde_json::Value::String(s)) => {
                        *s = format!("{xml}\n\n{s}");
                        return true;
                    }
                    Some(serde_json::Value::Array(arr)) => {
                        arr.insert(
                            0,
                            serde_json::json!({
                                "type": "input_text",
                                "text": format!("{xml}\n\n"),
                            }),
                        );
                        return true;
                    }
                    _ => {
                        item["content"] = serde_json::Value::String(xml.to_string());
                        return true;
                    }
                }
            }
            items.push(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": xml}],
            }));
            true
        }
        _ => false,
    }
}

/// Inject the handover XML into a JSON request payload as **user-level data**
/// (never elevated into `system` / `developer` / `instructions`).
///
/// - `messages` (Anthropic): final user message (after any `tool_result`
///   blocks); skipped when the request ends with an assistant prefill.
/// - `chat` (OpenAI): final user message, or a new trailing user message.
/// - `responses` (OpenAI Responses): prepends to `input` user content.
///
/// Returns true if injection succeeded and modified the payload.
pub fn inject_handover_into_payload(
    payload: &mut serde_json::Value,
    endpoint: &str,
    xml: &str,
) -> bool {
    let obj = match payload.as_object_mut() {
        Some(o) => o,
        None => return false,
    };

    match endpoint {
        "messages" | "chat" => {
            let messages = match obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
                Some(arr) => arr,
                None => return false,
            };
            inject_into_final_message(messages, xml, endpoint == "messages")
        }
        "responses" => match obj.get_mut("input") {
            Some(input) => prepend_to_responses_input(input, xml),
            None => {
                obj.insert(
                    "input".to_string(),
                    serde_json::Value::String(xml.to_string()),
                );
                true
            }
        },
        _ => false,
    }
}

/// Resolve the project root used when collecting git/workdir context for L3.
/// Preference order: explicit `header_project_root` — honored only when it is
/// an existing directory that xfade already analyzed (a `repo_maps` entry), so
/// a client cannot point the gateway at arbitrary paths — → latest cached
/// `repo_maps` root → process cwd (last resort).
pub fn resolve_project_root(
    db: &Database,
    header_project_root: Option<&str>,
) -> std::path::PathBuf {
    if let Some(raw) = header_project_root {
        let p = std::path::PathBuf::from(raw);
        if p.is_dir() && matches!(db.get_repo_map(raw), Ok(Some(_))) {
            return p;
        }
    }
    if let Ok(Some((root, _))) = db.latest_repo_map() {
        let p = std::path::PathBuf::from(&root);
        if p.is_dir() {
            return p;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

/// Collect handover context from the database and working tree.
/// Uses the globally newest session when `session_id` is `None` (L1/L2 CLI).
pub fn collect_handover(db: &Database, root: &Path, max_diff_chars: usize) -> HandoverBrief {
    collect_handover_for(db, root, max_diff_chars, None)
}

/// Like [`collect_handover`], but prefers the given `session_id` when present.
pub fn collect_handover_for(
    db: &Database,
    root: &Path,
    max_diff_chars: usize,
    session_id: Option<&str>,
) -> HandoverBrief {
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());

    // An explicit session id (L3) is authoritative: never substitute another
    // session's goal. Only the id-less L1/L2 path uses the newest row.
    let recent_session = match session_id {
        Some(sid) => db.get_session(sid).ok().flatten(),
        None => db.list_sessions(1).ok().and_then(|mut list| list.pop()),
    };
    let session_id = recent_session.as_ref().map(|s| s.id.clone());
    let tool = recent_session
        .as_ref()
        .map(|s| s.tool.clone())
        .filter(|t| !t.is_empty());
    let model = recent_session.as_ref().and_then(|s| s.model.clone());
    let goal = recent_session.as_ref().and_then(|s| s.first_prompt.clone());

    // 2. Query git pulse
    let pulse: Option<GitPulse> = git_pulse(root);
    let branch = pulse.as_ref().map(|p| p.branch.clone());
    let head = pulse.as_ref().map(|p| p.head.clone());
    let recent_commits = pulse.map(|p| p.recent_subjects).unwrap_or_default();

    // 3. Query working tree status and diff
    let changed_files = git_working_files(root);
    let diff_stat = git_diff_stat(root, max_diff_chars);

    HandoverBrief {
        session_id,
        tool,
        model,
        goal,
        branch,
        head,
        recent_commits,
        changed_files,
        diff_stat,
        created_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_markdown_renders_expected_blocks() {
        let brief = HandoverBrief {
            session_id: Some("s123-abc".into()),
            tool: Some("claude".into()),
            model: Some("claude-3-7-sonnet".into()),
            goal: Some("Refactor proxy forward logic".into()),
            branch: Some("main".into()),
            head: Some("7ea4a23".into()),
            recent_commits: vec!["fix: something".into(), "chore: sync".into()],
            changed_files: vec![" M src/main.rs".into(), "?? new.rs".into()],
            diff_stat: Some("src/main.rs | 2 +-\n 1 file changed, 1 insertion(+)".into()),
            created_at: "2026-11-15T12:00:00Z".into(),
        };

        let md = brief.to_markdown();
        assert!(md.contains("### [xfade Session Handover Context]"));
        assert!(md.contains("`s123-abc` (claude / claude-3-7-sonnet)"));
        assert!(md.contains("**Active Goal**: Refactor proxy forward logic"));
        assert!(md.contains("branch `main` @ `7ea4a23`"));
        assert!(md.contains("  - fix: something"));
        assert!(md.contains("  -  M src/main.rs"));
        assert!(md.contains("src/main.rs | 2 +-"));
        assert!(md.contains("Instructions: This context was handed over"));
    }

    #[test]
    fn to_markdown_sparse_renders_cleanly() {
        let brief = HandoverBrief {
            session_id: None,
            tool: None,
            model: None,
            goal: None,
            branch: None,
            head: None,
            recent_commits: Vec::new(),
            changed_files: Vec::new(),
            diff_stat: None,
            created_at: "2026-11-15T12:00:00Z".into(),
        };

        let md = brief.to_markdown();
        assert!(md.contains("### [xfade Session Handover Context]"));
        assert!(!md.contains("- **Previous Session**"));
        assert!(!md.contains("- **Active Goal**"));
        assert!(!md.contains("- **Working Tree Changes**"));
        assert!(md.contains("Instructions: This context was handed over"));
    }

    #[test]
    fn collect_handover_smoke_with_db() {
        let d = tempfile::tempdir().unwrap();
        let db_path = d.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Insert a test session
        db.insert_session(
            "s-test-1",
            "fp-1",
            "2026-11-15T10:00:00Z",
            Some("Solve quantum teleportation bug"),
        )
        .unwrap();
        db.update_session_progress(
            "s-test-1",
            "claude",
            "anthropic",
            Some("claude-3-7-sonnet"),
            100,
            200,
            "ok",
            "2026-11-15T10:05:00Z",
        )
        .unwrap();

        let brief = collect_handover(&db, d.path(), 500);
        assert_eq!(brief.session_id.as_deref(), Some("s-test-1"));
        assert_eq!(brief.tool.as_deref(), Some("claude"));
        assert_eq!(brief.model.as_deref(), Some("claude-3-7-sonnet"));
        assert_eq!(
            brief.goal.as_deref(),
            Some("Solve quantum teleportation bug")
        );
    }

    #[test]
    fn collect_handover_for_prefers_explicit_session() {
        let d = tempfile::tempdir().unwrap();
        let db_path = d.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        db.insert_session("s-old", "fp-old", "2026-11-15T09:00:00Z", Some("old goal"))
            .unwrap();
        db.update_session_progress(
            "s-old",
            "claude",
            "p1",
            Some("opus"),
            1,
            1,
            "ok",
            "2026-11-15T09:01:00Z",
        )
        .unwrap();
        db.insert_session(
            "s-new",
            "fp-new",
            "2026-11-15T10:00:00Z",
            Some("newest goal"),
        )
        .unwrap();
        db.update_session_progress(
            "s-new",
            "codex",
            "p2",
            Some("gpt"),
            1,
            1,
            "ok",
            "2026-11-15T10:01:00Z",
        )
        .unwrap();

        // Without id → newest
        let newest = collect_handover(&db, d.path(), 500);
        assert_eq!(newest.session_id.as_deref(), Some("s-new"));
        assert_eq!(newest.goal.as_deref(), Some("newest goal"));

        // With id → that session, even if older
        let older = collect_handover_for(&db, d.path(), 500, Some("s-old"));
        assert_eq!(older.session_id.as_deref(), Some("s-old"));
        assert_eq!(older.goal.as_deref(), Some("old goal"));
        assert_eq!(older.model.as_deref(), Some("opus"));
    }

    #[test]
    fn resolve_project_root_prefers_header_then_repo_map() {
        let d = tempfile::tempdir().unwrap();
        let project = d.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let db = Database::open_memory().unwrap();
        db.save_repo_map(&project.display().to_string(), "fp", r#"{"lang":"rust"}"#)
            .unwrap();

        let from_header = resolve_project_root(&db, Some(project.to_str().unwrap()));
        assert_eq!(from_header, project);

        let from_map = resolve_project_root(&db, Some("/nonexistent/path"));
        assert_eq!(from_map, project);

        let from_map_none = resolve_project_root(&db, None);
        assert_eq!(from_map_none, project);

        // An existing directory that xfade never analyzed must be rejected.
        let other = d.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let rejected = resolve_project_root(&db, Some(other.to_str().unwrap()));
        assert_eq!(rejected, project);
    }

    #[test]
    fn collect_handover_for_unknown_session_does_not_borrow_another() {
        let d = tempfile::tempdir().unwrap();
        let db = Database::open(&d.path().join("t.db")).unwrap();
        db.insert_session("s-other", "fp", "2026-11-15T10:00:00Z", Some("secret goal"))
            .unwrap();
        let brief = collect_handover_for(&db, d.path(), 500, Some("s-missing"));
        assert!(brief.session_id.is_none());
        assert!(brief.goal.is_none());
    }

    #[test]
    fn to_injection_xml_contains_tags_and_transition() {
        let brief = HandoverBrief {
            session_id: Some("s999".into()),
            tool: Some("claude".into()),
            model: Some("claude-opus".into()),
            goal: Some("Optimize database indexes".into()),
            branch: Some("feature/db".into()),
            head: Some("abc1234".into()),
            recent_commits: vec!["feat: add index".into()],
            changed_files: vec!["db/schema.sql".into()],
            diff_stat: Some("db/schema.sql | 10 ++++++++++".into()),
            created_at: "2026-11-15T12:00:00Z".into(),
        };

        let xml = brief.to_injection_xml(Some("claude-opus"), Some("claude-sonnet"));
        assert!(xml.starts_with("<xfade_handover_context>"));
        assert!(xml.ends_with("</xfade_handover_context>"));
        assert!(xml.contains("Model Transition: claude-opus -> claude-sonnet"));
        assert!(xml.contains("Session ID: s999"));
        assert!(xml.contains("Active Goal: Optimize database indexes"));
        assert!(xml.contains("Git State: feature/db @ abc1234"));
        assert!(xml.contains("db/schema.sql"));
    }

    #[test]
    fn inject_handover_into_payload_messages_prepends_last_user() {
        let mut payload = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "system": "You are a helpful assistant.",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "continue"}
            ]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        // system must stay untouched
        assert_eq!(payload["system"], "You are a helpful assistant.");
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(
            msgs[2]["content"].as_str().unwrap(),
            "<xfade_handover_context>context</xfade_handover_context>\n\ncontinue"
        );
    }

    #[test]
    fn inject_handover_into_payload_messages_array_content() {
        let mut payload = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "system": [{"type": "text", "text": "Base instructions"}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        // system array untouched
        assert_eq!(payload["system"].as_array().unwrap().len(), 1);
        let content = payload["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0]["text"].as_str().unwrap().contains(xml));
        assert_eq!(content[1]["text"], "hi");
    }

    #[test]
    fn inject_handover_into_payload_chat_keeps_system_and_prepends_user() {
        let mut payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "system", "content": "System prompt."},
                {"role": "user", "content": "hello"}
            ]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "chat", xml));
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "System prompt.");
        assert!(msgs[1]["content"]
            .as_str()
            .unwrap()
            .starts_with("<xfade_handover_context>"));
        assert!(msgs[1]["content"].as_str().unwrap().ends_with("hello"));
    }

    #[test]
    fn inject_handover_anthropic_tool_result_stays_first() {
        let mut payload = serde_json::json!({
            "model": "claude",
            "messages": [
                {"role": "user", "content": "run it"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "bash", "input": {}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]}
            ]
        });
        let xml = "<xfade_handover_context>c</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        let blocks = payload["messages"][2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[1]["type"], "text");
        // earlier history untouched
        assert_eq!(payload["messages"][0]["content"], "run it");
    }

    #[test]
    fn inject_handover_anthropic_assistant_prefill_is_skipped() {
        let mut payload = serde_json::json!({
            "model": "claude",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "{"}
            ]
        });
        let before = payload.clone();
        assert!(!inject_handover_into_payload(&mut payload, "messages", "x"));
        assert_eq!(payload, before);
    }

    #[test]
    fn inject_handover_chat_ending_in_tool_appends_user_not_history_rewrite() {
        let mut payload = serde_json::json!({
            "model": "gpt",
            "messages": [
                {"role": "user", "content": "do it"},
                {"role": "assistant", "content": null, "tool_calls": []},
                {"role": "tool", "tool_call_id": "c1", "content": "done"}
            ]
        });
        assert!(inject_handover_into_payload(&mut payload, "chat", "XML"));
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["content"], "do it");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "XML");
    }

    #[test]
    fn inject_handover_into_payload_chat_pushes_user_when_empty() {
        let mut payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": []
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "chat", xml));
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], xml);
    }

    #[test]
    fn inject_handover_into_payload_responses_input_not_instructions() {
        let mut payload = serde_json::json!({
            "model": "gpt-5.1",
            "instructions": "Be concise.",
            "input": "do the thing"
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "responses", xml));
        assert_eq!(payload["instructions"], "Be concise.");
        assert_eq!(
            payload["input"].as_str().unwrap(),
            "<xfade_handover_context>context</xfade_handover_context>\n\ndo the thing"
        );
    }
}
