use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use cap_std::{ambient_authority, fs::Dir};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

use crate::{
    clock::{Clock, SystemClock},
    error::MkoError,
};

const STALE_LOCK_TTL: Duration = Duration::minutes(15);
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
    let directory = repository_root.join(".knowledge-os/runtime/locks");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(MkoError::new("lock_read_failed", error.to_string())),
    };
    let mut inspections = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        if !name.ends_with(".lock") && !name.ends_with(".lock.takeover") {
            continue;
        }
        let state = match fs::read(&path)
            .ok()
            .and_then(|input| serde_json::from_slice::<LockRecord>(&input).ok())
        {
            Some(record) if record_is_stale(&record, clock)? => LockState::Stale,
            Some(_) => LockState::Active,
            None => LockState::Unreadable,
        };
        inspections.push(LockInspection { path, state });
    }
    inspections.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(inspections)
}

pub struct AssetLock {
    directory: Dir,
    filename: String,
    repository_root: PathBuf,
    asset_id: String,
    owner_token: String,
    identity: StableFileIdentity,
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
        clear_stale_takeover_if_requested(
            &directory,
            &filename,
            asset_id,
            clock,
            clear_stale_lock,
        )?;

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
            Err(_) if !clear_stale_lock => Err(lock_held_error()),
            Err(_) => {
                let _takeover =
                    TakeoverGuard::acquire(&directory, &filename, asset_id, command, clock, false)?;
                if !stale_lock(&directory, &filename, asset_id, clock)? {
                    return Err(lock_held_error());
                }
                directory
                    .remove_file(&filename)
                    .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
                match create_lock(
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
                }
            }
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
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(MkoError::new(
                "lock_write_failed",
                "lock directory is not a real directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match parent.create_dir(name) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
            }
            let metadata = parent
                .symlink_metadata(name)
                .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(MkoError::new(
                    "lock_write_failed",
                    "lock directory is not a real directory",
                ));
            }
        }
        Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
    }
    parent
        .open_dir(name)
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))
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
        let mut file = match directory.open_with(
            &filename,
            cap_std::fs::OpenOptions::new().write(true).create_new(true),
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !clear_stale_lock || !stale_lock(directory, &filename, asset_id, clock)? {
                    return Err(lock_held_error());
                }
                directory
                    .remove_file(&filename)
                    .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
                directory
                    .open_with(
                        &filename,
                        cap_std::fs::OpenOptions::new().write(true).create_new(true),
                    )
                    .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?
            }
            Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
        };
        if let Err(error) = write_record(&mut file, &record) {
            let _ = directory.remove_file(&filename);
            return Err(MkoError::new("lock_write_failed", error.to_string()));
        }
        let identity = stable_file_identity(
            &file
                .metadata()
                .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?,
        )?;
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
    let hostname = current_hostname()?;
    let owner_token = next_owner_token()?;
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
    let record = LockRecord {
        pid: std::process::id(),
        hostname,
        started_at: clock.now_utc(),
        command: command.into(),
        asset_id: asset_id.into(),
        owner_token: owner_token.clone(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        let _ = directory.remove_file(filename);
        return Err(MkoError::new("lock_write_failed", error.to_string()));
    }
    let identity = stable_file_identity(
        &file
            .metadata()
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?,
    )?;
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
    if !clear_stale_lock || !stale_lock(directory, &takeover_filename, asset_id, clock)? {
        return Err(lock_held_error());
    }
    directory
        .remove_file(&takeover_filename)
        .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))
}

fn remove_if_owned(
    directory: &Dir,
    filename: &str,
    owner_token: &str,
    identity: StableFileIdentity,
) {
    let Ok(cleanup_token) = secure_token() else {
        return;
    };
    let quarantine = format!(".{filename}.cleanup-{cleanup_token}");
    match directory.rename(filename, directory, &quarantine) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return,
    }
    let owned = read_lock_record(directory, &quarantine).is_ok_and(|(record, current_identity)| {
        current_identity == identity && record.owner_token == owner_token
    });
    if owned {
        let _ = directory.remove_file(&quarantine);
        return;
    }

    // The rename may have moved another actor's replacement. Restore its
    // public name with create-new semantics; otherwise preserve the quarantine.
    if directory
        .hard_link(&quarantine, directory, filename)
        .is_ok()
    {
        let _ = directory.remove_file(&quarantine);
    }
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

fn stale_lock(
    directory: &Dir,
    filename: &str,
    expected_asset_id: &str,
    clock: &dyn Clock,
) -> Result<bool, MkoError> {
    let (record, _) = read_lock_record(directory, filename)?;
    if record.asset_id != expected_asset_id {
        return Ok(false);
    }
    record_is_stale(&record, clock)
}

fn read_lock_record(
    directory: &Dir,
    filename: &str,
) -> Result<(LockRecord, StableFileIdentity), MkoError> {
    let mut file = directory
        .open(filename)
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let identity = stable_file_identity(
        &file
            .metadata()
            .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?,
    )?;
    let mut input = Vec::new();
    Read::by_ref(&mut file)
        .take(64 * 1024)
        .read_to_end(&mut input)
        .map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let record = serde_json::from_slice(&input).map_err(|_| {
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFileIdentity {
    volume_serial_number: u64,
    file_index: u64,
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableFileIdentity;

#[cfg(unix)]
fn stable_file_identity(metadata: &cap_std::fs::Metadata) -> Result<StableFileIdentity, MkoError> {
    use cap_std::fs::MetadataExt;
    Ok(StableFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_file_identity(metadata: &cap_std::fs::Metadata) -> Result<StableFileIdentity, MkoError> {
    use cap_std::fs::MetadataExt;
    Ok(StableFileIdentity {
        volume_serial_number: metadata.volume_serial_number().ok_or_else(|| {
            MkoError::new(
                "lock_write_failed",
                "lock file has no stable volume identity",
            )
        })?,
        file_index: metadata.file_index().ok_or_else(|| {
            MkoError::new("lock_write_failed", "lock file has no stable file identity")
        })?,
    })
}

#[cfg(not(any(unix, windows)))]
fn stable_file_identity(_: &cap_std::fs::Metadata) -> Result<StableFileIdentity, MkoError> {
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
    let system = System::new_all();
    system.process(Pid::from_u32(pid)).is_some()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{self, Write},
        sync::atomic::{AtomicU64, Ordering},
    };

    use cap_std::{ambient_authority, fs::Dir};
    use chrono::{DateTime, Utc};

    use super::{AssetLock, LockRecord, TakeoverGuard};
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
}
