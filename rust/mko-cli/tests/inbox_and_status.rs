use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use mko_core::{
    context::Scope,
    json_v1::JsonV1Success,
    profile::{MachineProfileFile, PersonalProfile, ProfileStore},
};
use serde_json::{Value, json};

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(deprecated)]
fn inbox_json_v1_uses_shared_production_limits_and_schema() {
    let env = Env::new();
    write_pdf(&env.provider.join("paper.pdf"), "Inbox paper");

    let output = env
        .command()
        .args(["inbox", "--format", "json-v1"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let typed: JsonV1Success = serde_json::from_slice(&output).unwrap();
    let value = serde_json::to_value(typed).unwrap();
    validate_schema(&value);

    assert_eq!(value["command"], "inbox");
    assert_eq!(value["data"]["scan_complete"], true);
    assert_eq!(
        value["data"]["scan_limits"],
        json!({
            "max_entries": 4096,
            "max_total_bytes": 1_073_741_824_u64,
            "max_elapsed_ms": 5_000,
            "max_depth": 32,
            "max_batch_items": 20,
        })
    );
    assert_eq!(value["data"]["items"][0]["provider_locator"], "paper.pdf");
    assert_eq!(value["data"]["items"][0]["user_state"], "new");
    assert_eq!(value["data"]["items"][0]["next_action"], "add");
}

#[test]
#[allow(deprecated)]
fn status_json_v1_reports_all_counts_and_next_action() {
    let env = Env::new();
    write_pdf(&env.provider.join("paper.pdf"), "Status paper");

    let value = env.json(["status", "--format", "json-v1"]);
    validate_schema(&value);
    assert_eq!(
        value,
        json!({
            "schema_version": 1,
            "command": "status",
            "result": "ok",
            "data": {
                "healthy": true,
                "counts": {
                    "new": 1,
                    "registered": 0,
                    "incomplete": 0,
                    "review_pending": 0,
                    "processed": 0,
                    "blocked": 0
                },
                "primary_blocker": null,
                "next_action": "add"
            }
        })
    );
}

#[test]
#[allow(deprecated)]
fn human_inbox_and_status_output_hide_internal_ids() {
    let env = Env::new();
    let outside = env.root.join("outside.pdf");
    write_pdf(&outside, "Registered paper");
    env.command()
        .arg("add")
        .arg(&outside)
        .args(["--format", "json-v1"])
        .assert()
        .success();

    for command in ["inbox", "status"] {
        let stdout = env
            .command()
            .arg(command)
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone();
        let stdout = String::from_utf8(stdout).unwrap();
        assert!(!stdout.contains("personal-asset-"), "{stdout}");
        assert!(!stdout.contains("personal-source-"), "{stdout}");
    }
}

#[test]
#[allow(deprecated)]
fn missing_configuration_matches_frozen_inbox_and_status_errors() {
    let env = EmptyEnv::new();
    for (command, golden) in [
        (
            "inbox",
            include_str!("../../../tests/fixtures/json-v1/inbox-error.json"),
        ),
        (
            "status",
            include_str!("../../../tests/fixtures/json-v1/status-error.json"),
        ),
    ] {
        let output = env
            .command()
            .args([command, "--format", "json-v1"])
            .assert()
            .code(1)
            .stderr("")
            .get_output()
            .stdout
            .clone();
        let actual: Value = serde_json::from_slice(&output).unwrap();
        let expected: Value = serde_json::from_str(golden).unwrap();
        assert_eq!(actual, expected);
    }
}

struct Env {
    root: PathBuf,
    provider: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new() -> Self {
        let unique = NEXT_ENV.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mko-inbox-status-cli-{}-{unique}",
            std::process::id()
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let home = root.join("home");
        let config_home = config_home(&home);
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        )
        .unwrap();
        ProfileStore::at(config_home.join("mko/profiles.yaml"))
            .write(&MachineProfileFile {
                schema_version: 1,
                default_profile: "personal".into(),
                profiles: BTreeMap::from([(
                    "personal".into(),
                    PersonalProfile {
                        repository_root: repository,
                        provider_root: provider.clone(),
                        scope: Scope::Personal,
                    },
                )]),
            })
            .unwrap();
        Self {
            root,
            provider,
            home,
        }
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .env("HOME", &self.home)
            .env("APPDATA", config_home(&self.home))
            .current_dir(&self.root);
        command
    }

    fn json<const N: usize>(&self, arguments: [&str; N]) -> Value {
        let output = self
            .command()
            .args(arguments)
            .assert()
            .success()
            .stderr("")
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    }
}

#[cfg(windows)]
fn config_home(home: &Path) -> PathBuf {
    home.join("AppData/Roaming")
}

#[cfg(not(windows))]
fn config_home(home: &Path) -> PathBuf {
    home.join("Library/Application Support")
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct EmptyEnv {
    root: PathBuf,
    home: PathBuf,
}

impl EmptyEnv {
    fn new() -> Self {
        let unique = NEXT_ENV.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mko-empty-cli-{}-{unique}", std::process::id()));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Self { root, home }
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command.env("HOME", &self.home).current_dir(&self.root);
        command
    }
}

impl Drop for EmptyEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn validate_schema(value: &Value) {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/machine-output-v1.schema.json"
    ))
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(value)
        .unwrap();
}

fn write_pdf(path: &Path, text: &str) {
    let mut document = Document::with_version("1.5");
    let pages = document.new_object_id();
    let font = document.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let resources = document.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tj", vec![Object::string_literal(text.as_bytes())]),
            Operation::new("ET", vec![]),
        ],
    }
    .encode()
    .unwrap();
    let contents = document.add_object(Stream::new(dictionary! {}, content));
    let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => contents, "Resources" => resources, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] });
    document.objects.insert(
        pages,
        dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }
            .into(),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.renumber_objects();
    document.save(path).unwrap();
}
