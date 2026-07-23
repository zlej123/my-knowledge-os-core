use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration as StdDuration, Instant},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions, OpenOptionsExt},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::{
    clock::{Clock, SystemClock},
    error::MkoError,
};

const STALE_LOCK_TTL: Duration = Duration::minutes(15);
const LOCK_SCAN_ENTRY_LIMIT: usize = 64;
const LOCK_SCAN_TIME_LIMIT: StdDuration = StdDuration::from_millis(100);
const LOCK_RECORD_BYTE_LIMIT: u64 = 4096;
const REPOSITORY_MUTATION_FILENAME: &str = "repository-mutation.lock";
const REPOSITORY_MUTATION_SCOPE: &str = "repository-v2-mutation";
static NEXT_OWNER_TOKEN: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockRecord {
    pub pid: u32,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    pub command: String,
    pub asset_id: String,
    pub owner_token: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockState {
    Active,
    Stale,
    Unreadable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockInspection {
    pub path: PathBuf,
    pub state: LockState,
}

pub fn inspect_locks(
    repository_root: &Path,
    clock: &dyn Clock,
) -> Result<Vec<LockInspection>, MkoError> {
    let display_directory = repository_root.join(".knowledge-os/runtime/locks");
    let Some(directory) = open_existing_lock_directory(repository_root)? else {
        return Ok(Vec::new());
    };
    let entries = directory
        .read_dir(".")
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let mut inspections = Vec::new();
    let deadline = Instant::now() + LOCK_SCAN_TIME_LIMIT;
    for (index, entry) in entries.enumerate() {
        if index >= LOCK_SCAN_ENTRY_LIMIT || Instant::now() >= deadline {
            return Err(lock_scan_limit_error());
        }
        let entry = entry.map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_name().to_str().is_none() {
            continue;
        }
        let is_canonical = name.ends_with(".lock") || name.ends_with(".lock.takeover");
        let quarantine_target = authoritative_quarantine_target(&name);
        let is_quarantine = quarantine_target.is_some();
        if !is_canonical && !is_quarantine {
            continue;
        }
        let state = match read_quarantine_record_with_hook(&directory, &name, deadline, || {}) {
            Ok((Some(record), _)) => {
                if let Some((_, name_token)) = quarantine_target
                    && owner_token_secret(&record.owner_token) != Some(name_token)
                {
                    LockState::Unreadable
                } else if record_is_stale(&record, clock)? {
                    LockState::Stale
                } else {
                    LockState::Active
                }
            }
            Ok((None, _)) => LockState::Unreadable,
            Err(error) if error.code() == "lock_scan_limit" => return Err(error),
            Err(_) => LockState::Unreadable,
        };
        inspections.push(LockInspection {
            path: display_directory.join(name),
            state,
        });
    }
    inspections.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(inspections)
}

fn open_existing_lock_directory(repository_root: &Path) -> Result<Option<Dir>, MkoError> {
    let repository = match Dir::open_ambient_dir(repository_root, ambient_authority()) {
        Ok(repository) => repository,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    };
    let Some(knowledge) = open_existing_real_child_directory(&repository, ".knowledge-os")? else {
        return Ok(None);
    };
    let Some(runtime) = open_existing_real_child_directory(&knowledge, "runtime")? else {
        return Ok(None);
    };
    open_existing_real_child_directory(&runtime, "locks")
}

fn open_existing_real_child_directory(parent: &Dir, name: &str) -> Result<Option<Dir>, MkoError> {
    let metadata = match parent.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "lock_read_failed",
            "lock inspection path contains a link or non-directory",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    configure_lock_open(&mut options, true);
    let file = parent
        .open_with(name, &options)
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "lock_read_failed",
            "lock inspection path changed to a link or non-directory",
        ));
    }
    Ok(Some(Dir::from_std_file(file.into_std())))
}

pub struct AssetLock {
    directory: Dir,
    filename: String,
    repository_root: PathBuf,
    asset_id: String,
    owner_token: String,
    identity: StableFileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaleRepositoryLockPolicy {
    Preserve,
    Clear,
}

/// Serializes every schema-v2 mutation within one repository.
///
/// This lock is deliberately separate from `AssetLock`: it has a fixed
/// repository-scoped filename and does not accept an Asset ID from callers.
pub struct RepositoryMutationLock {
    directory: Dir,
    filename: String,
    repository_root: PathBuf,
    owner_token: String,
    identity: StableFileIdentity,
}

impl std::fmt::Debug for RepositoryMutationLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryMutationLock")
            .field("filename", &self.filename)
            .field("repository_root", &self.repository_root)
            .finish_non_exhaustive()
    }
}

impl RepositoryMutationLock {
    pub fn acquire(
        repository_root: &Path,
        command: &str,
        clock: &dyn Clock,
        stale_policy: StaleRepositoryLockPolicy,
    ) -> Result<Self, MkoError> {
        Self::acquire_inner(repository_root, command, clock, stale_policy)
            .map_err(map_repository_lock_error)
    }

    fn acquire_inner(
        repository_root: &Path,
        command: &str,
        clock: &dyn Clock,
        stale_policy: StaleRepositoryLockPolicy,
    ) -> Result<Self, MkoError> {
        let repository_root = fs::canonicalize(repository_root)
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
        let directory = secure_lock_directory(&repository_root)?;
        let filename = REPOSITORY_MUTATION_FILENAME;
        let clear_stale = stale_policy == StaleRepositoryLockPolicy::Clear;

        resolve_authoritative_quarantines(
            &directory,
            &format!("{filename}.takeover"),
            REPOSITORY_MUTATION_SCOPE,
            clock,
            clear_stale,
        )?;
        clear_stale_takeover_if_requested(
            &directory,
            filename,
            REPOSITORY_MUTATION_SCOPE,
            clock,
            clear_stale,
        )?;

        if clear_stale {
            let _takeover = TakeoverGuard::acquire(
                &directory,
                filename,
                REPOSITORY_MUTATION_SCOPE,
                command,
                clock,
                false,
            )?;
            resolve_authoritative_quarantines(
                &directory,
                filename,
                REPOSITORY_MUTATION_SCOPE,
                clock,
                true,
            )?;
            match create_repository_mutation_lock(&directory, &repository_root, command, clock) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.code() != "lock_exists" => return Err(error),
                Err(_) => {}
            }
            remove_stale_entry(&directory, filename, REPOSITORY_MUTATION_SCOPE, clock)?;
            return match create_repository_mutation_lock(
                &directory,
                &repository_root,
                command,
                clock,
            ) {
                Ok(lock) => Ok(lock),
                Err(error) if error.code() == "lock_exists" => Err(lock_held_error()),
                Err(error) => Err(error),
            };
        }

        resolve_authoritative_quarantines(
            &directory,
            filename,
            REPOSITORY_MUTATION_SCOPE,
            clock,
            false,
        )?;
        match create_repository_mutation_lock(&directory, &repository_root, command, clock) {
            Ok(lock) => Ok(lock),
            Err(error) if error.code() == "lock_exists" => Err(lock_held_error()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for RepositoryMutationLock {
    fn drop(&mut self) {
        let Ok(_takeover) = TakeoverGuard::acquire(
            &self.directory,
            &self.filename,
            REPOSITORY_MUTATION_SCOPE,
            "repository mutation lock release",
            &SystemClock,
            false,
        ) else {
            return;
        };
        remove_if_owned(
            &self.directory,
            &self.filename,
            &self.owner_token,
            self.identity,
        );
    }
}

impl std::fmt::Debug for AssetLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AssetLock")
            .field("filename", &self.filename)
            .field("repository_root", &self.repository_root)
            .field("asset_id", &self.asset_id)
            .finish_non_exhaustive()
    }
}

impl AssetLock {
    pub fn acquire(
        repository_root: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
    ) -> Result<Self, MkoError> {
        Self::acquire_with_directory_hook(
            repository_root,
            asset_id,
            command,
            clock,
            clear_stale_lock,
            || Ok(()),
        )
    }

    fn acquire_with_directory_hook<F>(
        repository_root: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
        after_directory_open: F,
    ) -> Result<Self, MkoError>
    where
        F: FnOnce() -> Result<(), MkoError>,
    {
        validate_asset_id(asset_id)?;
        let repository_root = fs::canonicalize(repository_root)
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
        let directory = secure_lock_directory(&repository_root)?;
        after_directory_open()?;
        let filename = format!("{asset_id}.lock");
        resolve_authoritative_quarantines(
            &directory,
            &format!("{filename}.takeover"),
            asset_id,
            clock,
            clear_stale_lock,
        )?;
        clear_stale_takeover_if_requested(
            &directory,
            &filename,
            asset_id,
            clock,
            clear_stale_lock,
        )?;

        if clear_stale_lock {
            let _takeover =
                TakeoverGuard::acquire(&directory, &filename, asset_id, command, clock, false)?;
            resolve_authoritative_quarantines(&directory, &filename, asset_id, clock, true)?;
            match create_lock(
                &directory,
                &filename,
                &repository_root,
                asset_id,
                command,
                clock,
            ) {
                Ok(lock) => return Ok(lock),
                Err(error) if error.code() != "lock_exists" => return Err(error),
                Err(_) => {}
            }
            remove_stale_entry(&directory, &filename, asset_id, clock)?;
            return match create_lock(
                &directory,
                &filename,
                &repository_root,
                asset_id,
                command,
                clock,
            ) {
                Ok(lock) => Ok(lock),
                Err(error) if error.code() == "lock_exists" => Err(lock_held_error()),
                Err(error) => Err(error),
            };
        }

        resolve_authoritative_quarantines(&directory, &filename, asset_id, clock, false)?;

        match create_lock(
            &directory,
            &filename,
            &repository_root,
            asset_id,
            command,
            clock,
        ) {
            Ok(lock) => Ok(lock),
            Err(error) if error.code() != "lock_exists" => Err(error),
            Err(_) => Err(lock_held_error()),
        }
    }

    pub(crate) fn assert_owned_for(
        &self,
        repository_root: &Path,
        asset_id: &str,
    ) -> Result<(), MkoError> {
        let repository_matches =
            fs::canonicalize(repository_root).is_ok_and(|root| root == self.repository_root);
        let owned = self.asset_id == asset_id
            && repository_matches
            && read_lock_record(&self.directory, &self.filename).is_ok_and(|(record, identity)| {
                identity == self.identity
                    && record.asset_id == asset_id
                    && record.owner_token == self.owner_token
            });
        if owned {
            Ok(())
        } else {
            Err(MkoError::new(
                "asset_lock_mismatch",
                "the held Asset lock does not authorize this publication",
            ))
        }
    }
}

fn secure_lock_directory(repository_root: &Path) -> Result<Dir, MkoError> {
    let repository = Dir::open_ambient_dir(repository_root, ambient_authority())
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
    let knowledge = ensure_real_child_directory(&repository, ".knowledge-os")?;
    let runtime = ensure_real_child_directory(&knowledge, "runtime")?;
    ensure_real_child_directory(&runtime, "locks")
}

fn ensure_real_child_directory(parent: &Dir, name: &str) -> Result<Dir, MkoError> {
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    configure_lock_open(&mut options, true);
    let file = parent
        .open_with(name, &options)
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "lock_write_failed",
            "lock directory is not a real directory",
        ));
    }
    Ok(Dir::from_std_file(file.into_std()))
}

impl Drop for AssetLock {
    fn drop(&mut self) {
        let Ok(_takeover) = TakeoverGuard::acquire(
            &self.directory,
            &self.filename,
            &self.asset_id,
            "asset lock release",
            &SystemClock,
            false,
        ) else {
            return;
        };
        remove_if_owned(
            &self.directory,
            &self.filename,
            &self.owner_token,
            self.identity,
        );
    }
}

#[derive(Debug)]
struct TakeoverGuard<'a> {
    directory: &'a Dir,
    filename: String,
    owner_token: String,
    identity: StableFileIdentity,
}

impl<'a> TakeoverGuard<'a> {
    fn acquire(
        directory: &'a Dir,
        lock_filename: &str,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
    ) -> Result<Self, MkoError> {
        Self::acquire_with_writer(
            directory,
            lock_filename,
            asset_id,
            command,
            clock,
            clear_stale_lock,
            |file, record| {
                let bytes =
                    serde_json::to_vec(record).map_err(|error| io_error(error.to_string()))?;
                file.write_all(&bytes).and_then(|_| file.sync_all())
            },
        )
    }

    fn acquire_with_writer<F>(
        directory: &'a Dir,
        lock_filename: &str,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
        write_record: F,
    ) -> Result<Self, MkoError>
    where
        F: FnOnce(&mut cap_std::fs::File, &LockRecord) -> std::io::Result<()>,
    {
        let filename = format!("{lock_filename}.takeover");
        let owner_token = next_owner_token()?;
        let record = LockRecord {
            pid: std::process::id(),
            hostname: current_hostname()?,
            started_at: clock.now_utc(),
            command: command.into(),
            asset_id: asset_id.into(),
            owner_token: owner_token.clone(),
        };
        ensure_no_authoritative_quarantine(directory, &filename, asset_id, clock)?;
        let mut file = match directory.open_with(
            &filename,
            cap_std::fs::OpenOptions::new().write(true).create_new(true),
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !clear_stale_lock {
                    return Err(lock_held_error());
                }
                remove_stale_entry(directory, &filename, asset_id, clock)?;
                directory
                    .open_with(
                        &filename,
                        cap_std::fs::OpenOptions::new().write(true).create_new(true),
                    )
                    .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?
            }
            Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
        };
        let identity = stable_file_identity(&file)?;
        if let Err(error) = write_record(&mut file, &record) {
            remove_identity_owned(directory, &filename, identity);
            return Err(MkoError::new("lock_write_failed", error.to_string()));
        }
        sync_lock_directory(directory)?;
        if let Err(error) =
            ensure_no_authoritative_quarantine(directory, &filename, asset_id, clock)
        {
            drop(file);
            remove_if_owned(directory, &filename, &owner_token, identity);
            return Err(error);
        }
        Ok(Self {
            directory,
            filename,
            owner_token,
            identity,
        })
    }
}

impl Drop for TakeoverGuard<'_> {
    fn drop(&mut self) {
        remove_if_owned(
            self.directory,
            &self.filename,
            &self.owner_token,
            self.identity,
        );
    }
}

fn create_lock(
    directory: &Dir,
    filename: &str,
    repository_root: &Path,
    asset_id: &str,
    command: &str,
    clock: &dyn Clock,
) -> Result<AssetLock, MkoError> {
    create_lock_with_writer(
        directory,
        filename,
        repository_root,
        asset_id,
        command,
        clock,
        |file, record| {
            let bytes = serde_json::to_vec(record).map_err(|error| io_error(error.to_string()))?;
            file.write_all(&bytes).and_then(|_| file.sync_all())
        },
    )
}

fn create_repository_mutation_lock(
    directory: &Dir,
    repository_root: &Path,
    command: &str,
    clock: &dyn Clock,
) -> Result<RepositoryMutationLock, MkoError> {
    let hostname = current_hostname()?;
    let owner_token = next_owner_token()?;
    ensure_no_authoritative_quarantine(
        directory,
        REPOSITORY_MUTATION_FILENAME,
        REPOSITORY_MUTATION_SCOPE,
        clock,
    )?;
    let mut file = match directory.open_with(
        REPOSITORY_MUTATION_FILENAME,
        cap_std::fs::OpenOptions::new().write(true).create_new(true),
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(MkoError::new(
                "lock_exists",
                "repository mutation lock already exists",
            ));
        }
        Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
    };
    let identity = stable_file_identity(&file)?;
    let record = LockRecord {
        pid: std::process::id(),
        hostname,
        started_at: clock.now_utc(),
        command: command.into(),
        asset_id: REPOSITORY_MUTATION_SCOPE.into(),
        owner_token: owner_token.clone(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        remove_identity_owned(directory, REPOSITORY_MUTATION_FILENAME, identity);
        return Err(MkoError::new("lock_write_failed", error.to_string()));
    }
    sync_lock_directory(directory)?;
    if let Err(error) = ensure_no_authoritative_quarantine(
        directory,
        REPOSITORY_MUTATION_FILENAME,
        REPOSITORY_MUTATION_SCOPE,
        clock,
    ) {
        drop(file);
        remove_if_owned(
            directory,
            REPOSITORY_MUTATION_FILENAME,
            &owner_token,
            identity,
        );
        return Err(error);
    }
    Ok(RepositoryMutationLock {
        directory: directory
            .try_clone()
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?,
        filename: REPOSITORY_MUTATION_FILENAME.into(),
        repository_root: repository_root.to_path_buf(),
        owner_token,
        identity,
    })
}

fn create_lock_with_writer<F>(
    directory: &Dir,
    filename: &str,
    repository_root: &Path,
    asset_id: &str,
    command: &str,
    clock: &dyn Clock,
    write_record: F,
) -> Result<AssetLock, MkoError>
where
    F: FnOnce(&mut cap_std::fs::File, &LockRecord) -> std::io::Result<()>,
{
    let hostname = current_hostname()?;
    let owner_token = next_owner_token()?;
    ensure_no_authoritative_quarantine(directory, filename, asset_id, clock)?;
    let mut file = match directory.open_with(
        filename,
        cap_std::fs::OpenOptions::new().write(true).create_new(true),
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(MkoError::new(
                "lock_exists",
                "asset operation lock already exists",
            ));
        }
        Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
    };
    let identity = stable_file_identity(&file)?;
    let record = LockRecord {
        pid: std::process::id(),
        hostname,
        started_at: clock.now_utc(),
        command: command.into(),
        asset_id: asset_id.into(),
        owner_token: owner_token.clone(),
    };
    if let Err(error) = write_record(&mut file, &record) {
        remove_identity_owned(directory, filename, identity);
        return Err(MkoError::new("lock_write_failed", error.to_string()));
    }
    sync_lock_directory(directory)?;
    if let Err(error) = ensure_no_authoritative_quarantine(directory, filename, asset_id, clock) {
        drop(file);
        remove_if_owned(directory, filename, &owner_token, identity);
        return Err(error);
    }
    Ok(AssetLock {
        directory: directory
            .try_clone()
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?,
        filename: filename.into(),
        repository_root: repository_root.to_path_buf(),
        asset_id: asset_id.into(),
        owner_token,
        identity,
    })
}

fn clear_stale_takeover_if_requested(
    directory: &Dir,
    lock_filename: &str,
    asset_id: &str,
    clock: &dyn Clock,
    clear_stale_lock: bool,
) -> Result<(), MkoError> {
    let takeover_filename = format!("{lock_filename}.takeover");
    match directory.symlink_metadata(&takeover_filename) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    }
    if !clear_stale_lock {
        return Err(lock_held_error());
    }
    remove_stale_entry(directory, &takeover_filename, asset_id, clock)
}

fn remove_if_owned(
    directory: &Dir,
    filename: &str,
    owner_token: &str,
    identity: StableFileIdentity,
) {
    remove_if_owned_with_observer(directory, filename, owner_token, identity, || {}, |_| {});
}

fn remove_identity_owned(directory: &Dir, filename: &str, identity: StableFileIdentity) {
    cleanup_lock_entry_with_observer(directory, filename, identity, None, || {}, |_| {});
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupDurabilityEvent {
    Quarantined,
    Restored,
    Removed,
}

fn remove_if_owned_with_observer<A, O>(
    directory: &Dir,
    filename: &str,
    owner_token: &str,
    identity: StableFileIdentity,
    after_quarantine: A,
    durability_observer: O,
) where
    A: FnOnce(),
    O: FnMut(CleanupDurabilityEvent),
{
    cleanup_lock_entry_with_observer(
        directory,
        filename,
        identity,
        Some(owner_token),
        after_quarantine,
        durability_observer,
    );
}

fn cleanup_lock_entry_with_observer<A, O>(
    directory: &Dir,
    filename: &str,
    identity: StableFileIdentity,
    expected_owner: Option<&str>,
    after_quarantine: A,
    mut durability_observer: O,
) where
    A: FnOnce(),
    O: FnMut(CleanupDurabilityEvent),
{
    let Ok(cleanup_token) = expected_owner
        .and_then(owner_token_secret)
        .map(str::to_owned)
        .map_or_else(secure_token, Ok)
    else {
        return;
    };
    let quarantine = format!(".{filename}.cleanup-{cleanup_token}");
    match directory.rename(filename, directory, &quarantine) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return,
    }
    if sync_lock_directory(directory).is_err() {
        return;
    }
    durability_observer(CleanupDurabilityEvent::Quarantined);
    after_quarantine();
    let owned = read_quarantine_record_with_hook(
        directory,
        &quarantine,
        Instant::now() + LOCK_SCAN_TIME_LIMIT,
        || {},
    )
    .ok()
    .is_some_and(|(record, current_identity)| {
        current_identity == identity
            && expected_owner
                .is_none_or(|owner| record.is_some_and(|record| record.owner_token == owner))
    });
    if owned {
        if directory.remove_file(&quarantine).is_ok() && sync_lock_directory(directory).is_ok() {
            durability_observer(CleanupDurabilityEvent::Removed);
        }
        return;
    }

    // The rename may have moved another actor's replacement. Restore its
    // public name with create-new semantics; otherwise preserve the quarantine.
    if directory
        .hard_link(&quarantine, directory, filename)
        .is_ok()
    {
        if sync_lock_directory(directory).is_err() {
            return;
        }
        durability_observer(CleanupDurabilityEvent::Restored);
        if directory.remove_file(&quarantine).is_ok() && sync_lock_directory(directory).is_ok() {
            durability_observer(CleanupDurabilityEvent::Removed);
        }
    }
}

fn authoritative_quarantine_target(name: &str) -> Option<(&str, &str)> {
    let (target, token) = name.strip_prefix('.')?.rsplit_once(".cleanup-")?;
    (token.len() == 32
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some((target, token))
}

#[derive(Debug)]
struct AssetQuarantine {
    filename: String,
    record: Option<LockRecord>,
    authenticated: bool,
    identity: Option<StableFileIdentity>,
}

fn scan_authoritative_quarantines(
    directory: &Dir,
    filename: &str,
) -> Result<Vec<AssetQuarantine>, MkoError> {
    let entries = directory
        .read_dir(".")
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let deadline = Instant::now() + LOCK_SCAN_TIME_LIMIT;
    let mut quarantines = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= LOCK_SCAN_ENTRY_LIMIT || Instant::now() >= deadline {
            return Err(lock_scan_limit_error());
        }
        let entry = entry.map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((target, name_token)) = authoritative_quarantine_target(&name) else {
            continue;
        };
        if target != filename {
            continue;
        }
        let (record, identity) =
            match read_quarantine_record_with_hook(directory, &name, deadline, || {}) {
                Ok((record, identity)) => (record, Some(identity)),
                Err(error) if error.code() == "lock_scan_limit" => return Err(error),
                Err(_) => {
                    check_lock_deadline(deadline)?;
                    let identity = stable_identity_of_nonblocking_entry(directory, &name)?;
                    check_lock_deadline(deadline)?;
                    (None, identity)
                }
            };
        let authenticated = record
            .as_ref()
            .is_some_and(|record| owner_token_secret(&record.owner_token) == Some(name_token));
        quarantines.push(AssetQuarantine {
            filename: name,
            record,
            authenticated,
            identity,
        });
    }
    Ok(quarantines)
}

fn read_quarantine_record_with_hook<H>(
    directory: &Dir,
    name: &str,
    deadline: Instant,
    after_metadata: H,
) -> Result<(Option<LockRecord>, StableFileIdentity), MkoError>
where
    H: FnOnce(),
{
    check_lock_deadline(deadline)?;
    let metadata = directory
        .symlink_metadata(name)
        .map_err(|error| MkoError::new("lock_quarantine_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(lock_quarantine_invalid_error());
    }
    after_metadata();
    check_lock_deadline(deadline)?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure_lock_open(&mut options, false);
    let mut file = directory
        .open_with(name, &options)
        .map_err(|error| MkoError::new("lock_quarantine_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("lock_quarantine_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(lock_quarantine_invalid_error());
    }
    let identity = stable_file_identity(&file)?;
    let mut input = Vec::new();
    Read::by_ref(&mut file)
        .take(LOCK_RECORD_BYTE_LIMIT + 1)
        .read_to_end(&mut input)
        .map_err(|error| MkoError::new("lock_quarantine_invalid", error.to_string()))?;
    check_lock_deadline(deadline)?;
    if input.len() as u64 > LOCK_RECORD_BYTE_LIMIT {
        return Ok((None, identity));
    }
    Ok((serde_json::from_slice::<LockRecord>(&input).ok(), identity))
}

fn stable_identity_of_nonblocking_entry(
    directory: &Dir,
    name: &str,
) -> Result<Option<StableFileIdentity>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_lock_open(&mut options, false);
    match directory.open_with(name, &options) {
        Ok(file) => stable_file_identity(&file).map(Some),
        Err(_) => Ok(None),
    }
}

fn check_lock_deadline(deadline: Instant) -> Result<(), MkoError> {
    if Instant::now() >= deadline {
        Err(lock_scan_limit_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn configure_lock_open(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_DIRECTORY: i32 = 0x10_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(target_os = "macos")]
fn configure_lock_open(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    const O_DIRECTORY: i32 = 0x10_0000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(windows)]
fn configure_lock_open(options: &mut OpenOptions, directory: bool) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    options.custom_flags(
        FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            },
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_lock_open(_options: &mut OpenOptions, _directory: bool) {}

fn ensure_no_authoritative_quarantine(
    directory: &Dir,
    filename: &str,
    asset_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let quarantines = scan_authoritative_quarantines(directory, filename)?;
    if quarantines.is_empty() {
        return Ok(());
    }
    for quarantine in quarantines {
        let Some(record) = quarantine.record else {
            return Err(lock_quarantine_invalid_error());
        };
        if record.asset_id != asset_id
            || !owner_token_is_valid(&record.owner_token)
            || !quarantine.authenticated
        {
            return Err(lock_quarantine_invalid_error());
        }
        let _ = record_is_stale(&record, clock)?;
    }
    Err(lock_held_error())
}

fn resolve_authoritative_quarantines(
    directory: &Dir,
    filename: &str,
    asset_id: &str,
    clock: &dyn Clock,
    clear_stale: bool,
) -> Result<(), MkoError> {
    let quarantines = scan_authoritative_quarantines(directory, filename)?;
    for quarantine in &quarantines {
        match &quarantine.record {
            Some(record)
                if record.asset_id == asset_id
                    && owner_token_is_valid(&record.owner_token)
                    && quarantine.authenticated =>
            {
                if !record_is_stale(record, clock)? || !clear_stale {
                    return Err(lock_held_error());
                }
            }
            _ if !clear_stale => return Err(lock_quarantine_invalid_error()),
            _ => {}
        }
    }
    for quarantine in quarantines {
        reap_authoritative_quarantine(directory, &quarantine)?;
    }
    Ok(())
}

fn reap_authoritative_quarantine(
    directory: &Dir,
    quarantine: &AssetQuarantine,
) -> Result<(), MkoError> {
    reap_authoritative_quarantine_with_observer(directory, quarantine, |_| {})
}

fn reap_authoritative_quarantine_with_observer<O>(
    directory: &Dir,
    quarantine: &AssetQuarantine,
    after_private_rename: O,
) -> Result<(), MkoError>
where
    O: FnOnce(&str),
{
    let private = format!("{}.reap-{}", quarantine.filename, secure_token()?);
    directory
        .rename(&quarantine.filename, directory, &private)
        .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
    sync_lock_directory(directory)?;
    after_private_rename(&private);
    let Some(expected_identity) = quarantine.identity else {
        return Ok(());
    };
    let (current_record, current_identity) = read_quarantine_record_with_hook(
        directory,
        &private,
        Instant::now() + LOCK_SCAN_TIME_LIMIT,
        || {},
    )
    .map_err(|_| {
        MkoError::new(
            "lock_clear_failed",
            "quarantined lock could not be safely reopened during recovery",
        )
    })?;
    if current_identity != expected_identity {
        return Err(MkoError::new(
            "lock_clear_failed",
            "quarantined lock changed during recovery",
        ));
    }
    if let Some(expected) = &quarantine.record
        && current_record.as_ref() != Some(expected)
    {
        return Err(MkoError::new(
            "lock_clear_failed",
            "quarantined lock owner changed during recovery",
        ));
    }
    directory
        .remove_file(&private)
        .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
    sync_lock_directory(directory)
}

fn owner_token_is_valid(token: &str) -> bool {
    let Some(secret) = owner_token_secret(token) else {
        return false;
    };
    let prefix = &token[..token.len() - secret.len() - 1];
    prefix.split_once('-').is_some()
        && secret.len() == 32
        && secret
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn owner_token_secret(token: &str) -> Option<&str> {
    let (_, secret) = token.rsplit_once('-')?;
    (secret.len() == 32
        && secret
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(secret)
}

fn lock_scan_limit_error() -> MkoError {
    MkoError::new(
        "lock_scan_limit",
        "lock directory scan exceeded its bounded work limit; reduce unexpected entries and retry",
    )
}

fn lock_quarantine_invalid_error() -> MkoError {
    MkoError::new(
        "lock_quarantine_invalid",
        "quarantined lock metadata is invalid; inspect it or retry with --clear-stale-lock",
    )
}

fn remove_stale_entry(
    directory: &Dir,
    filename: &str,
    asset_id: &str,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    remove_stale_entry_with_observer(directory, filename, asset_id, clock, || {})
}

fn remove_stale_entry_with_observer<O>(
    directory: &Dir,
    filename: &str,
    asset_id: &str,
    clock: &dyn Clock,
    after_validation: O,
) -> Result<(), MkoError>
where
    O: FnOnce(),
{
    let (captured_record, captured_identity) = read_lock_record(directory, filename)?;
    if captured_record.asset_id != asset_id || !record_is_stale(&captured_record, clock)? {
        return Err(lock_held_error());
    }
    let quarantine_token = owner_token_secret(&captured_record.owner_token)
        .map(str::to_owned)
        .unwrap_or(secure_token()?);
    let quarantine = format!(".{filename}.cleanup-{quarantine_token}");
    directory
        .rename(filename, directory, &quarantine)
        .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
    sync_lock_directory(directory)?;
    let (current_record, current_identity) = read_lock_record(directory, &quarantine)?;
    if current_identity != captured_identity || current_record != captured_record {
        return Err(MkoError::new(
            "lock_clear_failed",
            "lock changed during stale recovery",
        ));
    }
    let stale = current_record.asset_id == asset_id && record_is_stale(&current_record, clock)?;
    after_validation();
    if stale {
        directory
            .remove_file(&quarantine)
            .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
        sync_lock_directory(directory)?;
        return Ok(());
    }

    if directory
        .hard_link(&quarantine, directory, filename)
        .is_ok()
    {
        sync_lock_directory(directory)?;
        directory
            .remove_file(&quarantine)
            .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
        sync_lock_directory(directory)?;
    }
    Err(lock_held_error())
}

#[cfg(unix)]
fn sync_lock_directory(directory: &Dir) -> Result<(), MkoError> {
    directory
        .open(".")
        .and_then(|file| file.sync_all())
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_lock_directory(_: &Dir) -> Result<(), MkoError> {
    // Windows has no supported POSIX-equivalent parent-directory fsync in this safe API layer.
    // Lock file content is flushed, but parent-entry crash durability is not claimed.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_lock_directory(_: &Dir) -> Result<(), MkoError> {
    Ok(())
}

fn next_owner_token() -> Result<String, MkoError> {
    Ok(format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT_OWNER_TOKEN.fetch_add(1, Ordering::Relaxed),
        secure_token()?
    ))
}

fn secure_token() -> Result<String, MkoError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| MkoError::new("lock_write_failed", "secure randomness is unavailable"))?;
    Ok(hex::encode(random))
}

fn io_error(message: String) -> std::io::Error {
    std::io::Error::other(message)
}

fn validate_asset_id(asset_id: &str) -> Result<(), MkoError> {
    let hash = asset_id.strip_prefix("personal-asset-").ok_or_else(|| {
        MkoError::new(
            "asset_id_invalid",
            "asset ID must be a content-addressed asset ID",
        )
    })?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(MkoError::new(
            "asset_id_invalid",
            "asset ID must be a content-addressed asset ID",
        ));
    }
    Ok(())
}

fn lock_held_error() -> MkoError {
    MkoError::new(
        "lock_held",
        "asset operation lock is held; a stale lock requires --clear-stale-lock",
    )
}

fn map_repository_lock_error(error: MkoError) -> MkoError {
    if error.code() == "lock_held" {
        return MkoError::new(
            "repository_lock_held",
            "repository mutation lock is held; a stale lock requires an explicit clear policy",
        );
    }
    let Some(suffix) = error.code().strip_prefix("lock_") else {
        return error;
    };
    MkoError::new(format!("repository_lock_{suffix}"), error.message())
}

fn read_lock_record(
    directory: &Dir,
    filename: &str,
) -> Result<(LockRecord, StableFileIdentity), MkoError> {
    let (record, identity) = read_quarantine_record_with_hook(
        directory,
        filename,
        Instant::now() + LOCK_SCAN_TIME_LIMIT,
        || {},
    )
    .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let record = record.ok_or_else(|| {
        MkoError::new(
            "lock_held",
            "asset operation lock is unreadable; inspect it manually",
        )
    })?;
    Ok((record, identity))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
type StableFileIdentity = mko_windows_acl::FileIdentity;

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFileIdentity;

#[cfg(unix)]
fn stable_file_identity(file: &cap_std::fs::File) -> Result<StableFileIdentity, MkoError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .try_clone()
        .and_then(|file| file.into_std().metadata())
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    Ok(StableFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_file_identity(file: &cap_std::fs::File) -> Result<StableFileIdentity, MkoError> {
    let file = file
        .try_clone()
        .map(cap_std::fs::File::into_std)
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    mko_windows_acl::file_identity(&file)
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn stable_file_identity(_: &cap_std::fs::File) -> Result<StableFileIdentity, MkoError> {
    Err(MkoError::new(
        "lock_write_failed",
        "stable lock file identity is unsupported on this platform",
    ))
}

fn record_is_stale(record: &LockRecord, clock: &dyn Clock) -> Result<bool, MkoError> {
    if record.hostname != current_hostname()? {
        return Ok(false);
    }
    let age = clock.now_utc().signed_duration_since(record.started_at);
    Ok(age > STALE_LOCK_TTL && !same_host_process_is_live(record.pid))
}

fn current_hostname() -> Result<String, MkoError> {
    hostname::get()
        .map(|hostname| hostname.to_string_lossy().into_owned())
        .map_err(|error| MkoError::new("lock_hostname_failed", error.to_string()))
}

fn same_host_process_is_live(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::time::{Duration, Instant};
    use std::{
        fs,
        io::{self, Write},
        sync::atomic::{AtomicU64, Ordering},
    };

    use cap_std::{ambient_authority, fs::Dir};
    use chrono::{DateTime, Utc};

    use super::{
        AssetLock, CleanupDurabilityEvent, LockRecord, LockState, TakeoverGuard,
        create_lock_with_writer, inspect_locks, read_lock_record,
        reap_authoritative_quarantine_with_observer, remove_if_owned_with_observer,
        remove_stale_entry_with_observer, scan_authoritative_quarantines,
    };
    #[cfg(unix)]
    use super::{read_quarantine_record_with_hook, stable_file_identity};
    use crate::clock::Clock;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.0
        }
    }

    fn asset_id() -> String {
        format!("personal-asset-{}", "b".repeat(64))
    }

    fn time(value: &str) -> FixedClock {
        FixedClock(
            DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&Utc),
        )
    }

    fn test_directory() -> std::path::PathBuf {
        let unique = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mko-takeover-lock-test-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn crashed_takeover_requires_an_explicit_stale_clear_before_asset_lock_reacquisition() {
        let repository = test_directory();
        let asset_id = asset_id();
        let lock_path = repository
            .join(".knowledge-os/runtime/locks")
            .join(format!("{asset_id}.lock"));
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let lock_directory =
            Dir::open_ambient_dir(lock_path.parent().unwrap(), ambient_authority()).unwrap();
        let lock_filename = lock_path.file_name().unwrap().to_str().unwrap();
        let takeover = TakeoverGuard::acquire(
            &lock_directory,
            lock_filename,
            &asset_id,
            "takeover",
            &time("2026-07-18T00:00:00Z"),
            false,
        )
        .unwrap();
        std::mem::forget(takeover);
        let crashed_record = LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "takeover".into(),
            asset_id: asset_id.clone(),
            owner_token: "crashed-owner".into(),
        };
        fs::write(
            lock_path.with_extension("lock.takeover"),
            serde_json::to_vec(&crashed_record).unwrap(),
        )
        .unwrap();

        let error = AssetLock::acquire(
            &repository,
            &asset_id,
            "asset-operation",
            &time("2026-07-18T00:16:00Z"),
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "lock_held");

        let recovered = AssetLock::acquire(
            &repository,
            &asset_id,
            "asset-operation",
            &time("2026-07-18T00:16:00Z"),
            true,
        );
        assert!(recovered.is_ok());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn takeover_record_write_failure_removes_its_path() {
        let repository = test_directory();
        let asset_id = asset_id();
        let lock_path = repository.join(format!("{asset_id}.lock"));
        let lock_directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let error = TakeoverGuard::acquire_with_writer(
            &lock_directory,
            lock_path.file_name().unwrap().to_str().unwrap(),
            &asset_id,
            "takeover",
            &time("2026-07-18T00:00:00Z"),
            false,
            |_, _| Err(io::Error::other("simulated write failure")),
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_write_failed");
        assert!(!lock_path.with_extension("lock.takeover").exists());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn takeover_record_sync_failure_removes_its_path() {
        let repository = test_directory();
        let asset_id = asset_id();
        let lock_path = repository.join(format!("{asset_id}.lock"));
        let lock_directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let error = TakeoverGuard::acquire_with_writer(
            &lock_directory,
            lock_path.file_name().unwrap().to_str().unwrap(),
            &asset_id,
            "takeover",
            &time("2026-07-18T00:00:00Z"),
            false,
            |file, record| {
                let bytes = serde_json::to_vec(record).unwrap();
                file.write_all(&bytes)?;
                Err(io::Error::other("simulated sync failure"))
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_write_failed");
        assert!(!lock_path.with_extension("lock.takeover").exists());
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn takeover_write_failure_never_deletes_a_replacement() {
        let repository = test_directory();
        let asset_id = asset_id();
        let lock_path = repository.join(format!("{asset_id}.lock"));
        let takeover_path = lock_path.with_extension("lock.takeover");
        let lock_directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let error = TakeoverGuard::acquire_with_writer(
            &lock_directory,
            lock_path.file_name().unwrap().to_str().unwrap(),
            &asset_id,
            "takeover",
            &time("2026-07-18T00:00:00Z"),
            false,
            |_, record| {
                fs::remove_file(&takeover_path)?;
                fs::write(&takeover_path, serde_json::to_vec(record).unwrap())?;
                Err(io::Error::other("simulated write failure"))
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_write_failed");
        assert!(takeover_path.is_file());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn non_owner_drop_does_not_delete_a_new_takeover_record() {
        let repository = test_directory();
        let asset_id = asset_id();
        let lock_path = repository.join(format!("{asset_id}.lock"));
        let lock_directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let takeover = TakeoverGuard::acquire(
            &lock_directory,
            lock_path.file_name().unwrap().to_str().unwrap(),
            &asset_id,
            "takeover",
            &time("2026-07-18T00:00:00Z"),
            false,
        )
        .unwrap();
        let replacement = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "replacement".into(),
            asset_id,
            owner_token: "replacement-owner".into(),
        };
        fs::write(
            lock_path.with_extension("lock.takeover"),
            serde_json::to_vec(&replacement).unwrap(),
        )
        .unwrap();

        drop(takeover);

        assert!(lock_path.with_extension("lock.takeover").exists());
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn retained_lock_directory_survives_ambient_rename_and_copied_owner_attack() {
        use std::os::unix::fs::symlink;

        let repository = test_directory();
        let asset_id = asset_id();
        let ambient_locks = repository.join(".knowledge-os/runtime/locks");
        let retained_locks = repository.join(".knowledge-os/runtime/retained-locks");
        let outside = repository.join("outside-locks");
        fs::create_dir(&outside).unwrap();

        let lock = AssetLock::acquire_with_directory_hook(
            &repository,
            &asset_id,
            "asset-operation",
            &time("2026-07-18T00:00:00Z"),
            false,
            || {
                fs::rename(&ambient_locks, &retained_locks).unwrap();
                symlink(&outside, &ambient_locks).unwrap();
                Ok(())
            },
        )
        .unwrap();

        let filename = format!("{asset_id}.lock");
        assert!(retained_locks.join(&filename).is_file());
        assert!(!outside.join(&filename).exists());
        fs::copy(retained_locks.join(&filename), outside.join(&filename)).unwrap();

        lock.assert_owned_for(&repository, &asset_id).unwrap();
        drop(lock);

        assert!(!retained_locks.join(&filename).exists());
        assert!(outside.join(&filename).is_file());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn quarantined_asset_lock_blocks_third_acquirer_and_survives_restore_failure() {
        let repository = test_directory();
        let asset_id = asset_id();
        let directory_path = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&directory_path).unwrap();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let filename = format!("{asset_id}.lock");
        let original = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "original".into(),
            asset_id: asset_id.clone(),
            owner_token: format!("1-1-{}", "1".repeat(32)),
        };
        directory
            .write(&filename, serde_json::to_vec(&original).unwrap())
            .unwrap();
        let original_file = directory.open(&filename).unwrap();
        let original_identity = super::stable_file_identity(&original_file).unwrap();
        #[cfg(windows)]
        drop(original_file);
        directory.remove_file(&filename).unwrap();
        directory
            .write(&filename, serde_json::to_vec(&original).unwrap())
            .unwrap();
        #[cfg(not(windows))]
        drop(original_file);

        let mut durability = Vec::new();
        remove_if_owned_with_observer(
            &directory,
            &filename,
            &original.owner_token,
            original_identity,
            || {
                let error = AssetLock::acquire(
                    &repository,
                    &asset_id,
                    "third-acquirer",
                    &time("2026-07-18T00:00:00Z"),
                    false,
                )
                .expect_err("quarantine must be authoritative");
                assert_eq!(error.code(), "lock_held");
                directory
                    .write(&filename, serde_json::to_vec(&original).unwrap())
                    .unwrap();
            },
            |event| durability.push(event),
        );

        assert!(directory.metadata(&filename).is_ok());
        assert!(directory.read_dir(".").unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(&format!(".{filename}.cleanup-"))
        }));
        assert_eq!(durability, vec![CleanupDurabilityEvent::Quarantined]);
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn replacement_restore_reports_each_durable_directory_transition() {
        let repository = test_directory();
        let asset_id = asset_id();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let filename = format!("{asset_id}.lock");
        let original = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "original".into(),
            asset_id: asset_id.clone(),
            owner_token: "original-owner".into(),
        };
        directory
            .write(&filename, serde_json::to_vec(&original).unwrap())
            .unwrap();
        let original_identity = read_lock_record(&directory, &filename).unwrap().1;
        directory.remove_file(&filename).unwrap();
        let replacement = LockRecord {
            owner_token: "replacement-owner".into(),
            ..original.clone()
        };
        directory
            .write(&filename, serde_json::to_vec(&replacement).unwrap())
            .unwrap();

        let mut durability = Vec::new();
        remove_if_owned_with_observer(
            &directory,
            &filename,
            &original.owner_token,
            original_identity,
            || {},
            |event| durability.push(event),
        );

        assert_eq!(
            read_lock_record(&directory, &filename).unwrap().0,
            replacement
        );
        assert_eq!(
            durability,
            vec![
                CleanupDurabilityEvent::Quarantined,
                CleanupDurabilityEvent::Restored,
                CleanupDurabilityEvent::Removed,
            ]
        );
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn create_lock_write_failure_never_deletes_a_replacement() {
        let repository = test_directory();
        let asset_id = asset_id();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let filename = format!("{asset_id}.lock");
        let replacement_path = repository.join(&filename);

        let error = create_lock_with_writer(
            &directory,
            &filename,
            &repository,
            &asset_id,
            "create",
            &time("2026-07-18T00:00:00Z"),
            |_, record| {
                fs::remove_file(&replacement_path)?;
                fs::write(&replacement_path, serde_json::to_vec(record).unwrap())?;
                Err(io::Error::other("simulated write failure"))
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_write_failed");
        assert!(replacement_path.is_file());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn stale_clear_never_deletes_a_new_canonical_replacement() {
        let repository = test_directory();
        let asset_id = asset_id();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let filename = format!("{asset_id}.lock");
        let stale = LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "stale".into(),
            asset_id: asset_id.clone(),
            owner_token: "stale-owner".into(),
        };
        directory
            .write(&filename, serde_json::to_vec(&stale).unwrap())
            .unwrap();
        let replacement_path = repository.join(&filename);

        remove_stale_entry_with_observer(
            &directory,
            &filename,
            &asset_id,
            &time("2026-07-18T00:16:00Z"),
            || fs::write(&replacement_path, b"replacement").unwrap(),
        )
        .unwrap();

        assert_eq!(fs::read(replacement_path).unwrap(), b"replacement");
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn cleanup_like_name_with_an_invalid_token_is_not_an_authoritative_lock() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        fs::write(
            locks.join(format!(".{asset_id}.lock.cleanup-not-a-token")),
            b"forged",
        )
        .unwrap();

        let lock = AssetLock::acquire(
            &repository,
            &asset_id,
            "acquire",
            &time("2026-07-18T00:00:00Z"),
            false,
        )
        .unwrap();

        drop(lock);
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn malformed_authoritative_quarantine_is_visible_and_fails_with_a_stable_error() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{}", "a".repeat(32)));
        fs::write(&quarantine, b"").unwrap();

        let error = AssetLock::acquire(
            &repository,
            &asset_id,
            "acquire",
            &time("2026-07-18T00:16:00Z"),
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), "lock_quarantine_invalid");

        let inspections = inspect_locks(&repository, &time("2026-07-18T00:16:00Z")).unwrap();
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].path, quarantine);
        assert_eq!(inspections[0].state, LockState::Unreadable);

        let recovered = AssetLock::acquire(
            &repository,
            &asset_id,
            "recover malformed quarantine",
            &time("2026-07-18T00:16:00Z"),
            true,
        )
        .unwrap();
        assert!(!quarantine.exists());
        drop(recovered);
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn quarantine_filename_token_mismatch_is_unreadable() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{}", "a".repeat(32)));
        let record = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "test".into(),
            asset_id,
            owner_token: format!("1-1-{}", "b".repeat(32)),
        };
        fs::write(&quarantine, serde_json::to_vec(&record).unwrap()).unwrap();

        let inspections = inspect_locks(&repository, &time("2026-07-18T00:00:00Z")).unwrap();

        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].state, LockState::Unreadable);
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn explicit_clear_recovers_a_stale_authoritative_quarantine() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{}", "b".repeat(32)));
        let stale = LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "crashed cleanup".into(),
            asset_id: asset_id.clone(),
            owner_token: format!("1-1-{}", "b".repeat(32)),
        };
        fs::write(&quarantine, serde_json::to_vec(&stale).unwrap()).unwrap();

        let inspections = inspect_locks(&repository, &time("2026-07-18T00:16:00Z")).unwrap();
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].state, LockState::Stale);

        let lock = AssetLock::acquire(
            &repository,
            &asset_id,
            "recover",
            &time("2026-07-18T00:16:00Z"),
            true,
        )
        .unwrap();

        assert!(!quarantine.exists());
        drop(lock);
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn active_authoritative_quarantine_blocks_even_an_explicit_stale_clear() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let secret = "c".repeat(32);
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{secret}"));
        let active = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:16:00Z").now_utc(),
            command: "active cleanup".into(),
            asset_id: asset_id.clone(),
            owner_token: format!("1-1-{secret}"),
        };
        fs::write(&quarantine, serde_json::to_vec(&active).unwrap()).unwrap();

        let error = AssetLock::acquire(
            &repository,
            &asset_id,
            "must block",
            &time("2026-07-18T00:16:00Z"),
            true,
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_held");
        assert!(quarantine.exists());
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn authoritative_quarantine_scan_has_a_hard_entry_bound() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        for index in 0..80 {
            fs::write(locks.join(format!("noise-{index:03}")), b"noise").unwrap();
        }

        let started = std::time::Instant::now();
        let error = AssetLock::acquire(
            &repository,
            &asset_id,
            "bounded",
            &time("2026-07-18T00:00:00Z"),
            false,
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_scan_limit");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn matching_quarantine_scan_has_the_same_hard_entry_bound() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        for index in 0..80 {
            fs::write(
                locks.join(format!(".{asset_id}.lock.cleanup-{index:032x}")),
                b"",
            )
            .unwrap();
        }

        let error = AssetLock::acquire(
            &repository,
            &asset_id,
            "bounded",
            &time("2026-07-18T00:00:00Z"),
            false,
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_scan_limit");
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn quarantine_clear_never_deletes_a_replacement_after_private_rename() {
        let repository = test_directory();
        let asset_id = asset_id();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let secret = "d".repeat(32);
        let filename = format!("{asset_id}.lock");
        let quarantine = format!(".{filename}.cleanup-{secret}");
        let stale = LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "stale".into(),
            asset_id,
            owner_token: format!("1-1-{secret}"),
        };
        directory
            .write(&quarantine, serde_json::to_vec(&stale).unwrap())
            .unwrap();
        let quarantine = scan_authoritative_quarantines(&directory, &filename)
            .unwrap()
            .pop()
            .unwrap();

        let error =
            reap_authoritative_quarantine_with_observer(&directory, &quarantine, |private| {
                directory.remove_file(private).unwrap();
                directory.write(private, b"replacement").unwrap();
            })
            .unwrap_err();

        assert_eq!(error.code(), "lock_clear_failed");
        assert!(directory.read_dir(".").unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".reap-")
        }));
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn owned_cleanup_fifo_replacement_never_blocks_or_deletes_the_replacement() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let repository = test_directory();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let asset_id = asset_id();
        let secret = "8".repeat(32);
        let owner_token = format!("1-1-{secret}");
        let filename = format!("{asset_id}.lock");
        let quarantine = format!(".{filename}.cleanup-{secret}");
        let record = LockRecord {
            pid: std::process::id(),
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "cleanup-race".into(),
            asset_id,
            owner_token: owner_token.clone(),
        };
        directory
            .write(&filename, serde_json::to_vec(&record).unwrap())
            .unwrap();
        let file = directory.open(&filename).unwrap();
        let identity = stable_file_identity(&file).unwrap();

        let started = Instant::now();
        remove_if_owned_with_observer(
            &directory,
            &filename,
            &owner_token,
            identity,
            || {
                directory.remove_file(&quarantine).unwrap();
                mkfifo(&repository.join(&quarantine), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
            },
            |_| {},
        );

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(repository.join(&filename).exists() || repository.join(&quarantine).exists());
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn authoritative_reap_fifo_replacement_fails_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let repository = test_directory();
        let asset_id = asset_id();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let secret = "9".repeat(32);
        let filename = format!("{asset_id}.lock");
        let quarantine_name = format!(".{filename}.cleanup-{secret}");
        let stale = LockRecord {
            pid: u32::MAX,
            hostname: hostname::get().unwrap().to_string_lossy().into_owned(),
            started_at: time("2026-07-18T00:00:00Z").now_utc(),
            command: "stale".into(),
            asset_id,
            owner_token: format!("1-1-{secret}"),
        };
        directory
            .write(&quarantine_name, serde_json::to_vec(&stale).unwrap())
            .unwrap();
        let quarantine = scan_authoritative_quarantines(&directory, &filename)
            .unwrap()
            .pop()
            .unwrap();

        let started = Instant::now();
        let error =
            reap_authoritative_quarantine_with_observer(&directory, &quarantine, |private| {
                directory.remove_file(private).unwrap();
                mkfifo(&repository.join(private), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
            })
            .unwrap_err();

        assert_eq!(error.code(), "lock_clear_failed");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(directory.read_dir(".").unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".reap-")
        }));
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn exact_quarantine_fifo_is_unreadable_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{}", "e".repeat(32)));
        mkfifo(&quarantine, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();

        let started = std::time::Instant::now();
        let inspections = inspect_locks(&repository, &time("2026-07-18T00:00:00Z")).unwrap();

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].state, LockState::Unreadable);
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn exact_quarantine_symlink_is_unreadable_without_reading_its_target() {
        use std::os::unix::fs::symlink;

        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        let outside = repository.join("outside-secret");
        fs::write(&outside, b"do-not-read-or-change").unwrap();
        let quarantine = locks.join(format!(".{asset_id}.lock.cleanup-{}", "2".repeat(32)));
        symlink(&outside, &quarantine).unwrap();

        let inspections = inspect_locks(&repository, &time("2026-07-18T00:00:00Z")).unwrap();

        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].state, LockState::Unreadable);
        assert_eq!(fs::read(&outside).unwrap(), b"do-not-read-or-change");
        let _ = fs::remove_dir_all(repository);
    }

    #[cfg(unix)]
    #[test]
    fn metadata_to_fifo_swap_is_rejected_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let repository = test_directory();
        let directory = Dir::open_ambient_dir(&repository, ambient_authority()).unwrap();
        let name = format!(".asset.lock.cleanup-{}", "f".repeat(32));
        directory.write(&name, b"{}").unwrap();
        let path = repository.join(&name);

        let started = std::time::Instant::now();
        let error = read_quarantine_record_with_hook(
            &directory,
            &name,
            std::time::Instant::now() + std::time::Duration::from_millis(100),
            || {
                fs::remove_file(&path).unwrap();
                mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
            },
        )
        .unwrap_err();

        assert_eq!(error.code(), "lock_quarantine_invalid");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let _ = fs::remove_dir_all(repository);
    }

    #[test]
    fn huge_quarantine_record_is_bounded_and_unreadable() {
        let repository = test_directory();
        let asset_id = asset_id();
        let locks = repository.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&locks).unwrap();
        fs::write(
            locks.join(format!(".{asset_id}.lock.cleanup-{}", "1".repeat(32))),
            vec![b'x'; 256 * 1024],
        )
        .unwrap();

        let inspections = inspect_locks(&repository, &time("2026-07-18T00:00:00Z")).unwrap();

        assert_eq!(inspections.len(), 1);
        assert_eq!(inspections[0].state, LockState::Unreadable);
        let _ = fs::remove_dir_all(repository);
    }
}
