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
            "> Instruction: This context was handed over from the developer's previous model session. Continue working seamlessly from this state.".to_string(),
        );
        lines.push("</xfade_handover_context>".to_string());

        lines.join("\n")
    }
}

/// Inject the handover XML into a JSON request payload based on the protocol endpoint.
///
/// - `messages` (Anthropic): Appends to `system` string or array, or sets `system` string.
/// - `chat` (OpenAI): Appends to existing system/developer message or inserts a system message at index 0.
/// - `responses` (OpenAI Responses): Appends to or sets `instructions`.
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
        "messages" => match obj.get_mut("system") {
            Some(serde_json::Value::String(s)) => {
                s.push_str("\n\n");
                s.push_str(xml);
                true
            }
            Some(serde_json::Value::Array(arr)) => {
                arr.push(serde_json::json!({
                    "type": "text",
                    "text": format!("\n\n{xml}"),
                }));
                true
            }
            Some(serde_json::Value::Null) | None => {
                obj.insert(
                    "system".to_string(),
                    serde_json::Value::String(xml.to_string()),
                );
                true
            }
            _ => false,
        },
        "chat" => {
            let messages = match obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
                Some(arr) => arr,
                None => return false,
            };

            // If first message is system or developer, append to it
            if let Some(first) = messages.first_mut() {
                if let Some(role) = first.get("role").and_then(|r| r.as_str()) {
                    if role == "system" || role == "developer" {
                        if let Some(content) = first.get_mut("content").and_then(|c| c.as_str()) {
                            let updated = format!("{content}\n\n{xml}");
                            first["content"] = serde_json::Value::String(updated);
                            return true;
                        }
                    }
                }
            }

            // Otherwise, prepend a system message
            messages.insert(
                0,
                serde_json::json!({
                    "role": "system",
                    "content": xml,
                }),
            );
            true
        }
        "responses" => match obj.get_mut("instructions") {
            Some(serde_json::Value::String(s)) => {
                s.push_str("\n\n");
                s.push_str(xml);
                true
            }
            Some(serde_json::Value::Null) | None => {
                obj.insert(
                    "instructions".to_string(),
                    serde_json::Value::String(xml.to_string()),
                );
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// Collect handover context from the database and working tree.
pub fn collect_handover(db: &Database, root: &Path, max_diff_chars: usize) -> HandoverBrief {
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string());

    // 1. Query most recent session from the DB
    let recent_session = db.list_sessions(1).ok().and_then(|mut list| list.pop());
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
    fn inject_handover_into_payload_messages_string_system() {
        let mut payload = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        let sys = payload["system"].as_str().unwrap();
        assert!(sys.starts_with("You are a helpful assistant.\n\n<xfade_handover_context>"));
    }

    #[test]
    fn inject_handover_into_payload_messages_array_system() {
        let mut payload = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "system": [{"type": "text", "text": "Base instructions"}],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        let arr = payload["system"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "Base instructions");
        assert!(arr[1]["text"].as_str().unwrap().contains(xml));
    }

    #[test]
    fn inject_handover_into_payload_messages_missing_system() {
        let mut payload = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "messages", xml));
        assert_eq!(payload["system"].as_str().unwrap(), xml);
    }

    #[test]
    fn inject_handover_into_payload_chat_existing_system() {
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
        assert!(msgs[0]["content"]
            .as_str()
            .unwrap()
            .contains("System prompt.\n\n<xfade_handover_context>"));
    }

    #[test]
    fn inject_handover_into_payload_chat_inserts_system_at_start() {
        let mut payload = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "chat", xml));
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], xml);
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn inject_handover_into_payload_responses_instructions() {
        let mut payload = serde_json::json!({
            "model": "gpt-5.1",
            "instructions": "Be concise."
        });
        let xml = "<xfade_handover_context>context</xfade_handover_context>";
        assert!(inject_handover_into_payload(&mut payload, "responses", xml));
        assert!(payload["instructions"]
            .as_str()
            .unwrap()
            .contains("Be concise.\n\n<xfade_handover_context>"));
    }
}
