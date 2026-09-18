use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

/// Run xfade with an isolated HOME
fn xfade(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("xfade").unwrap();
    cmd.env("HOME", home)
        .env("XFADE_DATA_DIR", home.join(".xfade-data"))
        .env("XFADE_SECRETS", "file");
    cmd
}

#[test]
fn add_ls_use_current_flow() {
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
            "sk-1",
            "--set",
            "model=kimi-k2.5",
        ])
        .assert()
        .success();

    xfade(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));

    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();

    let settings = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(settings.contains("api.moonshot.cn"));

    xfade(home.path())
        .args(["current"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));
}

#[test]
fn rm_active_fails() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "k",
            "--base-url",
            "https://x",
            "--key",
            "1",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "k", "--tool", "codex"])
        .assert()
        .success();
    xfade(home.path())
        .args(["rm", "k", "--tool", "codex"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("active"));
}

#[test]
fn presets_lists_all_tools() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
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

    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "kimi",
            "--base-url",
            "https://x",
            "--key",
            "sk-new",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();
    xfade(home.path())
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
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "kimi",
            "--base-url",
            "https://api.moonshot.cn/v1",
            "--key",
            "sk-1",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "kimi", "--tool", "codex"])
        .assert()
        .success();

    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(cfg.contains("model_provider = \"kimi\""));
    let auth = fs::read_to_string(home.path().join(".codex/auth.json")).unwrap();
    assert!(auth.contains("sk-1"));

    xfade(home.path())
        .args(["add", "official", "--tool", "codex"])
        .assert()
        .success(); // no --base-url => official
    xfade(home.path())
        .args(["use", "official", "--tool", "codex"])
        .assert()
        .success();
    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(!cfg.contains("model_provider ="));
}

#[test]
fn edit_updates_base_url_and_extra() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "kimi",
            "--base-url",
            "https://old",
            "--key",
            "sk-1",
        ])
        .assert()
        .success();
    xfade(home.path())
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
    xfade(home.path())
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
    xfade(home.path())
        .args(["import", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing"));
    xfade(home.path())
        .args(["backup", "ls", "--tool", "claude"])
        .assert()
        .success();
    fs::create_dir_all(home.path().join(".claude")).unwrap();
    fs::write(home.path().join(".claude/settings.json"), "{}").unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "k",
            "--base-url",
            "https://x",
            "--key",
            "1",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "k", "--tool", "claude"])
        .assert()
        .success();
    xfade(home.path())
        .args(["backup", "ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("settings.json"));
}

#[test]
fn completion_generates() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["completion", "zsh"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef xfade"));
}

#[test]
fn proxy_use_status_clear() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "yy",
            "--base-url",
            "http://x",
            "--key",
            "k",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["proxy", "use", "yy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("yy"));
    xfade(home.path())
        .args(["proxy", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("yy"));
    xfade(home.path())
        .args(["proxy", "use", "nope"])
        .assert()
        .failure();
    xfade(home.path())
        .args(["proxy", "clear"])
        .assert()
        .success();
}

#[test]
fn proxy_use_with_model_and_target() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "codex",
            "yy",
            "--base-url",
            "http://x",
            "--key",
            "k",
        ])
        .assert()
        .success();
    // proxy use with --model and --target chat
    xfade(home.path())
        .args([
            "proxy",
            "use",
            "yy",
            "--model",
            "gpt-5.6-luna",
            "--target",
            "chat",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("proxy route: yy"))
        .stdout(predicate::str::contains("model: gpt-5.6-luna"))
        .stdout(predicate::str::contains("target: chat"));
    // status reflects model + target
    xfade(home.path())
        .args(["proxy", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("routes: yy"))
        .stdout(predicate::str::contains("model: gpt-5.6-luna"))
        .stdout(predicate::str::contains("target: chat"));
    // target=messages path
    xfade(home.path())
        .args(["proxy", "use", "yy", "--target", "messages"])
        .assert()
        .success()
        .stdout(predicate::str::contains("model: (passthrough)"))
        .stdout(predicate::str::contains("target: messages"));
    xfade(home.path())
        .args(["proxy", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("model: (passthrough)"))
        .stdout(predicate::str::contains("target: messages"));
    // invalid target value rejected by value_parser
    xfade(home.path())
        .args(["proxy", "use", "yy", "--target", "bogus"])
        .assert()
        .failure();
}

#[test]
fn stats_empty_ok() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path()).args(["stats"]).assert().success();
}

#[test]
fn presets_include_local_proxy() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["presets"])
        .assert()
        .success()
        .stdout(predicate::str::contains("local-proxy"));
}

#[test]
fn serve_help_lists_options() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["serve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--port").and(predicate::str::contains("--auth-token")));
}

#[test]
fn use_third_party_preset_without_key_errors() {
    // Regression: `xfade use glm --tool claude` used to auto-create the provider with a
    // placeholder key, which got written into ANTHROPIC_AUTH_TOKEN and broke Claude Code auth.
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["use", "glm", "--tool", "claude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs an API key"));
}

#[test]
fn config_view_and_set_secrets() {
    let home = tempfile::tempdir().unwrap();

    // default backend is keyring
    xfade(home.path())
        .args(["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("secrets: keyring"));

    // set it to file, then view reflects the change
    xfade(home.path())
        .args(["config", "set", "secrets", "file"])
        .assert()
        .success();
    xfade(home.path())
        .args(["config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("secrets: file"));
}

#[test]
fn config_set_unknown_key_errors() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["config", "set", "bogus", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown config key"));
}

#[test]
fn add_uses_global_defaults() {
    let home = tempfile::tempdir().unwrap();

    // Set the shared "general" config once.
    xfade(home.path())
        .args(["config", "set", "secrets", "file"])
        .assert()
        .success();
    xfade(home.path())
        .args(["config", "set", "base_url", "https://global.example"])
        .assert()
        .success();
    xfade(home.path())
        .args(["config", "set", "model", "glm-5-2-260617"])
        .assert()
        .success();
    xfade(home.path())
        .args(["config", "set", "api_key", "sk-global"])
        .assert()
        .success();

    // add with no --base-url/--key/--set model → inherits global defaults.
    xfade(home.path())
        .args(["add", "g", "--tool", "claude"])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "g", "--tool", "claude"])
        .assert()
        .success();

    let s = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("https://global.example"));
    assert!(s.contains("sk-global"));
    assert!(s.contains("glm-5-2-260617"));
}

#[test]
fn add_official_flag_ignores_global_base_url() {
    let home = tempfile::tempdir().unwrap();

    xfade(home.path())
        .args(["config", "set", "base_url", "https://global.example"])
        .assert()
        .success();

    // --official forces official login, ignoring the global base_url.
    xfade(home.path())
        .args(["add", "official", "--tool", "codex", "--official"])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "official", "--tool", "codex"])
        .assert()
        .success();

    let cfg = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap();
    assert!(!cfg.contains("model_provider ="));
}

#[test]
fn aider_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "kimi",
            "--tool",
            "aider",
            "--base-url",
            "https://api.moonshot.cn/v1",
            "--key",
            "sk-1",
            "--set",
            "model=kimi-k2.5",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "kimi", "--tool", "aider"])
        .assert()
        .success();

    let s = fs::read_to_string(home.path().join(".aider.conf.yml")).unwrap();
    assert!(s.contains("openai/kimi-k2.5"));
    assert!(s.contains("sk-1"));
    assert!(s.contains("api.moonshot.cn"));
}
