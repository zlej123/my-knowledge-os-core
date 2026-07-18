use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{Value, json};

static NEXT_TEST_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn help_exposes_v01_command_groups() {
    Command::cargo_bin("mko")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("asset"))
        .stdout(predicate::str::contains("source"))
        .stdout(predicate::str::contains("check"))
        .stdout(predicate::str::contains("human"))
        .stdout(predicate::str::contains("hooks"));
}

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn asset_capture_emits_complete_json_for_created_and_existing_results() {
    let env = CliTestEnv::new();
    let pdf = env.provider.join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\nfixture").unwrap();

    let created = env.capture_json(&pdf);
    let existing = env.capture_json(&pdf);

    let id = created["asset_id"].as_str().unwrap();
    assert_eq!(
        created,
        json!({
            "result": "created",
            "asset_id": id,
            "registry_path": format!("assets/registry/{id}.md"),
        })
    );
    assert_eq!(
        existing,
        json!({
            "result": "existing",
            "asset_id": id,
            "registry_path": format!("assets/registry/{id}.md"),
        })
    );
}

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn asset_capture_emits_complete_json_for_runtime_errors() {
    let env = CliTestEnv::new();
    let pdf = env.provider.join("not-a-pdf.pdf");
    fs::write(&pdf, b"not a PDF").unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(env.capture_arguments(&pdf))
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        parse_json(&output),
        json!({
            "result": "error",
            "error": {
                "code": "invalid_pdf",
                "message": "file does not have a PDF signature",
            }
        })
    );
}

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn asset_capture_emits_complete_json_for_usage_errors() {
    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["asset", "capture", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let value = parse_json(&output);
    let message = value["error"]["message"]
        .as_str()
        .filter(|message| !message.is_empty())
        .unwrap();
    assert_eq!(
        value,
        json!({
            "result": "error",
            "error": {
                "code": "usage",
                "message": message,
            }
        })
    );
}

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn asset_lifecycle_commands_report_a_changed_asset_and_its_successor() {
    let env = CliTestEnv::new();
    let pdf = env.provider.join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\noriginal").unwrap();
    let captured = env.capture_json(&pdf);
    let old_asset_id = captured["asset_id"].as_str().unwrap();
    fs::write(&pdf, b"%PDF-1.7\nreplacement").unwrap();

    let inspected = Command::cargo_bin("mko")
        .unwrap()
        .args(env.lifecycle_arguments("inspect", old_asset_id))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        parse_json(&inspected),
        json!({
            "result": "changed",
            "asset_id": old_asset_id,
        })
    );

    let accepted = Command::cargo_bin("mko")
        .unwrap()
        .args(env.lifecycle_arguments("accept-change", old_asset_id))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let accepted = parse_json(&accepted);
    assert_eq!(accepted["result"], "accepted");
    assert_eq!(accepted["supersedes"], old_asset_id);
    assert!(
        accepted["asset_id"]
            .as_str()
            .is_some_and(|asset_id| asset_id != old_asset_id)
    );
}

#[test]
#[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
fn check_reports_interrupted_asset_lineage_that_needs_repair() {
    let env = CliTestEnv::new();
    let pdf = env.provider.join("paper.pdf");
    fs::write(&pdf, b"%PDF-1.7\noriginal").unwrap();
    let captured = env.capture_json(&pdf);
    let old_asset_id = captured["asset_id"].as_str().unwrap();
    fs::write(&pdf, b"%PDF-1.7\nreplacement").unwrap();
    Command::cargo_bin("mko")
        .unwrap()
        .args(env.lifecycle_arguments("inspect", old_asset_id))
        .assert()
        .success();
    let publication_lock = env
        .repository
        .join("assets/registry")
        .join(format!(".{old_asset_id}.md.publish.lock"));
    fs::write(&publication_lock, b"interrupt old record update").unwrap();
    Command::cargo_bin("mko")
        .unwrap()
        .args(env.lifecycle_arguments("accept-change", old_asset_id))
        .assert()
        .failure();
    fs::remove_file(publication_lock).unwrap();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(env.check_arguments())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(
        parse_json(&output),
        json!({
            "result": "repair_needed",
            "asset_ids": [old_asset_id],
        })
    );
}

struct CliTestEnv {
    root: PathBuf,
    repository: PathBuf,
    provider: PathBuf,
    local_config: PathBuf,
}

impl CliTestEnv {
    fn new() -> Self {
        let unique = NEXT_TEST_ENV.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mko-cli-test-{}-{unique}", std::process::id()));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let local_config = root.join("local-config.yaml");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        fs::write(
            &local_config,
            format!("provider_root: {}\n", provider.display()),
        )
        .unwrap();
        Self {
            root,
            repository,
            provider,
            local_config,
        }
    }

    #[allow(deprecated)] // Required by the v0.1 assert_cmd CLI contract.
    fn capture_json(&self, pdf: &std::path::Path) -> Value {
        let output = Command::cargo_bin("mko")
            .unwrap()
            .args(self.capture_arguments(pdf))
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        parse_json(&output)
    }

    fn capture_arguments(&self, pdf: &std::path::Path) -> Vec<String> {
        vec![
            "asset".into(),
            "capture".into(),
            "--repo".into(),
            self.repository.display().to_string(),
            "--local-config".into(),
            self.local_config.display().to_string(),
            "--file".into(),
            pdf.display().to_string(),
            "--json".into(),
        ]
    }

    fn lifecycle_arguments(&self, command: &str, asset_id: &str) -> Vec<String> {
        vec![
            "asset".into(),
            command.into(),
            "--repo".into(),
            self.repository.display().to_string(),
            "--local-config".into(),
            self.local_config.display().to_string(),
            "--asset-id".into(),
            asset_id.into(),
            "--json".into(),
        ]
    }

    fn check_arguments(&self) -> Vec<String> {
        vec![
            "check".into(),
            "--repo".into(),
            self.repository.display().to_string(),
            "--json".into(),
        ]
    }
}

impl Drop for CliTestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn parse_json(output: &[u8]) -> Value {
    serde_json::from_slice(output).unwrap()
}
