//! Git pulse: branch/head/recent-commit activity via `git` shell-outs.
//!
//! Best-effort: a non-git root yields `None`; git failures degrade to
//! partial data rather than errors.

use std::path::Path;
use std::process::Command;

/// Current git state of a project root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPulse {
    pub branch: String,
    pub head: String,
    /// Subject lines of the most recent commits (newest first).
    pub recent_subjects: Vec<String>,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", root.to_str()?, "--no-optional-locks"])
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Read the git pulse of `root`; `None` when the root is not a git repo.
pub fn git_pulse(root: &Path) -> Option<GitPulse> {
    // rev-parse inside a repo always succeeds; outside it fails.
    git(root, &["rev-parse", "--git-dir"])?;
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "HEAD".into());
    let head = git(root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let recent_subjects = git(root, &["log", "-n", "5", "--pretty=%s"])
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default();
    Some(GitPulse {
        branch,
        head,
        recent_subjects,
    })
}

/// Uncommitted working tree files (staged, modified, untracked) via `git status --porcelain`.
pub fn git_working_files(root: &Path) -> Vec<String> {
    git(root, &["status", "--porcelain"])
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Diff stat of uncommitted working tree changes.
pub fn git_diff_stat(root: &Path, max_chars: usize) -> Option<String> {
    let diff = git(root, &["diff", "HEAD", "--stat"]).or_else(|| git(root, &["diff", "--stat"]))?;
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= max_chars {
        Some(trimmed.to_string())
    } else {
        let truncated: String = trimmed.chars().take(max_chars).collect();
        Some(format!("{truncated}\n... (truncated)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(root: &Path) {
        Command::new("git")
            .args(["-C", root.to_str().unwrap(), "init", "-q"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", root.to_str().unwrap(), "config", "user.email", "t@t"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", root.to_str().unwrap(), "config", "user.name", "t"])
            .output()
            .unwrap();
    }

    fn commit(root: &Path, msg: &str) {
        let f = root.join(format!("f{}", msg.len()));
        std::fs::write(&f, msg).unwrap();
        Command::new("git")
            .args(["-C", root.to_str().unwrap(), "add", "-A"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", root.to_str().unwrap(), "commit", "-q", "-m", msg])
            .output()
            .unwrap();
    }

    #[test]
    fn non_git_root_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(git_pulse(d.path()).is_none());
    }

    #[test]
    fn git_repo_yields_branch_head_and_subjects() {
        let d = tempfile::tempdir().unwrap();
        init_repo(d.path());
        commit(d.path(), "initial commit");
        commit(d.path(), "second commit");

        let pulse = git_pulse(d.path()).expect("repo should pulse");
        assert!(!pulse.head.is_empty());
        assert!(!pulse.recent_subjects.is_empty());
        assert_eq!(pulse.recent_subjects[0], "second commit");
        assert!(pulse
            .recent_subjects
            .contains(&"initial commit".to_string()));
    }

    #[test]
    fn git_working_files_and_diff_stat_test() {
        let d = tempfile::tempdir().unwrap();
        init_repo(d.path());
        commit(d.path(), "initial commit");

        let test_file = d.path().join("modified.txt");
        std::fs::write(&test_file, "line 1\nline 2\n").unwrap();
        Command::new("git")
            .args(["-C", d.path().to_str().unwrap(), "add", "modified.txt"])
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                d.path().to_str().unwrap(),
                "commit",
                "-q",
                "-m",
                "add modified.txt",
            ])
            .output()
            .unwrap();

        std::fs::write(&test_file, "line 1\nline 2\nline 3\n").unwrap();
        let untracked = d.path().join("untracked.txt");
        std::fs::write(&untracked, "hello").unwrap();

        let files = git_working_files(d.path());
        assert_eq!(files.len(), 2);

        let stat = git_diff_stat(d.path(), 500);
        assert!(stat.is_some());
        assert!(stat.unwrap().contains("modified.txt"));
    }
}
