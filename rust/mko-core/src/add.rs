use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use cap_std::fs::File as CapFile;
use unicode_normalization::UnicodeNormalization;

use crate::{
    clock::Clock,
    config::CaptureConfig,
    context::ResolvedPersonalContext,
    error::MkoError,
    fingerprint::{
        FileSnapshot, asset_id, fingerprint_open_file, fingerprint_open_file_with_guard,
        validate_pdf_content,
    },
    inbox::{InboxAddScan, InboxScanRequest, scan_inbox_for_add},
    json_v1::{
        AddOutcome, ImportOutcome, JsonV1Error, NextAction, Recovery, RecoveryKind, UserState,
    },
    model::AssetRecord,
    path_policy::provider_path,
    path_policy::validate_portable_relative_path,
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderScanRequest, ScanDeadline, excluded_name,
        scan_provider_pdfs_with_deadline,
    },
    registry::{CaptureRequest, capture_asset, read_asset},
};

static NEXT_IMPORT_TEMP: AtomicU64 = AtomicU64::new(0);
const IMPORT_LOCK_WAIT: Duration = Duration::from_secs(1);
const IMPORT_LOCK_RETRY: Duration = Duration::from_millis(10);
const IMPORT_TEMP_MARKER: &str = "mko-import-temp-v1\n";
const IMPORT_TEMP_OWNER_CLAIM: &str = "mko-import-temp-owner-v1\n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupAttestation {
    OutsideOriginalRetained,
    UserVerified,
}

#[derive(Clone, Debug)]
pub struct AddRequest {
    context: ResolvedPersonalContext,
    input: AddInput,
    backup_attestation: BackupAttestation,
    temporary_source: bool,
}

impl AddRequest {
    pub fn new(context: ResolvedPersonalContext, input: impl Into<AddInput>) -> Self {
        Self {
            context,
            input: input.into(),
            backup_attestation: BackupAttestation::OutsideOriginalRetained,
            temporary_source: false,
        }
    }

    pub fn with_backup_attestation(mut self, attestation: BackupAttestation) -> Self {
        self.backup_attestation = attestation;
        self
    }

    pub fn with_temporary_source(mut self, temporary_source: bool) -> Self {
        self.temporary_source = temporary_source;
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddInput {
    File(PathBuf),
    InboxScan,
}

impl<T: AsRef<Path>> From<T> for AddInput {
    fn from(path: T) -> Self {
        Self::File(path.as_ref().to_path_buf())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddResult {
    pub add_outcome: AddOutcome,
    pub import_outcome: ImportOutcome,
    pub repository: PathBuf,
    pub asset_id: String,
    pub registry_path: String,
    pub provider_locator: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchItemResult {
    pub provider_locator: String,
    pub user_state: UserState,
    pub next_action: NextAction,
    pub asset_id: Option<String>,
    pub add_outcome: Option<AddOutcome>,
    pub error: Option<JsonV1Error>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchAddResult {
    pub scan_complete: bool,
    pub items: Vec<BatchItemResult>,
    pub remaining: u64,
}

#[derive(Clone, Debug)]
struct BatchItemSeed {
    provider_locator: String,
    user_state: UserState,
    next_action: NextAction,
    asset_id: Option<String>,
    diagnostic: Option<crate::json_v1::DiagnosticData>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AddRunResult {
    Single(AddResult),
    Batch(BatchAddResult),
}

pub fn add(
    request: AddRequest,
    audit_clock: &dyn Clock,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<AddRunResult, MkoError> {
    match request.input {
        AddInput::File(_) => add_pdf(request, audit_clock, elapsed_clock).map(AddRunResult::Single),
        AddInput::InboxScan => {
            add_inbox(request, audit_clock, elapsed_clock).map(AddRunResult::Batch)
        }
    }
}

pub fn add_pdf(
    request: AddRequest,
    audit_clock: &dyn Clock,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<AddResult, MkoError> {
    let config = CaptureConfig::from_resolved_context(&request.context)?;
    let AddInput::File(source) = &request.input else {
        return Err(MkoError::new(
            "add_input_invalid",
            "single PDF add requires a file input",
        ));
    };
    let source_path = absolute_path(source)?;
    let deadline = ScanDeadline::start(elapsed_clock, DEFAULT_SCAN_LIMITS);
    let scan = scan_provider_pdfs_with_deadline(
        ProviderScanRequest::new(&config.provider_root).with_limits(DEFAULT_SCAN_LIMITS),
        &deadline,
    )?;
    if !scan.scan_complete {
        let reason = scan
            .warnings
            .first()
            .map(|warning| warning.code.as_str())
            .unwrap_or("unknown");
        return Err(MkoError::new(
            "provider_scan_incomplete",
            format!("provider scan was incomplete ({reason}); retry after resolving the warning"),
        ));
    }
    let (source_file, canonical_source) = open_source_nofollow(&source_path)?;
    let source_snapshot = validated_snapshot_with_deadline(&source_file, &deadline)?;
    let id = asset_id(&source_snapshot.fingerprint)?;
    let existing_registry = config
        .repository_root
        .join("assets/registry")
        .join(format!("{id}.md"));
    let existing_asset = load_existing_asset(
        &existing_registry,
        &config.repository_root,
        &id,
        &source_snapshot,
        &config.provider_type,
    )?;
    let inside_provider = canonical_source.starts_with(&config.provider_root);

    if request.temporary_source && request.backup_attestation != BackupAttestation::UserVerified {
        return Err(backup_confirmation_required());
    }
    if inside_provider
        && existing_asset.is_none()
        && request.backup_attestation != BackupAttestation::UserVerified
    {
        return Err(backup_confirmation_required());
    }

    let (provider_locator, provider_relative_path, import_outcome) =
        if let Some(asset) = existing_asset.as_ref() {
            let persisted = scan
                .pdfs
                .iter()
                .find(|candidate| candidate.provider_locator == asset.provider.locator)
                .ok_or_else(|| {
                    MkoError::new(
                        "registry_provider_missing",
                        "the registered provider locator is missing; inspect and repair the asset",
                    )
                })?;
            if persisted.fingerprint != source_snapshot.fingerprint
                || persisted.size_bytes != source_snapshot.size_bytes
            {
                return Err(MkoError::new(
                    "registry_provider_mismatch",
                    "the registered provider locator no longer contains the registered PDF",
                ));
            }
            let import_outcome = if inside_provider
                && provider_relative_path(&config.provider_root, &canonical_source)?
                    == persisted.relative_path
            {
                ImportOutcome::AlreadyInInbox
            } else {
                ImportOutcome::ReusedInboxCopy
            };
            (
                asset.provider.locator.clone(),
                persisted.relative_path.clone(),
                import_outcome,
            )
        } else if inside_provider {
            let relative_path = provider_relative_path(&config.provider_root, &canonical_source)?;
            let locator = logical_provider_locator(&relative_path)?;
            if !scan.pdfs.iter().any(|candidate| {
                candidate.relative_path == relative_path
                    && candidate.fingerprint == source_snapshot.fingerprint
                    && candidate.size_bytes == source_snapshot.size_bytes
            }) {
                return Err(MkoError::new(
                    "provider_file_excluded",
                    "the Inbox file is hidden, temporary, or changed during scanning",
                ));
            }
            (locator, relative_path, ImportOutcome::AlreadyInInbox)
        } else if let Some(existing) = scan.pdfs.iter().find(|candidate| {
            candidate.fingerprint == source_snapshot.fingerprint
                && candidate.size_bytes == source_snapshot.size_bytes
        }) {
            (
                existing.provider_locator.clone(),
                existing.relative_path.clone(),
                ImportOutcome::ReusedInboxCopy,
            )
        } else {
            let locator = import_outside_pdf(
                &config.provider_root,
                &canonical_source,
                &source_file,
                &source_snapshot,
                request.backup_attestation,
            )?;
            (
                locator.clone(),
                PathBuf::from(&locator),
                ImportOutcome::Copied,
            )
        };

    let provider_file = config.provider_root.join(provider_relative_path);
    let capture = capture_asset(
        CaptureRequest::new(&config.repository_root, &provider_file)
            .with_resolved_context(request.context)
            .with_captured_at(audit_clock.now_utc())
            .with_expected_snapshot(&source_snapshot),
    )?;
    if capture.asset_id != id {
        return Err(MkoError::new(
            "fingerprint_changed",
            "captured PDF identity differs from the source selected for add",
        ));
    }
    let add_outcome = match capture.result.as_str() {
        "created" => AddOutcome::Created,
        "existing" => AddOutcome::Existing,
        _ => {
            return Err(MkoError::new(
                "capture_result_invalid",
                "capture returned an unknown result",
            ));
        }
    };
    Ok(AddResult {
        add_outcome,
        import_outcome,
        repository: config.repository_root,
        asset_id: capture.asset_id,
        registry_path: capture.registry_path,
        provider_locator,
    })
}

fn add_inbox(
    request: AddRequest,
    audit_clock: &dyn Clock,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<BatchAddResult, MkoError> {
    let scan = scan_inbox_for_add(
        InboxScanRequest::new(
            &request.context.repository_root,
            &request.context.provider_root,
        ),
        elapsed_clock,
    )?;
    apply_inbox_scan(request, audit_clock, scan)
}

fn apply_inbox_scan(
    request: AddRequest,
    audit_clock: &dyn Clock,
    scan: InboxAddScan,
) -> Result<BatchAddResult, MkoError> {
    let mutation_safe = scan.mutation_safe;
    let report = scan.report;
    let mut seeds = report
        .items
        .into_iter()
        .filter(|item| item.next_action != NextAction::None)
        .map(|item| BatchItemSeed {
            provider_locator: item.provider_locator,
            user_state: item.user_state,
            next_action: item.next_action,
            asset_id: item.asset_id,
            diagnostic: item.diagnostic,
        })
        .collect::<Vec<_>>();
    for diagnostic in report
        .warnings
        .iter()
        .chain(report.errors.iter())
        .filter(|diagnostic| provider_item_diagnostic(diagnostic))
    {
        let Some(locator) = diagnostic.path.clone() else {
            continue;
        };
        if validate_portable_relative_path(&locator).is_err()
            || seeds.iter().any(|item| item.provider_locator == locator)
        {
            continue;
        }
        seeds.push(BatchItemSeed {
            provider_locator: locator,
            user_state: UserState::Blocked,
            next_action: recovery_action_for_code(&diagnostic.code),
            asset_id: None,
            diagnostic: Some(diagnostic.clone()),
        });
    }
    let limit = usize::try_from(report.scan_limits.max_batch_items).unwrap_or(usize::MAX);
    let total_seed_count = seeds.len();
    seeds = select_batch_seeds(seeds, limit);
    let extra = total_seed_count.saturating_sub(seeds.len()) as u64;

    let mut items = Vec::new();
    for seed in seeds {
        let output_priority = batch_output_priority(&seed.next_action);
        let result = if seed.next_action == NextAction::Add && mutation_safe {
            let outcome = if request.backup_attestation != BackupAttestation::UserVerified {
                Err(backup_confirmation_required())
            } else {
                capture_discovered_item(
                    &request.context,
                    &seed.provider_locator,
                    scan.snapshots.get(&seed.provider_locator),
                    audit_clock,
                )
            };
            match outcome {
                Ok((add_outcome, asset_id)) => BatchItemResult {
                    provider_locator: seed.provider_locator,
                    user_state: UserState::Registered,
                    next_action: NextAction::Prepare,
                    asset_id: Some(asset_id),
                    add_outcome: Some(add_outcome),
                    error: None,
                },
                Err(error) => batch_error_item(
                    seed.provider_locator,
                    seed.user_state,
                    seed.next_action,
                    seed.asset_id,
                    error,
                ),
            }
        } else if seed.next_action == NextAction::Add {
            batch_error_item(
                seed.provider_locator,
                UserState::Blocked,
                NextAction::Retry,
                seed.asset_id,
                MkoError::new(
                    "provider_scan_incomplete",
                    "The inbox scan was incomplete; retry after resolving its blockers.",
                ),
            )
        } else {
            let add_outcome = (seed.asset_id.is_some() && seed.user_state != UserState::Blocked)
                .then_some(AddOutcome::Existing);
            BatchItemResult {
                provider_locator: seed.provider_locator,
                user_state: seed.user_state,
                next_action: seed.next_action,
                asset_id: seed.asset_id,
                add_outcome,
                error: seed.diagnostic.map(json_error_from_diagnostic),
            }
        };
        items.push((output_priority, result));
    }
    items.sort_by(|(left_priority, left), (right_priority, right)| {
        left_priority.cmp(right_priority).then_with(|| {
            normalized_locator_bytes(&left.provider_locator)
                .cmp(&normalized_locator_bytes(&right.provider_locator))
        })
    });
    Ok(BatchAddResult {
        scan_complete: report.scan_complete && mutation_safe && extra == 0,
        items: items.into_iter().map(|(_, item)| item).collect(),
        remaining: report.remaining.saturating_add(extra),
    })
}

fn capture_discovered_item(
    context: &ResolvedPersonalContext,
    locator: &str,
    expected: Option<&crate::inbox::DiscoveredPdfSnapshot>,
    audit_clock: &dyn Clock,
) -> Result<(AddOutcome, String), MkoError> {
    validate_portable_relative_path(locator)?;
    let config = CaptureConfig::from_resolved_context(context)?;
    let expected = expected.ok_or_else(|| {
        MkoError::new(
            "provider_scan_incomplete",
            "The discovered inbox item has no readable content snapshot.",
        )
    })?;
    let relative = &expected.physical_relative_path;
    let source = config.provider_root.join(relative);
    let mut retained = provider_path(&config.provider_root, &source)?.file;
    let actual = fingerprint_open_file(&mut retained)?;
    validate_pdf_content(&mut retained)?;
    if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
        return Err(MkoError::new(
            "fingerprint_changed",
            "The inbox item changed after discovery and was not registered.",
        ));
    }
    let capture = capture_asset(
        CaptureRequest::new(&config.repository_root, &source)
            .with_resolved_context(ResolvedPersonalContext {
                repository_root: config.repository_root,
                provider_root: config.provider_root,
                provider_type: config.provider_type,
                profile_name: context.profile_name.clone(),
                scope: context.scope,
                source: context.source,
            })
            .with_captured_at(audit_clock.now_utc())
            .with_expected_snapshot(&actual),
    )?;
    let outcome = match capture.result.as_str() {
        "created" => AddOutcome::Created,
        "existing" => AddOutcome::Existing,
        _ => {
            return Err(MkoError::new(
                "capture_result_invalid",
                "capture returned an unknown result",
            ));
        }
    };
    Ok((outcome, capture.asset_id))
}

fn normalized_locator_bytes(locator: &str) -> Vec<u8> {
    locator.nfc().collect::<String>().into_bytes()
}

fn select_batch_seeds(seeds: Vec<BatchItemSeed>, limit: usize) -> Vec<BatchItemSeed> {
    let mut executable = Vec::new();
    let mut display_only = Vec::new();
    for seed in seeds {
        if matches!(
            seed.next_action,
            NextAction::Add | NextAction::Prepare | NextAction::WriteDraft
        ) {
            executable.push(seed);
        } else {
            display_only.push(seed);
        }
    }
    for group in [&mut executable, &mut display_only] {
        group.sort_by(|left, right| {
            normalized_locator_bytes(&left.provider_locator)
                .cmp(&normalized_locator_bytes(&right.provider_locator))
        });
    }

    executable.truncate(limit);
    let display_slots = limit.saturating_sub(executable.len());
    display_only.truncate(display_slots);
    executable.extend(display_only);
    executable.sort_by(|left, right| {
        normalized_locator_bytes(&left.provider_locator)
            .cmp(&normalized_locator_bytes(&right.provider_locator))
    });
    executable
}

fn batch_output_priority(action: &NextAction) -> u8 {
    match action {
        NextAction::Add => 0,
        NextAction::Prepare => 1,
        NextAction::WriteDraft => 2,
        NextAction::Review => 3,
        NextAction::Repair => 4,
        NextAction::Hydrate => 5,
        NextAction::Retry => 6,
        NextAction::Configure => 7,
        NextAction::None => 8,
    }
}

fn provider_item_diagnostic(diagnostic: &crate::json_v1::DiagnosticData) -> bool {
    diagnostic.path.is_some()
        && matches!(
            diagnostic.code.as_str(),
            "invalid_pdf" | "pdf_too_large" | "scan_file_unreadable"
        )
}

fn batch_error_item(
    provider_locator: String,
    user_state: UserState,
    next_action: NextAction,
    asset_id: Option<String>,
    error: MkoError,
) -> BatchItemResult {
    BatchItemResult {
        provider_locator,
        user_state,
        next_action,
        asset_id,
        add_outcome: None,
        error: Some(json_error(&error)),
    }
}

fn json_error(error: &MkoError) -> JsonV1Error {
    reviewed_batch_error(error.code())
}

fn json_error_from_diagnostic(diagnostic: crate::json_v1::DiagnosticData) -> JsonV1Error {
    reviewed_batch_error(&diagnostic.code)
}

fn reviewed_batch_error(code: &str) -> JsonV1Error {
    let message = match code {
        "invalid_pdf" => "The PDF could not be validated.",
        "pdf_too_large" => "The PDF exceeds the supported processing limit.",
        "scan_file_unreadable" | "file_unreadable" => {
            "The inbox PDF could not be reopened safely; retry after it is available."
        }
        "fingerprint_changed" => "The inbox PDF changed during processing; retry the scan.",
        "provider_scan_incomplete" => {
            "The inbox scan was incomplete; retry after resolving its blockers."
        }
        "backup_confirmation_required" => {
            "confirm a verified second copy before registering an only-copy or temporary PDF"
        }
        "provider_missing" | "provider_hydration_failed" | "provider_not_hydrated" => {
            "The inbox PDF is not locally readable; hydrate it and retry."
        }
        "lock_active"
        | "lock_scan_incomplete"
        | "provider_import_locked"
        | "registry_locked"
        | "lock_held" => {
            "The inbox item is currently locked; retry after the other operation finishes."
        }
        "registry_provider_missing"
        | "registry_provider_mismatch"
        | "source_state_mismatch"
        | "lineage_repair_needed"
        | "repository_state_inconsistent" => {
            "The inbox item conflicts with repository state and requires repair."
        }
        _ => "The inbox item could not be processed safely.",
    };
    JsonV1Error {
        code: code.into(),
        message: message.into(),
        recovery: recovery_for_code(code),
    }
}

fn recovery_for_code(code: &str) -> Option<Recovery> {
    let kind = match code {
        "provider_missing" | "provider_hydration_failed" | "provider_not_hydrated" => {
            RecoveryKind::Hydrate
        }
        "backup_confirmation_required" => RecoveryKind::VerifyBackup,
        "lock_active"
        | "lock_scan_incomplete"
        | "provider_scan_incomplete"
        | "provider_import_locked"
        | "registry_locked"
        | "lock_held"
        | "fingerprint_changed"
        | "file_unreadable"
        | "scan_file_unreadable" => RecoveryKind::Retry,
        "invalid_pdf"
        | "pdf_too_large"
        | "registry_provider_missing"
        | "registry_provider_mismatch"
        | "source_state_mismatch"
        | "lineage_repair_needed"
        | "repository_state_inconsistent" => RecoveryKind::Repair,
        _ => return None,
    };
    Some(Recovery { kind })
}

fn recovery_action_for_code(code: &str) -> NextAction {
    match code {
        "provider_missing" => NextAction::Hydrate,
        "lock_active"
        | "lock_scan_incomplete"
        | "provider_scan_incomplete"
        | "scan_file_unreadable" => NextAction::Retry,
        _ => NextAction::Repair,
    }
}

fn import_outside_pdf(
    provider_root: &Path,
    canonical_source: &Path,
    source_file: &fs::File,
    expected: &FileSnapshot,
    attestation: BackupAttestation,
) -> Result<String, MkoError> {
    debug_assert!(matches!(
        attestation,
        BackupAttestation::OutsideOriginalRetained | BackupAttestation::UserVerified
    ));
    let mut source_name = canonical_source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MkoError::new("invalid_path", "PDF filename must be valid UTF-8"))?
        .nfc()
        .collect::<String>();
    if excluded_name(&source_name) {
        let hash = expected
            .fingerprint
            .value
            .strip_prefix("sha256:")
            .ok_or_else(|| MkoError::new("fingerprint_invalid", "fingerprint must use sha256"))?;
        source_name = format!("import-{}.pdf", &hash[..12]);
    }
    validate_portable_relative_path(&source_name)?;
    let lock = ImportLock::acquire(provider_root)?;
    loop {
        let destination_name = available_destination_name(provider_root, &source_name, expected)?;
        let destination = provider_root.join(&destination_name);

        if destination_exists(&destination)? {
            if validate_existing_pdf(&destination, expected).is_ok() {
                return Ok(destination_name);
            }
            continue;
        }

        let temporary = lock.temporary_path(&destination_name);
        let result = (|| {
            let mut input = source_file
                .try_clone()
                .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
            input
                .seek(SeekFrom::Start(0))
                .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
            copy_exact_snapshot(&mut input, &mut output, expected.size_bytes)?;
            output
                .sync_all()
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
            drop(output);

            let source_after = validated_snapshot(source_file)?;
            if &source_after != expected {
                return Err(MkoError::new(
                    "fingerprint_changed",
                    "source PDF changed during import; no destination was published",
                ));
            }
            validate_existing_pdf(&temporary, expected)?;
            if attestation == BackupAttestation::OutsideOriginalRetained {
                validate_outside_original(canonical_source, expected)?;
            }

            match fs::hard_link(&temporary, &destination) {
                Ok(()) => {
                    fs::remove_file(&temporary).map_err(|error| {
                        MkoError::new("provider_import_failed", error.to_string())
                    })?;
                    sync_directory(provider_root)?;
                    validate_existing_pdf(&destination, expected)?;
                    Ok(true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Ok(validate_existing_pdf(&destination, expected).is_ok())
                }
                Err(error) => Err(MkoError::new("provider_import_failed", error.to_string())),
            }
        })();
        let _ = fs::remove_file(&temporary);
        if result? {
            return Ok(destination_name);
        }
    }
}

fn available_destination_name(
    provider_root: &Path,
    source_name: &str,
    expected: &FileSnapshot,
) -> Result<String, MkoError> {
    if let Some(existing) = collision_entry(provider_root, source_name)? {
        if existing.is_file() && validate_existing_pdf(&existing, expected).is_ok() {
            return Ok(source_name.to_owned());
        }
    } else {
        return Ok(source_name.to_owned());
    }

    let source_path = Path::new(source_name);
    let stem = source_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| MkoError::new("invalid_path", "PDF filename has no valid stem"))?;
    let hash = expected
        .fingerprint
        .value
        .strip_prefix("sha256:")
        .ok_or_else(|| MkoError::new("fingerprint_invalid", "fingerprint must use sha256"))?;
    for prefix_len in (12..=64).step_by(4) {
        let candidate = format!("{stem}-{}.pdf", &hash[..prefix_len]);
        validate_portable_relative_path(&candidate)?;
        if let Some(existing) = collision_entry(provider_root, &candidate)? {
            if existing.is_file() && validate_existing_pdf(&existing, expected).is_ok() {
                return Ok(candidate);
            }
        } else {
            return Ok(candidate);
        }
    }
    Err(MkoError::new(
        "path_collision",
        "all deterministic PDF import destinations are occupied",
    ))
}

fn collision_entry(provider_root: &Path, candidate: &str) -> Result<Option<PathBuf>, MkoError> {
    let expected_key = collision_key(candidate);
    let mut matches = Vec::new();
    for entry in fs::read_dir(provider_root)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?
    {
        let entry =
            entry.map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| {
            MkoError::new("invalid_path", "provider filename must be valid UTF-8")
        })?;
        if collision_key(name) == expected_key {
            matches.push(entry.path());
        }
    }
    if matches.len() > 1 {
        return Err(MkoError::new(
            "path_collision",
            "provider contains a case or Unicode-normalization filename collision",
        ));
    }
    Ok(matches.pop())
}

fn validated_snapshot(file: &fs::File) -> Result<FileSnapshot, MkoError> {
    let cloned = file
        .try_clone()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    let mut file = CapFile::from_std(cloned);
    let snapshot = fingerprint_open_file(&mut file)?;
    validate_pdf_content(&mut file)?;
    Ok(snapshot)
}

fn validated_snapshot_with_deadline(
    file: &fs::File,
    deadline: &ScanDeadline<'_>,
) -> Result<FileSnapshot, MkoError> {
    let cloned = file
        .try_clone()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    let mut file = CapFile::from_std(cloned);
    let mut check_deadline = || deadline.check();
    let snapshot = fingerprint_open_file_with_guard(&mut file, &mut check_deadline)?;
    deadline.check()?;
    validate_pdf_content(&mut file)?;
    deadline.check()?;
    Ok(snapshot)
}

fn validate_existing_pdf(path: &Path, expected: &FileSnapshot) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "provider_destination_invalid",
            "PDF import destination must be a regular non-link file",
        ));
    }
    let file = fs::File::open(path)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    let actual = validated_snapshot(&file)?;
    if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
        return Err(MkoError::new(
            "provider_destination_conflict",
            "PDF import destination contains different content",
        ));
    }
    Ok(())
}

fn validate_outside_original(path: &Path, expected: &FileSnapshot) -> Result<(), MkoError> {
    let (reopened, reopened_canonical) =
        open_source_nofollow(path).map_err(|_| backup_confirmation_required())?;
    if reopened_canonical != path {
        return Err(backup_confirmation_required());
    }
    let actual = validated_snapshot(&reopened)?;
    if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
        return Err(backup_confirmation_required());
    }
    Ok(())
}

fn copy_exact_snapshot(
    input: &mut fs::File,
    output: &mut fs::File,
    expected_size: u64,
) -> Result<(), MkoError> {
    let copied = std::io::copy(&mut input.take(expected_size.saturating_add(1)), output)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    if copied != expected_size {
        return Err(MkoError::new(
            "fingerprint_changed",
            "source PDF size changed during import",
        ));
    }
    output
        .flush()
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
}

fn provider_relative_path(provider_root: &Path, source: &Path) -> Result<PathBuf, MkoError> {
    source
        .strip_prefix(provider_root)
        .map(Path::to_path_buf)
        .map_err(|_| {
            MkoError::new(
                "outside_allowed_root",
                "file is outside the configured provider root",
            )
        })
}

fn logical_provider_locator(relative: &Path) -> Result<String, MkoError> {
    let locator = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| MkoError::new("invalid_path", "provider path must be valid UTF-8"))?
        .into_iter()
        .map(|component| component.nfc().collect::<String>())
        .collect::<Vec<_>>()
        .join("/");
    validate_portable_relative_path(&locator)?;
    Ok(locator)
}

fn ensure_pdf_extension(path: &Path) -> Result<(), MkoError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "unsupported_media_type",
            "add accepts PDF files only",
        ))
    }
}

fn load_existing_asset(
    path: &Path,
    repository_root: &Path,
    asset_id: &str,
    expected: &FileSnapshot,
    provider_type: &str,
) -> Result<Option<AssetRecord>, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => Err(MkoError::new(
            "registry_destination_invalid",
            "deterministic registry destination is not a regular file",
        ))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MkoError::new("registry_unreadable", error.to_string())),
    }
    let asset = read_asset(repository_root, asset_id)?;
    if asset.id != asset_id
        || asset.fingerprint != expected.fingerprint
        || asset.size_bytes != expected.size_bytes
        || asset.provider.r#type != provider_type
    {
        return Err(MkoError::new(
            "registry_identity_conflict",
            "the deterministic registry record does not match the requested PDF identity",
        ));
    }
    Ok(Some(asset))
}

fn open_source_nofollow(path: &Path) -> Result<(fs::File, PathBuf), MkoError> {
    ensure_pdf_extension(path)?;
    let file = open_path_nofollow(path)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    if !opened_metadata.is_file() || metadata_is_link_or_reparse(&opened_metadata) {
        return Err(MkoError::new(
            "file_unreadable",
            "add input must be a regular non-link file",
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|error| {
        MkoError::new(
            "file_unreadable",
            format!("cannot resolve {}: {error}", path.display()),
        )
    })?;
    let canonical_metadata = fs::metadata(&canonical)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    if !same_open_file_identity(&file, &canonical, &canonical_metadata)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?
    {
        return Err(MkoError::new(
            "file_unreadable",
            "add input changed while it was being opened",
        ));
    }
    Ok((file, canonical))
}

#[cfg(target_os = "linux")]
fn open_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_NONBLOCK: i32 = 0x800;
    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(path)
}

#[cfg(target_os = "macos")]
fn open_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0x100;
    const O_NONBLOCK: i32 = 0x4;
    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(path)
}

#[cfg(target_os = "windows")]
fn open_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("symbolic links are not accepted"));
    }
    fs::File::open(path)
}

#[cfg(unix)]
fn same_open_file_identity(
    left: &fs::File,
    _right_path: &Path,
    right: &fs::Metadata,
) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let left = left.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(windows)]
fn same_open_file_identity(
    left: &fs::File,
    right_path: &Path,
    _right: &fs::Metadata,
) -> std::io::Result<bool> {
    let right = open_path_nofollow(right_path)?;
    Ok(mko_windows_acl::file_identity(left)? == mko_windows_acl::file_identity(&right)?)
}

#[cfg(not(any(unix, windows)))]
fn same_open_file_identity(
    left: &fs::File,
    _right_path: &Path,
    right: &fs::Metadata,
) -> std::io::Result<bool> {
    let left = left.metadata()?;
    Ok(left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.created().ok() == right.created().ok())
}

fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

fn destination_exists(path: &Path) -> Result<bool, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("provider_import_failed", error.to_string())),
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, MkoError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))
    }
}

fn backup_confirmation_required() -> MkoError {
    MkoError::new(
        "backup_confirmation_required",
        "confirm a verified second copy before registering an only-copy or temporary PDF",
    )
}

fn collision_key(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

struct ImportLock {
    temporary_directory: PathBuf,
    _file: fs::File,
}

impl ImportLock {
    fn acquire(provider_root: &Path) -> Result<Self, MkoError> {
        let path = provider_root.join(".mko-import-naming.lock");
        let mut file = open_import_lock(&path)?;
        let deadline = Instant::now() + IMPORT_LOCK_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(MkoError::new(
                            "provider_import_locked",
                            "another PDF import still owns the provider naming lock",
                        ));
                    }
                    thread::sleep(IMPORT_LOCK_RETRY);
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(MkoError::new("provider_import_locked", error.to_string()));
                }
            }
        }
        validate_retained_import_lock(&path, &file)?;
        let owner_token = format!(
            "{}-{}",
            std::process::id(),
            NEXT_IMPORT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let record = format!(
            "pid={}\nhost={}\ntoken={owner_token}\n",
            std::process::id(),
            current_hostname()?
        );
        file.set_len(0)
            .and_then(|_| file.seek(SeekFrom::Start(0)))
            .and_then(|_| file.write_all(record.as_bytes()))
            .and_then(|_| file.sync_all())
            .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;

        let temporary_root = provider_root.join(".mko-import-tmp");
        reset_reserved_temp_root(&temporary_root)?;
        let temporary_directory = temporary_root.join(&owner_token);
        fs::create_dir(&temporary_directory)
            .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
        Ok(Self {
            temporary_directory,
            _file: file,
        })
    }

    fn temporary_path(&self, destination_name: &str) -> PathBuf {
        self.temporary_directory.join(format!(
            "{destination_name}.{}.import.tmp",
            NEXT_IMPORT_TEMP.fetch_add(1, Ordering::Relaxed)
        ))
    }
}

impl Drop for ImportLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temporary_directory);
    }
}

fn open_import_lock(path: &Path) -> Result<fs::File, MkoError> {
    open_import_lock_with_before_validate(path, || {})
}

fn open_import_lock_with_before_validate<F>(
    path: &Path,
    before_validate: F,
) -> Result<fs::File, MkoError>
where
    F: FnOnce(),
{
    let file = open_import_lock_path_nofollow(path)
        .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?;
    before_validate();
    validate_retained_import_lock(path, &file)?;
    Ok(file)
}

fn validate_retained_import_lock(path: &Path, file: &fs::File) -> Result<(), MkoError> {
    let retained = file
        .metadata()
        .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?;
    let current = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?;
    if !retained.is_file()
        || metadata_is_link_or_reparse(&retained)
        || !current.is_file()
        || metadata_is_link_or_reparse(&current)
        || !same_open_file_identity(file, path, &current)
            .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?
    {
        return Err(MkoError::new(
            "provider_import_locked",
            "provider import lock must remain the intended regular non-link file",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_import_lock_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0x20_000;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(O_NOFOLLOW)
        .open(path)
}

#[cfg(target_os = "macos")]
fn open_import_lock_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0x100;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(O_NOFOLLOW)
        .open(path)
}

#[cfg(target_os = "windows")]
fn open_import_lock_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_import_lock_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(std::io::Error::other("symbolic links are not accepted"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn reset_reserved_temp_root(path: &Path) -> Result<(), MkoError> {
    let owner_claim = path.with_file_name(".mko-import-tmp.owner");
    let root_exists = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            return Err(MkoError::new(
                "provider_import_failed",
                "reserved import temp path must be a non-link directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(MkoError::new("provider_import_failed", error.to_string())),
    };
    let claim_valid = ownership_marker_matches(&owner_claim, IMPORT_TEMP_OWNER_CLAIM)?;
    let root_marker_valid = if root_exists {
        ownership_marker_matches(&path.join(".mko-owned"), IMPORT_TEMP_MARKER)?
    } else {
        false
    };
    if root_exists && !claim_valid && !root_marker_valid {
        return Err(MkoError::new(
            "provider_import_failed",
            "reserved import temp directory is not attributable to Core",
        ));
    }
    if !claim_valid {
        publish_owner_claim(&owner_claim, root_marker_valid)?;
    }
    if root_exists {
        fs::remove_dir_all(path)
            .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    }
    fs::create_dir(path)
        .and_then(|_| {
            let mut marker = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.join(".mko-owned"))?;
            marker.write_all(IMPORT_TEMP_MARKER.as_bytes())?;
            marker.sync_all()
        })
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
}

fn ownership_marker_matches(path: &Path, expected: &str) -> Result<bool, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::read_to_string(path)
                .map(|contents| contents == expected)
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
        }
        Ok(_) => Err(MkoError::new(
            "provider_import_failed",
            "import temp ownership marker must be a regular non-link file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("provider_import_failed", error.to_string())),
    }
}

fn publish_owner_claim(path: &Path, replace_invalid_legacy_claim: bool) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if replace_invalid_legacy_claim
                && metadata.is_file()
                && !metadata.file_type().is_symlink() =>
        {
            fs::remove_file(path)
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
        }
        Ok(_) => {
            return Err(MkoError::new(
                "provider_import_failed",
                "import temp owner claim must be a regular non-link file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(MkoError::new("provider_import_failed", error.to_string())),
    }
    let temporary = path.with_file_name(format!(
        ".mko-import-tmp.owner.{}.{}.tmp",
        std::process::id(),
        NEXT_IMPORT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut claim = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    let publish = claim
        .write_all(IMPORT_TEMP_OWNER_CLAIM.as_bytes())
        .and_then(|_| claim.sync_all())
        .and_then(|_| fs::hard_link(&temporary, path));
    let _ = fs::remove_file(&temporary);
    publish.map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn current_hostname() -> Result<String, MkoError> {
    hostname::get()
        .map(|hostname| hostname.to_string_lossy().into_owned())
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), MkoError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), MkoError> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct TestClock;

    impl Clock for TestClock {
        fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
            chrono::DateTime::UNIX_EPOCH
        }
    }

    #[test]
    fn mutation_unsafe_scan_globally_blocks_an_unaffected_new_item() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        fs::create_dir_all(repository.join("assets/registry")).unwrap();
        fs::create_dir_all(&provider).unwrap();
        fs::write(provider.join("safe.pdf"), b"%PDF-1.7\nsafe").unwrap();
        let request = AddRequest::new(
            ResolvedPersonalContext {
                repository_root: repository.clone(),
                provider_root: provider,
                provider_type: "google-drive-stream".into(),
                profile_name: "personal".into(),
                scope: crate::context::Scope::Personal,
                source: crate::context::ContextSource::Profile,
            },
            AddInput::InboxScan,
        )
        .with_backup_attestation(BackupAttestation::UserVerified);
        let scan = InboxAddScan {
            report: crate::inbox::InboxScanResult {
                scan_complete: false,
                scan_limits: DEFAULT_SCAN_LIMITS,
                items: vec![crate::catalog::CatalogItem {
                    provider_locator: "safe.pdf".into(),
                    user_state: UserState::New,
                    asset_id: None,
                    next_action: NextAction::Add,
                    diagnostic: None,
                }],
                errors: vec![],
                warnings: vec![crate::json_v1::DiagnosticData {
                    code: "fingerprint_changed".into(),
                    message: "internal path details must not escape".into(),
                    path: Some("raced.pdf".into()),
                }],
                remaining: 0,
                state_counts: std::collections::BTreeMap::new(),
                primary_blocker: None,
                recommended_action: NextAction::Retry,
            },
            snapshots: std::collections::HashMap::new(),
            mutation_safe: false,
        };

        let result = apply_inbox_scan(request, &TestClock, scan).unwrap();

        assert!(!result.scan_complete);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].provider_locator, "safe.pdf");
        assert_eq!(result.items[0].next_action, NextAction::Retry);
        assert_eq!(
            result.items[0].error.as_ref().unwrap().code,
            "provider_scan_incomplete"
        );
        assert_eq!(
            fs::read_dir(repository.join("assets/registry"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn selector_uses_global_nfc_order_across_executable_actions() {
        let seed = |locator: &str, next_action| BatchItemSeed {
            provider_locator: locator.into(),
            user_state: UserState::Registered,
            next_action,
            asset_id: None,
            diagnostic: None,
        };
        let mut seeds = (0..20)
            .map(|index| seed(&format!("a-add-{index:02}.pdf"), NextAction::Add))
            .collect::<Vec<_>>();
        seeds.push(seed("z-prepare.pdf", NextAction::Prepare));
        seeds.push(seed("00-review.pdf", NextAction::Review));

        let selected = select_batch_seeds(seeds, 20);

        assert_eq!(selected.len(), 20);
        assert!(
            selected
                .iter()
                .all(|seed| seed.next_action == NextAction::Add)
        );
        assert_eq!(selected[0].provider_locator, "a-add-00.pdf");
        assert_eq!(selected[19].provider_locator, "a-add-19.pdf");
    }

    #[test]
    fn selector_fills_spare_capacity_with_review_and_blocker_items() {
        let seed = |locator: &str, next_action| BatchItemSeed {
            provider_locator: locator.into(),
            user_state: UserState::Registered,
            next_action,
            asset_id: None,
            diagnostic: None,
        };
        let selected = select_batch_seeds(
            vec![
                seed("z-add.pdf", NextAction::Add),
                seed("a-review.pdf", NextAction::Review),
                seed("m-retry.pdf", NextAction::Retry),
            ],
            20,
        );

        assert_eq!(
            selected
                .iter()
                .map(|seed| seed.provider_locator.as_str())
                .collect::<Vec<_>>(),
            vec!["a-review.pdf", "m-retry.pdf", "z-add.pdf"]
        );
    }

    #[test]
    fn retained_lock_open_rejects_a_path_swapped_to_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let lock = root.path().join("lock");
        let target = root.path().join("target");
        fs::write(&lock, b"old lock").unwrap();
        fs::write(&target, b"sentinel").unwrap();

        let result = open_import_lock_with_before_validate(&lock, || {
            fs::remove_file(&lock).unwrap();
            std::os::unix::fs::symlink(&target, &lock).unwrap();
        });

        assert!(result.is_err());
        assert_eq!(fs::read(target).unwrap(), b"sentinel");
    }
}
