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
