//! Static fingerprint of a project root: language markers from manifest
//! files, read as plain strings (no parsing, no tree-sitter — <10ms).

/// Language detected from a manifest file present at the project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Go,
    JavaScript,
    Python,
}

impl Lang {
    pub fn as_str(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Go => "go",
            Lang::JavaScript => "javascript",
            Lang::Python => "python",
        }
    }
}

/// Scan a project root for language manifests.
///
/// Precedence: rust > go > python > javascript (Cargo.toml is the most
/// unambiguous marker; package.json alone can mean a docs site).
pub fn detect_lang(root: &std::path::Path) -> Option<Lang> {
    let has = |name: &str| root.join(name).is_file();
    if has("Cargo.toml") {
        Some(Lang::Rust)
    } else if has("go.mod") {
        Some(Lang::Go)
    } else if has("pyproject.toml") {
        Some(Lang::Python)
    } else if has("package.json") {
        Some(Lang::JavaScript)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &std::path::Path, name: &str) {
        std::fs::write(root.join(name), "").unwrap();
    }

    #[test]
    fn detects_rust_first() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "Cargo.toml");
        touch(dir.path(), "package.json"); // mixed repo: rust wins
        assert_eq!(detect_lang(dir.path()), Some(Lang::Rust));
    }

    #[test]
    fn detects_go_python_javascript() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "go.mod");
        assert_eq!(detect_lang(d.path()), Some(Lang::Go));

        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "pyproject.toml");
        assert_eq!(detect_lang(d.path()), Some(Lang::Python));

        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "package.json");
        assert_eq!(detect_lang(d.path()), Some(Lang::JavaScript));
    }

    #[test]
    fn empty_root_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(detect_lang(d.path()), None);
    }
}
