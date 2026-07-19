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
    windows_acl::inspect_path(path)
}

#[cfg(windows)]
mod windows_acl {
    use std::{
        ffi::c_void,
        fs, iter,
        mem::size_of,
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            io::AsRawHandle,
        },
        path::Path,
        ptr::{null, null_mut},
        slice,
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree},
        Security::{
            Authorization::{
                EXPLICIT_ACCESS_W, GRANT_ACCESS, GetExplicitEntriesFromAclW, GetNamedSecurityInfoW,
                SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
                TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
            },
            CopySid, DACL_SECURITY_INFORMATION, EqualSid, GetLengthSid,
            GetSecurityDescriptorControl, GetTokenInformation, OWNER_SECURITY_INFORMATION,
            PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    use super::{
        MkoError, WINDOWS_FULL_CONTROL_MASK, WindowsAceInspection, WindowsAclInspection,
        validate_windows_acl_inspection,
    };

    struct OwnedSid {
        words: Vec<usize>,
    }

    impl OwnedSid {
        fn current_user(error_code: &'static str) -> Result<Self, MkoError> {
            let mut token: HANDLE = null_mut();
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(last_api_error(error_code, "open the current process token"));
            }
            let token = TokenHandle(token);
            let mut required = 0;
            unsafe {
                GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required);
            }
            if required == 0 {
                return Err(last_api_error(error_code, "size the current user token"));
            }
            let word_count = (required as usize).div_ceil(size_of::<usize>());
            let mut token_buffer = vec![0usize; word_count];
            if unsafe {
                GetTokenInformation(
                    token.0,
                    TokenUser,
                    token_buffer.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(last_api_error(error_code, "read the current user token"));
            }
            let token_user = unsafe { &*token_buffer.as_ptr().cast::<TOKEN_USER>() };
            let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
            if sid_length == 0 {
                return Err(last_api_error(error_code, "size the current user SID"));
            }
            let mut words = vec![0usize; (sid_length as usize).div_ceil(size_of::<usize>())];
            if unsafe { CopySid(sid_length, words.as_mut_ptr().cast(), token_user.User.Sid) } == 0 {
                return Err(last_api_error(error_code, "copy the current user SID"));
            }
            Ok(Self { words })
        }

        fn as_psid(&self) -> PSID {
            self.words.as_ptr().cast_mut().cast()
        }
    }

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    struct LocalAllocation(*mut c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0);
                }
            }
        }
    }

    pub(super) fn apply_to_path(path: &Path, inherit: bool) -> Result<(), MkoError> {
        let user = OwnedSid::current_user("profile_write_failed")?;
        let acl = owner_only_acl(&user, inherit)?;
        let path_wide = wide_path(path, "profile_write_failed")?;
        let status = unsafe {
            SetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                user.as_psid(),
                null_mut(),
                acl.0.cast(),
                null(),
            )
        };
        win32_status(status, "profile_write_failed", "apply the owner-only ACL")
    }

    pub(super) fn apply_to_file(file: &fs::File) -> Result<(), MkoError> {
        let path = final_path(file)?;
        apply_to_path(&path, false)?;
        validate_windows_acl_inspection(&inspect_path(&path)?)
    }

    pub(super) fn inspect_path(path: &Path) -> Result<WindowsAclInspection, MkoError> {
        let path_wide = wide_path(path, "profile_permissions_invalid")?;
        let mut owner: PSID = null_mut();
        let mut dacl = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        win32_status(
            status,
            "profile_permissions_invalid",
            "read the machine profile ACL",
        )?;
        let descriptor_guard = LocalAllocation(descriptor);
        inspect_descriptor(owner, dacl, descriptor_guard.0)
    }

    fn inspect_descriptor(
        owner: PSID,
        dacl: *mut windows_sys::Win32::Security::ACL,
        descriptor: PSECURITY_DESCRIPTOR,
    ) -> Result<WindowsAclInspection, MkoError> {
        if owner.is_null() || dacl.is_null() || descriptor.is_null() {
            return Err(permission_error(
                "machine profile has a missing owner or DACL",
            ));
        }
        let user = OwnedSid::current_user("profile_permissions_invalid")?;
        let mut control = 0;
        let mut revision = 0;
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(last_api_error(
                "profile_permissions_invalid",
                "inspect machine profile ACL protection",
            ));
        }
        let mut entry_count = 0;
        let mut entries = null_mut();
        let status = unsafe { GetExplicitEntriesFromAclW(dacl, &mut entry_count, &mut entries) };
        win32_status(
            status,
            "profile_permissions_invalid",
            "inspect machine profile ACL entries",
        )?;
        let entries_guard = LocalAllocation(entries.cast());
        let entries = if entry_count == 0 {
            Vec::new()
        } else {
            if entries.is_null() {
                return Err(permission_error(
                    "machine profile ACL entries could not be inspected",
                ));
            }
            unsafe { slice::from_raw_parts(entries, entry_count as usize) }
                .iter()
                .map(|entry| WindowsAceInspection {
                    allows_current_user: (entry.grfAccessMode == SET_ACCESS
                        || entry.grfAccessMode == GRANT_ACCESS)
                        && entry.Trustee.TrusteeForm == TRUSTEE_IS_SID
                        && !entry.Trustee.ptstrName.is_null()
                        && unsafe { EqualSid(entry.Trustee.ptstrName.cast(), user.as_psid()) } != 0,
                    access_mask: entry.grfAccessPermissions,
                })
                .collect()
        };
        drop(entries_guard);

        Ok(WindowsAclInspection {
            owner_is_current_user: unsafe { EqualSid(owner, user.as_psid()) } != 0,
            dacl_is_protected: control & SE_DACL_PROTECTED != 0,
            entries,
        })
    }

    fn final_path(file: &fs::File) -> Result<std::path::PathBuf, MkoError> {
        let handle = file.as_raw_handle().cast();
        let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
        let mut buffer = vec![0u16; 512];
        loop {
            let written = unsafe {
                GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
            };
            if written == 0 {
                return Err(last_api_error(
                    "profile_write_failed",
                    "resolve the temporary machine profile path",
                ));
            }
            if (written as usize) < buffer.len() {
                buffer.truncate(written as usize);
                return Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer,
                )));
            }
            buffer.resize(written as usize, 0);
        }
    }

    fn owner_only_acl(user: &OwnedSid, inherit: bool) -> Result<LocalAllocation, MkoError> {
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: WINDOWS_FULL_CONTROL_MASK,
            grfAccessMode: SET_ACCESS,
            grfInheritance: if inherit {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                0
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user.as_psid().cast(),
            },
        };
        let mut acl = null_mut();
        let status = unsafe { SetEntriesInAclW(1, &entry, null(), &mut acl) };
        win32_status(status, "profile_write_failed", "build the owner-only ACL")?;
        if acl.is_null() {
            return Err(MkoError::new(
                "profile_write_failed",
                "cannot build the owner-only ACL: Windows returned no ACL",
            ));
        }
        Ok(LocalAllocation(acl.cast()))
    }

    fn wide_path(path: &Path, error_code: &'static str) -> Result<Vec<u16>, MkoError> {
        let path_wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect();
        if path_wide[..path_wide.len() - 1].contains(&0) {
            return Err(MkoError::new(
                error_code,
                "machine profile path contains an embedded NUL",
            ));
        }
        Ok(path_wide)
    }

    fn win32_status(status: u32, error_code: &'static str, action: &str) -> Result<(), MkoError> {
        if status == ERROR_SUCCESS {
            return Ok(());
        }
        Err(MkoError::new(
            error_code,
            format!(
                "cannot {action}: {}",
                std::io::Error::from_raw_os_error(status as i32)
            ),
        ))
    }

    fn last_api_error(error_code: &'static str, action: &str) -> MkoError {
        MkoError::new(
            error_code,
            format!("cannot {action}: {}", std::io::Error::last_os_error()),
        )
    }

    fn permission_error(message: &str) -> MkoError {
        MkoError::new("profile_permissions_invalid", message)
    }
}

#[cfg(unix)]
fn set_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| profile_io_error("secure directory for", path, error))
}

#[cfg(windows)]
fn set_owner_private_directory(path: &Path) -> Result<(), MkoError> {
    windows_acl::apply_to_path(path, true)?;
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
    windows_acl::apply_to_file(file)
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
