use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// 以隔离 HOME 运行 asw
fn asw(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("asw").unwrap();
    cmd.env("HOME", home)
        .env("ASW_DATA_DIR", home.join(".asw-data"))
        .env("ASW_MOCK_SECRETS", "1");
    cmd
}

#[test]
fn add_ls_use_current_flow() {
    let home = tempfile::tempdir().unwrap();

    asw(home.path())
        .args(["add", "--tool", "claude", "--name", "kimi",
               "--base-url", "https://api.moonshot.cn/anthropic", "--key", "sk-1",
               "--set", "model=kimi-k2.5"])
        .assert()
        .success();

    asw(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));

    asw(home.path()).args(["use", "kimi", "--tool", "claude"]).assert().success();

    let settings = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(settings.contains("api.moonshot.cn"));

    asw(home.path())
        .args(["current"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));
}

#[test]
fn rm_active_fails() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "k", "--base-url", "https://x", "--key", "1"])
        .assert()
        .success();
    asw(home.path()).args(["use", "k", "--tool", "codex"]).assert().success();
    asw(home.path())
        .args(["rm", "k", "--tool", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("active"));
}

#[test]
fn presets_lists_all_tools() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["presets"])
        .assert()
        .success()
        .stdout(predicate::str::contains("official").and(predicate::str::contains("kimi")));
}
