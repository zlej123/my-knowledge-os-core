use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

// The handshake must work before any setup: no profile, no repository, no
// provider. Every command below runs from an empty directory with an isolated
// HOME so a developer machine's real profile can never leak in.
#[allow(deprecated)] // Required by the assert_cmd CLI harness convention.
fn command(home: &TempDir) -> Command {
    let mut command = Command::cargo_bin("mko").unwrap();
    command.env("HOME", home.path()).current_dir(home.path());
    #[cfg(target_os = "linux")]
    command.env("XDG_CONFIG_HOME", home.path().join("config"));
    #[cfg(windows)]
    command
        .env("APPDATA", home.path().join("config"))
        .env("USERPROFILE", home.path());
    command
}

#[test]
fn matching_skill_version_returns_the_handshake_envelope() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args([
            "handshake",
            "--skill-version",
            mko_core::version::PRODUCT_VERSION,
            "--format",
            "json-v2",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["schema_version"], 2);
    assert_eq!(envelope["command"], "handshake");
    assert_eq!(envelope["result"], "ok");
    assert_eq!(
        envelope["data"]["cli_version"],
        mko_core::version::PRODUCT_VERSION
    );
    assert_eq!(
        envelope["data"]["skill_version"],
        mko_core::version::PRODUCT_VERSION
    );
}

#[test]
fn mismatched_skill_version_blocks_with_a_typed_reinstall_failure() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args([
            "handshake",
            "--skill-version",
            "0.0.0",
            "--format",
            "json-v2",
        ])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["schema_version"], 2);
    assert_eq!(envelope["command"], "handshake");
    assert_eq!(envelope["result"], "error");
    assert_eq!(envelope["error"]["code"], "skill_version_mismatch");
    assert_eq!(envelope["error"]["retryable"], false);
    assert_eq!(envelope["error"]["next_action"], "reinstall");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("0.0.0"));
    assert!(message.contains(mko_core::version::PRODUCT_VERSION));
    assert!(message.contains("reinstall"));
}

#[test]
fn handshake_usage_errors_still_answer_in_the_machine_envelope() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["handshake", "--format", "json-v2"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["command"], "handshake");
    assert_eq!(envelope["result"], "error");
    assert_eq!(envelope["error"]["code"], "usage");
}

#[test]
fn handshake_supports_human_output_and_rejects_json_v1() {
    let home = tempfile::tempdir().unwrap();
    command(&home)
        .args([
            "handshake",
            "--skill-version",
            mko_core::version::PRODUCT_VERSION,
        ])
        .assert()
        .success();

    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args([
            "handshake",
            "--skill-version",
            mko_core::version::PRODUCT_VERSION,
            "--format",
            "json-v1",
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("format_unsupported")
    );
}

#[test]
fn mismatch_in_human_format_fails_with_the_reinstall_instruction() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["handshake", "--skill-version", "0.0.0"])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stderr
        .clone();

    let stderr = String::from_utf8(output).unwrap();
    assert!(stderr.contains("skill_version_mismatch"));
    assert!(stderr.contains("reinstall"));
}
