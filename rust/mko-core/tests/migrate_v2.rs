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
fn migrating_an_already_current_kb_completes_idempotently_without_a_clean_tree_gate() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    git_commit_all(&repository, "freeze current-contract KB");
    // Dirty the tree: resuming/repeating an already-current migration must
    // not gate on git cleanliness at all, because there is no contract
    // change left to roll back.
    fs::write(repository.join("uncommitted.txt"), b"draft notes").unwrap();

    // The freshly scaffolded KB is already on the current contract. There is
    // nothing to migrate from, but the call must still succeed as a resumed,
    // idempotent completion rather than being refused.
    let result = migrate_v2(&repository, &clock()).unwrap();

    assert!(result.resumed);
    assert_eq!(result.to_contract_version, CONTRACT_VERSION_V2);
}

#[test]
fn migration_refuses_a_kb_declaring_an_unsupported_contract_version() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let config_path = repository.join("knowledge-os.yaml");
    let text = fs::read_to_string(&config_path).unwrap();
    assert!(text.contains(&format!("contract_version: {CONTRACT_VERSION_V2}")));
    let unsupported = text.replace(
        &format!("contract_version: {CONTRACT_VERSION_V2}"),
        "contract_version: 0.2.0",
    );
    fs::write(&config_path, unsupported).unwrap();
    git_commit_all(&repository, "freeze KB on an unsupported contract");

    // Neither current nor the one migratable prior contract: a genuine
    // refusal, not a resumed completion.
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

    // Migrating an already-migrated KB again is a resumed, idempotent
    // completion, not a silent panic or a refusal that strands a KB whose
    // first run crashed after this point. Deliberately do not commit the
    // migration's own writes first: resuming must not gate on tree
    // cleanliness at all.
    let result = migrate_v2(&repository, &clock()).unwrap();
    assert!(result.resumed);
    assert_eq!(result.to_contract_version, CONTRACT_VERSION_V2);
    assert!(
        !repository.join(LEGACY_REVIEW_QUEUE_VIEW_PATH).exists(),
        "resuming must not resurrect the retired view"
    );
}

/// Simulates a crash between the two phases of `migrate_v2`: the contract is
/// already stamped current and the legacy view already retired (the
/// in-lock work), but the dashboard/view regeneration that runs after the
/// lock is released never happened, and the tree is left dirty exactly as an
/// interrupted run would leave it. Re-running `mko migrate` must resume and
/// complete rather than being refused by the clean-tree gate or by
/// `read_for_migration` treating the now-current contract as unmigratable.
#[test]
fn migration_resumes_after_a_crash_between_the_stamp_and_the_dashboard_regeneration() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");

    // Do the in-lock half of the migration by hand, then stop — standing in
    // for a process that crashed right after this point.
    let config_path = repository.join("knowledge-os.yaml");
    let text = fs::read_to_string(&config_path).unwrap();
    let stamped = text.replace(
        "contract_version: 0.3.0",
        &format!("contract_version: {CONTRACT_VERSION_V2}"),
    );
    fs::write(&config_path, stamped).unwrap();
    fs::remove_file(repository.join(LEGACY_REVIEW_QUEUE_VIEW_PATH)).unwrap();
    // Leave the stale (pre-migration-vocabulary) dashboard/view files
    // unregenerated, and the tree uncommitted and dirty — exactly the state
    // an interrupted run leaves behind.
    assert!(!repository.join("views/unconfirmed.base").is_file());

    let result = migrate_v2(&repository, &clock()).unwrap();

    assert!(result.resumed);
    assert_eq!(result.to_contract_version, CONTRACT_VERSION_V2);
    assert!(
        repository.join("views/unconfirmed.base").is_file(),
        "resuming must finish the idempotent dashboard/view regeneration"
    );
    let unconfirmed = fs::read_to_string(repository.join("views/unconfirmed.base")).unwrap();
    assert!(unconfirmed.contains("derived_state != \"confirmed\""));
    KnowledgeConfigV2::read(&repository).unwrap();
}

/// `require_clean_git_tree` must see untracked files even when the
/// repository (or the owner's global config) has set
/// `status.showUntrackedFiles no`, which makes plain `git status
/// --porcelain` silently omit them — otherwise the clean-tree gate can be
/// bypassed by a config setting that has nothing to do with the migration.
#[test]
fn migration_refuses_a_dirty_tree_even_when_untracked_files_are_configured_hidden() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    downgrade_to_migratable_contract(&repository);
    git_commit_all(&repository, "freeze pre-Phase-0 KB");
    git(&repository, &["config", "status.showUntrackedFiles", "no"]);
    fs::write(repository.join("untracked.txt"), b"draft notes").unwrap();

    let error = migrate_v2(&repository, &clock()).unwrap_err();

    assert_eq!(error.code(), "kb_git_tree_dirty");
    assert!(
        fs::read_to_string(repository.join("knowledge-os.yaml"))
            .unwrap()
            .contains("contract_version: 0.3.0")
    );
}
