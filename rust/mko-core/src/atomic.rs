use std::{
    fs,
    io::{Read, Write},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use atomic_write_file::AtomicWriteFile;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};

use crate::error::MkoError;

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
    let temporary_identity = stable_capability_identity(
        &file
            .metadata()
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?,
    )?;
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
    let temporary_identity = stable_capability_identity(
        &file
            .metadata()
            .map_err(|error| MkoError::new("registry_write_failed", error.to_string()))?,
    )?;
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
    owner_token: String,
    identity: StableCapabilityIdentity,
}

impl<'a> CapabilityPublicationLock<'a> {
    fn acquire(directory: &'a Dir, filename: &str) -> Result<Self, MkoError> {
        let lock_filename = format!(".{filename}.publish.lock");
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match directory.open_with(
                &lock_filename,
                CapOpenOptions::new().write(true).create_new(true),
            ) {
                Ok(mut file) => {
                    let owner_token = secure_cleanup_token()?;
                    writeln!(file, "owner={owner_token}").map_err(lock_error)?;
                    file.sync_all().map_err(lock_error)?;
                    let identity =
                        stable_capability_identity(&file.metadata().map_err(lock_error)?)?;
                    return Ok(Self {
                        directory,
                        filename: lock_filename,
                        owner_token,
                        identity,
                    });
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

impl Drop for CapabilityPublicationLock<'_> {
    fn drop(&mut self) {
        cleanup_capability_entry(
            self.directory,
            &self.filename,
            self.identity,
            Some(&format!("owner={}\n", self.owner_token)),
        );
    }
}

fn cleanup_capability_entry(
    directory: &Dir,
    filename: &str,
    identity: StableCapabilityIdentity,
    expected_contents: Option<&str>,
) {
    let Ok(token) = secure_cleanup_token() else {
        return;
    };
    let quarantine = format!(".{filename}.cleanup-{token}");
    match directory.rename(filename, directory, &quarantine) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => return,
    }

    let owned = directory.open(&quarantine).ok().is_some_and(|mut file| {
        let same_identity = file
            .metadata()
            .ok()
            .and_then(|metadata| stable_capability_identity(&metadata).ok())
            == Some(identity);
        let same_contents = expected_contents.is_none_or(|expected| {
            let mut contents = String::new();
            file.read_to_string(&mut contents).is_ok() && contents == expected
        });
        same_identity && same_contents
    });
    if owned {
        let _ = directory.remove_file(&quarantine);
        return;
    }

    // Never delete a replacement moved by the atomic rename. Restore its
    // public name with create-new semantics, or leave the quarantine orphaned.
    if directory
        .hard_link(&quarantine, directory, filename)
        .is_ok()
    {
        let _ = directory.remove_file(&quarantine);
    }
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableCapabilityIdentity {
    volume_serial_number: u64,
    file_index: u64,
}

#[cfg(not(any(unix, windows)))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StableCapabilityIdentity;

#[cfg(unix)]
fn stable_capability_identity(
    metadata: &cap_std::fs::Metadata,
) -> Result<StableCapabilityIdentity, MkoError> {
    use cap_std::fs::MetadataExt;
    Ok(StableCapabilityIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn stable_capability_identity(
    metadata: &cap_std::fs::Metadata,
) -> Result<StableCapabilityIdentity, MkoError> {
    use cap_std::fs::MetadataExt;
    Ok(StableCapabilityIdentity {
        volume_serial_number: metadata.volume_serial_number().ok_or_else(|| {
            MkoError::new(
                "registry_write_failed",
                "file has no stable volume identity",
            )
        })?,
        file_index: metadata.file_index().ok_or_else(|| {
            MkoError::new("registry_write_failed", "file has no stable file identity")
        })?,
    })
}

#[cfg(not(any(unix, windows)))]
fn stable_capability_identity(
    _: &cap_std::fs::Metadata,
) -> Result<StableCapabilityIdentity, MkoError> {
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

#[cfg(not(unix))]
fn sync_capability_directory(_directory: &Dir) -> Result<(), MkoError> {
    Ok(())
}

struct PublicationLock {
    directory: Dir,
    filename: String,
    owner_token: String,
    identity: StableCapabilityIdentity,
}

impl PublicationLock {
    fn acquire(parent: &Path, filename: &str) -> Result<Self, MkoError> {
        let directory =
            Dir::open_ambient_dir(parent, cap_std::ambient_authority()).map_err(lock_error)?;
        let lock_filename = format!(".{filename}.publish.lock");
        let owner_token = secure_cleanup_token()?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match directory.open_with(
                &lock_filename,
                CapOpenOptions::new().write(true).create_new(true),
            ) {
                Ok(mut file) => {
                    writeln!(file, "owner={owner_token}").map_err(lock_error)?;
                    file.sync_all().map_err(lock_error)?;
                    let identity =
                        stable_capability_identity(&file.metadata().map_err(lock_error)?)?;
                    return Ok(Self {
                        directory,
                        filename: lock_filename,
                        owner_token,
                        identity,
                    });
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
        cleanup_capability_entry(
            &self.directory,
            &self.filename,
            self.identity,
            Some(&format!("owner={}\n", self.owner_token)),
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

    fn test_directory() -> std::path::PathBuf {
        let unique = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("mko-atomic-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }
}
