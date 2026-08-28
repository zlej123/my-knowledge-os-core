use std::{fs, path::Path, process::Command};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    config_v2::{CONTRACT_VERSION_V2, KnowledgeConfigV2},
    migrate_v2::migrate_v2,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

/// The pre-Phase-0 dashboard path `views/unconfirmed.base` replaces. Matches
/// `dashboard_v2::LEGACY_REVIEW_QUEUE_VIEW_PATH`, which is crate-private.
const LEGACY_REVIEW_QUEUE_VIEW_PATH: &str = "views/review-queue.base";

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

fn clock() -> FixedClock {
    FixedClock(
        DateTime::parse_from_rfc3339("2026-08-28T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
    )
}

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

/// Rewrites the freshly scaffolded (current-contract) KB's config to declare
/// the exact prior contract this migration upgrades from, simulating a KB
/// that predates Phase 0. `views/review-queue.base` is written too, with
/// stand-in pre-Phase-0 content, so the migration's retirement of that file
/// is exercised as well as the contract stamp.
fn downgrade_to_migratable_contract(repository_root: &Path) {
    let config_path = repository_root.join("knowledge-os.yaml");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains(&format!("contract_version: {CONTRACT_VERSION_V2}")));
    let downgraded = text.replace(
        &format!("contract_version: {CONTRACT_VERSION_V2}"),
        "contract_version: 0.3.0",
    );
    fs::write(&config_path, downgraded).unwrap();
    fs::write(
        repository_root.join(LEGACY_REVIEW_QUEUE_VIEW_PATH),
        b"filters:\n  and:\n    - file.inFolder(\"views/records\")\n    - 'derived_state != \"approved\"'\n",
    )
    .unwrap();
}

#[test]
fn old_contract_kb_is_refused_loudly_with_migration_guidance() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");

    let error = KnowledgeConfigV2::read(&repository).unwrap_err();

    assert_eq!(error.code(), "kb_contract_outdated");
    assert!(
        error.message().contains("mko migrate"),
        "refusal must guide the owner to the migration command: {}",
        error.message()
    );
    assert!(error.message().contains("clean git tree"));
}

#[test]
fn migration_refuses_a_dirty_git_tree() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");
    // Dirty the tree after the commit; the migration must still be reachable
    // (and refused) on a repository that has real content to preserve.
    fs::write(repository.join("uncommitted.txt"), b"draft notes").unwrap();

    let error = migrate_v2(&repository, &clock()).unwrap_err();

    assert_eq!(error.code(), "kb_git_tree_dirty");
    // Refusing must not touch the declared contract version.
    assert!(
        fs::read_to_string(repository.join("knowledge-os.yaml"))
            .unwrap()
            .contains("contract_version: 0.3.0")
    );
}

#[test]
fn migration_refuses_a_kb_that_is_not_on_the_migratable_contract() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    git_commit_all(&repository, "freeze current-contract KB");

    // The freshly scaffolded KB is already on the current contract, so there
    // is nothing to migrate from.
    let error = migrate_v2(&repository, &clock()).unwrap_err();

    assert_eq!(error.code(), "kb_contract_not_migratable");
}

#[test]
fn migration_stamps_the_new_contract_and_retires_the_review_queue_view() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");
    assert!(repository.join(LEGACY_REVIEW_QUEUE_VIEW_PATH).is_file());

    let result = migrate_v2(&repository, &clock()).unwrap();

    assert_eq!(result.from_contract_version, "0.3.0");
    assert_eq!(result.to_contract_version, CONTRACT_VERSION_V2);
    assert!(
        !repository.join(LEGACY_REVIEW_QUEUE_VIEW_PATH).exists(),
        "the superseded view must be retired, not left orphaned"
    );
    assert!(repository.join("views/unconfirmed.base").is_file());
    let unconfirmed = fs::read_to_string(repository.join("views/unconfirmed.base")).unwrap();
    assert!(unconfirmed.contains("derived_state != \"confirmed\""));

    // A CLI meeting the migrated KB under the strict, current-contract-only
    // path must now accept it.
    KnowledgeConfigV2::read(&repository).unwrap();

    // Migrating an already-migrated KB again is a clean, typed refusal, not a
    // silent no-op or a panic. Commit the migration's own writes first so the
    // second attempt is refused for being already current, not for a dirty
    // tree left behind by the first run.
    git(&repository, &["add", "."]);
    git(
        &repository,
        &["commit", "--quiet", "-m", "post-migration state"],
    );
    let error = migrate_v2(&repository, &clock()).unwrap_err();
    assert_eq!(error.code(), "kb_contract_not_migratable");
}
