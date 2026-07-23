use super::ToolAdapter;
use crate::error::Result;
use crate::models::{Provider, ToolKind};
use std::path::{Path, PathBuf};

pub struct CodexAdapter {
    home: PathBuf,
}

impl CodexAdapter {
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }
}

impl ToolAdapter for CodexAdapter {
    fn tool(&self) -> ToolKind {
        ToolKind::Codex
    }
    fn config_paths(&self) -> Vec<PathBuf> {
        vec![]
    }
    fn apply(&self, _provider: &Provider, _api_key: Option<&str>) -> Result<()> {
        Ok(())
    }
    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>> {
        Ok(None)
    }
}
