use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    clock::{Clock, SystemClock},
    config::KnowledgeConfig,
    context::{PlatformEnvironment, SystemPlatformEnvironment},
    error::MkoError,
    hooks::{HookState, inspect_hook},
    json_v1::{DoctorCheckStatus, NextAction, RecoveryKind},
    lock::{LockState, inspect_locks},
    profile::ProfileStore,
    version::{KNOWLEDGE_CONTRACT_VERSION, PRODUCT_VERSION},
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DoctorRequest {
    repository_root: Option<PathBuf>,
}

impl DoctorRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_repository(mut self, repository_root: impl AsRef<Path>) -> Self {
        self.repository_root = Some(repository_root.as_ref().to_path_buf());
        self
    }
}

pub trait DoctorEnvironment {
    fn platform(&self) -> &dyn PlatformEnvironment;
    fn clock(&self) -> &dyn Clock;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemDoctorEnvironment {
    platform: SystemPlatformEnvironment,
    clock: SystemClock,
}

impl DoctorEnvironment for SystemDoctorEnvironment {
    fn platform(&self) -> &dyn PlatformEnvironment {
        &self.platform
    }

    fn clock(&self) -> &dyn Clock {
        &self.clock
    }
}

pub use crate::json_v1::DoctorCheckStatus as DoctorStatus;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorCheck {
    pub code: String,
    pub status: DoctorCheckStatus,
    pub message: String,
    pub path: Option<PathBuf>,
    pub recovery: Option<RecoveryKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorReport {
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
    pub next_action: NextAction,
}

pub fn diagnose(request: DoctorRequest, environment: &dyn DoctorEnvironment) -> DoctorReport {
    let mut checks = vec![
        healthy("product_version", PRODUCT_VERSION, None),
        healthy("contract_version", KNOWLEDGE_CONTRACT_VERSION, None),
    ];
    let profile =
        ProfileStore::from_platform(environment.platform()).and_then(|store| store.read());
    let profile = match profile {
        Ok(Some(profile)) => Some(profile),
        Ok(None) => {
            checks.push(blocked(
                "profile_missing",
                "machine profile is not configured",
                None,
                RecoveryKind::Configure,
            ));
            None
        }
        Err(_) => {
            checks.push(blocked(
                "profile_unreadable",
                "machine profile could not be read",
                None,
                RecoveryKind::Configure,
            ));
            None
        }
    };

    let repository = request.repository_root.or_else(|| {
        profile.as_ref().and_then(|profile| {
            profile
                .profiles
                .get(&profile.default_profile)
                .map(|personal| personal.repository_root.clone())
        })
    });
    let repository = repository.and_then(|path| match canonical_repository(&path) {
        Ok(path) => Some(path),
        Err(_) => {
            checks.push(blocked(
                "repository_incompatible",
                "repository is not a compatible Personal knowledge base",
                Some(&path),
                RecoveryKind::Configure,
            ));
            None
        }
    });
    let provider = profile.as_ref().and_then(|profile| {
        profile
            .profiles
            .get(&profile.default_profile)
            .map(|personal| personal.provider_root.clone())
    });
    if let Some(provider) = provider.as_deref() {
        checks.extend(provider_checks(provider));
    }

    if let Some(repository) = repository.as_deref() {
        match KnowledgeConfig::read(repository) {
            Ok(_) => checks.push(healthy(
                "repository_access",
                "repository is compatible",
                Some(repository),
            )),
            Err(_) => checks.push(blocked(
                "repository_incompatible",
                "repository is not a compatible Personal knowledge base",
                Some(repository),
                RecoveryKind::Configure,
            )),
        }
        checks.push(hook_check(repository));
        checks.extend(lock_checks(repository, environment.clock()));
    }

    let next_action = next_action(&checks);
    DoctorReport {
        healthy: checks
            .iter()
            .all(|check| check.status == DoctorCheckStatus::Healthy),
        checks,
        next_action,
    }
}

pub(crate) fn final_setup_checks(
    repository: &Path,
    provider: &Path,
    clock: &dyn Clock,
) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    match KnowledgeConfig::read(repository) {
        Ok(_) => checks.push(healthy(
            "repository_access",
            "repository is compatible",
            Some(repository),
        )),
        Err(_) => checks.push(blocked(
            "repository_incompatible",
            "repository is not a compatible Personal knowledge base",
            Some(repository),
            RecoveryKind::Configure,
        )),
    }
    checks.extend(provider_checks(provider));
    checks.push(hook_check(repository));
    checks.extend(lock_checks(repository, clock));
    checks
}

pub(crate) fn setup_checks_are_healthy(
    repository: &Path,
    provider: &Path,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    let checks = final_setup_checks(repository, provider, clock);
    if let Some(failed) = checks
        .into_iter()
        .find(|check| check.status == DoctorCheckStatus::Blocked)
    {
        return Err(MkoError::new(failed.code, failed.message));
    }
    Ok(())
}

fn canonical_repository(path: &Path) -> Result<PathBuf, MkoError> {
    let metadata = fs::metadata(path)
        .map_err(|error| MkoError::new("repository_incompatible", error.to_string()))?;
    if !metadata.is_dir() {
        return Err(MkoError::new(
            "repository_incompatible",
            "repository path is not a directory",
        ));
    }
    fs::canonicalize(path)
        .map_err(|error| MkoError::new("repository_incompatible", error.to_string()))
}

fn provider_checks(provider: &Path) -> Vec<DoctorCheck> {
    let metadata = match fs::metadata(provider) {
        Ok(metadata) if metadata.is_dir() => metadata,
        Ok(_) => {
            return vec![blocked(
                "provider_unreadable",
                "Personal Inbox is not a directory",
                Some(provider),
                RecoveryKind::FixPermissions,
            )];
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return vec![blocked(
                "provider_missing",
                "Personal Inbox does not exist",
                Some(provider),
                RecoveryKind::Configure,
            )];
        }
        Err(_) => {
            return vec![blocked(
                "provider_unreadable",
                "Personal Inbox cannot be read",
                Some(provider),
                RecoveryKind::FixPermissions,
            )];
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o444 == 0 {
            return vec![blocked(
                "provider_unreadable",
                "Personal Inbox cannot be read",
                Some(provider),
                RecoveryKind::FixPermissions,
            )];
        }
        if mode & 0o222 == 0 {
            return vec![blocked(
                "provider_unwritable",
                "Personal Inbox cannot be written",
                Some(provider),
                RecoveryKind::FixPermissions,
            )];
        }
    }
    if metadata.permissions().readonly() {
        return vec![blocked(
            "provider_unwritable",
            "Personal Inbox cannot be written",
            Some(provider),
            RecoveryKind::FixPermissions,
        )];
    }
    let entries = match fs::read_dir(provider) {
        Ok(entries) => entries,
        Err(_) => {
            return vec![blocked(
                "provider_unreadable",
                "Personal Inbox cannot be read",
                Some(provider),
                RecoveryKind::FixPermissions,
            )];
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
            && fs::metadata(&path).map_or(true, |metadata| metadata.len() == 0)
        {
            return vec![blocked(
                "provider_hydration_failed",
                "a Personal PDF is not hydrated for reading",
                Some(&path),
                RecoveryKind::Hydrate,
            )];
        }
    }
    vec![healthy(
        "provider_access",
        "Personal Inbox is readable and writable",
        Some(provider),
    )]
}

fn hook_check(repository: &Path) -> DoctorCheck {
    match inspect_hook(repository) {
        Ok(inspection) if inspection.state == HookState::Managed => healthy(
            "hook_managed",
            "managed Git hook is installed",
            Some(repository),
        ),
        Ok(_) => blocked(
            "hook_missing",
            "managed Git hook is not installed",
            Some(repository),
            RecoveryKind::Repair,
        ),
        Err(error) if error.code() == "hook_conflict" => blocked(
            "hook_conflict",
            "a custom Git hook must be preserved",
            Some(repository),
            RecoveryKind::ResolveHookConflict,
        ),
        Err(_) => blocked(
            "hook_unreadable",
            "Git hook state could not be inspected",
            Some(repository),
            RecoveryKind::Repair,
        ),
    }
}

fn lock_checks(repository: &Path, clock: &dyn Clock) -> Vec<DoctorCheck> {
    match inspect_locks(repository, clock) {
        Ok(inspections) => inspections
            .into_iter()
            .map(|inspection| match inspection.state {
                LockState::Stale => warning(
                    "stale_lock",
                    "a stale operation lock needs explicit recovery",
                    Some(inspection.path),
                    RecoveryKind::Repair,
                ),
                LockState::Unreadable => warning(
                    "lock_unreadable",
                    "an operation lock could not be read",
                    Some(inspection.path),
                    RecoveryKind::Repair,
                ),
                LockState::Active => warning(
                    "lock_active",
                    "an operation lock is active",
                    Some(inspection.path),
                    RecoveryKind::Retry,
                ),
            })
            .collect(),
        Err(_) => vec![warning(
            "lock_unreadable",
            "operation locks could not be inspected",
            Some(repository.to_path_buf()),
            RecoveryKind::Repair,
        )],
    }
}

fn next_action(checks: &[DoctorCheck]) -> NextAction {
    let priority = [
        ("profile_missing", NextAction::Configure),
        ("profile_unreadable", NextAction::Configure),
        ("repository_incompatible", NextAction::Configure),
        ("provider_missing", NextAction::Configure),
        ("provider_unreadable", NextAction::Repair),
        ("provider_unwritable", NextAction::Repair),
        ("provider_hydration_failed", NextAction::Hydrate),
        ("hook_conflict", NextAction::Repair),
        ("hook_missing", NextAction::Repair),
        ("hook_unreadable", NextAction::Repair),
        ("stale_lock", NextAction::Repair),
        ("lock_unreadable", NextAction::Repair),
        ("lock_active", NextAction::Retry),
    ];
    priority
        .iter()
        .find_map(|(code, action)| {
            checks
                .iter()
                .any(|check| check.code == *code)
                .then(|| action.clone())
        })
        .unwrap_or(NextAction::None)
}

fn healthy(code: &str, message: &str, path: Option<&Path>) -> DoctorCheck {
    DoctorCheck {
        code: code.into(),
        status: DoctorCheckStatus::Healthy,
        message: message.into(),
        path: path.map(Path::to_path_buf),
        recovery: None,
    }
}

fn blocked(code: &str, message: &str, path: Option<&Path>, recovery: RecoveryKind) -> DoctorCheck {
    DoctorCheck {
        code: code.into(),
        status: DoctorCheckStatus::Blocked,
        message: message.into(),
        path: path.map(Path::to_path_buf),
        recovery: Some(recovery),
    }
}

fn warning(
    code: &str,
    message: &str,
    path: Option<PathBuf>,
    recovery: RecoveryKind,
) -> DoctorCheck {
    DoctorCheck {
        code: code.into(),
        status: DoctorCheckStatus::Warning,
        message: message.into(),
        path,
        recovery: Some(recovery),
    }
}
