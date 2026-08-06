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

#[test]
#[allow(deprecated)]
fn add_inbox_returns_partial_success_without_hiding_blocked_items() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(provider.join("papers")).unwrap();
    fs::write(provider.join("papers/a.pdf"), b"%PDF-1.7\nfirst").unwrap();
    fs::write(provider.join("papers/b.pdf"), b"not a pdf").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--inbox", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let output: serde_json::Value = serde_json::from_slice(&output).unwrap();

    assert_eq!(output["command"], "add");
    assert_eq!(output["data"]["scan_complete"], true);
    assert_eq!(output["data"]["remaining"], 0);
    assert_eq!(output["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        output["data"]["items"][0]["logical_locator"],
        "papers/a.pdf"
    );
    assert_eq!(output["data"]["items"][0]["outcome"], "created");
    assert!(output["data"]["items"][0]["error"].is_null());
    assert_eq!(
        output["data"]["items"][1]["logical_locator"],
        "papers/b.pdf"
    );
    assert!(output["data"]["items"][1]["asset_id"].is_null());
    assert_eq!(output["data"]["items"][1]["error"]["code"], "invalid_pdf");

    let second = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--inbox", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second: serde_json::Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["items"][0]["outcome"], "existing");
    assert_eq!(
        second["data"]["items"][0]["asset_id"],
        output["data"]["items"][0]["asset_id"]
    );
}

// The agent fetches and hands Core the text; Core does the deterministic part.
// The text arrives in a file because a page body does not belong on a command
// line — it would reach process listings and shell history.
#[test]
#[allow(deprecated)]
fn a_page_the_agent_read_becomes_registered_evidence() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("page.txt");
    fs::write(&text, "The page said this.").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args([
            "--url",
            "https://example.com/page",
            "--title",
            "Example page",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["command"], "add");
    assert_eq!(report["data"]["outcome"], "created");
    assert_eq!(
        report["data"]["logical_locator"],
        "https://example.com/page"
    );
    assert!(
        report["data"]["asset_id"]
            .as_str()
            .unwrap()
            .starts_with("personal-asset-")
    );
}

// A page that rendered nothing readable must say so. Registering an Asset
// holding whitespace would put material in the waiting list that no session
// could ever draft.
#[test]
#[allow(deprecated)]
fn a_page_with_no_readable_text_reports_why_instead_of_registering() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("empty.txt");
    fs::write(&text, "   \n\t ").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args([
            "--url",
            "https://example.com/js-only",
            "--title",
            "JavaScript page",
            "--format",
            "json-v2",
        ])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(report["error"]["code"], "snapshot_text_empty");
    assert_eq!(report["error"]["retryable"], false);
    assert_eq!(
        repository
            .join("assets/registry")
            .read_dir()
            .unwrap()
            .count(),
        0,
        "nothing may be registered for a page that produced no text"
    );
}

// --snapshot without its address would produce evidence nobody can trace.
#[test]
#[allow(deprecated)]
fn a_snapshot_without_an_address_is_refused() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    let provider = root.path().join("Personal Inbox");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir_all(&provider).unwrap();
    let text = root.path().join("page.txt");
    fs::write(&text, "The page said this.").unwrap();

    Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--snapshot"])
        .arg(&text)
        .args(["--title", "Example page", "--format", "json-v2"])
        .env("MKO_PERSONAL_PROVIDER_ROOT", &provider)
        .current_dir(&repository)
        .assert()
        .code(1)
        .stdout(predicates::str::contains("snapshot_arguments_incomplete"));
}
