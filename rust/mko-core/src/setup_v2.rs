use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    asset_v2::validated_disjoint_roots,
    context::{PlatformEnvironment, Scope},
    dashboard_v2::{DashboardResultV2, ensure_dashboard_v2},
    error::MkoError,
    profile::{
        MachineProfileFile, PROFILE_SCHEMA_VERSION, PersonalProfile, ProfileMutationLock,
        ProfileSnapshot, ProfileStore,
    },
    scaffold_v2::{ScaffoldOutcomeV2, scaffold_personal_kb_v2},
};

const PROFILE_NAME: &str = "personal";
const INBOX_COMPONENTS: [&str; 3] = ["My-Knowledge-OS-Assets", "personal", "inbox"];

pub struct SetupPersonalV2Request<'a> {
    pub repository_root: &'a Path,
    pub drive_account_root: &'a Path,
    pub replace_profile: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupPersonalV2Result {
    pub repository_root: PathBuf,
    pub provider_root: PathBuf,
    pub scaffold: ScaffoldOutcomeV2,
    pub dashboard: DashboardResultV2,
    pub profile_changed: bool,
}

pub fn setup_personal_v2(
    request: SetupPersonalV2Request<'_>,
    platform: &dyn PlatformEnvironment,
) -> Result<SetupPersonalV2Result, MkoError> {
    let store = ProfileStore::from_platform(platform)?;
    let mutation_lock = store.acquire_mutation_lock()?;
    let expected_profile = store.read_snapshot()?;
    setup_personal_v2_locked(request, platform, &store, &mutation_lock, expected_profile)
}

pub(crate) fn setup_personal_v2_locked(
    request: SetupPersonalV2Request<'_>,
    platform: &dyn PlatformEnvironment,
    store: &ProfileStore,
    mutation_lock: &ProfileMutationLock,
    existing_snapshot: ProfileSnapshot,
) -> Result<SetupPersonalV2Result, MkoError> {
    let drive_account_root =
        canonical_real_directory(request.drive_account_root, "provider_root_invalid")?;
    let provider_root = INBOX_COMPONENTS
        .iter()
        .fold(drive_account_root.clone(), |path, component| {
            path.join(component)
        });
    let repository_candidate = destination_candidate(request.repository_root, platform)?;
    if repository_candidate.starts_with(&drive_account_root)
        || provider_root.starts_with(&repository_candidate)
    {
        return Err(MkoError::new(
            "storage_roots_overlap",
            "the Git KB must remain outside the selected Drive account",
        ));
    }
    let existing_profiles = existing_snapshot.profile.clone();
    let candidate_profile = PersonalProfile {
        repository_root: repository_candidate.clone(),
        provider_root: provider_root.clone(),
        scope: Scope::Personal,
    };
    if let Some(existing) = existing_profiles
        .as_ref()
        .and_then(|profiles| profiles.profiles.get(PROFILE_NAME))
        && existing != &candidate_profile
        && !request.replace_profile
    {
        return Err(MkoError::new(
            "profile_conflict",
            "a different Personal profile exists; rerun setup after explicitly choosing replacement",
        ));
    }
    ensure_provider_inbox(&drive_account_root, &provider_root)?;
    let scaffold = scaffold_personal_kb_v2(request.repository_root)?;
    let (repository_root, provider_root) =
        validated_disjoint_roots(request.repository_root, &provider_root)?;
    let dashboard = ensure_dashboard_v2(&repository_root)?;

    let mut profiles = existing_profiles.unwrap_or(MachineProfileFile {
        schema_version: PROFILE_SCHEMA_VERSION,
        default_profile: PROFILE_NAME.into(),
        profiles: BTreeMap::new(),
    });
    let desired = PersonalProfile {
        repository_root: repository_root.clone(),
        provider_root: provider_root.clone(),
        scope: Scope::Personal,
    };
    let profile_changed = profiles.profiles.get(PROFILE_NAME) != Some(&desired)
        || profiles.default_profile != PROFILE_NAME;
    profiles.schema_version = PROFILE_SCHEMA_VERSION;
    profiles.default_profile = PROFILE_NAME.into();
    profiles.profiles.insert(PROFILE_NAME.into(), desired);
    if profile_changed {
        store.write_if_unchanged(mutation_lock, &existing_snapshot, &profiles)?;
    }

    Ok(SetupPersonalV2Result {
        repository_root,
        provider_root,
        scaffold,
        dashboard,
        profile_changed,
    })
}

fn destination_candidate(
    path: &Path,
    platform: &dyn PlatformEnvironment,
) -> Result<PathBuf, MkoError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        platform.current_dir()?.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(MkoError::new(
            "repository_root_invalid",
            "repository destination cannot contain parent traversal",
        ));
    }
    let parent = absolute.parent().ok_or_else(|| {
        MkoError::new(
            "repository_root_invalid",
            "repository destination has no parent",
        )
    })?;
    let parent = fs::canonicalize(parent)
        .map_err(|error| MkoError::new("repository_root_invalid", error.to_string()))?;
    let name = absolute.file_name().ok_or_else(|| {
        MkoError::new(
            "repository_root_invalid",
            "repository destination has no directory name",
        )
    })?;
    Ok(parent.join(name))
}

fn ensure_provider_inbox(account_root: &Path, inbox: &Path) -> Result<(), MkoError> {
    if !inbox.starts_with(account_root) {
        return Err(MkoError::new(
            "provider_root_invalid",
            "Personal Inbox must remain inside the selected Drive account",
        ));
    }
    fs::create_dir_all(inbox)
        .map_err(|error| MkoError::new("provider_create_failed", error.to_string()))?;
    let canonical = canonical_real_directory(inbox, "provider_root_invalid")?;
    if !canonical.starts_with(account_root) {
        return Err(MkoError::new(
            "provider_root_invalid",
            "Personal Inbox escaped the selected Drive account",
        ));
    }
    Ok(())
}

fn canonical_real_directory(path: &Path, code: &str) -> Result<PathBuf, MkoError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| MkoError::new(code, error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(code, "path must be a real directory"));
    }
    fs::canonicalize(path).map_err(|error| MkoError::new(code, error.to_string()))
}
