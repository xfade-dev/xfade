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
    #[cfg(windows)]
    {
        // On Windows, set owner-only DACL before renaming over target.
        let _ = set_private_dacl(&tmp);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Ensure `path` has private, owner-only permissions.
/// - On Unix: sets file mode to `0o600`.
/// - On Windows: sets DACL to Owner + SYSTEM full control, blocking inherited ACEs.
pub fn set_private_dacl(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(windows)]
    {
        win_security::set_owner_only_dacl(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(windows)]
mod win_security {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    type BOOL = i32;
    type DWORD = u32;
    type LPCWSTR = *const u16;
    type HLOCAL = *mut std::ffi::c_void;
    type PSECURITY_DESCRIPTOR = *mut std::ffi::c_void;

    const SDDL_REVISION_1: DWORD = 1;
    const DACL_SECURITY_INFORMATION: DWORD = 0x00000004;
    const PROTECTED_DACL_SECURITY_INFORMATION: DWORD = 0x80000000;

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            StringSecurityDescriptor: LPCWSTR,
            StringSDRevision: DWORD,
            SecurityDescriptor: *mut PSECURITY_DESCRIPTOR,
            SecurityDescriptorSize: *mut DWORD,
        ) -> BOOL;

        fn SetFileSecurityW(
            lpFileName: LPCWSTR,
            SecurityInformation: DWORD,
            pSecurityDescriptor: PSECURITY_DESCRIPTOR,
        ) -> BOOL;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(hMem: HLOCAL) -> HLOCAL;
    }

    pub fn set_owner_only_dacl(path: &Path) -> std::io::Result<()> {
        let mut wide_path: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide_path.push(0);

        // SDDL:
        // D:P(A;;GA;;;OW)(A;;GA;;;SY)
        // D: DACL
        // P: SE_DACL_PROTECTED (disables inheritance)
        // (A;;GA;;;OW): Allow Generic All to Owner
        // (A;;GA;;;SY): Allow Generic All to Local System
        let sddl: Vec<u16> = "D:P(A;;GA;;;OW)(A;;GA;;;SY)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut p_sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut p_sd,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(std::io::Error::last_os_error());
            }

            let ret = SetFileSecurityW(
                wide_path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                p_sd,
            );
            LocalFree(p_sd);

            if ret == 0 {
                let err = std::io::Error::last_os_error();
                // If running on a filesystem without ACL support (e.g., FAT32/exFAT),
                // Windows returns ERROR_NOT_SUPPORTED (50) or ERROR_INVALID_FUNCTION (1).
                if let Some(code) = err.raw_os_error() {
                    if code == 1 || code == 50 {
                        return Ok(());
                    }
                }
                return Err(err);
            }
        }
        Ok(())
    }
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
