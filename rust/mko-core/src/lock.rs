use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

use crate::{clock::Clock, error::MkoError};

const STALE_LOCK_TTL: Duration = Duration::minutes(15);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockRecord {
    pub pid: u32,
    pub hostname: String,
    pub started_at: DateTime<Utc>,
    pub command: String,
    pub asset_id: String,
}

#[derive(Debug)]
pub struct AssetLock {
    path: PathBuf,
}

impl AssetLock {
    pub fn acquire(
        repository_root: &Path,
        asset_id: &str,
        command: &str,
        clock: &dyn Clock,
        clear_stale_lock: bool,
    ) -> Result<Self, MkoError> {
        let directory = repository_root.join(".knowledge-os/runtime/locks");
        fs::create_dir_all(&directory)
            .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
        let path = directory.join(format!("{asset_id}.lock"));

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let record = LockRecord {
                        pid: std::process::id(),
                        hostname: current_hostname()?,
                        started_at: clock.now_utc(),
                        command: command.into(),
                        asset_id: asset_id.into(),
                    };
                    let bytes = serde_json::to_vec(&record)
                        .map_err(|error| MkoError::new("lock_write_failed", error.to_string()))?;
                    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
                        let _ = fs::remove_file(&path);
                        return Err(MkoError::new("lock_write_failed", error.to_string()));
                    }
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if clear_stale_lock && stale_lock(&path, asset_id, clock)? {
                        fs::remove_file(&path).map_err(|remove_error| {
                            MkoError::new("lock_clear_failed", remove_error.to_string())
                        })?;
                        continue;
                    }
                    return Err(MkoError::new(
                        "lock_held",
                        "asset operation lock is held; a stale lock requires --clear-stale-lock",
                    ));
                }
                Err(error) => return Err(MkoError::new("lock_write_failed", error.to_string())),
            }
        }
    }
}

impl Drop for AssetLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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
