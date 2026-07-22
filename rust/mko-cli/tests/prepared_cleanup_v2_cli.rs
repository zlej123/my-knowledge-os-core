#[path = "../../mko-core/tests/support/pdf_fixture.rs"]
mod pdf_fixture;

use std::{fs, path::Path};

use assert_cmd::Command;
use chrono::{DateTime, Duration, Utc};
use mko_core::{
    asset_v2::{HydrationConfirmationV2, RegisterAssetRequestV2, register_pdf_asset_v2},
    clock::Clock,
    model_v2::PreparedMetadataV2,
    prepared_v2::{PreparePdfAssetRequestV2, prepare_pdf_asset_v2_with_extractor_and_clock},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::TempDir;

use pdf_fixture::write_pdf;

struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

struct PreparedFixture {
    _root: TempDir,
    repository: std::path::PathBuf,
    provider: std::path::PathBuf,
    bundle_path: std::path::PathBuf,
    bundle_id: String,
}

fn prepared_fixture(created_at: DateTime<Utc>) -> PreparedFixture {
    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("repository");
    let provider = root.path().join("provider");
    scaffold_personal_kb_v2(&repository).unwrap();
    fs::create_dir(&provider).unwrap();
    write_pdf(&provider.join("paper.pdf"), &["CLI cleanup fixture".into()]);
    let asset = register_pdf_asset_v2(RegisterAssetRequestV2 {
        repository_root: &repository,
        provider_root: &provider,
        logical_locator: "paper.pdf",
        hydration_confirmation: HydrationConfirmationV2::Confirmed,
    })
    .unwrap()
    .asset;
    let prepared = prepare_pdf_asset_v2_with_extractor_and_clock(
        PreparePdfAssetRequestV2 {
            repository_root: &repository,
            provider_root: &provider,
            asset_id: &asset.id,
            metadata: PreparedMetadataV2 {
                title: Some("CLI cleanup fixture".into()),
                authors: Vec::new(),
                created_at: None,
            },
            hydration_confirmation: HydrationConfirmationV2::Confirmed,
        },
        &FixedClock(created_at),
        |_, _| Ok(vec!["Prepared plaintext".into()]),
    )
    .unwrap();
    PreparedFixture {
        _root: root,
        repository,
        provider,
        bundle_path: prepared.bundle_path,
        bundle_id: prepared.bundle.bundle_id,
    }
}

fn queue(repository: &Path) -> assert_cmd::assert::Assert {
    #[allow(deprecated)]
    Command::cargo_bin("mko")
        .unwrap()
        .arg("queue")
        .arg("--repo")
        .arg(repository)
        .args(["--format", "json-v2"])
        .assert()
}

fn dashboard(repository: &Path) -> assert_cmd::assert::Assert {
    #[allow(deprecated)]
    Command::cargo_bin("mko")
        .unwrap()
        .arg("dashboard")
        .arg("--repo")
        .arg(repository)
        .args(["--format", "json-v2"])
        .assert()
}

#[test]
fn queue_cleans_a_recognized_crash_temp_during_ordinary_read_use() {
    let fixture = prepared_fixture(Utc::now());
    let digest = fixture
        .bundle_id
        .strip_prefix("prepared-content-sha256-")
        .unwrap();
    let temporary = fixture
        .bundle_path
        .parent()
        .unwrap()
        .join(format!(".mko-prepared-session-{digest}-999-1.tmp"));
    fs::copy(&fixture.bundle_path, &temporary).unwrap();
    fs::write(&temporary, b"partial crash output").unwrap();

    queue(&fixture.repository).success();

    assert!(!temporary.exists());
    assert!(fixture.bundle_path.exists());
}

#[test]
fn dashboard_cleans_an_expired_session_during_ordinary_read_use() {
    let fixture = prepared_fixture(Utc::now() - Duration::hours(25));
    assert!(fixture.bundle_path.exists());

    dashboard(&fixture.repository).success();

    assert!(!fixture.bundle_path.exists());
}

#[test]
fn read_commands_fail_closed_on_unmanaged_entries_and_preserve_them() {
    let fixture = prepared_fixture(Utc::now());
    let unmanaged = fixture
        .bundle_path
        .parent()
        .unwrap()
        .join("operator-note.txt");
    fs::write(&unmanaged, b"do not delete").unwrap();

    let queue_output = queue(&fixture.repository)
        .failure()
        .get_output()
        .stdout
        .clone();
    let queue_failure: serde_json::Value = serde_json::from_slice(&queue_output).unwrap();
    assert_eq!(
        queue_failure["error"]["code"],
        "prepared_session_directory_invalid"
    );
    assert!(
        queue_failure["error"]["message"]
            .as_str()
            .unwrap()
            .contains("inspect and relocate")
    );
    assert_eq!(fs::read(&unmanaged).unwrap(), b"do not delete");

    let dashboard_output = dashboard(&fixture.repository)
        .failure()
        .get_output()
        .stdout
        .clone();
    let dashboard_failure: serde_json::Value = serde_json::from_slice(&dashboard_output).unwrap();
    assert_eq!(
        dashboard_failure["error"]["code"],
        "prepared_session_directory_invalid"
    );
    assert_eq!(fs::read(&unmanaged).unwrap(), b"do not delete");
    assert!(fixture.provider.join("paper.pdf").exists());
}
