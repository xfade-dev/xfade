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
    assert!(settings.contains("http://127.0.0.1:24860"));
    assert!(settings.contains("xfade-local-claude-"));
    assert!(!settings.contains("sk-"));

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
        .args(["use", "kimi", "--tool", "claude", "--direct"])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "imported", "--tool", "claude", "--direct"])
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
        .args(["use", "kimi", "--tool", "codex", "--direct"])
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
        .args(["use", "kimi", "--tool", "claude", "--direct"])
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
fn root_help_displays_command_categories() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["--help"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("CORE ROUTING & SWITCHING:")
                .and(predicate::str::contains("GATEWAY & TRAFFIC GOVERNANCE:"))
                .and(predicate::str::contains("SECURITY & TRUST:"))
                .and(predicate::str::contains("CONFIGURATION & ASSETS:"))
                .and(predicate::str::contains("TOOL LAUNCHERS"))
                .and(predicate::str::contains("GENERAL:")),
        );
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
fn use_local_proxy_openclaw_requires_model() {
    // Regression: `xfade use local-proxy --tool openclaw` used to auto-create a
    // provider with no model, then fail in `apply` (OpenClaw needs a model to set
    // `agents.defaults.model.primary`), leaving a half-added provider behind.
    let home = tempfile::tempdir().unwrap();

    // No global model → clear error, and no half-added provider.
    xfade(home.path())
        .args(["use", "local-proxy", "--tool", "openclaw"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a model"));
    xfade(home.path())
        .args(["ls", "--tool", "openclaw"])
        .assert()
        .success()
        .stdout(predicate::str::contains("local-proxy").not());

    // With a global default model, the auto-create succeeds and pins the primary.
    xfade(home.path())
        .args(["config", "set", "model", "gpt-4o"])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "local-proxy", "--tool", "openclaw"])
        .assert()
        .success();

    let doc = fs::read_to_string(home.path().join(".openclaw/openclaw.json")).unwrap();
    assert!(doc.contains("xfade/gpt-4o"));
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
        .args(["use", "g", "--tool", "claude", "--direct"])
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
        .args(["use", "kimi", "--tool", "aider", "--direct"])
        .assert()
        .success();

    let s = fs::read_to_string(home.path().join(".aider.conf.yml")).unwrap();
    assert!(s.contains("openai/kimi-k2.5"));
    assert!(s.contains("sk-1"));
    assert!(s.contains("api.moonshot.cn"));
}

#[test]
fn add_per_slot_model_writes_default_vars() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args([
            "add",
            "--tool",
            "claude",
            "work",
            "--base-url",
            "https://gw.example/anthropic",
            "--key",
            "sk-1",
            "--opus-model",
            "deepseek/deepseek-v4-pro",
            "--sonnet-model",
            "deepseek/deepseek-v4-flash",
            "--haiku-model",
            "deepseek/deepseek-v4-flash",
        ])
        .assert()
        .success();
    xfade(home.path())
        .args(["use", "work", "--tool", "claude"])
        .assert()
        .success();

    let s = fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
    assert!(s.contains("ANTHROPIC_DEFAULT_OPUS_MODEL"));
    assert!(s.contains("deepseek/deepseek-v4-pro"));
    assert!(s.contains("ANTHROPIC_DEFAULT_SONNET_MODEL"));
    assert!(s.contains("ANTHROPIC_DEFAULT_HAIKU_MODEL"));
    // Per-slot mode must not also pin the single ANTHROPIC_MODEL.
    assert!(!s.contains("\"ANTHROPIC_MODEL\""));
}

#[test]
fn per_slot_flag_on_non_claude_fails() {
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
            "--opus-model",
            "some-model",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only valid for `--tool claude`"));
}

// --- instant-launch subcommands (`xfade claude ...`) -----------------------
//
// Any test that reaches the exec step sets PATH to the (binary-less) fake
// HOME, so spawning the tool fails deterministically with the mapped
// "not found in PATH" error instead of launching a real CLI.

#[test]
fn instant_config_file_tool_points_to_use() {
    let home = tempfile::tempdir().unwrap();
    // codex/opencode have a verified injection contract (launch.rs) and pass
    // this gate; the remaining config-file tools are still refused.
    for tool in ["pi", "omp", "cline", "hermes", "openclaw"] {
        xfade(home.path())
            .args([tool])
            .assert()
            .failure()
            .stderr(predicate::str::contains("use `xfade use` instead"));
    }
    // The `omp` variant also answers its `oh-my-pi` alias.
    xfade(home.path())
        .args(["oh-my-pi"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("use `xfade use` instead"));
    // Refused before any side effects: nothing persisted.
    xfade(home.path())
        .args(["ls"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn instant_codex_opencode_pass_the_support_gate() {
    // codex/opencode are no longer refused by the tool gate: an unknown name
    // fails at provider resolution, not with the `use` hint. (Never reaches
    // exec, so no real binary is spawned.)
    let home = tempfile::tempdir().unwrap();
    for tool in ["codex", "opencode"] {
        xfade(home.path())
            .args([tool, "no-such-provider"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("use `xfade use` instead").not());
    }
}

#[test]
fn instant_named_preset_autocreates_and_launches() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .env("PATH", home.path())
        .args(["claude", "kimi", "--key", "sk-1"])
        .assert()
        .failure() // exec fails: no `claude` binary in the fake PATH
        .stdout(predicate::str::contains(
            "auto-created claude/kimi from preset",
        ))
        .stderr(predicate::str::contains("not found in PATH"));
    // The provider persisted for future runs.
    xfade(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi"));
}

#[test]
fn instant_registered_provider_launches_without_recreate() {
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
        .env("PATH", home.path())
        .args(["claude", "kimi"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("auto-created").not())
        .stderr(predicate::str::contains("not found in PATH"));
}

#[test]
fn project_prints_analyzer_fingerprint() {
    let home = tempfile::tempdir().unwrap();
    // The test process cwd is inside the xfade repo (Cargo.toml + git).
    xfade(home.path())
        .args(["project"])
        .assert()
        .success()
        .stdout(predicate::str::contains("root:"))
        .stdout(predicate::str::contains("lang: rust"))
        .stdout(predicate::str::contains("branch:"));
    // The analysis was cached into repo_maps (a second consumer of it is
    // `xfade proxy status`).
    xfade(home.path())
        .args(["proxy", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("project:"));
}

#[test]
fn instant_model_reverse_lookup_autocreates() {
    // No name: `--model kimi-k2.5` prefix-matches the `kimi` preset uniquely.
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .env("PATH", home.path())
        .args(["claude", "--model", "kimi-k2.5", "--key", "sk-1"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "auto-created claude/kimi from preset",
        ))
        .stderr(predicate::str::contains("not found in PATH"));
}

#[test]
fn instant_model_no_match_errors() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["claude", "--model", "totally-unknown-model"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "no provider or preset matches model",
        ));
}

#[test]
fn instant_third_party_preset_needs_key() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["claude", "kimi"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs an API key"));
    // No half-add: the preset wasn't persisted without a key.
    xfade(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("kimi").not());
}

#[test]
fn instant_global_key_satisfies_third_party_preset() {
    // Unlike `use` (which demands an explicit add), a launch may fall back to
    // the global key: it never writes the tool config, so a bad key only
    // fails this one request.
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["config", "set", "api_key", "sk-global"])
        .assert()
        .success();
    xfade(home.path())
        .env("PATH", home.path())
        .args(["claude", "kimi"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "auto-created claude/kimi from preset",
        ))
        .stderr(predicate::str::contains("not found in PATH"));
}

#[test]
fn instant_adhoc_base_url_creates() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .env("PATH", home.path())
        .args([
            "claude",
            "my-gw",
            "--base-url",
            "https://gw.example/anthropic",
            "--key",
            "sk-1",
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("auto-created claude/my-gw"))
        .stderr(predicate::str::contains("not found in PATH"));
    xfade(home.path())
        .args(["ls", "--tool", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("my-gw"));
}

#[test]
fn instant_adhoc_base_url_requires_key() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["claude", "my-gw", "--base-url", "https://gw.example"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs an API key"));
}

#[test]
fn instant_base_url_flip_official_requires_key() {
    // --base-url flips the official provider to third-party; a key becomes
    // mandatory instead of silently launching unauthenticated.
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["claude", "official", "--base-url", "https://gw.example"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no API key"));
}

#[test]
fn instant_bare_launch_official_when_no_active() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .env("PATH", home.path())
        .args(["claude"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("auto-created").not())
        .stderr(predicate::str::contains("not found in PATH"));
}

// --- unified session view ---------------------------------------------------

#[test]
fn sessions_empty_and_show_not_found() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no sessions yet"));
    xfade(home.path())
        .args(["sessions", "show", "s-nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn stats_by_session_accepted() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["stats", "--by", "session"])
        .assert()
        .success();
    // Unknown dimension is rejected by the value parser.
    xfade(home.path())
        .args(["stats", "--by", "bogus"])
        .assert()
        .failure();
}

#[test]
fn stats_and_sessions_display_cost_estimation() {
    let home = tempfile::tempdir().unwrap();
    let data_dir = home.path().join(".xfade-data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let db = xfade_core::store::db::Database::open(&data_dir.join("xfade.db")).unwrap();

    // Insert a request log with cost_usd_micros: 39_600 = $0.0396
    db.insert_request_log(&xfade_core::store::db::RequestLog {
        ts: "2026-11-20T12:00:00Z".into(),
        endpoint: "chat".into(),
        model: Some("claude-3-7-sonnet".into()),
        provider_id: "prov1".into(),
        tool: "claude".into(),
        status: 200,
        prompt_tokens: 10_000,
        completion_tokens: 1_000,
        duration_ms: 120,
        error: None,
        session_id: Some("s-test-cost-1".into()),
        cost_usd_micros: 39_600,
    })
    .unwrap();

    // Insert session and update progress with cost_usd_micros
    db.insert_session(
        "s-test-cost-1",
        "fp1",
        "2026-11-20T12:00:00Z",
        Some("first cost prompt"),
    )
    .unwrap();
    db.update_session_progress(
        "s-test-cost-1",
        "claude",
        "prov1",
        Some("claude-3-7-sonnet"),
        10_000,
        1_000,
        39_600,
        "ok",
        "2026-11-20T12:01:00Z",
    )
    .unwrap();

    // 1. Verify `xfade stats` header, row, and footnote
    xfade(home.path())
        .args(["stats", "--since", "30d"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cost (est)"))
        .stdout(predicate::str::contains("$0.0396"))
        .stdout(predicate::str::contains(
            "* cost estimates based on official token pricing (estimated)",
        ));

    // 2. Verify `xfade sessions` list includes cost (est) and $0.0396
    xfade(home.path())
        .args(["sessions"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cost (est)"))
        .stdout(predicate::str::contains("$0.0396"))
        .stdout(predicate::str::contains("s-test-cost-1"));

    // 3. Verify `xfade sessions show s-test-cost-1` displays cost: $0.0396 (estimated)
    xfade(home.path())
        .args(["sessions", "show", "s-test-cost-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cost:     $0.0396 (estimated)"));
}

#[test]
fn handover_command_prints_brief() {
    let home = tempfile::tempdir().unwrap();
    xfade(home.path())
        .args(["handover"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "### [xfade Session Handover Context]",
        ));

    xfade(home.path())
        .args(["handover", "--raw"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "### [xfade Session Handover Context]",
        ));
}

#[test]
fn doctor_security_command_checks_and_fixes() {
    let home = tempfile::tempdir().unwrap();

    // Clean initial run: should be GUARDED
    xfade(home.path())
        .args(["doctor", "--security"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Trust Status: GUARDED"));

    // Inject a leaked raw key in claude config
    let claude_dir = home.path().join(".claude");
    fs::create_dir_all(&claude_dir).unwrap();
    fs::write(
        claude_dir.join("settings.json"),
        r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-ant-api03-leaked-key-12345"}}"#,
    )
    .unwrap();

    // Now doctor should report EXPOSED
    xfade(home.path())
        .args(["doctor", "--security"])
        .assert()
        .success()
        .stdout(predicate::str::contains("EXPOSED"))
        .stdout(predicate::str::contains("claude"));

    // Status should also reflect EXPOSED
    xfade(home.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("trust: EXPOSED"));

    // Register kimi in xfade and run --fix
    xfade(home.path())
        .args([
            "add",
            "kimi",
            "--tool",
            "claude",
            "--base-url",
            "https://api.moonshot.cn",
            "--key",
            "sk-kimi-key-12345678",
        ])
        .assert()
        .success();

    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude"])
        .assert()
        .success();

    xfade(home.path())
        .args(["doctor", "--security", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Trust Status: GUARDED"));

    let settings = fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    assert!(!settings.contains("sk-ant-api03-leaked-key-12345"));
    assert!(settings.contains("xfade-local-claude-"));
}

#[test]
fn audit_command_and_export() {
    let home = tempfile::tempdir().unwrap();

    // Empty audit log
    xfade(home.path())
        .args(["audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No audit records found"));

    // Insert an audit log entry directly via SQLite
    let data_dir = home.path().join(".xfade-data");
    fs::create_dir_all(&data_dir).unwrap();
    let db = xfade_core::store::Db::open(&data_dir.join("xfade.db")).unwrap();
    db.insert_audit_log(&xfade_core::store::db::AuditLog {
        id: 0,
        ts: 1700000000000,
        client_tool: "claude".into(),
        client_token_id: "token:claude".into(),
        provider_id: "kimi".into(),
        upstream_domain: "api.moonshot.cn".into(),
        upstream_model: "kimi-k2.5".into(),
        key_ref: "ref:kimi#sha256:abcd1234".into(),
        status_code: 200,
        duration_ms: 120,
        prompt_tokens: 30,
        completion_tokens: 15,
        failover_path: None,
    })
    .unwrap();

    // Table view
    xfade(home.path())
        .args(["audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api.moonshot.cn"))
        .stdout(predicate::str::contains("ref:kimi#sha256:abcd1234"))
        .stdout(predicate::str::contains("200"));

    // JSON export
    xfade(home.path())
        .args(["audit", "--export", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"upstream_domain\": \"api.moonshot.cn\"",
        ));

    // CSV export
    xfade(home.path())
        .args(["audit", "--export", "--format", "csv"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "api.moonshot.cn,kimi-k2.5,ref:kimi#sha256:abcd1234,200",
        ));
}

#[test]
fn egress_command_whitelist_and_policy() {
    let home = tempfile::tempdir().unwrap();

    // Default policy
    xfade(home.path())
        .args(["egress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Policy Mode: registered"))
        .stdout(predicate::str::contains("(none)"));

    // Add a provider and check it appears under registered domains
    xfade(home.path())
        .args([
            "add",
            "openai",
            "--tool",
            "claude",
            "--base-url",
            "https://api.openai.com/v1",
            "--key",
            "sk-secret",
        ])
        .assert()
        .success();

    xfade(home.path())
        .args(["egress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api.openai.com"));

    // Add custom whitelist domain
    xfade(home.path())
        .args(["egress", "add", "https://api.groq.com/openai/v1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Added 'api.groq.com' to egress whitelist.",
        ));

    xfade(home.path())
        .args(["egress", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api.groq.com"));

    // Remove custom domain
    xfade(home.path())
        .args(["egress", "remove", "api.groq.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed 'api.groq.com' from egress whitelist.",
        ));

    // Change policy
    xfade(home.path())
        .args(["egress", "policy", "unrestricted"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Egress policy set to 'unrestricted'.",
        ));

    xfade(home.path())
        .args(["egress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Policy Mode: unrestricted"));

    // Revert policy
    xfade(home.path())
        .args(["egress", "policy", "registered"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Egress policy set to 'registered'.",
        ));
}

#[test]
fn e2e_visible_trust_audit_and_egress_lifecycle() {
    let home = tempfile::tempdir().unwrap();

    // 1. Initial status and doctor on empty home: Guarded
    xfade(home.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("trust: GUARDED"));

    xfade(home.path())
        .args(["doctor", "--security"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GUARDED"));

    // 2. Add provider with remote base URL
    xfade(home.path())
        .args([
            "add",
            "kimi",
            "--tool",
            "claude",
            "--base-url",
            "https://api.moonshot.cn/v1",
            "--key",
            "sk-secret1234567890abcdef",
        ])
        .assert()
        .success();

    // 3. Egress automatically includes api.moonshot.cn
    xfade(home.path())
        .args(["egress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api.moonshot.cn"))
        .stdout(predicate::str::contains("Policy Mode: registered"));

    // 4. Insert an audit log record with key fingerprint and failover trajectory
    let db = xfade_core::store::Db::open(&home.path().join(".xfade-data/xfade.db")).unwrap();
    db.insert_audit_log(&xfade_core::store::db::AuditLog {
        id: 0,
        ts: 1700000000000,
        client_tool: "claude".into(),
        client_token_id: "token:claude".into(),
        provider_id: "kimi".into(),
        upstream_domain: "api.moonshot.cn".into(),
        upstream_model: "kimi-k2.5".into(),
        key_ref: "ref:kimi#sha256:7f9a1234".into(),
        status_code: 200,
        duration_ms: 180,
        prompt_tokens: 42,
        completion_tokens: 24,
        failover_path: Some(
            r#"[{"provider":"primary","status":502},{"provider":"kimi","status":200}]"#.into(),
        ),
    })
    .unwrap();

    // 5. Query audit table and export
    xfade(home.path())
        .args(["audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("api.moonshot.cn"))
        .stdout(predicate::str::contains("ref:kimi#sha256:7f9a1234"));

    xfade(home.path())
        .args(["audit", "--export", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"failover_path\": \"[{\\\"provider\\\":\\\"primary\\\",\\\"status\\\":502},{\\\"provider\\\":\\\"kimi\\\",\\\"status\\\":200}]\""));

    // 6. Direct mode escape hatch makes trust state DIRECT
    xfade(home.path())
        .args(["use", "kimi", "--tool", "claude", "--direct"])
        .assert()
        .success();

    xfade(home.path())
        .args(["status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("trust: DIRECT"));
}

#[test]
fn cli_add_and_list_with_explicit_protocol() {
    let home = tempfile::tempdir().unwrap();

    // Add provider with explicit responses protocol
    xfade(home.path())
        .args([
            "add",
            "codex-upstream",
            "--tool",
            "codex",
            "--base-url",
            "https://api.openai.com/v1",
            "--key",
            "sk-test",
            "--protocol",
            "responses",
        ])
        .assert()
        .success();

    // List and check protocol column
    xfade(home.path())
        .args(["ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("responses"))
        .stdout(predicate::str::contains("codex-upstream"));
}

#[test]
fn test_cli_pricing_commands() {
    let home = tempfile::tempdir().unwrap();

    // 1. List shows built-in models
    xfade(home.path())
        .args(["pricing", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-sonnet-5.5"))
        .stdout(predicate::str::contains("gpt-6.2-astra"))
        .stdout(predicate::str::contains("builtin"));

    // 2. Set custom pricing override
    xfade(home.path())
        .args([
            "pricing",
            "set",
            "my-custom-model",
            "--input",
            "1.50",
            "--output",
            "6.00",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Set custom pricing for my-custom-model",
        ));

    // 3. List with filter
    xfade(home.path())
        .args(["pricing", "list", "--filter", "my-custom"])
        .assert()
        .success()
        .stdout(predicate::str::contains("my-custom-model"))
        .stdout(predicate::str::contains("custom"))
        .stdout(predicate::str::contains("$1.5000"))
        .stdout(predicate::str::contains("$6.0000"));

    // 4. Remove custom override
    xfade(home.path())
        .args(["pricing", "rm", "my-custom-model"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Removed custom pricing override for my-custom-model",
        ));

    // 5. Reset synced pricing
    xfade(home.path())
        .args(["pricing", "reset", "--synced"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Cleared 0 synced model pricing entries",
        ));

    // 6. Validation: reject empty model name
    xfade(home.path())
        .args(["pricing", "set", "  ", "--input", "1.0", "--output", "2.0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("model pattern cannot be empty"));

    // 7. Validation: reject negative rates
    xfade(home.path())
        .args([
            "pricing", "set", "bad-rate", "--input", "-1.0", "--output", "2.0",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "pricing rates must be non-negative",
        ));

    // 8. Validation: reset without --synced fails with guidance
    xfade(home.path())
        .args(["pricing", "reset"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing required flag: --synced"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cli_pricing_sync_custom_url() {
    use axum::{routing::get, Json, Router};

    let payload = serde_json::json!({
        "data": [
            {
                "id": "mock/special-model-2026",
                "pricing": {
                    "prompt": "0.000003",
                    "completion": "0.000015"
                }
            }
        ]
    });

    let app = Router::new().route("/models", get(move || async move { Json(payload) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let home = tempfile::tempdir().unwrap();
    let sync_url = format!("http://{addr}/models");

    // Sync from mock URL
    xfade(home.path())
        .args(["pricing", "sync", "--url", &sync_url])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "synced: 2 models loaded into local catalog",
        ));

    // Verify list displays synced model
    xfade(home.path())
        .args(["pricing", "list", "--filter", "special-model"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mock/special-model-2026"))
        .stdout(predicate::str::contains("special-model-2026"))
        .stdout(predicate::str::contains("synced"))
        .stdout(predicate::str::contains("$3.0000"))
        .stdout(predicate::str::contains("$15.0000"));
}
