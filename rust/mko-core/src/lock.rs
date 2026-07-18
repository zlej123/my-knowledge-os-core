use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

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

#[derive(Debug)]
pub struct AssetLock {
    path: PathBuf,
    asset_id: String,
    owner_token: String,
}

impl AssetLock {
    pub fn acquire(
        repository_root: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
    ) -> Result<Self, MkoError> {
        validate_asset_id(asset_id)?;
        let directory = repository_root.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&directory)
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
        let path = directory.join(format!("{asset_id}.lock"));
        clear_stale_takeover_if_requested(&path, asset_id, clock, clear_stale_lock)?;

        match create_lock(&path, asset_id, command, clock) {
            Ok(lock) => Ok(lock),
            Err(error) if error.code() != "lock_exists" => Err(error),
            Err(_) if !clear_stale_lock => Err(lock_held_error()),
            Err(_) => {
                let _takeover = TakeoverGuard::acquire(&path, asset_id, command, clock, false)?;
                if !stale_lock(&path, asset_id, clock)? {
                    return Err(lock_held_error());
                }
                fs::remove_file(&path)
                    .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
                match create_lock(&path, asset_id, command, clock) {
                    Ok(lock) => Ok(lock),
                    Err(error) if error.code() == "lock_exists" => Err(lock_held_error()),
                    Err(error) => Err(error),
                }
            }
        }
    }
}

impl Drop for AssetLock {
    fn drop(&mut self) {
        let Ok(_takeover) = TakeoverGuard::acquire(
            &self.path,
            &self.asset_id,
            "asset lock release",
            &SystemClock,
            false,
        ) else {
            return;
        };
        let owned = fs::read(&self.path)
            .ok()
            .and_then(|input| serde_json::from_slice::<LockRecord>(&input).ok())
            .is_some_and(|record| record.owner_token == self.owner_token);
        if owned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug)]
struct TakeoverGuard {
    path: PathBuf,
    owner_token: String,
}

impl TakeoverGuard {
    fn acquire(
        lock_path: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
    ) -> Result<Self, MkoError> {
        Self::acquire_with_writer(
            lock_path,
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
        lock_path: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
        write_record: F,
    ) -> Result<Self, MkoError>
    where
        F: FnOnce(&mut fs::File, &LockRecord) -> std::io::Result<()>,
    {
        let path = lock_path.with_extension("lock.takeover");
        let owner_token = next_owner_token();
        let record = LockRecord {
            pid: std::process::id(),
            hostname: current_hostname()?,
            started_at: clock.now_utc(),
            command: command.into(),
            asset_id: asset_id.into(),
            owner_token: owner_token.clone(),
        };
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if !clear_stale_lock || !stale_lock(&path, asset_id, clock)? {
                    return Err(lock_held_error());
                }
                fs::remove_file(&path)
                    .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?
            }
            Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
        };
        if let Err(error) = write_record(&mut file, &record) {
            let _ = fs::remove_file(&path);
            return Err(MkoError::new("lock_write_failed", error.to_string()));
        }
        Ok(Self { path, owner_token })
    }
}

impl Drop for TakeoverGuard {
    fn drop(&mut self) {
        remove_if_owned(&self.path, &self.owner_token);
    }
}

fn create_lock(
    path: &Path,
    asset_id: &str,
    command: &str,
    clock: &dyn Clock,
) -> Result<AssetLock, MkoError> {
    let hostname = current_hostname()?;
    let owner_token = next_owner_token();
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
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
        let _ = fs::remove_file(path);
        return Err(MkoError::new("lock_write_failed", error.to_string()));
    }
    Ok(AssetLock {
        path: path.to_path_buf(),
        asset_id: asset_id.into(),
        owner_token,
    })
}

fn clear_stale_takeover_if_requested(
    lock_path: &Path,
    asset_id: &str,
    clock: &dyn Clock,
    clear_stale_lock: bool,
) -> Result<(), MkoError> {
    let takeover_path = lock_path.with_extension("lock.takeover");
    if !takeover_path.exists() {
        return Ok(());
    }
    if !clear_stale_lock || !stale_lock(&takeover_path, asset_id, clock)? {
        return Err(lock_held_error());
    }
    fs::remove_file(&takeover_path)
        .map_err(|error| MkoError::new("lock_clear_failed", error.to_string()))
}

fn remove_if_owned(path: &Path, owner_token: &str) {
    let owned = fs::read(path)
        .ok()
        .and_then(|input| serde_json::from_slice::<LockRecord>(&input).ok())
        .is_some_and(|record| record.owner_token == owner_token);
    if owned {
        let _ = fs::remove_file(path);
    }
}

fn next_owner_token() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_OWNER_TOKEN.fetch_add(1, Ordering::Relaxed)
    )
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

fn stale_lock(path: &Path, expected_asset_id: &str, clock: &dyn Clock) -> Result<bool, MkoError> {
    let input =
        fs::read(path).map_err(|error| MkoError::new("lock_read_failed", error.to_string()))?;
    let record: LockRecord = serde_json::from_slice(&input).map_err(|_| {
        MkoError::new(
            "lock_held",
            "asset operation lock is unreadable; inspect it manually",
        )
    })?;
    if record.asset_id != expected_asset_id || record.hostname != current_hostname()? {
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
        let takeover = TakeoverGuard::acquire(
            &lock_path,
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
        let error = TakeoverGuard::acquire_with_writer(
            &lock_path,
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
        let error = TakeoverGuard::acquire_with_writer(
            &lock_path,
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
        let takeover = TakeoverGuard::acquire(
            &lock_path,
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
}
