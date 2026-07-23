use crate::error::Result;
use std::fs;
use std::path::{Path, PathBuf};

const KEEP: usize = 10;

/// 备份 path 到 backup_dir/<filename>.<millis>；源不存在返回 Ok(None)
pub fn backup_file(path: &Path, backup_dir: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::create_dir_all(backup_dir)?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let name = format!("{}.{}", path.file_name().unwrap().to_string_lossy(), millis);
    let dest = backup_dir.join(name);
    fs::copy(path, &dest)?;
    rotate(backup_dir, KEEP)?;
    Ok(Some(dest))
}

/// 按文件名（时间戳后缀）排序，删除最旧的直到剩 keep 份
pub fn rotate(dir: &Path, keep: usize) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    while entries.len() > keep {
        fs::remove_file(entries.remove(0))?;
    }
    Ok(())
}

pub fn restore(backup: &Path, target: &Path) -> Result<()> {
    fs::copy(backup, target)?;
    Ok(())
}

/// 列出备份，新→旧
pub fn list_backups(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    entries.reverse();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("config.json");
        std::fs::write(&src, b"v1").unwrap();
        let bak_dir = dir.path().join("backups");

        let bak = backup_file(&src, &bak_dir).unwrap().unwrap();
        std::fs::write(&src, b"v2").unwrap();
        restore(&bak, &src).unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"v1");
    }

    #[test]
    fn backup_missing_source_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let r = backup_file(&dir.path().join("nope.json"), &dir.path().join("b")).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn rotate_keeps_latest_n() {
        let dir = tempfile::tempdir().unwrap();
        let bak_dir = dir.path().join("b");
        let src = dir.path().join("c.json");
        for i in 0..15 {
            std::fs::write(&src, format!("v{i}")).unwrap();
            backup_file(&src, &bak_dir).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(std::fs::read_dir(&bak_dir).unwrap().count(), 10);
    }
}
