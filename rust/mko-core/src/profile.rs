use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

use crate::{
    config::{KnowledgeConfig, LocalConfig},
    context::{PlatformEnvironment, Scope},
    error::MkoError,
    path_policy::canonical_directory,
    safe_yaml::validate_yaml_input,
};

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
const PROFILE_DIRECTORY: &str = "mko";
const PROFILE_FILENAME: &str = "profiles.yaml";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfileFile {
    pub schema_version: u32,
    pub default_profile: String,
    pub profiles: BTreeMap<String, PersonalProfile>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalProfile {
    pub repository_root: PathBuf,
    pub provider_root: PathBuf,
    pub scope: Scope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProfileSnapshot {
    pub profile: Option<MachineProfileFile>,
    bytes: Option<Vec<u8>>,
}

pub(crate) struct ProfileMutationLock {
    path: PathBuf,
    _file: fs::File,
}

impl ProfileStore {
    pub fn from_platform(platform: &dyn PlatformEnvironment) -> Result<Self, MkoError> {
        Ok(Self {
            path: platform
                .config_home()?
                .join(PROFILE_DIRECTORY)
                .join(PROFILE_FILENAME),
        })
    }

    pub fn at(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&self) -> Result<Option<MachineProfileFile>, MkoError> {
        Ok(self.read_snapshot()?.profile)
    }

    pub(crate) fn read_snapshot(&self) -> Result<ProfileSnapshot, MkoError> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ProfileSnapshot {
                    profile: None,
                    bytes: None,
                });
            }
            Err(error) => return Err(profile_io_error("read", &self.path, error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MkoError::new(
                "profile_path_invalid",
                "machine profile path must be a regular file and must not be a symlink",
            ));
        }
        ensure_owner_private_directory(self.path.parent().ok_or_else(|| {
            MkoError::new(
                "profile_path_invalid",
                "machine profile path has no parent directory",
            )
        })?)?;
        ensure_owner_private_file(&self.path, &metadata)?;
        let bytes =
            fs::read(&self.path).map_err(|error| profile_io_error("read", &self.path, error))?;
        let input = std::str::from_utf8(&bytes)
            .map_err(|error| MkoError::new("profile_invalid", error.to_string()))?;
        let profile = parse_profile(input)?;
        validate_profile(&profile)?;
        Ok(ProfileSnapshot {
            profile: Some(profile),
            bytes: Some(bytes),
        })
    }

    pub fn write(&self, profile: &MachineProfileFile) -> Result<(), MkoError> {
        let mutation_lock = self.acquire_mutation_lock()?;
        let expected = self.read_snapshot()?;
        self.write_if_unchanged(&mutation_lock, &expected, profile)
    }

    pub(crate) fn acquire_mutation_lock(&self) -> Result<ProfileMutationLock, MkoError> {
        let parent = self.path.parent().ok_or_else(|| {
            MkoError::new(
                "profile_path_invalid",
                "machine profile path has no parent directory",
            )
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| profile_io_error("create directory for", &self.path, error))?;
        ensure_private_directory(parent)?;
        let lock_path = parent.join("setup-profile.lock");
        reject_unsafe_lock_destination(&lock_path)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        configure_private_lock_open(&mut options);
        let file = options
            .open(&lock_path)
            .map_err(|error| profile_io_error("open lock for", &lock_path, error))?;
        set_owner_private_file(&file)?;
        match file.try_lock() {
            Ok(()) => Ok(ProfileMutationLock {
                path: lock_path,
                _file: file,
            }),
            Err(std::fs::TryLockError::WouldBlock) => Err(MkoError::new(
                "setup_profile_locked",
                "another setup or profile mutation is already in progress",
            )),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(MkoError::new("setup_profile_locked", error.to_string()))
            }
        }
    }

    pub(crate) fn write_if_unchanged(
        &self,
        mutation_lock: &ProfileMutationLock,
        expected: &ProfileSnapshot,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError> {
        if mutation_lock.path.parent() != self.path.parent() {
            return Err(MkoError::new(
                "setup_profile_lock_invalid",
                "the profile mutation lock does not protect this profile store",
            ));
        }
        let current = self.read_snapshot()?;
        if current.bytes != expected.bytes {
            return Err(MkoError::new(
                "profile_snapshot_changed",
                "profiles.yaml changed after setup inspection; create and approve a new setup plan",
            ));
        }
        validate_profile(profile)?;
        let serialized = serde_saphyr::to_string(profile)
            .map_err(|error| MkoError::new("profile_invalid", error.to_string()))?;
        let parsed = parse_profile(&serialized)?;
        validate_profile(&parsed)?;

        let parent = self.path.parent().ok_or_else(|| {
            MkoError::new(
                "profile_path_invalid",
                "machine profile path has no parent directory",
            )
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| profile_io_error("create directory for", &self.path, error))?;
        ensure_private_directory(parent)?;
        reject_unsafe_destination(&self.path)?;

        let mut destination = AtomicWriteFile::open(&self.path)
            .map_err(|error| profile_io_error("open temporary file for", &self.path, error))?;
        set_owner_private_file(destination.as_file())?;
        destination
            .write_all(serialized.as_bytes())
            .map_err(|error| profile_io_error("write", &self.path, error))?;
        destination
            .as_file()
            .sync_all()
            .map_err(|error| profile_io_error("sync", &self.path, error))?;
        destination
            .commit()
            .map_err(|error| profile_io_error("replace", &self.path, error))?;
        sync_directory(parent)?;
        Ok(())
    }

    pub fn import_legacy_personal(
        &self,
        repository_root: &Path,
        platform: &dyn PlatformEnvironment,
    ) -> Result<MachineProfileFile, MkoError> {
        if self.read()?.is_some() {
            return Err(MkoError::new(
                "profile_exists",
                "machine profiles already exist; legacy import would replace them",
            ));
        }
        let repository_root = canonical_directory(repository_root, "repository_root_invalid")?;
        let knowledge = KnowledgeConfig::read(&repository_root)?;
        if knowledge.scope != Scope::Personal.as_str() {
            return Err(MkoError::new(
                "scope_conflict",
                "legacy local configuration can be imported only for Personal scope",
            ));
        }
        let legacy_path = platform
            .home_dir()?
            .join(".config")
            .join("mko")
            .join("personal.yaml");
        let legacy = LocalConfig::read(&legacy_path)?;
        let provider_root = canonical_directory(&legacy.provider_root, "provider_root_invalid")?;
        let profile = MachineProfileFile {
            schema_version: PROFILE_SCHEMA_VERSION,
            default_profile: "personal".into(),
            profiles: BTreeMap::from([(
                "personal".into(),
                PersonalProfile {
                    repository_root,
                    provider_root,
                    scope: Scope::Personal,
                },
            )]),
        };
        self.write(&profile)?;
        Ok(profile)
    }
}

fn reject_unsafe_lock_destination(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MkoError::new(
            "profile_path_invalid",
            "machine profile lock must be a regular file and must not be a symlink",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(profile_io_error("inspect", path, error)),
    }
}

#[cfg(unix)]
fn configure_private_lock_open(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_private_lock_open(options: &mut fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_private_lock_open(_options: &mut fs::OpenOptions) {}

fn validate_profile(profile: &MachineProfileFile) -> Result<(), MkoError> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION
        || profile.default_profile.trim().is_empty()
        || !profile.profiles.contains_key(&profile.default_profile)
        || profile.profiles.iter().any(|(name, value)| {
            name.trim().is_empty()
                || value.repository_root.as_os_str().is_empty()
                || value.repository_root.is_relative()
                || value.provider_root.as_os_str().is_empty()
                || value.provider_root.is_relative()
        })
    {
        return Err(MkoError::new(
            "profile_invalid",
            "machine profile schema, default, names, and paths must be valid",
        ));
    }
    Ok(())
}

fn parse_profile(input: &str) -> Result<MachineProfileFile, MkoError> {
    validate_yaml_input(input)?;
    serde_saphyr::from_str(input)
        .map_err(|error| MkoError::new("profile_invalid", error.to_string()))
}

fn reject_unsafe_destination(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MkoError::new(
            "profile_path_invalid",
            "machine profile path must be a regular file and must not be a symlink",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(profile_io_error("inspect", path, error)),
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| profile_io_error("inspect directory for", path, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(MkoError::new(
            "profile_path_invalid",
            "machine profile directory must be a real directory",
        ));
    }
    set_owner_private_directory(path)
}

#[cfg(any(test, windows))]
const WINDOWS_FULL_CONTROL_MASK: u32 = 0x001f_01ff;

#[cfg(any(test, windows))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct WindowsAceInspection {
    allows_current_user: bool,
    access_mask: u32,
}

#[cfg(any(test, windows))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct WindowsAclInspection {
    owner_is_current_user: bool,
    dacl_is_protected: bool,
    entries: Vec<WindowsAceInspection>,
}

#[cfg(any(test, windows))]
fn validate_windows_acl_inspection(inspection: &WindowsAclInspection) -> Result<(), MkoError> {
    let is_private = inspection.owner_is_current_user
        && inspection.dacl_is_protected
        && inspection.entries.as_slice()
            == [WindowsAceInspection {
                allows_current_user: true,
                access_mask: WINDOWS_FULL_CONTROL_MASK,
            }];
    if !is_private {
        return Err(MkoError::new(
            "profile_permissions_invalid",
            "machine profile ACL must grant full control only to the current user",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn inspect_windows_acl(path: &Path) -> Result<WindowsAclInspection, MkoError> {
    mko_windows_acl::inspect_path(path)
        .map(windows_acl_inspection)
        .map_err(windows_acl_error)
}

#[cfg(windows)]
fn windows_acl_inspection(inspection: mko_windows_acl::AclInspection) -> WindowsAclInspection {
    WindowsAclInspection {
        owner_is_current_user: inspection.owner_is_current_user,
        dacl_is_protected: inspection.dacl_is_protected,
        entries: inspection
            .entries
            .into_iter()
            .map(|entry| WindowsAceInspection {
                allows_current_user: entry.allows_current_user,
                access_mask: entry.access_mask,
            })
            .collect(),
    }
}

#[cfg(windows)]
fn windows_acl_error(error: mko_windows_acl::Error) -> MkoError {
    let error_code = match error.kind() {
        mko_windows_acl::ErrorKind::Write => "profile_write_failed",
        mko_windows_acl::ErrorKind::Permissions => "profile_permissions_invalid",
    };
    MkoError::new(error_code, error.to_string())
}

#[cfg(unix)]
fn set_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| profile_io_error("secure directory for", path, error))
}

#[cfg(windows)]
fn set_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_path(
        path,
        mko_windows_acl::Inheritance::ContainersAndObjects,
    )
    .map_err(windows_acl_error)?;
    validate_windows_acl_inspection(&inspect_windows_acl(path)?)
}

#[cfg(not(any(unix, windows)))]
fn set_owner_private_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(windows)]
fn ensure_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl_inspection(&inspect_windows_acl(path)?)
}

#[cfg(not(windows))]
fn ensure_owner_private_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_private_file(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("profile_write_failed", error.to_string()))
}

#[cfg(windows)]
fn set_owner_private_file(file: &fs::File) -> Result<(), MkoError> {
    let inspection = mko_windows_acl::apply_owner_only_to_file(file).map_err(windows_acl_error)?;
    validate_windows_acl_inspection(&windows_acl_inspection(inspection))
}

#[cfg(not(any(unix, windows)))]
fn set_owner_private_file(_file: &fs::File) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_private_file(_path: &Path, metadata: &fs::Metadata) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MkoError::new(
            "profile_permissions_invalid",
            "machine profile must be readable and writable only by its owner",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_owner_private_file(path: &Path, _metadata: &fs::Metadata) -> Result<(), MkoError> {
    validate_windows_acl_inspection(&inspect_windows_acl(path)?)
}

#[cfg(not(any(unix, windows)))]
fn ensure_owner_private_file(_path: &Path, _metadata: &fs::Metadata) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), MkoError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| profile_io_error("sync directory for", path, error))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

fn profile_io_error(action: &str, path: &Path, error: std::io::Error) -> MkoError {
    MkoError::new(
        "profile_write_failed",
        format!(
            "cannot {action} machine profile {}: {error}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        WINDOWS_FULL_CONTROL_MASK, WindowsAceInspection, WindowsAclInspection,
        validate_windows_acl_inspection,
    };

    #[test]
    fn windows_acl_policy_accepts_one_protected_current_user_entry() {
        let inspection = WindowsAclInspection {
            owner_is_current_user: true,
            dacl_is_protected: true,
            entries: vec![WindowsAceInspection {
                allows_current_user: true,
                access_mask: WINDOWS_FULL_CONTROL_MASK,
            }],
        };

        validate_windows_acl_inspection(&inspection).unwrap();
    }

    #[test]
    fn windows_acl_policy_rejects_inheritance_and_additional_principals() {
        let inherited = WindowsAclInspection {
            owner_is_current_user: true,
            dacl_is_protected: false,
            entries: vec![WindowsAceInspection {
                allows_current_user: true,
                access_mask: WINDOWS_FULL_CONTROL_MASK,
            }],
        };
        let additional_principal = WindowsAclInspection {
            owner_is_current_user: true,
            dacl_is_protected: true,
            entries: vec![
                WindowsAceInspection {
                    allows_current_user: true,
                    access_mask: WINDOWS_FULL_CONTROL_MASK,
                },
                WindowsAceInspection {
                    allows_current_user: false,
                    access_mask: WINDOWS_FULL_CONTROL_MASK,
                },
            ],
        };

        assert_eq!(
            validate_windows_acl_inspection(&inherited)
                .unwrap_err()
                .code(),
            "profile_permissions_invalid"
        );
        assert_eq!(
            validate_windows_acl_inspection(&additional_principal)
                .unwrap_err()
                .code(),
            "profile_permissions_invalid"
        );
    }

    #[test]
    fn profile_compare_and_swap_rejects_a_stale_full_map() {
        use std::{collections::BTreeMap, fs};

        use tempfile::TempDir;

        use super::{MachineProfileFile, PersonalProfile, ProfileStore};
        use crate::context::Scope;

        let root = TempDir::new().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        let other_repository = root.path().join("other-repository");
        let other_provider = root.path().join("other-provider");
        for path in [&repository, &provider, &other_repository, &other_provider] {
            fs::create_dir_all(path).unwrap();
        }
        let store = ProfileStore::at(root.path().join("config/mko/profiles.yaml"));
        let initial = MachineProfileFile {
            schema_version: 1,
            default_profile: "personal".into(),
            profiles: BTreeMap::from([(
                "personal".into(),
                PersonalProfile {
                    repository_root: repository.clone(),
                    provider_root: provider.clone(),
                    scope: Scope::Personal,
                },
            )]),
        };
        store.write(&initial).unwrap();

        let mutation_lock = store.acquire_mutation_lock().unwrap();
        let expected = store.read_snapshot().unwrap();
        let mut concurrently_changed = initial.clone();
        concurrently_changed.profiles.insert(
            "other".into(),
            PersonalProfile {
                repository_root: other_repository,
                provider_root: other_provider,
                scope: Scope::Personal,
            },
        );
        fs::write(
            store.path(),
            serde_saphyr::to_string(&concurrently_changed).unwrap(),
        )
        .unwrap();

        let mut stale_replacement = initial;
        stale_replacement.default_profile = "personal".into();
        let error = store
            .write_if_unchanged(&mutation_lock, &expected, &stale_replacement)
            .unwrap_err();
        assert_eq!(error.code(), "profile_snapshot_changed");
        assert_eq!(store.read().unwrap(), Some(concurrently_changed));
    }

    #[cfg(windows)]
    #[test]
    fn profile_store_applies_native_windows_owner_only_acls() {
        use std::{collections::BTreeMap, fs};

        use tempfile::TempDir;

        use super::{MachineProfileFile, PersonalProfile, ProfileStore, inspect_windows_acl};
        use crate::context::Scope;

        let root = TempDir::new().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        fs::create_dir_all(&repository).unwrap();
        fs::create_dir_all(&provider).unwrap();
        let store = ProfileStore::at(root.path().join("config/mko/profiles.yaml"));
        let profile = MachineProfileFile {
            schema_version: 1,
            default_profile: "personal".into(),
            profiles: BTreeMap::from([(
                "personal".into(),
                PersonalProfile {
                    repository_root: repository,
                    provider_root: provider,
                    scope: Scope::Personal,
                },
            )]),
        };

        store.write(&profile).unwrap();

        assert_eq!(store.read().unwrap(), Some(profile));
        validate_windows_acl_inspection(
            &inspect_windows_acl(store.path().parent().unwrap()).unwrap(),
        )
        .unwrap();
        validate_windows_acl_inspection(&inspect_windows_acl(store.path()).unwrap()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_post_commit_sync_never_reports_failure_after_replacement() {
        use std::path::Path;

        super::sync_directory(Path::new(
            r"Z:\mko-path-that-must-not-be-opened-after-commit",
        ))
        .unwrap();
    }
}
