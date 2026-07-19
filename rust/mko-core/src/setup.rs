use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    config::KnowledgeConfig,
    context::{ContextSource, PlatformEnvironment, ResolvedPersonalContext, Scope},
    error::MkoError,
    hooks::{HookState, inspect_hook, install_hooks},
    path_policy::canonical_directory,
    profile::{MachineProfileFile, PROFILE_SCHEMA_VERSION, PersonalProfile, ProfileStore},
};

const PROFILE_NAME: &str = "personal";
const INBOX_COMPONENTS: [&str; 3] = ["My-Knowledge-OS-Assets", "personal", "inbox"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownDriveRoot {
    pub account_label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupRequest {
    repository_root: PathBuf,
    drive_root: Option<PathBuf>,
}

impl SetupRequest {
    pub fn new(repository_root: impl AsRef<Path>) -> Self {
        Self {
            repository_root: repository_root.as_ref().to_path_buf(),
            drive_root: None,
        }
    }

    pub fn with_drive_root(mut self, drive_root: impl AsRef<Path>) -> Self {
        self.drive_root = Some(drive_root.as_ref().to_path_buf());
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SetupStep {
    Inbox,
    Profile,
    Hook,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupFailure {
    pub step: SetupStep,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupOutcome {
    pub completed_steps: Vec<SetupStep>,
    pub changed_steps: Vec<SetupStep>,
    pub incomplete_steps: Vec<SetupStep>,
    pub failure: Option<SetupFailure>,
}

impl SetupOutcome {
    pub fn is_complete(&self) -> bool {
        self.failure.is_none() && self.incomplete_steps.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct SetupPreflight {
    context: ResolvedPersonalContext,
    store: ProfileStore,
    profile: MachineProfileFile,
    inbox_needs_create: bool,
    profile_needs_write: bool,
    hook_needs_install: bool,
}

impl SetupPreflight {
    pub fn context(&self) -> &ResolvedPersonalContext {
        &self.context
    }
}

pub trait SetupWriter {
    fn create_inbox(&self, path: &Path) -> Result<(), MkoError>;
    fn write_profile(
        &self,
        store: &ProfileStore,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError>;
    fn install_hook(&self, repository_root: &Path) -> Result<(), MkoError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemSetupWriter;

impl SetupWriter for SystemSetupWriter {
    fn create_inbox(&self, path: &Path) -> Result<(), MkoError> {
        fs::create_dir_all(path).map_err(|error| {
            MkoError::new(
                "provider_create_failed",
                format!("cannot create Personal Inbox {}: {error}", path.display()),
            )
        })
    }

    fn write_profile(
        &self,
        store: &ProfileStore,
        profile: &MachineProfileFile,
    ) -> Result<(), MkoError> {
        store.write(profile)
    }

    fn install_hook(&self, repository_root: &Path) -> Result<(), MkoError> {
        install_hooks(repository_root).map(|_| ())
    }
}

pub fn detect_google_drive_roots(
    platform: &dyn PlatformEnvironment,
) -> Result<Vec<KnownDriveRoot>, MkoError> {
    let home = platform.home_dir()?;
    let mut candidates = Vec::new();
    detect_macos_roots(&home, &mut candidates)?;

    if let Some(user_profile) = platform.environment_value(OsStr::new("USERPROFILE")) {
        let user_profile = PathBuf::from(user_profile);
        candidates.push((
            "Google Drive".into(),
            user_profile.join("Google Drive/My Drive"),
        ));
        candidates.push(("My Drive".into(), user_profile.join("My Drive")));
    }
    for name in ["GOOGLE_DRIVE", "GoogleDrive"] {
        if let Some(root) = platform.environment_value(OsStr::new(name)) {
            candidates.push((name.into(), PathBuf::from(root)));
        }
    }

    let mut canonical_roots = BTreeSet::new();
    let mut roots = Vec::new();
    for (account_label, path) in candidates {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(MkoError::new(
                    "drive_detection_failed",
                    format!(
                        "cannot inspect known Drive root {}: {error}",
                        path.display()
                    ),
                ));
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let canonical = canonical_directory(&path, "drive_detection_failed")?;
        if canonical_roots.insert(canonical) {
            roots.push(KnownDriveRoot {
                account_label,
                path,
            });
        }
    }
    roots.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(roots)
}

pub fn preflight_setup(
    request: SetupRequest,
    platform: &dyn PlatformEnvironment,
) -> Result<SetupPreflight, MkoError> {
    let repository_root = canonical_directory(&request.repository_root, "repository_root_invalid")?;
    let knowledge = KnowledgeConfig::read(&repository_root)?;
    if knowledge.scope != Scope::Personal.as_str() {
        return Err(MkoError::new(
            "scope_conflict",
            "setup supports only a Personal knowledge base",
        ));
    }

    let known_roots = detect_google_drive_roots(platform)?;
    let selected = select_drive_root(request.drive_root.as_deref(), &known_roots)?;
    let account_root = canonical_directory(&selected.path, "provider_root_invalid")?;
    ensure_writable_directory(&account_root)?;
    let inbox = fixed_inbox_path(&selected.path);
    let inbox_exists = inspect_inbox_path(&account_root, &selected.path, &inbox)?;

    let store = ProfileStore::from_platform(platform)?;
    let current_profile = store.read()?;
    inspect_profile_parent(&store)?;
    let profile = desired_profile(current_profile.as_ref(), &repository_root, &inbox);
    let profile_needs_write = current_profile.as_ref() != Some(&profile);

    let hook = inspect_hook(&repository_root)?;
    let context = ResolvedPersonalContext {
        repository_root,
        provider_root: inbox,
        provider_type: knowledge.provider.r#type,
        profile_name: PROFILE_NAME.into(),
        scope: Scope::Personal,
        source: ContextSource::Profile,
    };

    Ok(SetupPreflight {
        context,
        store,
        profile,
        inbox_needs_create: !inbox_exists,
        profile_needs_write,
        hook_needs_install: hook.state != HookState::Managed,
    })
}

pub fn apply_setup(
    preflight: SetupPreflight,
    writer: &dyn SetupWriter,
) -> Result<SetupOutcome, MkoError> {
    let mut completed_steps = Vec::new();
    let mut changed_steps = Vec::new();
    let needs_change = [
        (SetupStep::Inbox, preflight.inbox_needs_create),
        (SetupStep::Profile, preflight.profile_needs_write),
        (SetupStep::Hook, preflight.hook_needs_install),
    ];
    for (step, needs_change) in needs_change {
        if !needs_change {
            completed_steps.push(step);
        }
    }

    if preflight.inbox_needs_create {
        if let Err(error) = writer.create_inbox(&preflight.context.provider_root) {
            return Ok(failed_outcome(
                SetupStep::Inbox,
                error,
                completed_steps,
                changed_steps,
            ));
        }
        completed_steps.push(SetupStep::Inbox);
        changed_steps.push(SetupStep::Inbox);
    }
    if preflight.profile_needs_write {
        if let Err(error) = writer.write_profile(&preflight.store, &preflight.profile) {
            return Ok(failed_outcome(
                SetupStep::Profile,
                error,
                completed_steps,
                changed_steps,
            ));
        }
        completed_steps.push(SetupStep::Profile);
        changed_steps.push(SetupStep::Profile);
    }
    if preflight.hook_needs_install {
        if let Err(error) = writer.install_hook(&preflight.context.repository_root) {
            return Ok(failed_outcome(
                SetupStep::Hook,
                error,
                completed_steps,
                changed_steps,
            ));
        }
        completed_steps.push(SetupStep::Hook);
        changed_steps.push(SetupStep::Hook);
    }

    completed_steps.sort();
    changed_steps.sort();
    Ok(SetupOutcome {
        completed_steps,
        changed_steps,
        incomplete_steps: Vec::new(),
        failure: None,
    })
}

fn failed_outcome(
    step: SetupStep,
    error: MkoError,
    mut completed_steps: Vec<SetupStep>,
    changed_steps: Vec<SetupStep>,
) -> SetupOutcome {
    completed_steps.sort();
    let incomplete_steps = [SetupStep::Inbox, SetupStep::Profile, SetupStep::Hook]
        .into_iter()
        .filter(|candidate| !completed_steps.contains(candidate))
        .collect();
    SetupOutcome {
        completed_steps,
        changed_steps,
        incomplete_steps,
        failure: Some(SetupFailure {
            step,
            code: error.code().into(),
            message: error.message().into(),
        }),
    }
}

fn detect_macos_roots(
    home: &Path,
    candidates: &mut Vec<(String, PathBuf)>,
) -> Result<(), MkoError> {
    let cloud_storage = home.join("Library/CloudStorage");
    let entries = match fs::read_dir(&cloud_storage) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(MkoError::new(
                "drive_detection_failed",
                format!("cannot inspect {}: {error}", cloud_storage.display()),
            ));
        }
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| MkoError::new("drive_detection_failed", error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(account_label) = name.strip_prefix("GoogleDrive-") else {
            continue;
        };
        if account_label.is_empty() {
            continue;
        }
        candidates.push((account_label.into(), entry.path().join("My Drive")));
    }
    Ok(())
}

fn select_drive_root(
    selected: Option<&Path>,
    known_roots: &[KnownDriveRoot],
) -> Result<KnownDriveRoot, MkoError> {
    if known_roots.is_empty() {
        return Err(MkoError::new(
            "drive_root_not_found",
            "no platform-known Google Drive account root was found",
        ));
    }
    if let Some(selected) = selected {
        let selected = canonical_directory(selected, "drive_root_unknown")?;
        for known in known_roots {
            let known_canonical = canonical_directory(&known.path, "drive_detection_failed")?;
            if selected == known_canonical {
                return Ok(known.clone());
            }
        }
        return Err(MkoError::new(
            "drive_root_unknown",
            "selected root is not one of the bounded platform-known Google Drive roots",
        ));
    }
    if known_roots.len() != 1 {
        return Err(MkoError::new(
            "drive_root_ambiguous",
            "multiple Google Drive accounts were found; select one explicitly",
        ));
    }
    Ok(known_roots[0].clone())
}

fn fixed_inbox_path(account_root: &Path) -> PathBuf {
    INBOX_COMPONENTS
        .into_iter()
        .fold(account_root.to_path_buf(), |path, component| {
            path.join(component)
        })
}

fn inspect_inbox_path(
    canonical_account_root: &Path,
    selected_account_root: &Path,
    inbox: &Path,
) -> Result<bool, MkoError> {
    let mut current = selected_account_root.to_path_buf();
    for component in INBOX_COMPONENTS {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err(MkoError::new(
                        "provider_root_invalid",
                        format!("{} must be a directory", current.display()),
                    ));
                }
                let resolved = fs::canonicalize(&current)
                    .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
                if !resolved.starts_with(canonical_account_root) || !resolved.is_dir() {
                    return Err(MkoError::new(
                        "provider_root_invalid",
                        "Personal Inbox path escapes the selected Google Drive account",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = current.parent().ok_or_else(|| {
                    MkoError::new("provider_root_invalid", "Inbox path has no parent")
                })?;
                let existing_parent = nearest_existing_directory(parent)?;
                ensure_writable_directory(&existing_parent)?;
                return Ok(false);
            }
            Err(error) => {
                return Err(MkoError::new(
                    "provider_root_invalid",
                    format!("cannot inspect {}: {error}", current.display()),
                ));
            }
        }
    }
    ensure_writable_directory(inbox)?;
    Ok(true)
}

fn nearest_existing_directory(path: &Path) -> Result<PathBuf, MkoError> {
    let mut candidate = path;
    loop {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Ok(candidate.to_path_buf());
            }
            Ok(_) => {
                return Err(MkoError::new(
                    "provider_root_invalid",
                    format!("{} must be a real directory", candidate.display()),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                candidate = candidate.parent().ok_or_else(|| {
                    MkoError::new(
                        "provider_root_invalid",
                        "provider path has no existing parent",
                    )
                })?;
            }
            Err(error) => {
                return Err(MkoError::new("provider_root_invalid", error.to_string()));
            }
        }
    }
}

fn ensure_writable_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::metadata(path)
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
    if !metadata.is_dir() {
        return Err(MkoError::new(
            "provider_root_invalid",
            "provider path must be a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o222 == 0 {
            return Err(MkoError::new(
                "provider_permissions_invalid",
                format!("provider directory is not writable: {}", path.display()),
            ));
        }
    }
    if metadata.permissions().readonly() {
        return Err(MkoError::new(
            "provider_permissions_invalid",
            format!("provider directory is not writable: {}", path.display()),
        ));
    }
    Ok(())
}

fn inspect_profile_parent(store: &ProfileStore) -> Result<(), MkoError> {
    let parent = store.path().parent().ok_or_else(|| {
        MkoError::new(
            "profile_path_invalid",
            "machine profile path has no parent directory",
        )
    })?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            ensure_private_profile_directory(parent, &metadata)
        }
        Ok(_) => Err(MkoError::new(
            "profile_path_invalid",
            "machine profile directory must be a real directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MkoError::new(
            "profile_write_failed",
            format!("cannot inspect {}: {error}", parent.display()),
        )),
    }
}

#[cfg(unix)]
fn ensure_private_profile_directory(_path: &Path, metadata: &fs::Metadata) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(MkoError::new(
            "profile_permissions_invalid",
            "machine profile directory must be accessible only by its owner",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_private_profile_directory(path: &Path, _metadata: &fs::Metadata) -> Result<(), MkoError> {
    let inspection = mko_windows_acl::inspect_path(path)
        .map_err(|error| MkoError::new("profile_permissions_invalid", error.to_string()))?;
    validate_windows_profile_acl(&inspection)
}

#[cfg(windows)]
fn validate_windows_profile_acl(
    inspection: &mko_windows_acl::AclInspection,
) -> Result<(), MkoError> {
    const FULL_CONTROL_MASK: u32 = 0x001f_01ff;

    let is_private = inspection.owner_is_current_user
        && inspection.dacl_is_protected
        && inspection.entries.len() == 1
        && inspection.entries[0].allows_current_user
        && inspection.entries[0].access_mask == FULL_CONTROL_MASK;
    if !is_private {
        return Err(MkoError::new(
            "profile_permissions_invalid",
            "machine profile directory ACL must grant full control only to the current user",
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_profile_directory(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), MkoError> {
    Ok(())
}

fn desired_profile(
    current: Option<&MachineProfileFile>,
    repository_root: &Path,
    provider_root: &Path,
) -> MachineProfileFile {
    let mut profiles = current
        .map(|profile| profile.profiles.clone())
        .unwrap_or_default();
    profiles.insert(
        PROFILE_NAME.into(),
        PersonalProfile {
            repository_root: repository_root.to_path_buf(),
            provider_root: provider_root.to_path_buf(),
            scope: Scope::Personal,
        },
    );
    MachineProfileFile {
        schema_version: PROFILE_SCHEMA_VERSION,
        default_profile: PROFILE_NAME.into(),
        profiles,
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use mko_windows_acl::{AceInspection, AclInspection};

    use super::validate_windows_profile_acl;

    #[test]
    fn setup_preflight_rejects_non_private_native_windows_acl_shapes() {
        let inspection = AclInspection {
            owner_is_current_user: true,
            dacl_is_protected: true,
            entries: vec![
                AceInspection {
                    allows_current_user: true,
                    access_mask: 0x001f_01ff,
                },
                AceInspection {
                    allows_current_user: false,
                    access_mask: 0x001f_01ff,
                },
            ],
        };

        assert_eq!(
            validate_windows_profile_acl(&inspection)
                .unwrap_err()
                .code(),
            "profile_permissions_invalid"
        );
    }
}
