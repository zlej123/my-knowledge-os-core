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
        ensure_owner_private_file(&metadata)?;
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

#[cfg(unix)]
fn set_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| profile_io_error("secure directory for", path, error))
}

#[cfg(not(unix))]
fn set_owner_private_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(unix)]
fn set_owner_private_file(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("profile_write_failed", error.to_string()))
}

#[cfg(not(unix))]
fn set_owner_private_file(_file: &fs::File) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_private_file(metadata: &fs::Metadata) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MkoError::new(
            "profile_permissions_invalid",
            "machine profile must be readable and writable only by its owner",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_private_file(_metadata: &fs::Metadata) -> Result<(), MkoError> {
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), MkoError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| profile_io_error("sync directory for", path, error))
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
