use std::fs;

use assert_cmd::Command;
use mko_core::scaffold_v2::scaffold_personal_kb_v2;
use tempfile::tempdir;

#[test]
#[allow(deprecated)]
fn add_registers_an_inbox_pdf_and_reuses_its_content_identity() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(provider.join("papers")).unwrap();
    let pdf = provider.join("papers/example.pdf");
    fs::write(&pdf, b"%PDF-1.7\nfixture").unwrap();

    let first = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg(&pdf)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first: serde_json::Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["command"], "add");
    assert_eq!(first["data"]["outcome"], "created");
    assert_eq!(first["data"]["logical_locator"], "papers/example.pdf");
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .count()
            == 1
    );

    let second = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg("papers/example.pdf")
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["outcome"], "existing");
    assert_eq!(second["data"]["asset_id"], first["data"]["asset_id"]);
}

#[test]
#[allow(deprecated)]
fn add_rejects_an_outside_pdf_with_a_typed_recovery() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let outside = root.path().join("outside.pdf");
    fs::write(&outside, b"%PDF-1.7\noutside").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .arg("add")
        .arg(&outside)
        .args(["--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(output["command"], "add");
    assert_eq!(output["error"]["code"], "asset_outside_inbox");
    assert_eq!(output["error"]["next_action"], "add");
    assert!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}
