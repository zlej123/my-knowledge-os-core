use std::fs;

use assert_cmd::Command;
use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    dashboard_v2::ensure_dashboard_v2,
    records_v2::{AssetRecordV2, WriteSourceRecordRequestV2, write_source_record_v2},
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
#[allow(deprecated)]
fn dashboard_json_v2_is_typed_read_only_and_repairs_only_safe_drift() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let missing = root.path().join("views/review-queue.base");
    fs::remove_file(&missing).unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["command"], "dashboard");
    assert_eq!(value["data"]["projection_state"], "repair_required");
    assert_eq!(value["data"]["manifest_owned_drift"], true);
    let item = value["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "views/review-queue.base")
        .unwrap();
    assert_eq!(item["state"], "missing");
    assert_eq!(item["next_action"], "repair");
    assert!(
        !missing.exists(),
        "JSON inspection must not mutate the view"
    );

    let repaired = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--repair", "--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let repaired: serde_json::Value = serde_json::from_slice(&repaired).unwrap();
    assert_eq!(repaired["data"]["projection_state"], "current");
    assert!(missing.is_file());
}

#[test]
#[allow(deprecated)]
fn dashboard_repair_refuses_user_modified_owned_file_with_typed_guidance() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let home = root.path().join("HOME.md");
    let user_edit = b"# keep my dashboard\n";
    fs::write(&home, user_edit).unwrap();

    let inspected = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspected: serde_json::Value = serde_json::from_slice(&inspected).unwrap();
    let home_item = inspected["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == "HOME.md")
        .unwrap();
    assert_eq!(home_item["state"], "user_modified");
    assert_eq!(home_item["next_action"], "preserve_user_edit");

    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--repair", "--format", "json-v2"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let failure: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(failure["command"], "dashboard");
    assert_eq!(failure["error"]["code"], "dashboard_user_modified");
    assert_eq!(failure["error"]["next_action"], "preserve_user_edit");
    assert_eq!(fs::read(home).unwrap(), user_edit);
}

#[test]
#[allow(deprecated)]
fn record_projection_repair_next_action_runs_the_command_that_resolves_it() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let record_id = write_source(root.path());
    let projection_path = root
        .path()
        .join(format!("views/records/source-{record_id}.md"));
    fs::remove_file(&projection_path).unwrap();

    let inspected = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspected: serde_json::Value = serde_json::from_slice(&inspected).unwrap();
    let item = inspected["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == format!("views/records/source-{record_id}.md"))
        .unwrap();
    assert_eq!(item["state"], "missing");
    assert_eq!(item["next_action"], "repair");
    assert_eq!(inspected["data"]["next_action"], "repair");

    let repaired = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--repair", "--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let repaired: serde_json::Value = serde_json::from_slice(&repaired).unwrap();
    let item = repaired["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == format!("views/records/source-{record_id}.md"))
        .unwrap();
    assert_eq!(item["state"], "current");
    assert_eq!(item["next_action"], "none");
    assert_eq!(repaired["data"]["next_action"], "none");
    assert!(projection_path.is_file());
}

#[test]
#[allow(deprecated)]
fn user_modified_record_projection_has_non_destructive_typed_guidance() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let record_id = write_source(root.path());
    let projection_path = root
        .path()
        .join(format!("views/records/source-{record_id}.md"));
    let user_edit = b"# keep this projection edit\n";
    fs::write(&projection_path, user_edit).unwrap();

    let inspected = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--format", "json-v2"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspected: serde_json::Value = serde_json::from_slice(&inspected).unwrap();
    let item = inspected["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == format!("views/records/source-{record_id}.md"))
        .unwrap();
    assert_eq!(item["state"], "user_modified");
    assert_eq!(item["next_action"], "preserve_user_edit");
    assert_eq!(inspected["data"]["next_action"], "preserve_user_edit");

    let failure = Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(root.path())
        .args(["--repair", "--format", "json-v2"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let failure: serde_json::Value = serde_json::from_slice(&failure).unwrap();
    assert_eq!(
        failure["error"]["code"],
        "dashboard_projection_user_modified"
    );
    assert_eq!(failure["error"]["next_action"], "preserve_user_edit");
    assert_eq!(fs::read(projection_path).unwrap(), user_edit);
}

fn write_source(root: &std::path::Path) -> String {
    let asset: AssetRecordV2 =
        serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
            .unwrap();
    fs::write(
        root.join("assets/registry")
            .join(format!("{}.json", asset.id)),
        canonical_json_bytes(&asset).unwrap(),
    )
    .unwrap();
    let mut bundle: mko_core::model_v2::PreparedContentV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/prepared-content.json"
    ))
    .unwrap();
    let mut value = serde_json::to_value(&bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
    let response: mko_core::model_v2::SourceResponseV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-response.json"
    ))
    .unwrap();
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root,
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision: None,
        },
        &FixedClock("2026-07-23T00:00:00Z".parse().unwrap()),
    )
    .unwrap()
    .record_id
}
