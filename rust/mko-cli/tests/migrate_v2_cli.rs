use std::{fs, path::Path, process::Command};

use assert_cmd::Command as AssertCommand;
use mko_core::{config_v2::CONTRACT_VERSION_V2, scaffold_v2::scaffold_personal_kb_v2};
use tempfile::tempdir;

fn git(repository: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .status()
        .unwrap();
    assert!(status.success());
}

fn git_commit_all(repository: &Path, message: &str) {
    git(repository, &["init", "--quiet"]);
    git(
        repository,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(repository, &["config", "user.name", "Fixture"]);
    git(repository, &["add", "."]);
    git(repository, &["commit", "--quiet", "-m", message]);
}

fn downgrade_to_migratable_contract(repository_root: &Path) {
    let config_path = repository_root.join("knowledge-os.yaml");
    let text = fs::read_to_string(&config_path).unwrap();
    let downgraded = text.replace(
        &format!("contract_version: {CONTRACT_VERSION_V2}"),
        "contract_version: 0.3.0",
    );
    assert_ne!(text, downgraded, "the fixture must actually be downgraded");
    fs::write(&config_path, downgraded).unwrap();
}

#[test]
#[allow(deprecated)]
fn a_cli_meeting_an_old_contract_kb_refuses_loudly_and_migrate_fixes_it() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");

    // A CLI on the new contract must refuse an old-contract KB loudly,
    // pointing at the migration command rather than silently reimposing
    // gate semantics or debt displays (D12).
    AssertCommand::cargo_bin("mko")
        .unwrap()
        .args(["queue", "--repo"])
        .arg(&repository)
        .assert()
        .failure()
        .stderr(predicates::str::contains("kb_contract_outdated"))
        .stderr(predicates::str::contains("mko migrate"));

    AssertCommand::cargo_bin("mko")
        .unwrap()
        .args(["migrate", "--repo"])
        .arg(&repository)
        .assert()
        .success()
        .stdout(predicates::str::contains(CONTRACT_VERSION_V2));

    // The migrated KB now reads under the strict, current-contract-only
    // path, and the renamed unconfirmed view exists.
    AssertCommand::cargo_bin("mko")
        .unwrap()
        .args(["queue", "--repo"])
        .arg(&repository)
        .assert()
        .success();
    assert!(repository.join("views/unconfirmed.base").is_file());
    assert!(!repository.join("views/review-queue.base").exists());
}

#[test]
#[allow(deprecated)]
fn migrate_refuses_a_dirty_git_tree_from_the_cli() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");
    fs::write(repository.join("draft.txt"), b"uncommitted").unwrap();

    AssertCommand::cargo_bin("mko")
        .unwrap()
        .args(["migrate", "--repo"])
        .arg(&repository)
        .assert()
        .failure()
        .stderr(predicates::str::contains("kb_git_tree_dirty"));
}
