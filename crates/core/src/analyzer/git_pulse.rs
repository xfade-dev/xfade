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
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "HEAD".into());
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
        assert!(pulse.recent_subjects.contains(&"initial commit".to_string()));
    }
}
