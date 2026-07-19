use std::{fs, path::Path};

use cap_std::{ambient_authority, fs::Dir};
use mko_core::{atomic::write_replace_capability_validated_at_commit, error::MkoError};

#[test]
fn final_validation_rejects_destination_mutation_and_cleans_owned_temp() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record.md");
    fs::write(&path, "original\n").unwrap();
    let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();

    let error = write_replace_capability_validated_at_commit(
        &directory,
        Path::new("record.md"),
        b"approved\n",
        || fs::write(&path, "concurrent\n").map_err(io_error),
        || {
            if fs::read(&path).unwrap() == b"original\n" {
                Ok(())
            } else {
                Err(MkoError::new("snapshot_changed", "destination changed"))
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), "snapshot_changed");
    assert_eq!(fs::read(&path).unwrap(), b"concurrent\n");
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>(),
        vec!["record.md"]
    );
}

fn io_error(error: std::io::Error) -> MkoError {
    MkoError::new("test_io", error.to_string())
}

#[test]
fn hook_runs_after_synced_temp_exists_and_hook_failure_cleans_only_temp() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record.md");
    fs::write(&path, "original\n").unwrap();
    let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
    let root_path = root.path().to_path_buf();

    let error = write_replace_capability_validated_at_commit(
        &directory,
        Path::new("record.md"),
        b"approved\n",
        move || {
            let temp = fs::read_dir(&root_path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tmp"))
                .expect("temp exists before final validation");
            assert_eq!(fs::read(temp).unwrap(), b"approved\n");
            Err(MkoError::new("hook_failed", "stop publication"))
        },
        || Ok(()),
    )
    .unwrap_err();

    assert_eq!(error.code(), "hook_failed");
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn publication_lock_cleanup_never_removes_a_replacement_owner() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record.md");
    let lock = root.path().join(".record.md.publish.lock");
    fs::write(&path, "original\n").unwrap();
    let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
    let replacement_lock = lock.clone();

    write_replace_capability_validated_at_commit(
        &directory,
        Path::new("record.md"),
        b"approved\n",
        move || fs::write(&replacement_lock, "owner=replacement\n").map_err(io_error),
        || Ok(()),
    )
    .unwrap();

    assert_eq!(fs::read_to_string(lock).unwrap(), "owner=replacement\n");
    assert_eq!(fs::read(path).unwrap(), b"approved\n");
}

#[test]
fn temp_cleanup_never_removes_a_same_bytes_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record.md");
    fs::write(&path, "original\n").unwrap();
    let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
    let root_path = root.path().to_path_buf();

    let error = write_replace_capability_validated_at_commit(
        &directory,
        Path::new("record.md"),
        b"approved\n",
        move || {
            let temp = fs::read_dir(&root_path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tmp"))
                .expect("temp exists before final validation");
            let copied_bytes = fs::read(&temp).unwrap();
            fs::remove_file(&temp).unwrap();
            fs::write(&temp, copied_bytes).unwrap();
            Err(MkoError::new("hook_failed", "stop publication"))
        },
        || Ok(()),
    )
    .unwrap_err();

    assert_eq!(error.code(), "hook_failed");
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    assert!(fs::read_dir(root.path()).unwrap().any(|entry| {
        let entry = entry.unwrap();
        entry.path() != path && fs::read(entry.path()).unwrap() == b"approved\n"
    }));
}

#[test]
fn publication_lock_cleanup_never_removes_a_copied_owner_replacement() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("record.md");
    let lock = root.path().join(".record.md.publish.lock");
    fs::write(&path, "original\n").unwrap();
    let directory = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
    let replacement_lock = lock.clone();

    let error = write_replace_capability_validated_at_commit(
        &directory,
        Path::new("record.md"),
        b"approved\n",
        move || {
            let copied_owner = fs::read(&replacement_lock).unwrap();
            fs::remove_file(&replacement_lock).unwrap();
            fs::write(&replacement_lock, copied_owner).unwrap();
            Err(MkoError::new("hook_failed", "stop publication"))
        },
        || Ok(()),
    )
    .unwrap_err();

    assert_eq!(error.code(), "hook_failed");
    assert_eq!(fs::read(&path).unwrap(), b"original\n");
    let replacement: serde_json::Value = serde_json::from_slice(&fs::read(lock).unwrap()).unwrap();
    assert!(replacement["owner_token"].as_str().is_some());
}
