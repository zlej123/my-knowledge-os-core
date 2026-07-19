use std::{
    fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use mko_core::json_v1::{DoctorCheckStatus, JsonV1Success, NextAction};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

const CONFIG: &str = "system: my-knowledge-os\nscope: personal\ncore_version: 0.1.0\nschema_version: 1\nprovider:\n  name: personal_google_drive\n  type: google-drive-stream\n  root_env: MKO_PERSONAL_PROVIDER_ROOT\n";

#[test]
#[allow(deprecated)]
fn doctor_human_output_is_korean_first_and_json_uses_stable_codes() {
    let fixture = Fixture::new();

    let human = Command::cargo_bin("mko")
        .unwrap()
        .args(["doctor", "--repo"])
        .arg(&fixture.repository)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains("설정"), "{human}");
    assert!(!human.contains("profile_missing"), "{human}");

    let json = Command::cargo_bin("mko")
        .unwrap()
        .args(["doctor", "--repo"])
        .arg(&fixture.repository)
        .args(["--format", "json-v1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let JsonV1Success::Doctor { data, .. } = serde_json::from_slice(&json).unwrap() else {
        panic!("doctor must use the doctor JSON-v1 envelope")
    };
    assert!(!data.healthy);
    assert_eq!(data.next_action, NextAction::Configure);
    assert!(data.checks.iter().any(|check| {
        check.code == "profile_missing" && check.status == DoctorCheckStatus::Blocked
    }));
}

struct Fixture {
    root: PathBuf,
    repository: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "mko-cli-doctor-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let repository = root.join("repository");
        fs::create_dir(&repository).unwrap();
        fs::write(repository.join("knowledge-os.yaml"), CONFIG).unwrap();
        git(&repository, &["init", "--quiet"]);
        Self { root, repository }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(repository: &Path, arguments: &[&str]) {
    let status = ProcessCommand::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success());
}
