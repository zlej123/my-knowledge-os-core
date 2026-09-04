use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    clock::{Clock, SystemClock},
    config::KnowledgeConfig,
    config_v2::KnowledgeConfigV2,
    context::{
        ContextSource, PlatformEnvironment, ResolveContextRequest, SelectedPersonalContext,
        SystemPlatformEnvironment, profile_for_repository, select_personal_context,
    },
    error::MkoError,
    hooks::{HookState, inspect_hook},
    json_v1::{DoctorCheckStatus, NextAction, RecoveryKind},
    lock::{LockState, inspect_locks},
    profile::ProfileStore,
    provider_scan::{
        DEFAULT_SCAN_LIMITS, MonotonicElapsedClock, ProviderMetadataAccess,
        inspect_provider_metadata,
    },
    version::{KNOWLEDGE_CONTRACT_VERSION, PRODUCT_VERSION},
};

const PERSONAL_INBOX_SUFFIX: [&str; 3] = ["My-Knowledge-OS-Assets", "personal", "inbox"];

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderAccessInspection {
    Allowed,
    Denied,
    Indeterminate(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderEntryState {
    Hydrated,
    NotHydrated,
    Unsupported,
    Corrupt,
    Unreadable,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderEntryInspection {
    pub path: PathBuf,
    pub state: ProviderEntryState,
    pub detail: Option<String>,
}

impl ProviderEntryInspection {
    pub fn new(path: impl AsRef<Path>, state: ProviderEntryState) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            state,
            detail: None,
        }
    }

    pub fn with_detail(
        path: impl AsRef<Path>,
        state: ProviderEntryState,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            state,
            detail: Some(detail.into()),
        }
    }
}

pub trait DoctorEnvironment {
    fn platform(&self) -> &dyn PlatformEnvironment;
    fn clock(&self) -> &dyn Clock;

    fn inspect_provider_read_access(&self, provider: &Path) -> ProviderAccessInspection {
        inspect_system_provider_access(provider, ProviderAccess::ReadDirectory)
    }

    fn inspect_provider_write_access(&self, provider: &Path) -> ProviderAccessInspection {
        inspect_system_provider_access(provider, ProviderAccess::WriteDirectory)
    }

    fn inspect_provider_entries(
        &self,
        provider: &Path,
    ) -> Result<Vec<ProviderEntryInspection>, MkoError> {
        inspect_system_provider_entries(provider)
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticArea {
    Product,
    Profile,
    Repository,
    Provider,
    Hook,
    Lock,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoctorCheck {
    pub area: DiagnosticArea,
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

impl DoctorReport {
    pub fn primary_issue(&self) -> Option<&DoctorCheck> {
        primary_issue(&self.checks)
    }
}

pub fn diagnose(request: DoctorRequest, environment: &dyn DoctorEnvironment) -> DoctorReport {
    let selected = select_personal_context(
        request
            .repository_root
            .map_or_else(ResolveContextRequest::new, |repository| {
                ResolveContextRequest::new().with_explicit_repository(repository)
            }),
        environment.platform(),
    );

    // Report the KB's own declared contract for a v3 (json-v2 config)
    // repository — the legacy KNOWLEDGE_CONTRACT_VERSION otherwise (a v0.1
    // repository, or no repository resolved at all). This runs before the
    // repository is inspected below for its other checks, so it re-derives
    // the candidate root from `selected` rather than the fuller
    // `InspectedRepository` produced later.
    let contract_version = repository_root_hint(&selected)
        .and_then(|root| declared_contract_version_v2(&root))
        .unwrap_or_else(|| KNOWLEDGE_CONTRACT_VERSION.to_string());

    let mut checks = vec![
        healthy(
            DiagnosticArea::Product,
            "product_version",
            PRODUCT_VERSION,
            None,
        ),
        healthy(
            DiagnosticArea::Product,
            "contract_version",
            &contract_version,
            None,
        ),
    ];
    checks.push(profile_check(environment.platform()));

    let mut provider = None;
    let repository = match selected {
        Ok(SelectedPersonalContext::Repository {
            repository_root,
            source,
        }) => match inspect_repository(&repository_root) {
            Ok(inspected) => {
                let repository_root = inspected.root;
                checks.push(healthy(
                    DiagnosticArea::Repository,
                    "repository_access",
                    "repository is compatible",
                    Some(&repository_root),
                ));
                provider = environment
                    .platform()
                    .environment_value(OsStr::new(&inspected.provider_root_env))
                    .map(PathBuf::from);
                if provider.is_none() {
                    // Resolution answers this from the machine profile when the
                    // environment is silent. Asking a different question here
                    // made doctor report a healthy machine as unconfigured
                    // whenever it was run from inside the knowledge base.
                    provider = profile_for_repository(&repository_root, environment.platform())
                        .ok()
                        .flatten()
                        .map(|(_, provider_root)| provider_root);
                }
                if provider.is_none() {
                    checks.push(blocked(
                        DiagnosticArea::Provider,
                        "provider_missing",
                        "Personal Inbox environment path is not configured",
                        None,
                        RecoveryKind::Configure,
                    ));
                }
                Some((repository_root, source))
            }
            Err(_) => {
                checks.push(blocked(
                    DiagnosticArea::Repository,
                    "repository_incompatible",
                    "repository is not a compatible Personal knowledge base",
                    Some(&repository_root),
                    RecoveryKind::Configure,
                ));
                None
            }
        },
        Ok(SelectedPersonalContext::Profile { profile, .. }) => {
            provider = Some(profile.provider_root);
            match inspect_repository(&profile.repository_root) {
                Ok(InspectedRepository {
                    root: repository_root,
                    ..
                }) => {
                    checks.push(healthy(
                        DiagnosticArea::Repository,
                        "repository_access",
                        "repository is compatible",
                        Some(&repository_root),
                    ));
                    Some((repository_root, ContextSource::Profile))
                }
                Err(_) => {
                    checks.push(blocked(
                        DiagnosticArea::Repository,
                        "repository_incompatible",
                        "repository is not a compatible Personal knowledge base",
                        Some(&profile.repository_root),
                        RecoveryKind::Configure,
                    ));
                    None
                }
            }
        }
        Err(error)
            if error.code() == "context_not_found" || error.code().starts_with("profile_") =>
        {
            None
        }
        Err(_) => {
            checks.push(blocked(
                DiagnosticArea::Repository,
                "repository_incompatible",
                "repository context could not be inspected",
                None,
                RecoveryKind::Configure,
            ));
            None
        }
    };

    if let Some(provider) = provider.as_deref() {
        checks.extend(provider_checks(provider, environment));
    }

    if let Some((repository, _)) = repository.as_ref() {
        checks.extend(git_backup_check(repository));
        checks.push(hook_check(repository));
        checks.extend(lock_checks(repository, environment.clock()));
    }

    report(checks)
}

pub(crate) fn final_setup_checks(
    repository: &Path,
    provider: &Path,
    clock: &dyn Clock,
) -> Vec<DoctorCheck> {
    let environment = SetupDoctorEnvironment {
        platform: SystemPlatformEnvironment,
        clock,
    };
    let mut checks = Vec::new();
    match inspect_repository(repository) {
        Ok(InspectedRepository {
            root: repository, ..
        }) => checks.push(healthy(
            DiagnosticArea::Repository,
            "repository_access",
            "repository is compatible",
            Some(&repository),
        )),
        Err(_) => checks.push(blocked(
            DiagnosticArea::Repository,
            "repository_incompatible",
            "repository is not a compatible Personal knowledge base",
            Some(repository),
            RecoveryKind::Configure,
        )),
    }
    checks.extend(provider_checks(provider, &environment));
    checks.push(hook_check(repository));
    checks.extend(lock_checks(repository, clock));
    checks
}

pub(crate) fn setup_checks_are_healthy(
    repository: &Path,
    provider: &Path,
    clock: &dyn Clock,
) -> Result<(), DoctorCheck> {
    let checks = final_setup_checks(repository, provider, clock);
    if checks_are_healthy(&checks) {
        return Ok(());
    }
    let failed = primary_issue(&checks).expect("an unhealthy check set has a primary issue");
    Err(failed.clone())
}

struct SetupDoctorEnvironment<'a> {
    platform: SystemPlatformEnvironment,
    clock: &'a dyn Clock,
}

impl DoctorEnvironment for SetupDoctorEnvironment<'_> {
    fn platform(&self) -> &dyn PlatformEnvironment {
        &self.platform
    }

    fn clock(&self) -> &dyn Clock {
        self.clock
    }
}

fn report(checks: Vec<DoctorCheck>) -> DoctorReport {
    DoctorReport {
        healthy: checks_are_healthy(&checks),
        next_action: primary_issue(&checks)
            .map(action_for_issue)
            .unwrap_or(NextAction::None),
        checks,
    }
}

fn profile_check(platform: &dyn PlatformEnvironment) -> DoctorCheck {
    match ProfileStore::from_platform(platform) {
        Ok(store) => match store.read() {
            Ok(Some(_)) => healthy(
                DiagnosticArea::Profile,
                "profile_valid",
                "machine profile schema is valid",
                Some(store.path()),
            ),
            Ok(None) => blocked(
                DiagnosticArea::Profile,
                "profile_missing",
                "machine profile is not configured",
                Some(store.path()),
                RecoveryKind::Configure,
            ),
            Err(_) => blocked(
                DiagnosticArea::Profile,
                "profile_unreadable",
                "machine profile could not be read or validated",
                Some(store.path()),
                RecoveryKind::Configure,
            ),
        },
        Err(_) => blocked(
            DiagnosticArea::Profile,
            "profile_unreadable",
            "machine profile location could not be inspected",
            None,
            RecoveryKind::Configure,
        ),
    }
}

/// The repository root `select_personal_context` resolved, before it is
/// inspected further — used only to peek at the KB's declared contract
/// version for the `contract_version` doctor check.
fn repository_root_hint(selected: &Result<SelectedPersonalContext, MkoError>) -> Option<PathBuf> {
    match selected {
        Ok(SelectedPersonalContext::Repository {
            repository_root, ..
        }) => Some(repository_root.clone()),
        Ok(SelectedPersonalContext::Profile { profile, .. }) => {
            Some(profile.repository_root.clone())
        }
        _ => None,
    }
}

/// The contract version a v3 (json-v2 config) repository declares on disk,
/// or `None` for a legacy v0.1 repository, a damaged config, or a path that
/// is not a repository at all — any of which fall back to the legacy
/// `KNOWLEDGE_CONTRACT_VERSION` in the caller.
fn declared_contract_version_v2(repository_root: &Path) -> Option<String> {
    let canonical = fs::canonicalize(repository_root).ok()?;
    KnowledgeConfigV2::read(&canonical)
        .ok()
        .map(|configuration| configuration.contract_version)
}

/// What diagnosis needs from a knowledge repository, whichever generation it is.
///
/// Doctor is the tool an owner reaches for when something is wrong, so reading
/// only the v0.1 configuration made it report a healthy current repository as
/// incompatible and send them back to setup.
struct InspectedRepository {
    root: PathBuf,
    provider_root_env: String,
}

fn inspect_repository(path: &Path) -> Result<InspectedRepository, MkoError> {
    let metadata = fs::metadata(path)
        .map_err(|error| MkoError::new("repository_incompatible", error.to_string()))?;
    if !metadata.is_dir() {
        return Err(MkoError::new(
            "repository_incompatible",
            "repository path is not a directory",
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| MkoError::new("repository_incompatible", error.to_string()))?;
    let provider_root_env = match KnowledgeConfigV2::read(&canonical) {
        Ok(configuration) => configuration.provider.root_env,
        Err(v2_error) => match KnowledgeConfig::read(&canonical) {
            Ok(configuration) => configuration.provider.root_env,
            // Report the current generation's reason: a v0.3 repository with a
            // damaged configuration is the case an owner needs explained.
            Err(v1_error) => {
                let error = if canonical.join("knowledge-os.yaml").exists() {
                    v2_error
                } else {
                    v1_error
                };
                return Err(MkoError::new("repository_incompatible", error.message()));
            }
        },
    };
    Ok(InspectedRepository {
        root: canonical,
        provider_root_env,
    })
}

fn provider_checks(provider: &Path, environment: &dyn DoctorEnvironment) -> Vec<DoctorCheck> {
    let identity = provider_identity_check(provider);
    if identity.status != DoctorCheckStatus::Healthy {
        return vec![identity];
    }
    let mut checks = vec![identity];
    checks.push(provider_read_check(
        provider,
        environment.inspect_provider_read_access(provider),
    ));
    checks.push(provider_write_check(
        provider,
        environment.inspect_provider_write_access(provider),
    ));
    checks.extend(provider_entry_checks(
        provider,
        environment.inspect_provider_entries(provider),
    ));
    checks
}

fn provider_identity_check(provider: &Path) -> DoctorCheck {
    if !provider.is_absolute() || !is_exact_personal_inbox(provider) {
        return blocked(
            DiagnosticArea::Provider,
            "provider_root_invalid",
            "provider root must be the exact Personal Inbox",
            Some(provider),
            RecoveryKind::Configure,
        );
    }
    match fs::symlink_metadata(provider) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => healthy(
            DiagnosticArea::Provider,
            "provider_inbox",
            "provider root is the exact Personal Inbox",
            Some(provider),
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => blocked(
            DiagnosticArea::Provider,
            "provider_missing",
            "Personal Inbox does not exist",
            Some(provider),
            RecoveryKind::Configure,
        ),
        _ => blocked(
            DiagnosticArea::Provider,
            "provider_root_invalid",
            "Personal Inbox must be a real directory",
            Some(provider),
            RecoveryKind::Configure,
        ),
    }
}

fn is_exact_personal_inbox(provider: &Path) -> bool {
    let suffix = PERSONAL_INBOX_SUFFIX
        .into_iter()
        .fold(PathBuf::new(), |path, component| path.join(component));
    provider.ends_with(suffix)
        && provider
            .ancestors()
            .nth(PERSONAL_INBOX_SUFFIX.len())
            .is_some_and(|account_root| account_root.file_name().is_some())
}

fn provider_read_check(provider: &Path, inspection: ProviderAccessInspection) -> DoctorCheck {
    match inspection {
        ProviderAccessInspection::Allowed => healthy(
            DiagnosticArea::Provider,
            "provider_readable",
            "Personal Inbox is readable by the current process",
            Some(provider),
        ),
        ProviderAccessInspection::Denied => blocked(
            DiagnosticArea::Provider,
            "provider_unreadable",
            "Personal Inbox cannot be read by the current process",
            Some(provider),
            RecoveryKind::FixPermissions,
        ),
        ProviderAccessInspection::Indeterminate(detail) => blocked(
            DiagnosticArea::Provider,
            "provider_read_inspection_failed",
            &format!("Personal Inbox read access could not be determined: {detail}"),
            Some(provider),
            RecoveryKind::FixPermissions,
        ),
    }
}

fn provider_write_check(provider: &Path, inspection: ProviderAccessInspection) -> DoctorCheck {
    match inspection {
        ProviderAccessInspection::Allowed => healthy(
            DiagnosticArea::Provider,
            "provider_writable",
            "Personal Inbox is writable by the current process",
            Some(provider),
        ),
        ProviderAccessInspection::Denied => blocked(
            DiagnosticArea::Provider,
            "provider_unwritable",
            "Personal Inbox cannot be written by the current process",
            Some(provider),
            RecoveryKind::FixPermissions,
        ),
        ProviderAccessInspection::Indeterminate(detail) => blocked(
            DiagnosticArea::Provider,
            "provider_write_inspection_failed",
            &format!("Personal Inbox write access could not be determined: {detail}"),
            Some(provider),
            RecoveryKind::FixPermissions,
        ),
    }
}

fn provider_entry_checks(
    provider: &Path,
    inspections: Result<Vec<ProviderEntryInspection>, MkoError>,
) -> Vec<DoctorCheck> {
    let mut inspections = match inspections {
        Ok(inspections) => inspections,
        Err(error) => {
            return vec![blocked(
                DiagnosticArea::Provider,
                "provider_inspection_failed",
                &format!("Personal Inbox entries could not be inspected: {error}"),
                Some(provider),
                RecoveryKind::Repair,
            )];
        }
    };
    inspections.sort_by(|left, right| left.path.cmp(&right.path));
    let hydration_unsupported = inspections
        .iter()
        .any(|inspection| inspection.state == ProviderEntryState::Unsupported);
    let mut checks = inspections
        .into_iter()
        .filter_map(|inspection| {
            let detail = inspection
                .detail
                .as_deref()
                .map(|detail| format!(": {detail}"))
                .unwrap_or_default();
            match inspection.state {
                ProviderEntryState::Hydrated | ProviderEntryState::Unsupported => None,
                ProviderEntryState::NotHydrated => Some(blocked(
                    DiagnosticArea::Provider,
                    "provider_hydration_failed",
                    &format!("a Personal PDF is a cloud placeholder{detail}"),
                    Some(&inspection.path),
                    RecoveryKind::Hydrate,
                )),
                ProviderEntryState::Corrupt => Some(blocked(
                    DiagnosticArea::Provider,
                    "provider_pdf_corrupt",
                    &format!("a Personal PDF entry is invalid{detail}"),
                    Some(&inspection.path),
                    RecoveryKind::Repair,
                )),
                ProviderEntryState::Unreadable => Some(blocked(
                    DiagnosticArea::Provider,
                    "provider_pdf_unreadable",
                    &format!("a Personal PDF entry could not be inspected{detail}"),
                    Some(&inspection.path),
                    RecoveryKind::FixPermissions,
                )),
                ProviderEntryState::Unknown => Some(blocked(
                    DiagnosticArea::Provider,
                    "provider_inspection_failed",
                    &format!("a Personal PDF hydration state could not be determined{detail}"),
                    Some(&inspection.path),
                    RecoveryKind::Repair,
                )),
            }
        })
        .collect::<Vec<_>>();
    if checks.is_empty() {
        checks.push(if hydration_unsupported {
            healthy(
                DiagnosticArea::Provider,
                "provider_hydration_unsupported",
                "Personal PDF placeholder metadata is unsupported on this platform",
                Some(provider),
            )
        } else {
            healthy(
                DiagnosticArea::Provider,
                "provider_hydration",
                "Personal PDF placeholder metadata is healthy",
                Some(provider),
            )
        });
    }
    checks
}

#[derive(Clone, Copy)]
enum ProviderAccess {
    ReadDirectory,
    WriteDirectory,
    ReadFile,
}

#[cfg(unix)]
fn inspect_system_provider_access(
    provider: &Path,
    access: ProviderAccess,
) -> ProviderAccessInspection {
    use nix::{
        errno::Errno,
        fcntl::{AT_FDCWD, AtFlags},
        unistd::{AccessFlags, faccessat},
    };

    let mode = match access {
        ProviderAccess::ReadDirectory => AccessFlags::R_OK | AccessFlags::X_OK,
        ProviderAccess::WriteDirectory => AccessFlags::W_OK | AccessFlags::X_OK,
        ProviderAccess::ReadFile => AccessFlags::R_OK,
    };
    match faccessat(AT_FDCWD, provider, mode, AtFlags::AT_EACCESS) {
        Ok(()) => ProviderAccessInspection::Allowed,
        Err(Errno::EACCES | Errno::EPERM) => ProviderAccessInspection::Denied,
        Err(error) => ProviderAccessInspection::Indeterminate(error.to_string()),
    }
}

#[cfg(windows)]
fn inspect_system_provider_access(
    provider: &Path,
    access: ProviderAccess,
) -> ProviderAccessInspection {
    let access = match access {
        ProviderAccess::ReadDirectory => mko_windows_acl::EffectiveAccess::ReadDirectory,
        ProviderAccess::WriteDirectory => mko_windows_acl::EffectiveAccess::WriteDirectory,
        ProviderAccess::ReadFile => mko_windows_acl::EffectiveAccess::ReadFile,
    };
    match mko_windows_acl::check_effective_access(provider, access) {
        Ok(true) => ProviderAccessInspection::Allowed,
        Ok(false) => ProviderAccessInspection::Denied,
        Err(error) => ProviderAccessInspection::Indeterminate(error.to_string()),
    }
}

#[cfg(not(any(unix, windows)))]
fn inspect_system_provider_access(_: &Path, _: ProviderAccess) -> ProviderAccessInspection {
    ProviderAccessInspection::Indeterminate(
        "effective access inspection is unsupported on this platform".into(),
    )
}

fn inspect_system_provider_entries(
    provider: &Path,
) -> Result<Vec<ProviderEntryInspection>, MkoError> {
    let elapsed_clock = MonotonicElapsedClock::start();
    let walk = inspect_provider_metadata(provider, DEFAULT_SCAN_LIMITS, &elapsed_clock)?;
    let mut inspections = walk
        .entries
        .into_iter()
        .map(|entry| {
            let path = provider.join(&entry.relative_path);
            let state = match entry
                .inspect_access(|| inspect_system_provider_access(&path, ProviderAccess::ReadFile))
            {
                ProviderMetadataAccess::Placeholder => ProviderEntryState::NotHydrated,
                ProviderMetadataAccess::Supported(inspection) => {
                    state_after_access(inspection, false)
                }
                ProviderMetadataAccess::Unsupported(inspection) => {
                    state_after_access(inspection, true)
                }
            };
            ProviderEntryInspection::new(&path, state)
        })
        .collect::<Vec<_>>();
    inspections.extend(walk.issues.into_iter().map(|issue| {
        ProviderEntryInspection::with_detail(
            issue
                .relative_path
                .map_or_else(|| provider.to_path_buf(), |path| provider.join(path)),
            ProviderEntryState::Unknown,
            issue.message,
        )
    }));
    Ok(inspections)
}

fn state_after_access(
    inspection: ProviderAccessInspection,
    hydration_unsupported: bool,
) -> ProviderEntryState {
    match inspection {
        ProviderAccessInspection::Allowed if hydration_unsupported => {
            ProviderEntryState::Unsupported
        }
        ProviderAccessInspection::Allowed => ProviderEntryState::Hydrated,
        ProviderAccessInspection::Denied => ProviderEntryState::Unreadable,
        ProviderAccessInspection::Indeterminate(_) => ProviderEntryState::Unknown,
    }
}

enum GitBackup {
    Configured,
    NoRemote,
    NoRepository,
}

fn inspect_git_backup(repository: &Path) -> GitBackup {
    let inside = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    match inside {
        Ok(output) if output.status.success() => {}
        // No repository here, or no git on this machine: either way nothing
        // in this directory is being copied anywhere.
        _ => return GitBackup::NoRepository,
    }
    let remotes = Command::new("git")
        .arg("-C")
        .arg(repository)
        .arg("remote")
        .output();
    match remotes {
        Ok(output)
            if output.status.success() && !output.stdout.iter().all(u8::is_ascii_whitespace) =>
        {
            GitBackup::Configured
        }
        _ => GitBackup::NoRemote,
    }
}

/// Whether the records can survive this disk.
///
/// Git is optional — setup completes a local knowledge base without it and
/// offers a private remote afterwards — so its absence is not damage and must
/// not block anything. It is not health either: the forward-risk review found
/// the owner's live knowledge base had never been put under Git, so reviews,
/// immutable revisions and web-snapshot evidence existed as one copy on one
/// disk while doctor reported healthy. One warning, lowest priority, naming
/// the remedy. v0.1 repositories keep their frozen doctor surface, and setup's
/// final checks do not include this one.
fn git_backup_check(repository: &Path) -> Option<DoctorCheck> {
    if KnowledgeConfigV2::read(repository).is_err() {
        return None;
    }
    Some(match inspect_git_backup(repository) {
        GitBackup::Configured => healthy(
            DiagnosticArea::Repository,
            "git_backup_configured",
            "records are under Git with a remote",
            Some(repository),
        ),
        GitBackup::NoRemote => warning(
            DiagnosticArea::Repository,
            "git_remote_missing",
            "this knowledge base is under Git with no remote: its records exist as one copy on this disk; add a private remote and push",
            Some(repository.to_path_buf()),
            RecoveryKind::Configure,
        ),
        GitBackup::NoRepository => warning(
            DiagnosticArea::Repository,
            "git_backup_missing",
            "this knowledge base is not under Git: its records exist as one copy on this disk; initialize a Git repository with a private remote (setup offers one) so they survive it",
            Some(repository.to_path_buf()),
            RecoveryKind::Configure,
        ),
    })
}

fn hook_check(repository: &Path) -> DoctorCheck {
    // The managed hook runs `mko check`, which understands v0.1 records only.
    // On a v0.3 knowledge base it rejects every revision, so "installed" is
    // the broken state and "missing" is the sound one. Doctor used to demand
    // the hook here and call the result a repair; following that advice on
    // the owner's live knowledge base made every commit fail.
    let is_v2 = KnowledgeConfigV2::read(repository).is_ok();
    match inspect_hook(repository) {
        Ok(inspection) if inspection.state == HookState::Managed && is_v2 => blocked(
            DiagnosticArea::Hook,
            "hook_incompatible",
            "the installed pre-commit check reads v0.1 records only and rejects every v0.3 revision; remove .githooks/pre-commit and unset core.hooksPath",
            Some(repository),
            RecoveryKind::Repair,
        ),
        Ok(inspection) if inspection.state == HookState::Managed => healthy(
            DiagnosticArea::Hook,
            "hook_managed",
            "managed Git hook is installed",
            Some(repository),
        ),
        Ok(_) if is_v2 => healthy(
            DiagnosticArea::Hook,
            "hook_not_applicable",
            "v0.3 records are not covered by the pre-commit check yet, so no hook is required",
            Some(repository),
        ),
        Ok(_) => blocked(
            DiagnosticArea::Hook,
            "hook_missing",
            "managed Git hook is not installed",
            Some(repository),
            RecoveryKind::Repair,
        ),
        Err(error) if error.code() == "hook_conflict" => blocked(
            DiagnosticArea::Hook,
            "hook_conflict",
            "a custom Git hook must be preserved",
            Some(repository),
            RecoveryKind::ResolveHookConflict,
        ),
        // Git is an optional sync choice, not a precondition — setup completes a
        // local KB without it. Reporting a missing Git worktree as a broken hook
        // made a supported configuration look damaged, with a repair the owner
        // could not perform.
        Err(error) if error.code() == "git_repository_required" => healthy(
            DiagnosticArea::Hook,
            "hook_not_applicable",
            "this knowledge base does not use Git, so no hook is required",
            Some(repository),
        ),
        Err(_) => blocked(
            DiagnosticArea::Hook,
            "hook_unreadable",
            "Git hook state could not be inspected",
            Some(repository),
            RecoveryKind::Repair,
        ),
    }
}

fn lock_checks(repository: &Path, clock: &dyn Clock) -> Vec<DoctorCheck> {
    match inspect_locks(repository, clock) {
        Ok(inspections) if inspections.is_empty() => vec![healthy(
            DiagnosticArea::Lock,
            "locks_clear",
            "no operation locks are present",
            Some(repository),
        )],
        Ok(inspections) => inspections
            .into_iter()
            .map(|inspection| match inspection.state {
                LockState::Stale => warning(
                    DiagnosticArea::Lock,
                    "stale_lock",
                    "a stale operation lock needs explicit recovery",
                    Some(inspection.path),
                    RecoveryKind::Repair,
                ),
                LockState::Unreadable => warning(
                    DiagnosticArea::Lock,
                    "lock_unreadable",
                    "an operation lock could not be read",
                    Some(inspection.path),
                    RecoveryKind::Repair,
                ),
                LockState::Active => warning(
                    DiagnosticArea::Lock,
                    "lock_active",
                    "an operation lock is active",
                    Some(inspection.path),
                    RecoveryKind::Retry,
                ),
            })
            .collect(),
        Err(_) => vec![warning(
            DiagnosticArea::Lock,
            "lock_unreadable",
            "operation locks could not be inspected",
            Some(repository.to_path_buf()),
            RecoveryKind::Repair,
        )],
    }
}

fn checks_are_healthy(checks: &[DoctorCheck]) -> bool {
    checks
        .iter()
        .all(|check| check.status == DoctorCheckStatus::Healthy)
}

fn primary_issue(checks: &[DoctorCheck]) -> Option<&DoctorCheck> {
    checks
        .iter()
        .filter(|check| check.status != DoctorCheckStatus::Healthy)
        .min_by_key(|check| issue_priority(&check.code))
}

fn issue_priority(code: &str) -> usize {
    const PRIORITY: &[&str] = &[
        "profile_missing",
        "profile_unreadable",
        "repository_incompatible",
        "provider_root_invalid",
        "provider_missing",
        "provider_unreadable",
        "provider_read_inspection_failed",
        "provider_unwritable",
        "provider_write_inspection_failed",
        "provider_pdf_unreadable",
        "provider_pdf_corrupt",
        "provider_inspection_failed",
        "provider_hydration_failed",
        "hook_conflict",
        "hook_incompatible",
        "hook_missing",
        "hook_unreadable",
        "stale_lock",
        "lock_unreadable",
        "lock_active",
        // Durability last: a stale lock or a missing provider is what stops
        // the owner today; a missing backup is what loses everything one day.
        "git_remote_missing",
        "git_backup_missing",
    ];
    PRIORITY
        .iter()
        .position(|candidate| *candidate == code)
        .unwrap_or(PRIORITY.len())
}

fn action_for_issue(check: &DoctorCheck) -> NextAction {
    match check.code.as_str() {
        "profile_missing"
        | "profile_unreadable"
        | "repository_incompatible"
        | "provider_root_invalid"
        | "provider_missing"
        | "git_remote_missing"
        | "git_backup_missing" => NextAction::Configure,
        "provider_hydration_failed" => NextAction::Hydrate,
        "lock_active" => NextAction::Retry,
        _ => NextAction::Repair,
    }
}

fn healthy(area: DiagnosticArea, code: &str, message: &str, path: Option<&Path>) -> DoctorCheck {
    DoctorCheck {
        area,
        code: code.into(),
        status: DoctorCheckStatus::Healthy,
        message: message.into(),
        path: path.map(Path::to_path_buf),
        recovery: None,
    }
}

fn blocked(
    area: DiagnosticArea,
    code: &str,
    message: &str,
    path: Option<&Path>,
    recovery: RecoveryKind,
) -> DoctorCheck {
    DoctorCheck {
        area,
        code: code.into(),
        status: DoctorCheckStatus::Blocked,
        message: message.into(),
        path: path.map(Path::to_path_buf),
        recovery: Some(recovery),
    }
}

fn warning(
    area: DiagnosticArea,
    code: &str,
    message: &str,
    path: Option<PathBuf>,
    recovery: RecoveryKind,
) -> DoctorCheck {
    DoctorCheck {
        area,
        code: code.into(),
        status: DoctorCheckStatus::Warning,
        message: message.into(),
        path,
        recovery: Some(recovery),
    }
}
