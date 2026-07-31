use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    atomic::{AtomicWriteResult, write_new, write_replace_checked},
    clock::Clock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    records_v2::read_current_knowledge_revision_v2,
    revision_v2::canonical_json_bytes,
};

const HISTORY_PATH: &str = ".mko/runtime/resurface-history.json";
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;
const MAX_HISTORY_ENTRIES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResurfaceHistoryV2 {
    schema_version: u32,
    entries: Vec<ResurfaceOpenEntryV2>,
}

impl Default for ResurfaceHistoryV2 {
    fn default() -> Self {
        Self {
            schema_version: 2,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResurfaceOpenEntryV2 {
    knowledge_id: String,
    revision: String,
    opened_at: DateTime<Utc>,
}

pub(crate) fn read_resurface_opened_at_v2(
    repository_root: &Path,
) -> Result<BTreeMap<(String, String), DateTime<Utc>>, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let Some((history, _)) = read_optional_history(repository_root)? else {
        return Ok(BTreeMap::new());
    };
    Ok(history
        .entries
        .into_iter()
        .map(|entry| ((entry.knowledge_id, entry.revision), entry.opened_at))
        .collect())
}

pub fn record_resurfaced_knowledge_open_v2(
    repository_root: &Path,
    knowledge_id: &str,
    expected_revision: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 record resurfaced knowledge open",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let current = read_current_knowledge_revision_v2(repository_root, knowledge_id)?;
    if current.pointer.revision != expected_revision {
        return Err(MkoError::new(
            "resurface_selection_stale",
            "the selected Knowledge revision is no longer current",
        ));
    }

    let runtime = ensure_local_runtime(repository_root)?;
    let path = runtime.join("resurface-history.json");
    let existing = read_optional_history(repository_root)?;
    let (mut history, expected_bytes) = match existing {
        Some((history, bytes)) => (history, Some(bytes)),
        None => (ResurfaceHistoryV2::default(), None),
    };
    history
        .entries
        .retain(|entry| entry.knowledge_id != knowledge_id || entry.revision != expected_revision);
    history.entries.push(ResurfaceOpenEntryV2 {
        knowledge_id: knowledge_id.to_owned(),
        revision: expected_revision.to_owned(),
        opened_at: clock.now_utc(),
    });
    history.entries.sort_by(|left, right| {
        right
            .opened_at
            .cmp(&left.opened_at)
            .then(left.knowledge_id.cmp(&right.knowledge_id))
            .then(left.revision.cmp(&right.revision))
    });
    history.entries.truncate(MAX_HISTORY_ENTRIES);
    let bytes = canonical_json_bytes(&history)?;
    if bytes.len() as u64 > MAX_HISTORY_BYTES {
        return Err(MkoError::new(
            "resurface_history_limit",
            "local resurfacing history exceeds its bounded representation",
        ));
    }

    match expected_bytes {
        Some(expected) => write_replace_checked(&path, &bytes, || {
            let actual = read_regular_nofollow(&path)?;
            if actual == expected {
                Ok(())
            } else {
                Err(MkoError::new(
                    "resurface_history_stale",
                    "local resurfacing history changed before publication",
                ))
            }
        }),
        None => write_new(&path, &bytes, |existing| {
            let actual = read_regular_nofollow(existing)?;
            if actual == bytes {
                Ok(())
            } else {
                Err(MkoError::new(
                    "resurface_history_conflict",
                    "local resurfacing history path contains different bytes",
                ))
            }
        })
        .map(|outcome| match outcome {
            AtomicWriteResult::Created | AtomicWriteResult::Existing => (),
        }),
    }
    .map_err(|error| MkoError::new("resurface_history_write_failed", error.message()))?;
    protect_private_file(&path)?;
    Ok(())
}

fn read_optional_history(
    repository_root: &Path,
) -> Result<Option<(ResurfaceHistoryV2, Vec<u8>)>, MkoError> {
    let path = repository_root.join(HISTORY_PATH);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MkoError::new(
                "resurface_history_invalid",
                error.to_string(),
            ));
        }
    }
    validate_runtime_path(repository_root)?;
    let bytes = read_regular_nofollow(&path)?;
    ensure_private_file_permissions(&path)?;
    let history: ResurfaceHistoryV2 = serde_json::from_slice(&bytes).map_err(|_| {
        MkoError::new(
            "resurface_history_invalid",
            "local resurfacing history is not canonical JSON",
        )
    })?;
    validate_history(&history)?;
    if canonical_json_bytes(&history)? != bytes {
        return Err(MkoError::new(
            "resurface_history_invalid",
            "local resurfacing history bytes are not canonical",
        ));
    }
    Ok(Some((history, bytes)))
}

fn validate_history(history: &ResurfaceHistoryV2) -> Result<(), MkoError> {
    if history.schema_version != 2 || history.entries.len() > MAX_HISTORY_ENTRIES {
        return Err(MkoError::new(
            "resurface_history_invalid",
            "local resurfacing history schema or entry count is invalid",
        ));
    }
    let mut keys = BTreeMap::new();
    for entry in &history.entries {
        if !valid_prefixed_hash(&entry.knowledge_id, "personal-knowledge-")
            || !valid_digest(&entry.revision)
            || keys
                .insert((&entry.knowledge_id, &entry.revision), ())
                .is_some()
        {
            return Err(MkoError::new(
                "resurface_history_invalid",
                "local resurfacing history contains an invalid or duplicate entry",
            ));
        }
    }
    Ok(())
}

fn ensure_local_runtime(repository_root: &Path) -> Result<PathBuf, MkoError> {
    let mko = repository_root.join(".mko");
    require_real_directory(&mko)?;
    let ignore = read_small_regular_nofollow(&mko.join(".gitignore"), 1024)?;
    if ignore != b"runtime/\n" {
        return Err(MkoError::new(
            "local_runtime_policy_invalid",
            ".mko/.gitignore must exclude runtime before local history is persisted",
        ));
    }
    let runtime = mko.join("runtime");
    match fs::create_dir(&runtime) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(MkoError::new(
                "local_runtime_write_failed",
                error.to_string(),
            ));
        }
    }
    require_real_directory(&runtime)?;
    protect_private_directory(&runtime)?;
    Ok(runtime)
}

fn validate_runtime_path(repository_root: &Path) -> Result<(), MkoError> {
    let mko = repository_root.join(".mko");
    require_real_directory(&mko)?;
    let runtime = mko.join("runtime");
    require_real_directory(&runtime)?;
    ensure_private_directory_permissions(&runtime)
}

fn require_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime path must be a real directory",
        ))
    }
}

fn read_small_regular_nofollow(path: &Path, limit: u64) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime entry must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime entry exceeds its bounded input size",
        ));
    }
    Ok(bytes)
}

fn read_regular_nofollow(path: &Path) -> Result<Vec<u8>, MkoError> {
    read_small_regular_nofollow(path, MAX_HISTORY_BYTES)
}

#[cfg(unix)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(unix)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local runtime directory must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    ensure_private_windows_permissions(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_directory_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

#[cfg(windows)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_path(
        path,
        mko_windows_acl::Inheritance::ContainersAndObjects,
    )
    .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_directory(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be enforced on this platform",
    ))
}

#[cfg(unix)]
fn protect_private_file(path: &Path) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local history must be a regular non-link file",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(unix)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local resurfacing history must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    ensure_private_windows_permissions(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_file_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

#[cfg(windows)]
fn ensure_private_windows_permissions(path: &Path) -> Result<(), MkoError> {
    let inspection = mko_windows_acl::inspect_path(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    if inspection.is_owner_only_full_control() {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local runtime ACL must grant full control only to the current user",
        ))
    }
}

#[cfg(windows)]
fn protect_private_file(path: &Path) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local history must be a regular non-link file",
        ));
    }
    mko_windows_acl::apply_owner_only_to_file(&file)
        .map(|_| ())
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_file(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be enforced on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x20_000 | 0x800);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x100 | 0x4);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x0020_0000);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_prefixed_hash(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
