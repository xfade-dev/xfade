//! Project Analyzer: static repo fingerprint + git pulse.
//!
//! Phase 2 scope is deliberately scanner-free: the fingerprint reads
//! manifest files as plain strings (<10ms, no tree-sitter), and git pulse
//! shells out to `git` for branch/head/recent-commit info. The result is a
//! cacheable `RepoMap` used as an enrichment signal (future: Handover
//! context, LlmClassifier features) — phase-one Smart Fader classification
//! does not depend on it.

pub mod fingerprint;
pub mod git_pulse;

use crate::store::db::Database;
use serde_json::json;
use std::path::Path;

/// Analyzed project snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoMap {
    pub root: String,
    pub lang: Option<&'static str>,
    pub git: Option<git_pulse::GitPulse>,
}

impl RepoMap {
    pub fn to_json(&self) -> String {
        serde_json::to_string(&json!({
            "root": self.root,
            "lang": self.lang,
            "git": self.git.as_ref().map(|g| json!({
                "branch": g.branch,
                "head": g.head,
                "recent": g.recent_subjects,
            })),
        }))
        .unwrap_or_default()
    }
}

/// Cheap cache key: manifests' (name, mtime, size) digest. Any manifest
/// edit changes it; content hashing is unnecessary for a cache key.
fn cache_fingerprint(root: &Path) -> String {
    use std::fmt::Write as _;
    let mut fp = String::new();
    for name in ["Cargo.toml", "go.mod", "pyproject.toml", "package.json"] {
        let p = root.join(name);
        if let Ok(md) = std::fs::metadata(&p) {
            let _ = write!(
                fp,
                "{name}:{}:{};",
                md.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
                md.len()
            );
        }
    }
    fp
}

/// Analyze `root`, using `db` as the repo_map cache: when the manifest
/// fingerprint is unchanged since the last analysis, the cached map is
/// returned without shelling out to git.
pub fn analyze(root: &Path, db: &Database) -> RepoMap {
    let root_str = root.display().to_string();
    let fp = cache_fingerprint(root);
    if let Ok(Some((cached_fp, map_json))) = db.get_repo_map(&root_str) {
        if cached_fp == fp {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&map_json) {
                let git = v.get("git").map(|g| {
                    let recent = g
                        .get("recent")
                        .and_then(|r| r.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    git_pulse::GitPulse {
                        branch: g.get("branch").and_then(|b| b.as_str()).unwrap_or("").into(),
                        head: g.get("head").and_then(|h| h.as_str()).unwrap_or("").into(),
                        recent_subjects: recent,
                    }
                });
                // lang is recomputed, not round-tripped: it derives from
                // the same manifests the fingerprint digests, so the cache
                // hit already proves it unchanged.
                let lang = fingerprint::detect_lang(root).map(|l| l.as_str());
                return RepoMap {
                    root: root_str,
                    lang,
                    git,
                };
            }
        }
    }
    let lang = fingerprint::detect_lang(root).map(|l| l.as_str());
    let map = RepoMap {
        root: root_str,
        lang,
        git: git_pulse::git_pulse(root),
    };
    let _ = db.save_repo_map(&map.root, &fp, &map.to_json());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(root: &Path) {
        std::process::Command::new("git")
            .args(["-C", root.to_str().unwrap(), "init", "-q"])
            .output()
            .unwrap();
    }

    #[test]
    fn analyze_reports_lang_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        init_repo(dir.path());

        let db = Database::open_memory().unwrap();
        let m1 = analyze(dir.path(), &db);
        assert_eq!(m1.lang, Some("rust"));
        assert!(m1.git.is_some());

        // Cached read returns the same map (root + lang survive the
        // JSON round-trip).
        let m2 = analyze(dir.path(), &db);
        assert_eq!(m2.lang, Some("rust"));
        assert_eq!(m2.root, m1.root);
        assert!(m2.git.is_some());
    }

    #[test]
    fn analyze_without_manifests_or_git_still_maps() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open_memory().unwrap();
        let m = analyze(dir.path(), &db);
        assert_eq!(m.lang, None);
        assert!(m.git.is_none());
    }
}
