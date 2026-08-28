//! Phase 0 contract migration: `mko migrate`.
//!
//! In v3 lifecycle state is derived from the append-only review event graph,
//! not stored on records, so migrating from `CONTRACT_VERSION_V2` "0.3.0" to
//! "0.3.1" is chiefly reinterpretation, not rewriting: the derivation
//! vocabulary changes (approval becomes a confirmation badge), and every
//! projection and generated dashboard file is regenerated under it. Existing
//! `approve` review events already carry the confirming timestamp bound to
//! their exact revision; they simply mean something more honest now.
//!
//! Git is the rollback (D6): the migration refuses to run unless the
//! knowledge repository's working tree is clean, and keeps no per-record
//! trace of the prior contract.

use std::{path::Path, process::Command};

use cap_std::fs::Dir;

use crate::{
    atomic::write_replace_capability_compare_exchange_validated_at_commit,
    clock::Clock,
    config_v2::{CONTRACT_VERSION_V2, KnowledgeConfigV2},
    dashboard_v2::{DashboardResultV2, LEGACY_REVIEW_QUEUE_VIEW_PATH, repair_dashboard_v2},
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    projection_v2::retire_generated_dashboard_file_locked_v2,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationResultV2 {
    pub from_contract_version: String,
    pub to_contract_version: String,
    pub dashboard: DashboardResultV2,
}

/// Runs the Phase 0 contract migration against `repository_root`.
///
/// Refuses loudly (rather than migrating partially) unless:
/// - the knowledge repository is a Git working tree with no uncommitted
///   changes (D6 — this is the rollback, so it must exist and be usable);
/// - `knowledge-os.yaml` declares exactly the prior migratable contract.
///
/// On success the contract version is stamped first, then every projection
/// and generated dashboard file is regenerated under the new vocabulary via
/// the same `mko dashboard --repair` machinery a stale dashboard uses. A
/// failure partway through (for example a hand-edited projection `mko
/// dashboard --repair` would also refuse) leaves the contract version bumped
/// and the repository in a state `git status` describes exactly — commit,
/// stash, or `git checkout .` and retry after resolving it.
pub fn migrate_v2(
    repository_root: &Path,
    clock: &dyn Clock,
) -> Result<MigrationResultV2, MkoError> {
    require_clean_git_tree(repository_root)?;
    let from_contract_version = KnowledgeConfigV2::read_for_migration(repository_root)?
        .contract_version
        .clone();
    {
        let _lock = RepositoryMutationLock::acquire(
            repository_root,
            "v2 contract migration",
            clock,
            StaleRepositoryLockPolicy::Preserve,
        )?;
        // Re-read under the lock: a concurrent migration on another process
        // may have already stamped the new contract version.
        let config = KnowledgeConfigV2::read_for_migration(repository_root)?;
        stamp_contract_version_locked(repository_root, &config)?;
        retire_generated_dashboard_file_locked_v2(repository_root, LEGACY_REVIEW_QUEUE_VIEW_PATH)?;
    }
    let dashboard = repair_dashboard_v2(repository_root)?;
    Ok(MigrationResultV2 {
        from_contract_version,
        to_contract_version: CONTRACT_VERSION_V2.into(),
        dashboard,
    })
}

fn stamp_contract_version_locked(
    repository_root: &Path,
    config: &KnowledgeConfigV2,
) -> Result<(), MkoError> {
    let mut updated = config.clone();
    updated.contract_version = CONTRACT_VERSION_V2.into();
    let current_bytes = config.render()?;
    let new_bytes = updated.render()?;
    if current_bytes == new_bytes {
        // Another process already migrated this exact repository.
        return Ok(());
    }
    let directory = Dir::open_ambient_dir(repository_root, cap_std::ambient_authority())
        .map_err(|error| MkoError::new("kb_config_write_failed", error.to_string()))?;
    write_replace_capability_compare_exchange_validated_at_commit(
        &directory,
        Path::new("knowledge-os.yaml"),
        &current_bytes,
        &new_bytes,
        || Ok(()),
        || Ok(()),
    )
    .map_err(|error| MkoError::new("kb_config_write_failed", error.message()))
}

fn require_clean_git_tree(repository_root: &Path) -> Result<(), MkoError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_root)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|error| MkoError::new("git_unavailable", error.to_string()))?;
    if !output.status.success() {
        return Err(MkoError::new(
            "git_repository_required",
            "the knowledge repository is not a Git working tree; `mko migrate` requires one so \
             git can serve as the rollback",
        ));
    }
    if !output.stdout.is_empty() {
        return Err(MkoError::new(
            "kb_git_tree_dirty",
            "the knowledge repository has uncommitted changes; commit or stash them before \
             running `mko migrate` (git is the migration's rollback)",
        ));
    }
    Ok(())
}
