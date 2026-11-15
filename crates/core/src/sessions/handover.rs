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
}
