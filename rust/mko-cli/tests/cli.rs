use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use assert_cmd::Command;
use predicates::prelude::*;

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
fn asset_capture_emits_the_documented_json_result() {
    let env = CliTestEnv::new();
    let pdf = env.provider.join("paper.pdf");
    fs::write(&pdf, b"%PDF-fixture").unwrap();

    Command::cargo_bin("mko")
        .unwrap()
        .args([
            "asset",
            "capture",
            "--repo",
            env.repository.to_str().unwrap(),
            "--local-config",
            env.local_config.to_str().unwrap(),
            "--file",
            pdf.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\":\"created\""))
        .stdout(predicate::str::contains("\"asset_id\":\"personal-asset-"))
        .stdout(predicate::str::contains(
            "\"registry_path\":\"assets/registry/personal-asset-",
        ));
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
}

impl Drop for CliTestEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
