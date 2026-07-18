use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::error::MkoError;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);
const LOCK_WAIT: Duration = Duration::from_secs(1);
const LOCK_RETRY: Duration = Duration::from_millis(10);

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
    if path.exists() {
        validate_existing(path)?;
        return Ok(AtomicWriteResult::Existing);
    }
    let temporary = parent.join(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
    let write_result: Result<AtomicWriteResult, MkoError> = (|| {
        file.write_all(bytes)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        file.sync_all()
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        sync_directory(parent)?;
        Ok(AtomicWriteResult::Created)
    })();
    let _ = fs::remove_file(&temporary);
    write_result
}

struct PublicationLock {
    path: std::path::PathBuf,
}

impl PublicationLock {
    fn acquire(parent: &Path, filename: &str) -> Result<Self, MkoError> {
        let path = parent.join(format!(".{filename}.publish.lock"));
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "pid={}", std::process::id()).map_err(lock_error)?;
                    file.sync_all().map_err(lock_error)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(MkoError::new(
                            "registry_locked",
                            "registry publication lock is held or stale; inspect and remove it manually after validating the destination",
                        ));
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
        let _ = fs::remove_file(&self.path);
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

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{AtomicWriteResult, write_new};

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
}
