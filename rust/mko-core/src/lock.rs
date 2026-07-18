use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

use crate::{clock::Clock, error::MkoError};

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

        match create_lock(&path, asset_id, command, clock) {
            Ok(lock) => Ok(lock),
            Err(error) if error.code() != "lock_exists" => Err(error),
            Err(_) if !clear_stale_lock => Err(lock_held_error()),
            Err(_) => {
                let _takeover = TakeoverGuard::acquire(&path)?;
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
        let Ok(_takeover) = TakeoverGuard::acquire(&self.path) else {
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

struct TakeoverGuard {
    path: PathBuf,
}

impl TakeoverGuard {
    fn acquire(lock_path: &Path) -> Result<Self, MkoError> {
        let path = lock_path.with_extension("lock.takeover");
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(b"takeover")
                    .and_then(|_| file.sync_all())
                    .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
                Ok(Self { path })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(lock_held_error())
            }
            Err(error) => Err(MkoError::new("lock_write_failed", error.to_string())),
        }
    }
}

impl Drop for TakeoverGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn create_lock(
    path: &Path,
    asset_id: &str,
    command: &str,
    clock: &dyn Clock,
) -> Result<AssetLock, MkoError> {
    let hostname = current_hostname()?;
    let owner_token = format!(
        "{}-{}",
        std::process::id(),
        NEXT_OWNER_TOKEN.fetch_add(1, Ordering::Relaxed)
    );
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
        owner_token,
    })
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
