use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use serde_json::{Value, json};

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(deprecated)]
fn add_prepare_write_and_check_use_the_profile_and_emit_json_v1_only() {
    let env = JsonV1Env::new();
    let pdf = env.root.join("outside.pdf");
    write_pdf(&pdf, "Profile-backed PDF");

    let add = env
        .command(["add", &pdf.display().to_string(), "--format", "json-v1"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let add = one_json(&add);
    assert_eq!(add["schema_version"], 1);
    assert_eq!(add["command"], "add");
    assert_eq!(add["result"], "ok");
    assert_eq!(
        add["data"]["repository"],
        env.repository.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(add["data"]["import_outcome"], "copied");
    let asset_id = add["data"]["asset_id"].as_str().unwrap().to_owned();

    let bundle = env
        .repository
        .join(".knowledge-os/runtime/prepared")
        .join(format!("{asset_id}.json"));
    let prepared = env
        .command([
            "source",
            "prepare",
            "--asset-id",
            &asset_id,
            "--output",
            &bundle.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let prepared = one_json(&prepared);
    assert_eq!(
        prepared,
        json!({
            "schema_version": 1,
            "command": "source.prepare",
            "result": "ok",
            "data": {
                "asset_id": asset_id,
                "source_id": format!("personal-source-{}", add["data"]["asset_id"].as_str().unwrap().trim_start_matches("personal-asset-")),
                "bundle_path": bundle.canonicalize().unwrap().display().to_string(),
            }
        })
    );

    let response = env.root.join("semantic-response.json");
    fs::write(
        &response,
        include_bytes!("../../../tests/fixtures/semantic-response.json"),
    )
    .unwrap();
    let drafted = env
        .command([
            "source",
            "write-draft",
            "--bundle",
            prepared["data"]["bundle_path"].as_str().unwrap(),
            "--response",
            &response.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let drafted = one_json(&drafted);
    assert_eq!(drafted["schema_version"], 1);
    assert_eq!(drafted["command"], "source.write_draft");
    assert_eq!(drafted["result"], "ok");
    assert_eq!(drafted["data"]["draft_outcome"], "created");

    let checked = env
        .command(["check", "--format", "json-v1"])
        .assert()
        .code(1)
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let checked = one_json(&checked);
    assert_eq!(checked["schema_version"], 1);
    assert_eq!(checked["command"], "check");
    assert_eq!(checked["result"], "ok");
    assert_eq!(checked["data"]["valid"], false);
    assert!(
        checked["data"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "hook_missing")
    );
    assert_eq!(checked["data"]["warnings"], json!([]));
}

#[test]
#[allow(deprecated)]
fn json_v1_errors_are_single_stdout_objects_with_reviewed_recovery_only() {
    let env = JsonV1Env::new();
    let output = env
        .command(["add", "missing.pdf", "--format", "json-v1"])
        .assert()
        .code(1)
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let value = one_json(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "add");
    assert_eq!(value["result"], "error");
    assert_eq!(value["error"]["code"], "file_unreadable");
    assert!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(value["error"]["recovery"], Value::Null);
}

#[test]
#[allow(deprecated)]
fn json_v1_rejects_legacy_json_switch_without_mixing_stdout_or_stderr() {
    let env = JsonV1Env::new();
    let output = env
        .command(["add", "missing.pdf", "--json", "--format", "json-v1"])
        .assert()
        .code(2)
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let value = one_json(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "add");
    assert_eq!(value["result"], "error");
    assert_eq!(value["error"]["code"], "usage");
    assert_eq!(value["error"]["recovery"], Value::Null);
}

#[test]
#[allow(deprecated)]
fn legacy_detailed_commands_require_repo_without_touching_the_default_profile() {
    let env = JsonV1Env::new();
    let pdf = env.root.join("outside.pdf");
    write_pdf(&pdf, "legacy repo guard");
    let add = one_json(
        &env.command(["add", &pdf.display().to_string(), "--format", "json-v1"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let asset_id = add["data"]["asset_id"].as_str().unwrap();
    let bundle = env
        .repository
        .join(".knowledge-os/runtime/prepared")
        .join(format!("{asset_id}.json"));
    let response = env.root.join("response.json");
    fs::write(
        &response,
        include_bytes!("../../../tests/fixtures/semantic-response.json"),
    )
    .unwrap();

    for arguments in [
        vec![
            "source".into(),
            "prepare".into(),
            "--asset-id".into(),
            asset_id.into(),
            "--output".into(),
            bundle.display().to_string(),
        ],
        vec![
            "source".into(),
            "write-draft".into(),
            "--bundle".into(),
            bundle.display().to_string(),
            "--response".into(),
            response.display().to_string(),
            "--json".into(),
        ],
        vec!["check".into(), "--json".into()],
    ] {
        let output = env.command(arguments).assert().code(2).get_output().clone();
        assert!(output.stderr.is_empty() || output.stdout.is_empty());
        if !output.stdout.is_empty() {
            assert_eq!(one_json(&output.stdout)["error"]["code"], "usage");
        }
    }
    assert!(!env.repository.join("sources").exists());
}

#[test]
#[allow(deprecated)]
fn json_v1_prepare_rejects_an_external_suffix_lookalike_output() {
    let env = JsonV1Env::new();
    let pdf = env.root.join("outside.pdf");
    write_pdf(&pdf, "output containment");
    let add = one_json(
        &env.command(["add", &pdf.display().to_string(), "--format", "json-v1"])
            .assert()
            .success()
            .get_output()
            .stdout,
    );
    let asset_id = add["data"]["asset_id"].as_str().unwrap();
    let output = env
        .root
        .join("external/.knowledge-os/runtime/prepared")
        .join(format!("{asset_id}.json"));
    let result = env
        .command([
            "source",
            "prepare",
            "--asset-id",
            asset_id,
            "--output",
            &output.display().to_string(),
            "--format",
            "json-v1",
        ])
        .assert()
        .code(1)
        .stderr("")
        .get_output()
        .stdout
        .clone();
    assert_eq!(one_json(&result)["error"]["code"], "runtime_output_invalid");
    assert!(!output.exists());
}

#[test]
#[allow(deprecated)]
fn output_mode_never_treats_a_positional_json_v1_value_as_a_format_request() {
    let env = JsonV1Env::new();
    let human = env
        .command(["add", "json-v1", "--format", "human"])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert!(human.stdout.is_empty());
    assert!(!human.stderr.is_empty());

    env.command(["add", "json-v1", "--json"])
        .assert()
        .code(2)
        .stdout("");
    env.command(["add", "json-v1", "--json", "--format", "human"])
        .assert()
        .code(2)
        .stdout("");
}

#[test]
fn recovery_mapping_is_table_driven_and_unknown_codes_are_null() {
    use mko_cli::output::{RecoveryKind, recovery_for_error_code};

    let reviewed = [
        ("profile_missing", RecoveryKind::Configure),
        ("provider_hydration_failed", RecoveryKind::Hydrate),
        ("backup_confirmation_required", RecoveryKind::VerifyBackup),
        ("profile_permissions_invalid", RecoveryKind::FixPermissions),
        ("hook_conflict", RecoveryKind::ResolveHookConflict),
        ("extraction_timeout", RecoveryKind::Retry),
        ("provider_import_locked", RecoveryKind::Retry),
        ("registry_provider_mismatch", RecoveryKind::Repair),
    ];
    for (code, recovery) in reviewed {
        assert_eq!(recovery_for_error_code(code), Some(recovery), "{code}");
    }
    for unknown in [
        "provider_import_lock",
        "profile_missing_extra",
        "unreviewed_new_code",
    ] {
        assert_eq!(recovery_for_error_code(unknown), None, "{unknown}");
    }
}

struct JsonV1Env {
    root: PathBuf,
    repository: PathBuf,
    home: PathBuf,
}

impl JsonV1Env {
    fn new() -> Self {
        let unique = NEXT_ENV.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("mko-json-v1-cli-{}-{unique}", std::process::id()));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let home = root.join("home");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::create_dir_all(home.join("Library/Application Support/mko")).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), knowledge_config()).unwrap();
        fs::write(
            home.join("Library/Application Support/mko/profiles.yaml"),
            format!(
                "schema_version: 1\ndefault_profile: personal\nprofiles:\n  personal:\n    repository_root: {}\n    provider_root: {}\n    scope: personal\n",
                repository.display(), provider.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                home.join("Library/Application Support/mko"),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            fs::set_permissions(
                home.join("Library/Application Support/mko/profiles.yaml"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        Self {
            root,
            repository,
            home,
        }
    }

    #[allow(deprecated)]
    fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::cargo_bin("mko").unwrap();
        command
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.root);
        command
    }
}

impl Drop for JsonV1Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn knowledge_config() -> &'static str {
    "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n"
}

fn one_json(stdout: &[u8]) -> Value {
    assert_eq!(
        stdout.iter().filter(|&&byte| byte == b'\n').count(),
        1,
        "stdout must contain one JSON object"
    );
    serde_json::from_slice(stdout).unwrap()
}

fn write_pdf(path: &std::path::Path, text: &str) {
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
