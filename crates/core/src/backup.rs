use crate::error::{CoreError, Result};
use std::fs;
use std::path::{Path, PathBuf};

const KEEP: usize = 10;

fn is_backup_file(p: &Path) -> bool {
    p.is_file()
        && p.file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit_once('.'))
            .is_some_and(|(_, suffix)| {
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
            })
}

/// Back up `path` to `backup_dir/<filename>.<nanos>`; returns Ok(None) if the source is absent.
pub fn backup_file(path: &Path, backup_dir: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    fs::create_dir_all(backup_dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = path.file_name().ok_or_else(|| CoreError::ConfigParse {
        path: path.display().to_string(),
        msg: "path has no file name".into(),
    })?;
    let mut dest_name = name.to_os_string();
    dest_name.push(format!(".{}", nanos));
    let dest = backup_dir.join(dest_name);
    fs::copy(path, &dest)?;
    rotate(backup_dir, KEEP)?;
    Ok(Some(dest))
}

/// Sort by filename (timestamp suffix) and delete the oldest until `keep` remain.
pub fn rotate(dir: &Path, keep: usize) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
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

/// List backups, newest → oldest.
pub fn list_backups(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
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
