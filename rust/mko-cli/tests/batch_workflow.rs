use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use mko_cli::batch_add_data;
use mko_core::{
    add::{BatchAddResult, BatchItemResult},
    json_v1::{
        AddOutcome, AddPayload, JsonV1Error, JsonV1Success, NextAction, Recovery, RecoveryKind,
        SuccessResult, UserState,
    },
};
use serde_json::Value;

static NEXT_ENV: AtomicU64 = AtomicU64::new(0);

#[test]
fn frozen_mixed_batch_contract_round_trips_with_exact_item_order() {
    let expected = include_str!("../../../tests/fixtures/json-v1/add-inbox-mixed.json");
    let actual = JsonV1Success::Add {
        schema_version: 1,
        result: SuccessResult::Ok,
        data: AddPayload::Batch(batch_add_data(BatchAddResult {
            scan_complete: false,
            items: vec![
                BatchItemResult {
                    provider_locator: "inbox/new-paper.pdf".into(),
                    user_state: UserState::Registered,
                    next_action: NextAction::Prepare,
                    asset_id: Some("asset-002".into()),
                    add_outcome: Some(AddOutcome::Created),
                    error: None,
                },
                BatchItemResult {
                    provider_locator: "inbox/known-paper.pdf".into(),
                    user_state: UserState::ReviewPending,
                    next_action: NextAction::Review,
                    asset_id: Some("asset-003".into()),
                    add_outcome: Some(AddOutcome::Existing),
                    error: None,
                },
                BatchItemResult {
                    provider_locator: "inbox/broken.pdf".into(),
                    user_state: UserState::Blocked,
                    next_action: NextAction::Repair,
                    asset_id: None,
                    add_outcome: None,
                    error: Some(JsonV1Error {
                        code: "invalid_pdf".into(),
                        message: "The PDF could not be validated.".into(),
                        recovery: Some(Recovery {
                            kind: RecoveryKind::Repair,
                        }),
                    }),
                },
            ],
            remaining: 4,
        })),
    };
    let actual = serde_json::to_value(actual).unwrap();
    let expected: Value = serde_json::from_str(expected).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(
        actual["data"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["provider_locator"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "inbox/new-paper.pdf",
            "inbox/known-paper.pdf",
            "inbox/broken.pdf"
        ]
    );
}

#[test]
#[allow(deprecated)]
fn add_inbox_emits_the_strict_batch_shape_and_persists_successes() {
    let env = Env::new();
    fs::write(env.provider.join("new-paper.pdf"), b"%PDF-1.7\nnew").unwrap();
    fs::write(env.provider.join("broken.pdf"), b"not a PDF").unwrap();

    let output = env
        .command()
        .args(["add", "--inbox", "--verified-backup", "--format", "json-v1"])
        .assert()
        .success()
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let typed: JsonV1Success = serde_json::from_slice(&output).unwrap();
    let JsonV1Success::Add {
        data: AddPayload::Batch(batch),
        ..
    } = typed
    else {
        panic!("expected strict batch add envelope")
    };
    assert!(batch.items.iter().any(|item| item.add_outcome.is_some()));
    assert!(batch.items.iter().any(|item| item.error.is_some()));

    let value: Value = serde_json::from_slice(&output).unwrap();
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/machine-output-v1.schema.json"
    ))
    .unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
}

#[test]
#[allow(deprecated)]
fn add_inbox_configuration_failure_matches_the_frozen_error() {
    let root = std::env::temp_dir().join(format!(
        "mko-batch-empty-{}-{}",
        std::process::id(),
        NEXT_ENV.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["add", "--inbox", "--format", "json-v1"])
        .env("HOME", root.join("home"))
        .current_dir(&root)
        .assert()
        .code(1)
        .stderr("")
        .get_output()
        .stdout
        .clone();
    let actual: Value = serde_json::from_slice(&output).unwrap();
    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/json-v1/add-inbox-error.json"
    ))
    .unwrap();
    assert_eq!(actual, expected);
    let _ = fs::remove_dir_all(root);
}

struct Env {
    root: PathBuf,
    provider: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mko-batch-cli-{}-{}",
            std::process::id(),
            NEXT_ENV.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = root.join("repository");
        let provider = root.join("provider");
        let home = root.join("home");
        let config_home = home.join("Library/Application Support/mko");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::create_dir_all(&config_home).unwrap();
        fs::write(
            repository.join("knowledge-os.yaml"),
            "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n",
        ).unwrap();
        fs::write(
            config_home.join("profiles.yaml"),
            format!("schema_version: 1\ndefault_profile: personal\nprofiles:\n  personal:\n    repository_root: {}\n    provider_root: {}\n    scope: personal\n", repository.display(), provider.display()),
        ).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config_home, fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(
                config_home.join("profiles.yaml"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        Self {
            root,
            provider,
            home,
        }
    }

    #[allow(deprecated)]
    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("mko").unwrap();
        command.env("HOME", &self.home).current_dir(&self.root);
        command
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
