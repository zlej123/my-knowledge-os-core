#![cfg(windows)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    error,
    ffi::c_void,
    fmt, fs, iter,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetExplicitEntriesFromAclW, GetNamedSecurityInfoW,
            SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
            TRUSTEE_IS_USER, TRUSTEE_W,
        },
        CopySid, DACL_SECURITY_INFORMATION, EqualSid, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ADD_FILE, FILE_ADD_SUBDIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED, FILE_READ_DATA, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFinalPathNameByHandleW, OPEN_EXISTING,
        VOLUME_NAME_DOS,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

const FULL_CONTROL_MASK: u32 = 0x001f_01ff;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Inheritance {
    None,
    ContainersAndObjects,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AceInspection {
    pub allows_current_user: bool,
    pub access_mask: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclInspection {
    pub owner_is_current_user: bool,
    pub dacl_is_protected: bool,
    pub entries: Vec<AceInspection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveAccess {
    ReadDirectory,
    WriteDirectory,
    ReadFile,
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Write,
    Permissions,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl error::Error for Error {}

struct OwnedSid {
    words: Vec<usize>,
}

impl OwnedSid {
    fn current_user(kind: ErrorKind) -> Result<Self, Error> {
        let mut token: HANDLE = null_mut();
        // SAFETY: `token` points to writable storage and the pseudo process handle is valid.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_api_error(kind, "open the current process token"));
        }
        let token = TokenHandle(token);
        let mut required = 0;
        // SAFETY: A null buffer with length zero is the documented size-query call.
        unsafe {
            GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut required);
        }
        if required == 0 {
            return Err(last_api_error(kind, "size the current user token"));
        }
        let word_count = (required as usize).div_ceil(size_of::<usize>());
        let mut token_buffer = vec![0usize; word_count];
        // SAFETY: The aligned buffer is at least `required` bytes and remains alive for the call.
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
            return Err(last_api_error(kind, "read the current user token"));
        }
        // SAFETY: A successful TokenUser query initialized a TOKEN_USER at the aligned buffer start.
        let token_user = unsafe { &*token_buffer.as_ptr().cast::<TOKEN_USER>() };
        // SAFETY: Windows supplied this SID pointer in the successful token query.
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(last_api_error(kind, "size the current user SID"));
        }
        let mut words = vec![0usize; (sid_length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: The destination is suitably aligned and sized from GetLengthSid; the source is live.
        if unsafe { CopySid(sid_length, words.as_mut_ptr().cast(), token_user.User.Sid) } == 0 {
            return Err(last_api_error(kind, "copy the current user SID"));
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
        // SAFETY: This handle was returned by OpenProcessToken and is owned by this guard.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Windows allocated this pointer for release with LocalFree; this guard owns it.
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

pub fn apply_owner_only_to_path(path: &Path, inheritance: Inheritance) -> Result<(), Error> {
    let user = OwnedSid::current_user(ErrorKind::Write)?;
    let acl = owner_only_acl(&user, inheritance)?;
    let path_wide = wide_path(path, ErrorKind::Write)?;
    // SAFETY: All pointers reference live allocations for the call and the path is NUL-terminated.
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
    win32_status(status, ErrorKind::Write, "apply the owner-only ACL")
}

pub fn apply_owner_only_to_file(file: &fs::File) -> Result<AclInspection, Error> {
    let path = final_path(file)?;
    apply_owner_only_to_path(&path, Inheritance::None)?;
    inspect_path(&path)
}

pub fn inspect_path(path: &Path) -> Result<AclInspection, Error> {
    let path_wide = wide_path(path, ErrorKind::Permissions)?;
    let mut owner: PSID = null_mut();
    let mut dacl = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: Output pointers are writable and the NUL-terminated path remains live for the call.
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
        ErrorKind::Permissions,
        "read the machine profile ACL",
    )?;
    let descriptor_guard = LocalAllocation(descriptor);
    inspect_descriptor(owner, dacl, descriptor_guard.0)
}

pub fn check_effective_access(path: &Path, access: EffectiveAccess) -> Result<bool, Error> {
    let path_wide = wide_path(path, ErrorKind::Permissions)?;
    let desired_access = match access {
        EffectiveAccess::ReadDirectory => FILE_LIST_DIRECTORY,
        EffectiveAccess::WriteDirectory => FILE_ADD_FILE | FILE_ADD_SUBDIRECTORY,
        EffectiveAccess::ReadFile => FILE_READ_DATA,
    };
    // SAFETY: The path is NUL-terminated, the call only opens an existing directory for an ACL
    // access check, and no security attributes or template handle are supplied.
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            desired_access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Ok(false);
        }
        return Err(message_error(
            ErrorKind::Permissions,
            format!("cannot evaluate current-user directory access: {error}"),
        ));
    }
    drop(TokenHandle(handle));
    Ok(true)
}

fn inspect_descriptor(
    owner: PSID,
    dacl: *mut windows_sys::Win32::Security::ACL,
    descriptor: PSECURITY_DESCRIPTOR,
) -> Result<AclInspection, Error> {
    if owner.is_null() || dacl.is_null() || descriptor.is_null() {
        return Err(message_error(
            ErrorKind::Permissions,
            "machine profile has a missing owner or DACL",
        ));
    }
    let user = OwnedSid::current_user(ErrorKind::Permissions)?;
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: `descriptor` is live and both output pointers refer to writable local values.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(last_api_error(
            ErrorKind::Permissions,
            "inspect machine profile ACL protection",
        ));
    }
    let mut entry_count = 0;
    let mut entries = null_mut();
    // SAFETY: `dacl` belongs to the live descriptor and output pointers are writable.
    let status = unsafe { GetExplicitEntriesFromAclW(dacl, &mut entry_count, &mut entries) };
    win32_status(
        status,
        ErrorKind::Permissions,
        "inspect machine profile ACL entries",
    )?;
    let entries_guard = LocalAllocation(entries.cast());
    let entries = if entry_count == 0 {
        Vec::new()
    } else {
        if entries.is_null() {
            return Err(message_error(
                ErrorKind::Permissions,
                "machine profile ACL entries could not be inspected",
            ));
        }
        // SAFETY: Windows returned an array of `entry_count` entries owned by `entries_guard`.
        unsafe { slice::from_raw_parts(entries, entry_count as usize) }
            .iter()
            .map(|entry| AceInspection {
                allows_current_user: (entry.grfAccessMode == SET_ACCESS
                    || entry.grfAccessMode == GRANT_ACCESS)
                    && entry.Trustee.TrusteeForm == TRUSTEE_IS_SID
                    && !entry.Trustee.ptstrName.is_null()
                    // SAFETY: Both values are live SID pointers for this call.
                    && unsafe { EqualSid(entry.Trustee.ptstrName.cast(), user.as_psid()) } != 0,
                access_mask: entry.grfAccessPermissions,
            })
            .collect()
    };
    drop(entries_guard);

    Ok(AclInspection {
        // SAFETY: `owner` belongs to the live descriptor and `user` owns a valid copied SID.
        owner_is_current_user: unsafe { EqualSid(owner, user.as_psid()) } != 0,
        dacl_is_protected: control & SE_DACL_PROTECTED != 0,
        entries,
    })
}

fn final_path(file: &fs::File) -> Result<PathBuf, Error> {
    let handle = file.as_raw_handle().cast();
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: The OS file handle is borrowed for the call and the buffer is writable to its len.
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
        };
        if written == 0 {
            return Err(last_api_error(
                ErrorKind::Write,
                "resolve the temporary machine profile path",
            ));
        }
        if (written as usize) < buffer.len() {
            buffer.truncate(written as usize);
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
        }
        buffer.resize(written as usize, 0);
    }
}

fn owner_only_acl(user: &OwnedSid, inheritance: Inheritance) -> Result<LocalAllocation, Error> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FULL_CONTROL_MASK,
        grfAccessMode: SET_ACCESS,
        grfInheritance: match inheritance {
            Inheritance::None => 0,
            Inheritance::ContainersAndObjects => SUB_CONTAINERS_AND_OBJECTS_INHERIT,
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
    // SAFETY: `entry` and its SID remain live for the call; `acl` is writable output storage.
    let status = unsafe { SetEntriesInAclW(1, &entry, null(), &mut acl) };
    win32_status(status, ErrorKind::Write, "build the owner-only ACL")?;
    if acl.is_null() {
        return Err(message_error(
            ErrorKind::Write,
            "cannot build the owner-only ACL: Windows returned no ACL",
        ));
    }
    Ok(LocalAllocation(acl.cast()))
}

fn wide_path(path: &Path, kind: ErrorKind) -> Result<Vec<u16>, Error> {
    let path_wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    if path_wide[..path_wide.len() - 1].contains(&0) {
        return Err(message_error(
            kind,
            "machine profile path contains an embedded NUL",
        ));
    }
    Ok(path_wide)
}

fn win32_status(status: u32, kind: ErrorKind, action: &str) -> Result<(), Error> {
    if status == ERROR_SUCCESS {
        return Ok(());
    }
    Err(message_error(
        kind,
        format!(
            "cannot {action}: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ),
    ))
}

fn last_api_error(kind: ErrorKind, action: &str) -> Error {
    message_error(
        kind,
        format!("cannot {action}: {}", std::io::Error::last_os_error()),
    )
}

fn message_error(kind: ErrorKind, message: impl Into<String>) -> Error {
    Error {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{
        AclInspection, EffectiveAccess, Error, ErrorKind, FULL_CONTROL_MASK, Inheritance,
        apply_owner_only_to_file, apply_owner_only_to_path, check_effective_access, inspect_path,
    };

    #[test]
    fn public_acl_operations_are_safe_function_pointers() {
        let _: fn(&Path, Inheritance) -> Result<(), Error> = apply_owner_only_to_path;
        let _: fn(&fs::File) -> Result<AclInspection, Error> = apply_owner_only_to_file;
        let _: fn(&Path) -> Result<AclInspection, Error> = inspect_path;
    }

    #[test]
    fn applies_owner_only_acl_to_directory_and_file() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("profiles");
        fs::create_dir(&directory).unwrap();
        apply_owner_only_to_path(&directory, Inheritance::ContainersAndObjects).unwrap();
        assert_owner_only(&inspect_path(&directory).unwrap());

        let file = fs::File::create(directory.join("profiles.yaml")).unwrap();
        let inspection = apply_owner_only_to_file(&file).unwrap();
        assert_owner_only(&inspection);

        let nul_path = Path::new("profile\0.yaml");
        assert_eq!(
            apply_owner_only_to_path(nul_path, Inheritance::None)
                .unwrap_err()
                .kind(),
            ErrorKind::Write
        );
        assert_eq!(
            inspect_path(nul_path).unwrap_err().kind(),
            ErrorKind::Permissions
        );
    }

    fn assert_owner_only(inspection: &AclInspection) {
        assert!(inspection.owner_is_current_user);
        assert!(inspection.dacl_is_protected);
        assert_eq!(inspection.entries.len(), 1);
        assert!(inspection.entries[0].allows_current_user);
        assert_eq!(inspection.entries[0].access_mask, FULL_CONTROL_MASK);
    }

    #[test]
    fn current_user_effective_directory_access_is_evaluated_without_a_write_probe() {
        let root = tempfile::tempdir().unwrap();

        assert!(check_effective_access(root.path(), EffectiveAccess::ReadDirectory).unwrap());
        assert!(check_effective_access(root.path(), EffectiveAccess::WriteDirectory).unwrap());
        let file = root.path().join("paper.pdf");
        fs::write(&file, b"%PDF-1.7").unwrap();
        assert!(check_effective_access(&file, EffectiveAccess::ReadFile).unwrap());
    }
}
