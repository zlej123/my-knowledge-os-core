use std::{
    fs,
    io::{Read, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use atomic_write_file::AtomicWriteFile;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions, OpenOptionsExt};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

use crate::error::MkoError;

const LOCK_WAIT: Duration = Duration::from_secs(1);
const LOCK_RETRY: Duration = Duration::from_millis(10);
const PUBLICATION_STALE_TTL: ChronoDuration = ChronoDuration::minutes(15);
const PUBLICATION_SCAN_ENTRY_LIMIT: usize = 64;
const PUBLICATION_SCAN_TIME_LIMIT: Duration = Duration::from_millis(100);
const PUBLICATION_RECORD_BYTE_LIMIT: u64 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicWriteResult {
    Created,
    Existing,
}

pub fn write_new<F>(
    path: &Path,
    bytes: &[u8],
    validate_existing: F,
) -> Result<AtomicWriteResult, MkoError>
where
    F: FnOnce(&Path) -> Result<(), MkoError>,
{
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            "registry_write_failed",
            "registry path has no parent directory",
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MkoError::new(
                "registry_write_failed",
                "registry filename must be valid UTF-8",
            )
        })?;
    let _lock = PublicationLock::acquire(parent, filename)?;
    let parent_directory = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            validate_existing(path)?;
            return Ok(AtomicWriteResult::Existing);
        }
        Ok(_) => {
            return Err(MkoError::new(
                "registry_destination_invalid",
                "registry destination exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(MkoError::new("registry_write_failed", error.to_string())),
    }
    let temporary = format!(".{filename}.{}.tmp", secure_cleanup_token()?);
    let mut file = parent_directory
        .open_with(
            &temporary,
            CapOpenOptions::new().write(true).create_new(true),
        )
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    let temporary_identity = stable_capability_identity(&file)?;
    let write_result: Result<AtomicWriteResult, MkoError> = (|| {
        file.write_all(bytes)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        file.sync_all()
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        drop(file);
        parent_directory
            .rename(&temporary, &parent_directory, filename)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        sync_capability_directory(&parent_directory)?;
        Ok(AtomicWriteResult::Created)
    })();
    if write_result.is_err() {
        cleanup_capability_entry(&parent_directory, &temporary, temporary_identity, None);
    }
    write_result
}

pub fn write_replace(path: &Path, bytes: &[u8]) -> Result<(), MkoError> {
    write_replace_checked(path, bytes, || Ok(()))
}

pub fn write_replace_checked<F>(
    path: &Path,
    bytes: &[u8],
    validate_current: F,
) -> Result<(), MkoError>
where
    F: FnOnce() -> Result<(), MkoError>,
{
    let parent = path.parent().ok_or_else(|| {
        MkoError::new(
            "registry_write_failed",
            "registry path has no parent directory",
        )
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MkoError::new(
                "registry_write_failed",
                "registry filename must be valid UTF-8",
            )
        })?;
    let _lock = PublicationLock::acquire(parent, filename)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "registry_destination_invalid",
                "registry destination exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MkoError::new(
                "registry_not_found",
                "registry record does not exist",
            ));
        }
        Err(error) => return Err(MkoError::new("registry_write_failed", error.to_string())),
    }
    validate_current()?;
    let mut file = AtomicWriteFile::open(path)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    file.commit()
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    sync_directory(parent)
}

/// Replaces a file using only paths relative to a retained directory capability.
///
/// This is used for security-sensitive publication where an ambient parent path
/// may be renamed or replaced while approval is in progress.
pub fn write_replace_capability_checked<F>(
    directory: &Dir,
    filename: &Path,
    bytes: &[u8],
    validate_current: F,
) -> Result<(), MkoError>
where
    F: FnOnce() -> Result<(), MkoError>,
{
    let filename = capability_filename(filename)?;
    let _lock = CapabilityPublicationLock::acquire(directory, filename)?;
    validate_capability_destination(directory, filename)?;
    validate_current()?;
    write_capability_temp_and_rename(directory, filename, bytes, || Ok(()), || Ok(()))
}

/// Atomically replaces a capability-relative regular file after validating the
/// destination at the last possible point before rename.
///
/// Lock order is caller-owned Asset lock first, then this publication lock.
pub fn write_replace_capability_validated_at_commit<B, V>(
    directory: &Dir,
    filename: &Path,
    bytes: &[u8],
    before_final_validation: B,
    validate_current: V,
) -> Result<(), MkoError>
where
    B: FnOnce() -> Result<(), MkoError>,
    V: FnOnce() -> Result<(), MkoError>,
{
    let filename = capability_filename(filename)?;
    let _lock = CapabilityPublicationLock::acquire(directory, filename)?;
    validate_capability_destination(directory, filename)?;
    write_capability_temp_and_rename(
        directory,
        filename,
        bytes,
        before_final_validation,
        validate_current,
    )
}

fn validate_capability_destination(directory: &Dir, filename: &str) -> Result<(), MkoError> {
    match directory.symlink_metadata(filename) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "registry_destination_invalid",
                "registry destination exists but is not a regular file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MkoError::new(
                "registry_not_found",
                "registry record does not exist",
            ));
        }
        Err(error) => return Err(MkoError::new("registry_write_failed", error.to_string())),
    }
    Ok(())
}

fn write_capability_temp_and_rename<B, V>(
    directory: &Dir,
    filename: &str,
    bytes: &[u8],
    before_final_validation: B,
    validate_current: V,
) -> Result<(), MkoError>
where
    B: FnOnce() -> Result<(), MkoError>,
    V: FnOnce() -> Result<(), MkoError>,
{
    let temporary = format!(".{filename}.{}.tmp", secure_cleanup_token()?);
    let mut file = directory
        .open_with(
            &temporary,
            CapOpenOptions::new().write(true).create_new(true),
        )
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    let temporary_identity = stable_capability_identity(&file)?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        file.sync_all()
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        drop(file);
        before_final_validation()?;
        validate_current()?;
        directory
            .rename(&temporary, directory, filename)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        sync_capability_directory(directory)
    })();
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            cleanup_capability_entry(directory, &temporary, temporary_identity, None);
            Err(error)
        }
    }
}

fn capability_filename(path: &Path) -> Result<&str, MkoError> {
    let mut components = path.components();
    let Some(std::path::Component::Normal(filename)) = components.next() else {
        return Err(MkoError::new(
            "registry_write_failed",
            "registry filename must be a single relative component",
        ));
    };
    if components.next().is_some() {
        return Err(MkoError::new(
            "registry_write_failed",
            "registry filename must be a single relative component",
        ));
    }
    filename.to_str().ok_or_else(|| {
        MkoError::new(
            "registry_write_failed",
            "registry filename must be valid UTF-8",
        )
    })
}

struct CapabilityPublicationLock<'a> {
    directory: &'a Dir,
    filename: String,
    expected_contents: String,
    identity: StableCapabilityIdentity,
}

impl<'a> CapabilityPublicationLock<'a> {
    fn acquire(directory: &'a Dir, filename: &str) -> Result<Self, MkoError> {
        Self::acquire_with_quarantine_observer(directory, filename, |_| {})
    }

    fn acquire_with_quarantine_observer<H>(
        directory: &'a Dir,
        filename: &str,
        mut after_quarantine_discovery: H,
    ) -> Result<Self, MkoError>
    where
        H: FnMut(&[PublicationQuarantine]),
    {
        let lock_filename = format!(".{filename}.publish.lock");
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            if Instant::now() >= deadline {
                return Err(publication_locked_error());
            }
            let scan_deadline = publication_scan_deadline(deadline);
            if let Err(error) = resolve_publication_quarantines_with_observer(
                directory,
                &lock_filename,
                scan_deadline,
                &mut after_quarantine_discovery,
            ) {
                if error.code() == "registry_locked" && Instant::now() < deadline {
                    thread::sleep(LOCK_RETRY);
                    continue;
                }
                return Err(error);
            }
            match directory.open_with(
                &lock_filename,
                CapOpenOptions::new().write(true).create_new(true),
            ) {
                Ok(mut file) => {
                    let owner_token = secure_cleanup_token()?;
                    let record = PublicationLockRecord::new(owner_token.clone())?;
                    let expected_contents = serde_json::to_string(&record)
                        .map_err(|error| lock_error(std::io::Error::other(error.to_string())))?;
                    file.write_all(expected_contents.as_bytes())
                        .map_err(lock_error)?;
                    file.sync_all().map_err(lock_error)?;
                    sync_capability_directory(directory)?;
                    let identity = stable_capability_identity(&file)?;
                    let scan_deadline = publication_scan_deadline(deadline);
                    if let Err(error) =
                        ensure_no_publication_quarantine(directory, &lock_filename, scan_deadline)
                    {
                        drop(file);
                        cleanup_capability_entry(
                            directory,
                            &lock_filename,
                            identity,
                            Some(&expected_contents),
                        );
                        if error.code() == "registry_locked" && Instant::now() < deadline {
                            thread::sleep(LOCK_RETRY);
                            continue;
                        }
                        return Err(error);
                    }
                    return Ok(Self {
                        directory,
                        filename: lock_filename,
                        expected_contents,
                        identity,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(publication_locked_error());
                    }
                    thread::sleep(LOCK_RETRY);
                }
                Err(error) => return Err(lock_error(error)),
            }
        }
    }
}

impl Drop for CapabilityPublicationLock<'_> {
    fn drop(&mut self) {
        cleanup_capability_entry(
            self.directory,
            &self.filename,
            self.identity,
            Some(&self.expected_contents),
        );
    }
}

fn cleanup_capability_entry(
    directory: &Dir,
    filename: &str,
    identity: StableCapabilityIdentity,
    expected_contents: Option<&str>,
) {
    cleanup_capability_entry_with_observer(
        directory,
        filename,
        identity,
        expected_contents,
        || {},
        |_| {},
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupDurabilityEvent {
    Quarantined,
    Restored,
    Removed,
}

fn cleanup_capability_entry_with_observer<A, O>(
    directory: &Dir,
    filename: &str,
    identity: StableCapabilityIdentity,
    expected_contents: Option<&str>,
    after_quarantine: A,
    mut durability_observer: O,
) where
    A: FnOnce(),
    O: FnMut(CleanupDurabilityEvent),
{
    let Ok(token) = expected_contents
        .and_then(|contents| serde_json::from_str::<PublicationLockRecord>(contents).ok())
        .map(|record| record.owner_token)
        .map_or_else(secure_cleanup_token, Ok)
    else {
        return;
    };
    let quarantine = format!(".{filename}.cleanup-{token}");
    match directory.rename(filename, directory, &quarantine) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return,
    }
    if sync_capability_directory(directory).is_err() {
        return;
    }
    durability_observer(CleanupDurabilityEvent::Quarantined);
    after_quarantine();

    let deadline = Instant::now() + PUBLICATION_SCAN_TIME_LIMIT;
    let owned = if let Some(expected) = expected_contents {
        read_publication_bytes_with_hook(directory, &quarantine, deadline, || {})
            .ok()
            .is_some_and(|(contents, current_identity)| {
                current_identity == identity
                    && contents.is_some_and(|contents| contents == expected.as_bytes())
            })
    } else {
        publication_entry_identity_nonblocking(directory, &quarantine) == Some(identity)
    };
    if owned {
        if directory.remove_file(&quarantine).is_ok()
            && sync_capability_directory(directory).is_ok()
        {
            durability_observer(CleanupDurabilityEvent::Removed);
        }
        return;
    }

    // Never delete a replacement moved by the atomic rename. Restore its
    // public name with create-new semantics, or leave the quarantine orphaned.
    if directory
        .hard_link(&quarantine, directory, filename)
        .is_ok()
    {
        if sync_capability_directory(directory).is_err() {
            return;
        }
        durability_observer(CleanupDurabilityEvent::Restored);
        if directory.remove_file(&quarantine).is_ok()
            && sync_capability_directory(directory).is_ok()
        {
            durability_observer(CleanupDurabilityEvent::Removed);
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PublicationLockRecord {
    pid: u32,
    hostname: String,
    started_at: DateTime<Utc>,
    owner_token: String,
}

impl PublicationLockRecord {
    fn new(owner_token: String) -> Result<Self, MkoError> {
        Ok(Self {
            pid: std::process::id(),
            hostname: hostname::get()
                .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?
                .to_string_lossy()
                .into_owned(),
            started_at: Utc::now(),
            owner_token,
        })
    }
}

#[derive(Debug)]
struct PublicationQuarantine {
    filename: String,
    record: Option<PublicationLockRecord>,
    authenticated: bool,
    identity: Option<StableCapabilityIdentity>,
}

fn publication_quarantine_target(name: &str) -> Option<(&str, &str)> {
    let (target, token) = name.strip_prefix('.')?.rsplit_once(".cleanup-")?;
    (token.len() == 32
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some((target, token))
}

fn read_publication_record_with_hook<H>(
    directory: &Dir,
    name: &str,
    deadline: Instant,
    after_metadata: H,
) -> Result<(Option<PublicationLockRecord>, StableCapabilityIdentity), MkoError>
where
    H: FnOnce(),
{
    let (input, identity) =
        read_publication_bytes_with_hook(directory, name, deadline, after_metadata)?;
    Ok((
        input.and_then(|input| serde_json::from_slice::<PublicationLockRecord>(&input).ok()),
        identity,
    ))
}

fn read_publication_bytes_with_hook<H>(
    directory: &Dir,
    name: &str,
    deadline: Instant,
    after_metadata: H,
) -> Result<(Option<Vec<u8>>, StableCapabilityIdentity), MkoError>
where
    H: FnOnce(),
{
    check_publication_deadline(deadline)?;
    let metadata = directory.symlink_metadata(name).map_err(lock_error)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(registry_quarantine_invalid_error());
    }
    after_metadata();
    check_publication_deadline(deadline)?;
    let mut options = CapOpenOptions::new();
    options.read(true);
    configure_publication_open(&mut options);
    let mut file = directory.open_with(name, &options).map_err(lock_error)?;
    let metadata = file.metadata().map_err(lock_error)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(registry_quarantine_invalid_error());
    }
    let identity = stable_capability_identity(&file)?;
    let mut input = Vec::new();
    Read::by_ref(&mut file)
        .take(PUBLICATION_RECORD_BYTE_LIMIT + 1)
        .read_to_end(&mut input)
        .map_err(lock_error)?;
    check_publication_deadline(deadline)?;
    if input.len() as u64 > PUBLICATION_RECORD_BYTE_LIMIT {
        return Ok((None, identity));
    }
    Ok((Some(input), identity))
}

fn publication_entry_identity_nonblocking(
    directory: &Dir,
    name: &str,
) -> Option<StableCapabilityIdentity> {
    let mut options = CapOpenOptions::new();
    options.read(true);
    configure_publication_open(&mut options);
    directory
        .open_with(name, &options)
        .ok()
        .and_then(|file| stable_capability_identity(&file).ok())
}

fn publication_scan_deadline(acquire_deadline: Instant) -> Instant {
    std::cmp::min(
        acquire_deadline,
        Instant::now() + PUBLICATION_SCAN_TIME_LIMIT,
    )
}

fn check_publication_deadline(deadline: Instant) -> Result<(), MkoError> {
    if Instant::now() >= deadline {
        Err(registry_scan_limit_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn configure_publication_open(options: &mut CapOpenOptions) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_publication_open(options: &mut CapOpenOptions) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_publication_open(options: &mut CapOpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_publication_open(_: &mut CapOpenOptions) {}

fn scan_publication_quarantines(
    directory: &Dir,
    filename: &str,
    deadline: Instant,
) -> Result<Vec<PublicationQuarantine>, MkoError> {
    let entries = directory
        .read_dir(".")
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    let mut quarantines = Vec::new();
    let mut matching_candidates = 0;
    for entry in entries {
        if Instant::now() >= deadline {
            return Err(registry_scan_limit_error());
        }
        let entry =
            entry.map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((target, name_token)) = publication_quarantine_target(&name) else {
            continue;
        };
        if target != filename {
            continue;
        }
        if matching_candidates >= PUBLICATION_SCAN_ENTRY_LIMIT {
            return Err(registry_scan_limit_error());
        }
        matching_candidates += 1;
        let (record, identity) =
            match read_publication_record_with_hook(directory, &name, deadline, || {}) {
                Ok((record, identity)) => (record, Some(identity)),
                Err(error) if error.code() == "registry_scan_limit" => return Err(error),
                Err(_) => {
                    check_publication_deadline(deadline)?;
                    let identity = publication_entry_identity_nonblocking(directory, &name);
                    check_publication_deadline(deadline)?;
                    (None, identity)
                }
            };
        let authenticated = record
            .as_ref()
            .is_some_and(|record| record.owner_token == name_token);
        quarantines.push(PublicationQuarantine {
            filename: name,
            record,
            authenticated,
            identity,
        });
    }
    Ok(quarantines)
}

fn ensure_no_publication_quarantine(
    directory: &Dir,
    filename: &str,
    deadline: Instant,
) -> Result<(), MkoError> {
    if scan_publication_quarantines(directory, filename, deadline)?.is_empty() {
        Ok(())
    } else {
        Err(publication_locked_error())
    }
}

fn resolve_publication_quarantines(
    directory: &Dir,
    filename: &str,
    deadline: Instant,
) -> Result<(), MkoError> {
    resolve_publication_quarantines_with_observer(directory, filename, deadline, |_| {})
}

fn resolve_publication_quarantines_with_observer<O>(
    directory: &Dir,
    filename: &str,
    deadline: Instant,
    mut after_quarantine_discovery: O,
) -> Result<(), MkoError>
where
    O: FnMut(&[PublicationQuarantine]),
{
    let quarantines = scan_publication_quarantines(directory, filename, deadline)?;
    after_quarantine_discovery(&quarantines);
    for quarantine in quarantines {
        check_publication_deadline(deadline)?;
        let valid = quarantine.authenticated && quarantine.record.is_some();
        if valid && !publication_record_is_stale(quarantine.record.as_ref().unwrap(), deadline)? {
            return Err(publication_locked_error());
        }
        reap_publication_quarantine(directory, &quarantine, deadline)?;
        if !valid {
            return Err(registry_quarantine_invalid_error());
        }
    }
    Ok(())
}

fn reap_publication_quarantine(
    directory: &Dir,
    quarantine: &PublicationQuarantine,
    deadline: Instant,
) -> Result<(), MkoError> {
    check_publication_deadline(deadline)?;
    let private = format!("{}.reap-{}", quarantine.filename, secure_cleanup_token()?);
    match directory.rename(&quarantine.filename, directory, &private) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(publication_locked_error());
        }
        Err(error) => return Err(lock_error(error)),
    }
    sync_capability_directory(directory)?;
    check_publication_deadline(deadline)?;
    let Some(expected_identity) = quarantine.identity else {
        return Err(registry_quarantine_invalid_error());
    };
    let current_identity = publication_entry_identity_nonblocking(directory, &private)
        .ok_or_else(registry_quarantine_invalid_error)?;
    check_publication_deadline(deadline)?;
    if current_identity != expected_identity {
        return Err(MkoError::new(
            "registry_quarantine_invalid",
            "publication quarantine changed during recovery",
        ));
    }
    if let Some(expected) = &quarantine.record {
        let (current, identity) =
            read_publication_record_with_hook(directory, &private, deadline, || {})?;
        if identity != expected_identity {
            return Err(registry_quarantine_invalid_error());
        }
        let current = current.ok_or_else(registry_quarantine_invalid_error)?;
        if &current != expected {
            return Err(MkoError::new(
                "registry_quarantine_invalid",
                "publication quarantine owner changed during recovery",
            ));
        }
    }
    check_publication_deadline(deadline)?;
    directory.remove_file(&private).map_err(lock_error)?;
    sync_capability_directory(directory)
}

fn publication_record_is_stale(
    record: &PublicationLockRecord,
    deadline: Instant,
) -> Result<bool, MkoError> {
    check_publication_deadline(deadline)?;
    let hostname = hostname::get()
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?
        .to_string_lossy()
        .into_owned();
    if record.hostname != hostname {
        return Ok(false);
    }
    let age = Utc::now().signed_duration_since(record.started_at);
    if age <= PUBLICATION_STALE_TTL {
        return Ok(false);
    }
    check_publication_deadline(deadline)?;
    let system = System::new_all();
    check_publication_deadline(deadline)?;
    Ok(system.process(Pid::from_u32(record.pid)).is_none())
}

fn registry_scan_limit_error() -> MkoError {
    MkoError::new(
        "registry_scan_limit",
        "publication lock scan exceeded its bounded work limit; reduce unexpected entries and retry",
    )
}

fn registry_quarantine_invalid_error() -> MkoError {
    MkoError::new(
        "registry_quarantine_invalid",
        "invalid publication quarantine was safely removed; retry the operation",
    )
}

fn publication_locked_error() -> MkoError {
    MkoError::new(
        "registry_locked",
        "registry publication lock is held or stale; inspect and remove it manually after validating the destination",
    )
}

fn secure_cleanup_token() -> Result<String, MkoError> {
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|_| {
        MkoError::new(
            "registry_write_failed",
            "secure randomness is unavailable for publication cleanup",
        )
    })?;
    Ok(hex::encode(random))
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableCapabilityIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
type StableCapabilityIdentity = mko_windows_acl::FileIdentity;

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableCapabilityIdentity;

#[cfg(unix)]
fn stable_capability_identity(
    file: &cap_std::fs::File,
) -> Result<StableCapabilityIdentity, MkoError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .try_clone()
        .and_then(|file| file.into_std().metadata())
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    Ok(StableCapabilityIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_capability_identity(
    file: &cap_std::fs::File,
) -> Result<StableCapabilityIdentity, MkoError> {
    let file = file
        .try_clone()
        .map(cap_std::fs::File::into_std)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    mko_windows_acl::file_identity(&file)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn stable_capability_identity(_: &cap_std::fs::File) -> Result<StableCapabilityIdentity, MkoError> {
    Err(MkoError::new(
        "registry_write_failed",
        "stable file identity is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn sync_capability_directory(directory: &Dir) -> Result<(), MkoError> {
    directory
        .open(".")
        .and_then(|file| file.sync_all())
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_capability_directory(_directory: &Dir) -> Result<(), MkoError> {
    // Windows has no supported POSIX-equivalent parent-directory fsync in this safe API layer.
    // File content is flushed before atomic rename, but parent-entry crash durability is not claimed.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_capability_directory(_directory: &Dir) -> Result<(), MkoError> {
    Ok(())
}

struct PublicationLock {
    directory: Dir,
    filename: String,
    expected_contents: String,
    identity: StableCapabilityIdentity,
}

impl PublicationLock {
    fn acquire(parent: &Path, filename: &str) -> Result<Self, MkoError> {
        let directory =
            Dir::open_ambient_dir(parent, cap_std::ambient_authority()).map_err(lock_error)?;
        let lock_filename = format!(".{filename}.publish.lock");
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            if Instant::now() >= deadline {
                return Err(publication_locked_error());
            }
            let scan_deadline = publication_scan_deadline(deadline);
            if let Err(error) =
                resolve_publication_quarantines(&directory, &lock_filename, scan_deadline)
            {
                if error.code() == "registry_locked" && Instant::now() < deadline {
                    thread::sleep(LOCK_RETRY);
                    continue;
                }
                return Err(error);
            }
            match directory.open_with(
                &lock_filename,
                CapOpenOptions::new().write(true).create_new(true),
            ) {
                Ok(mut file) => {
                    let owner_token = secure_cleanup_token()?;
                    let record = PublicationLockRecord::new(owner_token.clone())?;
                    let expected_contents = serde_json::to_string(&record)
                        .map_err(|error| lock_error(std::io::Error::other(error.to_string())))?;
                    file.write_all(expected_contents.as_bytes())
                        .map_err(lock_error)?;
                    file.sync_all().map_err(lock_error)?;
                    sync_capability_directory(&directory)?;
                    let identity = stable_capability_identity(&file)?;
                    let scan_deadline = publication_scan_deadline(deadline);
                    if let Err(error) =
                        ensure_no_publication_quarantine(&directory, &lock_filename, scan_deadline)
                    {
                        drop(file);
                        cleanup_capability_entry(
                            &directory,
                            &lock_filename,
                            identity,
                            Some(&expected_contents),
                        );
                        if error.code() == "registry_locked" && Instant::now() < deadline {
                            thread::sleep(LOCK_RETRY);
                            continue;
                        }
                        return Err(error);
                    }
                    return Ok(Self {
                        directory,
                        filename: lock_filename,
                        expected_contents,
                        identity,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(publication_locked_error());
                    }
                    thread::sleep(LOCK_RETRY);
                }
                Err(error) => return Err(lock_error(error)),
            }
        }
    }
}

impl Drop for PublicationLock {
    fn drop(&mut self) {
        cleanup_capability_entry(
            &self.directory,
            &self.filename,
            self.identity,
            Some(&self.expected_contents),
        );
    }
}

fn lock_error(error: std::io::Error) -> MkoError {
    MkoError::new("registry_write_failed", error.to_string())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), MkoError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), MkoError> {
    // Windows has no supported POSIX-equivalent parent-directory fsync in this safe API layer.
    // File content is flushed before atomic rename, but parent-entry crash durability is not claimed.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    use cap_std::{ambient_authority, fs::Dir};

    use super::{
        AtomicWriteResult, CapabilityPublicationLock, CleanupDurabilityEvent,
        PublicationLockRecord, cleanup_capability_entry_with_observer,
        read_publication_record_with_hook, stable_capability_identity, write_new,
    };

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn publish_uses_a_synced_temp_and_preserves_an_existing_destination() {
        let unique = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("mko-atomic-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("record.md");

        assert_eq!(
            write_new(&destination, b"first", |_| Ok(())).unwrap(),
            AtomicWriteResult::Created
        );
        assert_eq!(
            write_new(&destination, b"second", |_| Ok(())).unwrap(),
            AtomicWriteResult::Existing
        );
        assert_eq!(fs::read(&destination).unwrap(), b"first");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_dangling_symlink_destination_without_replacing_it() {
        let directory = test_directory();
        let destination = directory.join("record.md");
        std::os::unix::fs::symlink(directory.join("missing.md"), &destination).unwrap();

        let error = write_new(&destination, b"replacement", |_| Ok(())).unwrap_err();

        assert_eq!(error.code(), "registry_destination_invalid");
        assert!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rejects_a_directory_destination_without_replacing_it() {
        let directory = test_directory();
        let destination = directory.join("record.md");
        fs::create_dir(&destination).unwrap();

        let error = write_new(&destination, b"replacement", |_| Ok(())).unwrap_err();

        assert_eq!(error.code(), "registry_destination_invalid");
        assert!(destination.is_dir());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn quarantined_publication_lock_blocks_third_acquirer_and_survives_restore_failure() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let filename = ".record.md.publish.lock";
        let original_record = PublicationLockRecord::new("a".repeat(32)).unwrap();
        let original_contents = serde_json::to_string(&original_record).unwrap();
        let mut original = directory.create(filename).unwrap();
        original.write_all(original_contents.as_bytes()).unwrap();
        original.sync_all().unwrap();
        let original_identity = stable_capability_identity(&original).unwrap();
        drop(original);
        directory.remove_file(filename).unwrap();
        directory
            .write(filename, original_contents.as_bytes())
            .unwrap();

        let mut durability = Vec::new();
        cleanup_capability_entry_with_observer(
            &directory,
            filename,
            original_identity,
            Some(&original_contents),
            || {
                let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
                    Ok(_) => panic!("quarantine must be authoritative"),
                    Err(error) => error,
                };
                assert_eq!(error.code(), "registry_locked");
                directory.write(filename, b"owner=third\n").unwrap();
            },
            |event| durability.push(event),
        );

        assert_eq!(directory.read_to_string(filename).unwrap(), "owner=third\n");
        assert!(directory.read_dir(".").unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("..record.md.publish.lock.cleanup-")
        }));
        assert_eq!(durability, vec![CleanupDurabilityEvent::Quarantined]);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn publication_replacement_restore_reports_each_durable_directory_transition() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let filename = ".record.md.publish.lock";
        let original_record = PublicationLockRecord::new("b".repeat(32)).unwrap();
        let original_contents = serde_json::to_string(&original_record).unwrap();
        let mut original = directory.create(filename).unwrap();
        original.write_all(original_contents.as_bytes()).unwrap();
        original.sync_all().unwrap();
        let original_identity = stable_capability_identity(&original).unwrap();
        drop(original);
        directory.remove_file(filename).unwrap();
        directory.write(filename, b"owner=replacement\n").unwrap();

        let mut durability = Vec::new();
        cleanup_capability_entry_with_observer(
            &directory,
            filename,
            original_identity,
            Some(&original_contents),
            || {},
            |event| durability.push(event),
        );

        assert_eq!(
            directory.read_to_string(filename).unwrap(),
            "owner=replacement\n"
        );
        assert_eq!(
            durability,
            vec![
                CleanupDurabilityEvent::Quarantined,
                CleanupDurabilityEvent::Restored,
                CleanupDurabilityEvent::Removed,
            ]
        );
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn publication_cleanup_like_name_with_invalid_token_is_ignored() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        directory
            .write("..record.md.publish.lock.cleanup-not-a-token", b"forged")
            .unwrap();

        let lock = CapabilityPublicationLock::acquire(&directory, "record.md").unwrap();

        drop(lock);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn stale_publication_quarantine_is_recovered_on_next_acquire() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let quarantine = format!("..record.md.publish.lock.cleanup-{}", "d".repeat(32));
        let stale = serde_json::json!({
            "pid": u32::MAX,
            "hostname": hostname::get().unwrap().to_string_lossy(),
            "started_at": "2000-01-01T00:00:00Z",
            "owner_token": "d".repeat(32),
        });
        directory
            .write(&quarantine, serde_json::to_vec(&stale).unwrap())
            .unwrap();

        let lock = CapabilityPublicationLock::acquire(&directory, "record.md").unwrap();

        assert!(directory.metadata(&quarantine).is_err());
        drop(lock);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn publication_lock_retries_when_discovered_cleanup_quarantine_vanishes() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let secret = "4".repeat(32);
        let quarantine = format!("..record.md.publish.lock.cleanup-{secret}");
        let stale = serde_json::json!({
            "pid": u32::MAX,
            "hostname": hostname::get().unwrap().to_string_lossy(),
            "started_at": "2000-01-01T00:00:00Z",
            "owner_token": secret,
        });
        directory
            .write(&quarantine, serde_json::to_vec(&stale).unwrap())
            .unwrap();
        let mut discoveries = 0;

        let lock = CapabilityPublicationLock::acquire_with_quarantine_observer(
            &directory,
            "record.md",
            |quarantines| {
                if quarantines.is_empty() {
                    return;
                }
                discoveries += 1;
                assert_eq!(quarantines.len(), 1);
                assert_eq!(quarantines[0].filename, quarantine);
                directory.remove_file(&quarantine).unwrap();
            },
        )
        .expect("a vanished cleanup quarantine must be retried");

        assert_eq!(discoveries, 1);
        drop(lock);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn active_publication_quarantine_blocks_the_next_acquirer() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let secret = "e".repeat(32);
        let quarantine = format!("..record.md.publish.lock.cleanup-{secret}");
        let active = PublicationLockRecord::new(secret).unwrap();
        directory
            .write(&quarantine, serde_json::to_vec(&active).unwrap())
            .unwrap();

        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("active publication quarantine must block"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_locked");
        assert!(directory.metadata(&quarantine).is_ok());
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn malformed_publication_quarantine_fails_with_a_stable_error() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        directory
            .write(
                format!("..record.md.publish.lock.cleanup-{}", "f".repeat(32)),
                b"",
            )
            .unwrap();

        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("malformed quarantine must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_quarantine_invalid");
        let recovered = CapabilityPublicationLock::acquire(&directory, "record.md").unwrap();
        drop(recovered);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[cfg(unix)]
    #[test]
    fn exact_publication_quarantine_fifo_is_rejected_without_blocking() {
        use std::process::Command;

        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let quarantine = format!("..record.md.publish.lock.cleanup-{}", "1".repeat(32));
        assert!(
            Command::new("mkfifo")
                .arg(directory_path.join(&quarantine))
                .status()
                .unwrap()
                .success()
        );

        let started = Instant::now();
        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("FIFO quarantine must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_quarantine_invalid");
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = fs::remove_dir_all(directory_path);
    }

    #[cfg(unix)]
    #[test]
    fn exact_publication_quarantine_symlink_does_not_read_its_target() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let outside = directory_path.with_extension("outside");
        fs::write(&outside, b"outside-secret").unwrap();
        let quarantine = format!("..record.md.publish.lock.cleanup-{}", "2".repeat(32));
        std::os::unix::fs::symlink(&outside, directory_path.join(&quarantine)).unwrap();

        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("symlink quarantine must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_quarantine_invalid");
        assert_eq!(fs::read(&outside).unwrap(), b"outside-secret");
        let _ = fs::remove_file(outside);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn huge_publication_quarantine_record_is_bounded() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let quarantine = format!("..record.md.publish.lock.cleanup-{}", "3".repeat(32));
        directory.write(&quarantine, vec![b'x'; 1_000_000]).unwrap();

        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("huge quarantine must fail closed"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_quarantine_invalid");
        let _ = fs::remove_dir_all(directory_path);
    }

    #[cfg(unix)]
    #[test]
    fn publication_metadata_to_fifo_swap_is_rejected_without_blocking() {
        use std::process::Command;

        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        let name = "quarantine";
        directory.write(name, b"{}").unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);

        let started = Instant::now();
        let error = read_publication_record_with_hook(&directory, name, deadline, || {
            directory.remove_file(name).unwrap();
            assert!(
                Command::new("mkfifo")
                    .arg(directory_path.join(name))
                    .status()
                    .unwrap()
                    .success()
            );
        })
        .unwrap_err();

        assert_eq!(error.code(), "registry_quarantine_invalid");
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn publication_quarantine_limit_ignores_ordinary_records() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        for index in 0..80 {
            directory
                .write(format!("noise-{index:03}"), b"noise")
                .unwrap();
        }

        let started = Instant::now();
        let lock = CapabilityPublicationLock::acquire(&directory, "record.md")
            .expect("ordinary records do not consume the quarantine candidate limit");

        assert!(started.elapsed() < Duration::from_secs(1));
        drop(lock);
        let _ = fs::remove_dir_all(directory_path);
    }

    #[test]
    fn matching_publication_quarantines_have_the_same_hard_scan_bound() {
        let directory_path = test_directory();
        let directory = Dir::open_ambient_dir(&directory_path, ambient_authority()).unwrap();
        for index in 0..80 {
            directory
                .write(
                    format!("..record.md.publish.lock.cleanup-{index:032x}"),
                    b"",
                )
                .unwrap();
        }

        let error = match CapabilityPublicationLock::acquire(&directory, "record.md") {
            Ok(_) => panic!("matching quarantine scan must remain bounded"),
            Err(error) => error,
        };

        assert_eq!(error.code(), "registry_scan_limit");
        let _ = fs::remove_dir_all(directory_path);
    }

    fn test_directory() -> std::path::PathBuf {
        let unique = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("mko-atomic-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }
}
