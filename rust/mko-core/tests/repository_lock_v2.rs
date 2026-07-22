use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};

use chrono::{DateTime, Duration, Utc};
use mko_core::{
    clock::Clock,
    lock::{LockRecord, RepositoryMutationLock, StaleRepositoryLockPolicy},
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
fn repository_mutation_lock_serializes_all_v2_writers_in_one_repository() {
    let temporary = tempdir().expect("temporary directory");
    let repository = temporary.path().to_path_buf();
    let start = Arc::new(Barrier::new(2));
    let acquired = Arc::new(Barrier::new(2));

    let handles = ["v2 source writer", "v2 review writer"].map(|command| {
        let repository = repository.clone();
        let start = Arc::clone(&start);
        let acquired = Arc::clone(&acquired);
        thread::spawn(move || {
            start.wait();
            let result = RepositoryMutationLock::acquire(
                &repository,
                command,
                &FixedClock(Utc::now()),
                StaleRepositoryLockPolicy::Preserve,
            );
            let outcome = result
                .as_ref()
                .map(|_| "acquired")
                .unwrap_or_else(|error| error.code());
            acquired.wait();
            (outcome.to_owned(), result)
        })
    });

    let results = handles.map(|handle| handle.join().expect("writer thread"));
    let acquired_count = results
        .iter()
        .filter(|(outcome, _)| outcome == "acquired")
        .count();
    let held_count = results
        .iter()
        .filter(|(outcome, _)| outcome == "repository_lock_held")
        .count();

    assert_eq!(acquired_count, 1);
    assert_eq!(held_count, 1);
}

#[test]
fn different_repositories_have_independent_mutation_locks() {
    let first = tempdir().expect("first repository");
    let second = tempdir().expect("second repository");
    let clock = FixedClock(Utc::now());

    let _first = RepositoryMutationLock::acquire(
        first.path(),
        "writer one",
        &clock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect("first lock");
    let _second = RepositoryMutationLock::acquire(
        second.path(),
        "writer two",
        &clock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect("independent second lock");
}

#[test]
fn stale_repository_lock_is_cleared_only_when_explicitly_requested() {
    let temporary = tempdir().expect("temporary directory");
    let now = Utc::now();
    let clock = FixedClock(now);
    let lock = RepositoryMutationLock::acquire(
        temporary.path(),
        "initial writer",
        &clock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect("initialize lock directory");
    drop(lock);

    let lock_path = repository_lock_path(temporary.path());
    let stale = LockRecord {
        pid: u32::MAX,
        hostname: hostname::get()
            .expect("hostname")
            .to_string_lossy()
            .into_owned(),
        started_at: now - Duration::minutes(16),
        command: "crashed v2 writer".into(),
        asset_id: "repository-v2-mutation".into(),
        owner_token: format!("1-1-{}", "a".repeat(32)),
    };
    fs::write(
        &lock_path,
        serde_json::to_vec(&stale).expect("stale record"),
    )
    .expect("seed stale lock");

    let error = RepositoryMutationLock::acquire(
        temporary.path(),
        "next writer",
        &clock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect_err("implicit stale clearing must not occur");
    assert_eq!(error.code(), "repository_lock_held");
    assert!(lock_path.exists());

    let recovered = RepositoryMutationLock::acquire(
        temporary.path(),
        "next writer",
        &clock,
        StaleRepositoryLockPolicy::Clear,
    )
    .expect("explicit stale clear");
    assert!(lock_path.exists());
    drop(recovered);
    assert!(!lock_path.exists());
}

#[test]
fn dropping_a_non_owner_does_not_remove_a_replacement_lock() {
    let temporary = tempdir().expect("temporary directory");
    let clock = FixedClock(Utc::now());
    let lock = RepositoryMutationLock::acquire(
        temporary.path(),
        "original writer",
        &clock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect("original lock");
    let lock_path = repository_lock_path(temporary.path());
    let replacement = b"replacement owner bytes";
    let replacement_path = lock_path.with_extension("replacement");
    fs::write(&replacement_path, replacement).expect("replacement lock");
    fs::remove_file(&lock_path).expect("remove original namespace entry");
    fs::rename(&replacement_path, &lock_path).expect("publish replacement lock");

    drop(lock);

    assert_eq!(
        fs::read(lock_path).expect("replacement survives"),
        replacement
    );
}

#[test]
fn non_directory_lock_path_is_rejected() {
    let temporary = tempdir().expect("temporary directory");
    fs::create_dir(temporary.path().join(".knowledge-os")).expect("knowledge directory");
    fs::write(
        temporary.path().join(".knowledge-os/runtime"),
        b"not a directory",
    )
    .expect("non-directory runtime path");

    let error = RepositoryMutationLock::acquire(
        temporary.path(),
        "writer",
        &FixedClock(Utc::now()),
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect_err("non-directory lock path must fail");

    assert_eq!(error.code(), "repository_lock_write_failed");
}

#[cfg(unix)]
#[test]
fn symlink_lock_path_is_rejected_without_following() {
    use std::os::unix::fs::symlink;

    let temporary = tempdir().expect("temporary directory");
    let outside = tempdir().expect("outside directory");
    fs::create_dir(temporary.path().join(".knowledge-os")).expect("knowledge directory");
    symlink(
        outside.path(),
        temporary.path().join(".knowledge-os/runtime"),
    )
    .expect("runtime symlink");

    let error = RepositoryMutationLock::acquire(
        temporary.path(),
        "writer",
        &FixedClock(Utc::now()),
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect_err("symlink lock path must fail");

    assert_eq!(error.code(), "repository_lock_write_failed");
    assert!(
        fs::read_dir(outside.path())
            .expect("outside directory")
            .next()
            .is_none()
    );
}

fn repository_lock_path(repository: &std::path::Path) -> std::path::PathBuf {
    repository.join(".knowledge-os/runtime/locks/repository-mutation.lock")
}
