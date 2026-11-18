use assert_cmd::Command;
use predicates::prelude::*;

fn xfade(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("xfade").unwrap();
    cmd.env("HOME", home)
        .env("XFADE_DATA_DIR", home.join(".xfade-data"))
        .env("XFADE_SECRETS", "file");
    cmd
}

#[test]
fn test_status_output() {
    let home = tempfile::tempdir().unwrap();

    // 1. Initial status with no routes
    xfade(home.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("routes: (none)"));

    // 2. Add providers and configure routes
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "upstream-a",
            "--base-url",
            "http://127.0.0.1:8001",
            "--key",
            "k1",
        ])
        .assert()
        .success();

    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "upstream-b",
            "--base-url",
            "http://127.0.0.1:8002",
            "--key",
            "k2",
        ])
        .assert()
        .success();

    xfade(home.path())
        .args(["proxy", "use", "upstream-a", "upstream-b"])
        .assert()
        .success();

    // 3. Verify `xfade status` surfaces three-state circuit status for configured providers
    xfade(home.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("routes: upstream-a -> upstream-b"))
        .stdout(predicate::str::contains("upstream-a"))
        .stdout(predicate::str::contains("[closed]"))
        .stdout(predicate::str::contains("upstream-b"));
}

#[test]
fn test_xfade_use_default_gateway_writes_zero_raw_keys() {
    let home = tempfile::tempdir().unwrap();

    // Add provider with real key
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "kimi",
            "--base-url",
            "https://api.moonshot.cn/anthropic",
            "--key",
            "sk-secret-key-12345",
        ])
        .assert()
        .success();

    // Switch to provider without --direct (defaults to gateway)
    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();

    let settings = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(
        !settings.contains("sk-secret-key-12345"),
        "raw key must not leak to disk"
    );
    assert!(settings.contains("http://127.0.0.1:24860"));
    assert!(settings.contains("xfade-local-claude-"));
}

#[test]
fn test_xfade_use_direct_escape_hatch_writes_raw_key() {
    let home = tempfile::tempdir().unwrap();

    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "kimi",
            "--base-url",
            "https://api.moonshot.cn/anthropic",
            "--key",
            "sk-secret-key-12345",
        ])
        .assert()
        .success();

    // Switch with --direct
    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude", "--direct"])
        .assert()
        .success();

    let settings = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(
        settings.contains("sk-secret-key-12345"),
        "direct escape hatch must write real key"
    );
}
