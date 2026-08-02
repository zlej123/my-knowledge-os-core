use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

// The schema surface is context-free like the handshake: it must serve the
// embedded contracts from an empty directory with no profile and no
// repository, because a fresh install consults it before `mko setup`.
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

fn repository_file(relative: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    serde_json::from_slice(
        &std::fs::read(&path).unwrap_or_else(|error| {
            panic!("{} must exist and be readable: {error}", path.display())
        }),
    )
    .unwrap()
}

#[test]
fn schema_list_names_every_embedded_contract() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["schema", "list", "--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["schema_version"], 2);
    assert_eq!(envelope["command"], "schema.list");
    assert_eq!(envelope["result"], "ok");
    let names = envelope["data"]["schemas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|schema| schema["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "source-response-v2",
            "knowledge-response-v2",
            "review-feedback-input-v2",
        ]
    );
}

#[test]
fn schema_show_serves_the_exact_repository_contract_and_example() {
    for (name, schema_path, example_path) in [
        (
            "source-response-v2",
            "schemas/v2/source-response.schema.json",
            "tests/fixtures/json-v2/source-response.json",
        ),
        (
            "knowledge-response-v2",
            "schemas/v2/knowledge-response.schema.json",
            "tests/fixtures/json-v2/knowledge-response.json",
        ),
        (
            "review-feedback-input-v2",
            "schemas/v2/review-feedback-input.schema.json",
            "tests/fixtures/json-v2/review-feedback-input.json",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let output = command(&home)
            .args(["schema", "show", name, "--format", "json-v2"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();

        let envelope: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(envelope["schema_version"], 2);
        assert_eq!(envelope["command"], "schema.show");
        assert_eq!(envelope["result"], "ok");
        assert_eq!(envelope["data"]["name"], name);
        assert_eq!(envelope["data"]["schema"], repository_file(schema_path));
        assert_eq!(envelope["data"]["example"], repository_file(example_path));
    }
}

#[test]
fn unknown_schema_name_blocks_with_the_reinstall_failure() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args([
            "schema",
            "show",
            "source-response-v9",
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
    assert_eq!(envelope["command"], "schema.show");
    assert_eq!(envelope["result"], "error");
    assert_eq!(envelope["error"]["code"], "schema_not_found");
    assert_eq!(envelope["error"]["retryable"], false);
    assert_eq!(envelope["error"]["next_action"], "reinstall");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("source-response-v2"));
    assert!(message.contains("review-feedback-input-v2"));
}

#[test]
fn schema_commands_support_human_output_and_reject_json_v1() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["schema", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("review-feedback-input-v2")
    );

    let home = tempfile::tempdir().unwrap();
    command(&home)
        .args(["schema", "show", "source-response-v2"])
        .assert()
        .success();

    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["schema", "list", "--format", "json-v1"])
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
fn schema_usage_errors_still_answer_in_the_machine_envelope() {
    let home = tempfile::tempdir().unwrap();
    let output = command(&home)
        .args(["schema", "show", "--format", "json-v2"])
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let envelope: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(envelope["command"], "schema.show");
    assert_eq!(envelope["result"], "error");
    assert_eq!(envelope["error"]["code"], "usage");
}
