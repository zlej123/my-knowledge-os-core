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
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
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
        let input = fs::read_to_string(&self.path)
            .map_err(|error| profile_io_error("read", &self.path, error))?;
        let profile = parse_profile(&input)?;
        validate_profile(&profile)?;
        Ok(Some(profile))
    }

    pub fn write(&self, profile: &MachineProfileFile) -> Result<(), MkoError> {
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
