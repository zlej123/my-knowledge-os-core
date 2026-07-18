use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::error::MkoError;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicWriteResult {
    Created,
    Existing,
}

pub fn write_new(path: &Path, bytes: &[u8]) -> Result<AtomicWriteResult, MkoError> {
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
        if path.exists() {
            return Ok(AtomicWriteResult::Existing);
        }
        fs::rename(&temporary, path)
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?;
        sync_directory(parent)?;
        Ok(AtomicWriteResult::Created)
    })();
    let _ = fs::remove_file(&temporary);
    write_result
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
            write_new(&destination, b"first").unwrap(),
            AtomicWriteResult::Created
        );
        assert_eq!(
            write_new(&destination, b"second").unwrap(),
            AtomicWriteResult::Existing
        );
        assert_eq!(fs::read(&destination).unwrap(), b"first");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let _ = fs::remove_dir_all(directory);
    }
}
