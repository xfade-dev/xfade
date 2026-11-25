use crate::adapters::adapter_for;
use crate::error::{CoreError, Result};
use crate::models::ToolKind;
use crate::service::Core;
use crate::store::config::{SecretsBackend, GLOBAL_API_KEY_REF};
use crate::store::secrets::SecretStore;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustState {
    Guarded,
    Direct,
    Exposed,
}

impl std::fmt::Display for TrustState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Guarded => write!(f, "GUARDED"),
            Self::Direct => write!(f, "DIRECT"),
            Self::Exposed => write!(f, "EXPOSED"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pass => write!(f, "PASS"),
            Self::Warn => write!(f, "WARN"),
            Self::Fail => write!(f, "FAIL"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityCheckItem {
    pub name: String,
    pub status: CheckStatus,
    pub details: String,
    pub fixable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTrustReport {
    pub tool: ToolKind,
    pub trust: TrustState,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityDoctorReport {
    pub overall_trust: TrustState,
    pub tool_trust: Vec<ToolTrustReport>,
    pub checks: Vec<SecurityCheckItem>,
}

pub struct SecurityDoctor<'a> {
    core: &'a Core,
}

impl<'a> SecurityDoctor<'a> {
    pub fn new(core: &'a Core) -> Self {
        Self { core }
    }

    /// Perform a full security audit across tool configurations, permissions, and daemon settings.
    pub fn check(&self) -> Result<SecurityDoctorReport> {
        let mut checks = Vec::new();
        let mut tool_reports = Vec::new();

        // 1. Audit each supported tool
        let mut any_exposed = false;
        let mut any_direct = false;

        for tool in ToolKind::ALL {
            let adapter = adapter_for(tool, self.core.home());
            let paths = adapter.config_paths();
            let mut tool_has_file = false;
            let mut tool_raw_key_found = false;
            let mut tool_is_loopback = false;
            let mut leaked_files = Vec::new();

            for path in &paths {
                if path.exists() {
                    tool_has_file = true;
                    if let Ok(content) = std::fs::read_to_string(path) {
                        if contains_raw_key(&content) {
                            tool_raw_key_found = true;
                            leaked_files.push(path.display().to_string());
                        }
                    }
                }
            }

            if let Ok(Some((provider, token_opt))) = adapter.read_current() {
                if let Some(base) = &provider.base_url {
                    if is_loopback_url(base) {
                        tool_is_loopback = true;
                    }
                }
                if let Some(token) = token_opt {
                    if contains_raw_key(&token) {
                        tool_raw_key_found = true;
                    }
                }
            }

            let direct_mode = self.core.is_direct(tool).unwrap_or(false);

            let (trust, summary) = if !tool_has_file {
                (
                    TrustState::Guarded,
                    "no configuration file present".to_string(),
                )
            } else if tool_raw_key_found {
                if direct_mode {
                    (
                        TrustState::Direct,
                        "direct mode active (--direct), raw upstream key stored".to_string(),
                    )
                } else {
                    (
                        TrustState::Exposed,
                        format!(
                            "raw key detected on disk ({})",
                            if leaked_files.is_empty() {
                                "in config"
                            } else {
                                &leaked_files[0]
                            }
                        ),
                    )
                }
            } else if tool_is_loopback {
                (
                    TrustState::Guarded,
                    "isolated via local loopback gateway with local client token".to_string(),
                )
            } else if direct_mode {
                (
                    TrustState::Direct,
                    "direct mode active (--direct)".to_string(),
                )
            } else {
                (
                    TrustState::Guarded,
                    "clean (no sensitive keys found)".to_string(),
                )
            };

            match trust {
                TrustState::Exposed => any_exposed = true,
                TrustState::Direct => any_direct = true,
                TrustState::Guarded => {}
            }

            tool_reports.push(ToolTrustReport {
                tool,
                trust,
                summary,
            });

            if tool_has_file {
                let status = match trust {
                    TrustState::Guarded => CheckStatus::Pass,
                    TrustState::Direct => CheckStatus::Warn,
                    TrustState::Exposed => CheckStatus::Fail,
                };
                checks.push(SecurityCheckItem {
                    name: format!("Tool Config: {tool}"),
                    status,
                    details: format!("{trust}: {}", tool_reports.last().unwrap().summary),
                    fixable: trust == TrustState::Exposed,
                });
            }
        }

        // 2. Audit file permissions
        let perm_checks = self.audit_file_permissions();
        for check in perm_checks {
            if check.status == CheckStatus::Fail {
                any_exposed = true;
            }
            checks.push(check);
        }

        // 3. Audit daemon gateway binding
        let daemon_check = self.audit_daemon_binding();
        if daemon_check.status == CheckStatus::Fail {
            any_exposed = true;
        }
        checks.push(daemon_check);

        // Compute overall trust state
        let overall_trust = if any_exposed {
            TrustState::Exposed
        } else if any_direct {
            TrustState::Direct
        } else {
            TrustState::Guarded
        };

        Ok(SecurityDoctorReport {
            overall_trust,
            tool_trust: tool_reports,
            checks,
        })
    }

    /// Automatically fix remediable security issues:
    /// - Re-secures permissive file permissions to 0600/0700.
    /// - For tools marked Exposed that have an active provider registered in xfade,
    ///   re-applies gateway mode to wash out raw keys into local tokens.
    pub fn fix(&self) -> Result<Vec<String>> {
        let mut actions = Vec::new();

        // 1. Fix file permissions
        let data_dir = self.core.data_dir();
        if data_dir.exists() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(data_dir) {
                    if meta.permissions().mode() & 0o077 != 0 {
                        let _ = std::fs::set_permissions(
                            data_dir,
                            std::fs::Permissions::from_mode(0o700),
                        );
                        actions.push(format!("chmod 0700 {}", data_dir.display()));
                    }
                }
                if let Ok(entries) = std::fs::read_dir(data_dir) {
                    for entry in entries.flatten() {
                        let p = entry.path();
                        if let Ok(meta) = std::fs::metadata(&p) {
                            if meta.is_file() && meta.permissions().mode() & 0o077 != 0 {
                                let _ = std::fs::set_permissions(
                                    &p,
                                    std::fs::Permissions::from_mode(0o600),
                                );
                                actions.push(format!("chmod 0600 {}", p.display()));
                            }
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            if let Ok(entries) = std::fs::read_dir(data_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        if crate::fsutil::set_private_dacl(&p).is_ok() {
                            actions.push(format!("set DACL (owner-only) {}", p.display()));
                        }
                    }
                }
            }
        }

        // 2. Fix exposed tool configs
        for tool in ToolKind::ALL {
            let adapter = adapter_for(tool, self.core.home());
            let mut has_leak = false;
            for path in adapter.config_paths() {
                if path.exists() {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if contains_raw_key(&content) {
                            has_leak = true;
                            break;
                        }
                    }
                }
            }

            if has_leak && !self.core.is_direct(tool).unwrap_or(false) {
                // If there's an active provider in xfade, re-apply gateway mode
                if let Ok(providers) = self.core.list(Some(tool)) {
                    if let Some(active) = providers.iter().find(|p| p.is_active) {
                        let _ = self.core.use_provider(tool, &active.id, false);
                        actions.push(format!(
                            "Re-applied zero-raw-key gateway for {tool} (provider: {})",
                            active.id
                        ));
                    }
                }
            }
        }

        // 3. On macOS with Keyring + keychain_allow_all, normalize ACLs for active/routed
        //    keychain entries so Homebrew upgrades do not re-trigger authorization prompts.
        actions.extend(self.fix_keychain_acls());

        Ok(actions)
    }

    /// Re-write keychain entries for active and routed providers (plus global API key)
    /// so macOS Keychain items receive the allow-all ACL (`-T ""`).
    /// Only touches in-use entries to avoid prompting for unused legacy keys.
    fn fix_keychain_acls(&self) -> Vec<String> {
        let actions = Vec::new();
        if !cfg!(target_os = "macos")
            || self.core.active_secrets_backend() != Some(SecretsBackend::Keyring)
            || !self.core.config().keychain_allow_all
        {
            return actions;
        }

        let mut route_ids: HashSet<String> = HashSet::new();
        if let Ok(Some((routes, _, _))) = self.core.db().get_routes() {
            route_ids.extend(routes);
        }
        for tool in ToolKind::ALL {
            if let Ok(Some((routes, _, _))) = self.core.db().get_tool_routes(tool.as_str()) {
                route_ids.extend(routes);
            }
        }

        let mut key_refs: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        if let Ok(providers) = self.core.list(None) {
            for p in &providers {
                if !p.is_official()
                    && (p.is_active || route_ids.contains(&p.id))
                    && seen.insert(p.key_ref.clone())
                {
                    key_refs.push(p.key_ref.clone());
                }
            }
            for p in providers {
                if !p.is_official() && seen.insert(p.key_ref.clone()) {
                    key_refs.push(p.key_ref);
                }
            }
        }
        if seen.insert(GLOBAL_API_KEY_REF.to_string()) {
            key_refs.push(GLOBAL_API_KEY_REF.to_string());
        }

        normalize_keychain_acls(self.core.secrets(), &key_refs)
    }

    fn audit_file_permissions(&self) -> Vec<SecurityCheckItem> {
        let mut items = Vec::new();
        let data_dir = self.core.data_dir();
        if !data_dir.exists() {
            return items;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(data_dir) {
                let mode = meta.permissions().mode();
                if mode & 0o077 != 0 {
                    items.push(SecurityCheckItem {
                        name: "File Permissions: Data Directory".into(),
                        status: CheckStatus::Fail,
                        details: format!(
                            "{} permissions are {:04o} (expected 0700)",
                            data_dir.display(),
                            mode & 0o777
                        ),
                        fixable: true,
                    });
                } else {
                    items.push(SecurityCheckItem {
                        name: "File Permissions: Data Directory".into(),
                        status: CheckStatus::Pass,
                        details: format!(
                            "{} is private ({:04o})",
                            data_dir.display(),
                            mode & 0o777
                        ),
                        fixable: false,
                    });
                }
            }

            if let Ok(entries) = std::fs::read_dir(data_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if meta.is_file() {
                            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                            if file_name.ends_with("-wal") || file_name.ends_with("-shm") {
                                continue;
                            }
                            let mode = meta.permissions().mode();
                            if mode & 0o077 != 0 {
                                items.push(SecurityCheckItem {
                                    name: format!("File Permissions: {file_name}"),
                                    status: CheckStatus::Fail,
                                    details: format!(
                                        "{} permissions are {:04o} (expected 0600)",
                                        path.display(),
                                        mode & 0o777
                                    ),
                                    fixable: true,
                                });
                            } else {
                                items.push(SecurityCheckItem {
                                    name: format!("File Permissions: {file_name}"),
                                    status: CheckStatus::Pass,
                                    details: format!("{} is private (0600)", file_name),
                                    fixable: false,
                                });
                            }
                        }
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            if let Ok(entries) = std::fs::read_dir(data_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if file_name.ends_with("-wal") || file_name.ends_with("-shm") {
                            continue;
                        }
                        items.push(SecurityCheckItem {
                            name: format!("File Permissions: {file_name}"),
                            status: CheckStatus::Pass,
                            details: format!("{} is protected by owner-only DACL", file_name),
                            fixable: false,
                        });
                    }
                }
            }
        }

        items
    }

    fn audit_daemon_binding(&self) -> SecurityCheckItem {
        if let Some(cfg) = crate::daemon::read(self.core.data_dir()) {
            let is_loopback =
                cfg.host == "127.0.0.1" || cfg.host == "localhost" || cfg.host == "::1";
            if !is_loopback && cfg.auth_token.is_none() {
                SecurityCheckItem {
                    name: "Gateway Binding".into(),
                    status: CheckStatus::Fail,
                    details: format!(
                        "Gateway bound to external interface '{}' without authentication token",
                        cfg.host
                    ),
                    fixable: false,
                }
            } else {
                SecurityCheckItem {
                    name: "Gateway Binding".into(),
                    status: CheckStatus::Pass,
                    details: format!(
                        "Gateway bound to secure interface '{}:{}'",
                        cfg.host, cfg.port
                    ),
                    fixable: false,
                }
            }
        } else {
            SecurityCheckItem {
                name: "Gateway Binding".into(),
                status: CheckStatus::Pass,
                details: "No active external daemon config detected (on-demand spawning ready)"
                    .into(),
                fixable: false,
            }
        }
    }
}

/// Helper function to detect raw API keys in text.
/// Matches `sk-...` patterns or bearer tokens that are not internal xfade tokens.
fn contains_raw_key(text: &str) -> bool {
    for line in text.lines() {
        if let Some(idx) = line.find("sk-") {
            let tail = &line[idx..];
            let token_end = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .unwrap_or(tail.len());
            let candidate = &tail[..token_end];
            // Typical API keys (OpenAI, Anthropic, Moonshot, etc.) are at least 15 chars
            if candidate.len() >= 15 && !candidate.starts_with("xfade-local-") {
                return true;
            }
        }
    }
    false
}

fn is_loopback_url(url: &str) -> bool {
    url.contains("127.0.0.1") || url.contains("localhost") || url.contains("[::1]")
}

/// Refresh the single master key ACL (if any) and migrate/normalize each
/// `key_ref` into the encrypted vault with allow-all ACLs. Stops immediately
/// on any keyring error (e.g. user canceled or denied authorization).
fn normalize_keychain_acls(secrets: &dyn SecretStore, key_refs: &[String]) -> Vec<String> {
    let mut actions = Vec::new();
    match secrets.normalize_master_key() {
        Ok(true) => {
            actions.push("Updated macOS Keychain ACL (allow-all) for xfade/master_key".into())
        }
        Ok(false) => {}
        Err(e) => {
            actions.push(format!(
                "Stopped macOS Keychain ACL fix at xfade/master_key: {e}"
            ));
            return actions;
        }
    }
    for key_ref in key_refs {
        match secrets.get(key_ref) {
            Ok(secret) => match secrets.set(key_ref, &secret) {
                Ok(()) => {
                    actions.push(format!(
                        "Updated macOS Keychain ACL (allow-all) for {key_ref}"
                    ));
                }
                Err(e) => {
                    actions.push(format!("Stopped macOS Keychain ACL fix at {key_ref}: {e}"));
                    break;
                }
            },
            Err(CoreError::SecretNotFound(_)) => continue,
            Err(e) => {
                actions.push(format!("Stopped macOS Keychain ACL fix at {key_ref}: {e}"));
                break;
            }
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Provider;
    use std::sync::Arc;

    fn setup_test_core() -> (tempfile::TempDir, Core) {
        std::env::set_var("XFADE_TEST_SKIP_SPAWN", "1");
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700));
        }

        let secrets = Arc::new(crate::store::secrets::FileStore::new(
            data.join("secrets.json"),
        ));
        let core = Core::with_paths(&home, &data, secrets).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(entries) = std::fs::read_dir(&data) {
                for e in entries.flatten() {
                    let _ =
                        std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o600));
                }
            }
        }

        (dir, core)
    }

    #[test]
    fn test_contains_raw_key_detection() {
        assert!(contains_raw_key(
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-api03-abcdefghijklmn"}}"#
        ));
        assert!(contains_raw_key(r#"openai-api-key: sk-1234567890abcdef"#));
        assert!(!contains_raw_key(
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"xfade-local-claude-abcdef"}}"#
        ));
        assert!(!contains_raw_key("random string without secret"));
    }

    #[test]
    fn test_doctor_detects_guarded_tool() {
        let (_dir, core) = setup_test_core();
        let claude_dir = core.home().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:24860","ANTHROPIC_AUTH_TOKEN":"xfade-local-claude-test"}}"#,
        )
        .unwrap();

        let doctor = SecurityDoctor::new(&core);
        let report = doctor.check().unwrap();
        let claude_report = report
            .tool_trust
            .iter()
            .find(|t| t.tool == ToolKind::ClaudeCode)
            .unwrap();

        assert_eq!(claude_report.trust, TrustState::Guarded);
        assert_eq!(report.overall_trust, TrustState::Guarded);
    }

    #[test]
    fn test_doctor_detects_exposed_raw_key() {
        let (_dir, core) = setup_test_core();
        let claude_dir = core.home().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://api.anthropic.com","ANTHROPIC_AUTH_TOKEN":"sk-ant-api03-leaked-raw-secret"}}"#,
        )
        .unwrap();

        let doctor = SecurityDoctor::new(&core);
        let report = doctor.check().unwrap();
        let claude_report = report
            .tool_trust
            .iter()
            .find(|t| t.tool == ToolKind::ClaudeCode)
            .unwrap();

        assert_eq!(claude_report.trust, TrustState::Exposed);
        assert_eq!(report.overall_trust, TrustState::Exposed);
    }

    #[test]
    fn test_doctor_detects_direct_mode() {
        let (_dir, core) = setup_test_core();
        let p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn".into()),
        );
        core.add_provider(p, Some("sk-secret-1234567890")).unwrap();
        core.use_provider(ToolKind::ClaudeCode, "kimi", true)
            .unwrap();

        let doctor = SecurityDoctor::new(&core);
        let report = doctor.check().unwrap();
        let claude_report = report
            .tool_trust
            .iter()
            .find(|t| t.tool == ToolKind::ClaudeCode)
            .unwrap();

        assert_eq!(claude_report.trust, TrustState::Direct);
        assert_eq!(report.overall_trust, TrustState::Direct);
    }

    #[test]
    #[cfg(unix)]
    fn test_doctor_file_permission_audit_and_fix() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, core) = setup_test_core();
        let db_path = core.data_dir().join("test.txt");
        std::fs::write(&db_path, "test").unwrap();
        std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let doctor = SecurityDoctor::new(&core);
        let report = doctor.check().unwrap();
        let perm_item = report
            .checks
            .iter()
            .find(|c| c.name.contains("test.txt"))
            .unwrap();
        assert_eq!(perm_item.status, CheckStatus::Fail);
        assert!(perm_item.fixable);

        let actions = doctor.fix().unwrap();
        assert!(!actions.is_empty());

        let meta = std::fs::metadata(&db_path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn test_doctor_fix_cleans_exposed_tool_config() {
        let (_dir, core) = setup_test_core();
        let p = Provider::new(
            "kimi",
            ToolKind::ClaudeCode,
            Some("https://api.moonshot.cn".into()),
        );
        core.add_provider(p, Some("sk-secret-1234567890")).unwrap();

        // Simulate a leak on disk
        let claude_dir = core.home().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-secret-1234567890"}}"#,
        )
        .unwrap();

        // Mark kimi as active
        core.db().set_active(ToolKind::ClaudeCode, "kimi").unwrap();

        let doctor = SecurityDoctor::new(&core);
        let report = doctor.check().unwrap();
        assert_eq!(report.overall_trust, TrustState::Exposed);

        let actions = doctor.fix().unwrap();
        assert!(actions.iter().any(|a| a.contains("claude")));

        let content = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
        assert!(!content.contains("sk-secret-1234567890"));
        assert!(content.contains("http://127.0.0.1:24860"));
        assert!(content.contains("xfade-local-claude-"));
    }

    #[test]
    fn audit_daemon_binding_uses_core_data_dir() {
        let (_dir, core) = setup_test_core();
        assert_ne!(core.data_dir(), crate::daemon::data_dir(core.home()));
        let cfg = crate::daemon::DaemonConfig {
            host: "0.0.0.0".into(),
            port: 24860,
            auth_token: None,
        };
        crate::daemon::write(core.data_dir(), &cfg).unwrap();

        let doctor = SecurityDoctor::new(&core);
        let item = doctor.audit_daemon_binding();
        assert_eq!(item.status, CheckStatus::Fail);
        assert!(item.details.contains("0.0.0.0"));
    }

    #[test]
    fn normalize_keychain_acls_stops_on_first_keyring_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FailingStore {
            gets: AtomicUsize,
        }
        impl SecretStore for FailingStore {
            fn set(&self, _key_ref: &str, _secret: &str) -> Result<()> {
                Ok(())
            }
            fn get(&self, _key_ref: &str) -> Result<String> {
                self.gets.fetch_add(1, Ordering::SeqCst);
                Err(CoreError::Keyring("User canceled".into()))
            }
            fn delete(&self, _key_ref: &str) -> Result<()> {
                Ok(())
            }
        }

        let store = FailingStore {
            gets: AtomicUsize::new(0),
        };
        let refs = vec![
            "xfade/aider/office".to_string(),
            "xfade/claude/office".to_string(),
            GLOBAL_API_KEY_REF.to_string(),
        ];
        let actions = normalize_keychain_acls(&store, &refs);
        assert_eq!(store.gets.load(Ordering::SeqCst), 1);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].contains("xfade/aider/office"));
        assert!(actions[0].contains("Stopped"));
    }
}
