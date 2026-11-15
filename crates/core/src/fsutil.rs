//! Filesystem helpers for files that may contain credentials.

use std::io::Write;
use std::path::Path;

/// Atomically write `contents` to `path` with owner-only permissions (0600 on
/// Unix). The temp file is *created* with the restrictive mode, so the secret
/// is never visible with wider permissions, then renamed over the target.
/// Parent directories are created as needed.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp-xfade");
    // A stale temp file from a crashed run may have looser permissions.
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    let res = f.write_all(contents).and_then(|_| f.flush());
    drop(f);
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Copy `src` to `dest` with owner-only permissions (0600 on Unix).
pub fn copy_private(src: &Path, dest: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(src)?;
    write_private(dest, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_private_creates_dirs_and_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b/secret.json");
        write_private(&p, b"1").unwrap();
        write_private(&p, b"2").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"2");
        assert!(!d.path().join("a/b/secret.tmp-xfade").exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_private_is_owner_only_even_over_wide_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("settings.json");
        std::fs::write(&p, "old").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&p, b"new").unwrap();
        assert_eq!(mode(&p), 0o600);
        // Stale wide-open temp file must not leak its mode either.
        let tmp = d.path().join("settings.tmp-xfade");
        std::fs::write(&tmp, "stale").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o666)).unwrap();
        write_private(&p, b"newer").unwrap();
        assert_eq!(mode(&p), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn copy_private_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::write(&src, "k").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).unwrap();
        let dest = d.path().join("dest");
        copy_private(&src, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"k");
        assert_eq!(mode(&dest), 0o600);
    }
}
