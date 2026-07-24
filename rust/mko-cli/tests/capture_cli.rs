use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::tempdir;

fn capture_fixture(name: &str) -> &'static str {
    match name {
        "general" => include_str!("../../../tests/fixtures/capture/v1/general-explicit.json"),
        "finance" => include_str!("../../../tests/fixtures/capture/v1/finance-explicit.json"),
        "invalid" => include_str!("../../../tests/fixtures/capture/v1/invalid-credential.json"),
        _ => panic!("unknown fixture"),
    }
}

#[allow(deprecated)]
fn command_json(arguments: &[&str]) -> Value {
    let stdout = Command::cargo_bin("mko")
        .unwrap()
        .args(arguments)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output = serde_json::from_slice(&stdout).unwrap();
    assert_machine_output(&output);
    output
}

fn assert_machine_output(output: &Value) {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    if let Err(error) = validator.validate(output) {
        panic!("Capture CLI output failed machine-output v2 validation: {error}");
    }
}

#[test]
#[allow(deprecated)]
fn capture_validate_reports_only_typed_envelope_fields() {
    let root = tempdir().unwrap();
    let input = root.path().join("capture.json");
    fs::write(&input, capture_fixture("general")).unwrap();

    let output = command_json(&[
        "capture",
        "validate",
        "--input",
        input.to_str().unwrap(),
        "--format",
        "json-v2",
    ]);

    assert_eq!(output["schema_version"], 2);
    assert_eq!(output["command"], "capture.validate");
    assert_eq!(output["result"], "ok");
    assert_eq!(output["data"]["capture_id"], "cap_general_lecture_001");
    assert_eq!(output["data"]["channel"], "telegram");
    assert_eq!(output["data"]["input_type"], "youtube");
    assert_eq!(output["data"]["selected_scope"], "general");
}

#[test]
#[allow(deprecated)]
fn capture_route_uses_selected_scope_without_a_classifier_proposal() {
    let root = tempdir().unwrap();
    let input = root.path().join("capture.json");
    fs::write(&input, capture_fixture("finance")).unwrap();

    let output = command_json(&[
        "capture",
        "route",
        "--input",
        input.to_str().unwrap(),
        "--format",
        "json-v2",
    ]);

    assert_eq!(output["command"], "capture.route");
    assert_eq!(output["data"]["outcome"], "ready_finance");
    assert_eq!(output["data"]["confirmed_scope"], "finance");
    assert_eq!(output["data"]["routing_authority"], "user_selected");
    assert_eq!(output["data"]["proposal"], Value::Null);
    assert_eq!(output["data"]["next_action"], "none");
}

#[test]
#[allow(deprecated)]
fn capture_route_requires_confirmation_for_an_unselected_classifier_proposal() {
    let root = tempdir().unwrap();
    let input = root.path().join("capture.json");
    let proposal = root.path().join("proposal.json");
    let mut envelope: Value = serde_json::from_str(capture_fixture("general")).unwrap();
    envelope.as_object_mut().unwrap().remove("selected_scope");
    fs::write(&input, serde_json::to_vec(&envelope).unwrap()).unwrap();
    fs::write(
        &proposal,
        r#"{"proposed_scope":"general","confidence":95,"mixed_subjects":false,"conflicting":false}"#,
    )
    .unwrap();

    let output = command_json(&[
        "capture",
        "route",
        "--input",
        input.to_str().unwrap(),
        "--proposal",
        proposal.to_str().unwrap(),
        "--format",
        "json-v2",
    ]);

    assert_eq!(output["data"]["outcome"], "general_confirmation_required");
    assert_eq!(output["data"]["confirmed_scope"], Value::Null);
    assert_eq!(output["data"]["next_action"], "confirm_routing");
    assert_eq!(output["data"]["proposal"]["confidence"], 95);
}

#[test]
#[allow(deprecated)]
fn capture_route_accepts_a_matching_explicit_confirmation_without_mutating_domain_state() {
    let root = tempdir().unwrap();
    let input = root.path().join("capture.json");
    let proposal = root.path().join("proposal.json");
    let mut envelope: Value = serde_json::from_str(capture_fixture("general")).unwrap();
    envelope.as_object_mut().unwrap().remove("selected_scope");
    fs::write(&input, serde_json::to_vec(&envelope).unwrap()).unwrap();
    fs::write(
        &proposal,
        r#"{"proposed_scope":"finance","confidence":95,"mixed_subjects":false,"conflicting":false}"#,
    )
    .unwrap();

    let output = command_json(&[
        "capture",
        "route",
        "--input",
        input.to_str().unwrap(),
        "--proposal",
        proposal.to_str().unwrap(),
        "--confirm",
        "finance",
        "--format",
        "json-v2",
    ]);

    assert_eq!(output["data"]["outcome"], "ready_finance");
    assert_eq!(output["data"]["confirmed_scope"], "finance");
    assert_eq!(
        output["data"]["routing_authority"],
        "user_confirmed_proposal"
    );
    assert_eq!(
        fs::read_dir(root.path()).unwrap().count(),
        2,
        "route validation must not create Asset, Knowledge, Delivery, or Project 2035 state"
    );
}

#[test]
#[allow(deprecated)]
fn capture_validate_reports_invalid_input_through_json_v2() {
    let root = tempdir().unwrap();
    let input = root.path().join("capture.json");
    fs::write(&input, capture_fixture("invalid")).unwrap();

    let stdout = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "capture",
            "validate",
            "--input",
            input.to_str().unwrap(),
            "--format",
            "json-v2",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let output: Value = serde_json::from_slice(&stdout).unwrap();
    assert_machine_output(&output);
    assert_eq!(output["command"], "capture.validate");
    assert_eq!(output["result"], "error");
    assert_eq!(output["error"]["code"], "capture_envelope_invalid");
    assert_eq!(output["error"]["next_action"], "none");
}

#[cfg(unix)]
#[test]
#[allow(deprecated)]
fn capture_validate_rejects_a_symlink_input() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    let target = root.path().join("capture.json");
    let link = root.path().join("capture-link.json");
    fs::write(&target, capture_fixture("general")).unwrap();
    symlink(&target, &link).unwrap();

    let stdout = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "capture",
            "validate",
            "--input",
            link.to_str().unwrap(),
            "--format",
            "json-v2",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let output: Value = serde_json::from_slice(&stdout).unwrap();
    assert_machine_output(&output);
    assert_eq!(output["error"]["code"], "capture_input_unreadable");
}
