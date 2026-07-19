use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use cap_std::fs::File as CapFile;
use sysinfo::{Pid, System};
use unicode_normalization::UnicodeNormalization;

use crate::{
    clock::Clock,
    config::CaptureConfig,
    context::ResolvedPersonalContext,
    error::MkoError,
    fingerprint::{FileSnapshot, asset_id, fingerprint_open_file, validate_pdf_content},
    json_v1::{AddOutcome, ImportOutcome},
    path_policy::validate_portable_relative_path,
    provider_scan::{DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderScanRequest, scan_provider_pdfs},
    registry::{CaptureRequest, capture_asset},
};

static NEXT_IMPORT_TEMP: AtomicU64 = AtomicU64::new(0);
const IMPORT_LOCK_WAIT: Duration = Duration::from_secs(1);
const IMPORT_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackupAttestation {
    OutsideOriginalRetained,
    UserVerified,
}

#[derive(Clone, Debug)]
pub struct AddRequest {
    context: ResolvedPersonalContext,
    source: PathBuf,
    backup_attestation: BackupAttestation,
    temporary_source: bool,
}

impl AddRequest {
    pub fn new(context: ResolvedPersonalContext, source: impl AsRef<Path>) -> Self {
        Self {
            context,
            source: source.as_ref().to_path_buf(),
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
pub struct AddResult {
    pub add_outcome: AddOutcome,
    pub import_outcome: ImportOutcome,
    pub repository: PathBuf,
    pub asset_id: String,
    pub registry_path: String,
    pub provider_locator: String,
}

pub fn add_pdf(
    request: AddRequest,
    audit_clock: &dyn Clock,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<AddResult, MkoError> {
    let config = CaptureConfig::from_resolved_context(&request.context)?;
    let source_path = absolute_path(&request.source)?;
    reject_source_link(&source_path)?;
    let canonical_source = fs::canonicalize(&source_path).map_err(|error| {
        MkoError::new(
            "file_unreadable",
            format!("cannot resolve {}: {error}", source_path.display()),
        )
    })?;
    ensure_pdf_extension(&canonical_source)?;
    let source_file = fs::File::open(&canonical_source)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    if !source_file
        .metadata()
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?
        .is_file()
    {
        return Err(MkoError::new(
            "file_unreadable",
            "add input must be a regular file",
        ));
    }
    let source_snapshot = validated_snapshot(&source_file)?;
    let id = asset_id(&source_snapshot.fingerprint)?;
    let existing_registry = config
        .repository_root
        .join("assets/registry")
        .join(format!("{id}.md"));
    let asset_already_registered = existing_registry_is_file(&existing_registry)?;
    let inside_provider = canonical_source.starts_with(&config.provider_root);

    if request.temporary_source && request.backup_attestation != BackupAttestation::UserVerified {
        return Err(backup_confirmation_required());
    }
    if inside_provider
        && !asset_already_registered
        && request.backup_attestation != BackupAttestation::UserVerified
    {
        return Err(backup_confirmation_required());
    }

    let scan = scan_provider_pdfs(
        ProviderScanRequest::new(&config.provider_root).with_limits(DEFAULT_SCAN_LIMITS),
        elapsed_clock,
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

    let (provider_locator, provider_relative_path, import_outcome) = if inside_provider {
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
    let source_name = canonical_source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| MkoError::new("invalid_path", "PDF filename must be valid UTF-8"))?
        .nfc()
        .collect::<String>();
    validate_portable_relative_path(&source_name)?;
    let _lock = ImportLock::acquire(provider_root)?;
    loop {
        let destination_name = available_destination_name(provider_root, &source_name, expected)?;
        let destination = provider_root.join(&destination_name);

        if destination_exists(&destination)? {
            if validate_existing_pdf(&destination, expected).is_ok() {
                return Ok(destination_name);
            }
            continue;
        }

        let temporary_name = format!(
            ".{destination_name}.{}.{}.import.tmp",
            std::process::id(),
            NEXT_IMPORT_TEMP.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = provider_root.join(&temporary_name);
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
    reject_source_link(path)?;
    let reopened = fs::File::open(path).map_err(|_| backup_confirmation_required())?;
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

fn reject_source_link(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "file_unreadable",
            "add input must not be a symbolic link or reparse-point link",
        ));
    }
    Ok(())
}

fn existing_registry_is_file(path: &Path) -> Result<bool, MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(MkoError::new(
            "registry_destination_invalid",
            "deterministic registry destination is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("registry_unreadable", error.to_string())),
    }
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
    path: PathBuf,
    owner_token: String,
}

impl ImportLock {
    fn acquire(provider_root: &Path) -> Result<Self, MkoError> {
        let path = provider_root.join(".mko-import-naming.lock");
        let deadline = Instant::now() + IMPORT_LOCK_WAIT;
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let owner_token = format!(
                        "{}-{}",
                        std::process::id(),
                        NEXT_IMPORT_TEMP.fetch_add(1, Ordering::Relaxed)
                    );
                    let record = format!("pid={}\ntoken={owner_token}\n", std::process::id());
                    if let Err(error) = file
                        .write_all(record.as_bytes())
                        .and_then(|_| file.sync_all())
                    {
                        let _ = fs::remove_file(&path);
                        return Err(MkoError::new("provider_import_failed", error.to_string()));
                    }
                    let lock = Self {
                        path,
                        owner_token: owner_token.clone(),
                    };
                    cleanup_orphan_import_temps(provider_root)?;
                    return Ok(lock);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if import_lock_is_stale(&path)? {
                        match fs::remove_file(&path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => {
                                return Err(MkoError::new(
                                    "provider_import_failed",
                                    error.to_string(),
                                ));
                            }
                        }
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(MkoError::new(
                            "provider_import_locked",
                            "PDF import lock is held or stale; inspect it before retrying",
                        ));
                    }
                    thread::sleep(IMPORT_LOCK_RETRY);
                }
                Err(error) => {
                    return Err(MkoError::new("provider_import_failed", error.to_string()));
                }
            }
        }
    }
}

impl Drop for ImportLock {
    fn drop(&mut self) {
        let owned = fs::read_to_string(&self.path)
            .ok()
            .and_then(|contents| import_lock_fields(&contents).map(|(_, token)| token))
            .is_some_and(|token| token == self.owner_token);
        if owned {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn import_lock_is_stale(path: &Path) -> Result<bool, MkoError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?;
    let Some((pid, _)) = import_lock_fields(&contents) else {
        return Ok(false);
    };
    let system = System::new_all();
    Ok(system.process(Pid::from_u32(pid)).is_none())
}

fn import_lock_fields(contents: &str) -> Option<(u32, String)> {
    let mut pid = None;
    let mut token = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.parse().ok();
        } else if let Some(value) = line.strip_prefix("token=") {
            token = Some(value.to_owned());
        }
    }
    Some((pid?, token?))
}

fn cleanup_orphan_import_temps(provider_root: &Path) -> Result<(), MkoError> {
    for entry in fs::read_dir(provider_root)
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?
    {
        let entry =
            entry.map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') && name.ends_with(".import.tmp") {
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                fs::remove_file(entry.path())
                    .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
            }
        }
    }
    Ok(())
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
