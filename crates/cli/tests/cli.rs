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
        .args([
            "add",
            "--tool",
            "claude",
            "--name",
            "kimi",
            "--base-url",
            "https://api.moonshot.cn/anthropic",
            "--key",
            "sk-1",
            "--set",
            "model=kimi-k2.5",
        ])
        .assert()
        .success();

    asw(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));

    asw(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();

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
        .args([
            "add",
            "--tool",
            "codex",
            "--name",
            "k",
            "--base-url",
            "https://x",
            "--key",
            "1",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "k", "--tool", "codex"])
        .assert()
        .success();
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

#[test]
fn switch_back_to_imported_snapshot() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".claude")).unwrap();
    fs::write(
        home.path().join(".claude/settings.json"),
        r#"{"env":{"ANTHROPIC_BASE_URL":"https://relay","ANTHROPIC_AUTH_TOKEN":"sk-old"}}"#,
    )
    .unwrap();

    asw(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "--name",
            "kimi",
            "--base-url",
            "https://x",
            "--key",
            "sk-new",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "imported", "--tool", "claude"])
        .assert()
        .success();

    let s = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("https://relay"));
    assert!(s.contains("sk-old"));
}

#[test]
fn codex_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "--name",
            "kimi",
            "--base-url",
            "https://api.moonshot.cn/v1",
            "--key",
            "sk-1",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "kimi", "--tool", "codex"])
        .assert()
        .success();

    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(cfg.contains("model_provider = \"kimi\""));
    let auth = fs::read_to_string(home.path().join(".codex/auth.json")).unwrap();
    assert!(auth.contains("sk-1"));

    asw(home.path())
        .args(["add", "--tool", "codex", "--name", "official"])
        .assert()
        .success(); // 无 --base-url => 官方
    asw(home.path())
        .args(["use", "official", "--tool", "codex"])
        .assert()
        .success();
    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(!cfg.contains("model_provider ="));
}

#[test]
fn edit_updates_base_url_and_extra() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "--name",
            "kimi",
            "--base-url",
            "https://old",
            "--key",
            "sk-1",
        ])
        .assert()
        .success();
    asw(home.path())
        .args([
            "edit",
            "kimi",
            "--tool",
            "claude",
            "--base-url",
            "https://new",
            "--set",
            "model=k2",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();
    let s = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("https://new"));
    assert!(s.contains("k2")); // ANTHROPIC_MODEL from extra.model
}

#[test]
fn import_and_backup_commands() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["import", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing"));
    asw(home.path())
        .args(["backup", "ls", "--tool", "claude"])
        .assert()
        .success();
    fs::create_dir_all(home.path().join(".claude")).unwrap();
    fs::write(home.path().join(".claude/settings.json"), "{}").unwrap();
    asw(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "--name",
            "k",
            "--base-url",
            "https://x",
            "--key",
            "1",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["use", "k", "--tool", "claude"])
        .assert()
        .success();
    asw(home.path())
        .args(["backup", "ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("settings.json"));
}

#[test]
fn completion_generates() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["completion", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef asw"));
}

#[test]
fn proxy_use_status_clear() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "--name",
            "yy",
            "--base-url",
            "http://x",
            "--key",
            "k",
        ])
        .assert()
        .success();
    asw(home.path())
        .args(["proxy", "use", "yy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("yy"));
    asw(home.path())
        .args(["proxy", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("yy"));
    asw(home.path())
        .args(["proxy", "use", "nope"])
        .assert()
        .failure();
    asw(home.path()).args(["proxy", "clear"]).assert().success();
}

#[test]
fn stats_empty_ok() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path()).args(["stats"]).assert().success();
}

#[test]
fn presets_include_local_proxy() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["presets"])
        .assert()
        .success()
        .stdout(predicate::str::contains("local-proxy"));
}

#[test]
fn serve_help_lists_options() {
    let home = tempfile::tempdir().unwrap();
    asw(home.path())
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--port").and(predicate::str::contains("--auth-token")));
}
