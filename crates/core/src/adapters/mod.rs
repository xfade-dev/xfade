pub mod claude_code;
pub mod codex;
pub mod opencode;

use crate::error::Result;
use crate::models::{Provider, ToolKind};
use std::path::{Path, PathBuf};

pub trait ToolAdapter: Send + Sync {
    fn tool(&self) -> ToolKind;
    /// 切换时会被修改的配置文件（service 层据此备份）
    fn config_paths(&self) -> Vec<PathBuf>;
    /// 写入 provider 到工具配置。
    /// `provider.base_url` 为 None 时表示切回官方（清除/恢复快照），api_key 随之忽略。
    fn apply(&self, provider: &Provider, api_key: Option<&str>) -> Result<()>;
    /// 读取工具当前生效的配置（首次导入用）；无第三方配置时返回 Ok(None)。
    /// 返回 (provider, api_key)；api_key 可能为 None（读不到密钥时）。
    fn read_current(&self) -> Result<Option<(Provider, Option<String>)>>;
}

pub fn adapter_for(tool: ToolKind, home: &Path) -> Box<dyn ToolAdapter> {
    match tool {
        ToolKind::ClaudeCode => Box::new(claude_code::ClaudeCodeAdapter::new(home)),
        ToolKind::Codex => Box::new(codex::CodexAdapter::new(home)),
        ToolKind::OpenCode => Box::new(opencode::OpenCodeAdapter::new(home)),
    }
}

/// 临时文件 + rename 原子写入；自动创建父目录
pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp-asw");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c.json");
        atomic_write(&target, b"{}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"{}");
    }

    #[test]
    fn atomic_write_overwrites_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("c.json");
        atomic_write(&target, b"1").unwrap();
        atomic_write(&target, b"2").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"2");
        assert!(!dir.path().join("c.tmp-asw").exists());
    }
}
