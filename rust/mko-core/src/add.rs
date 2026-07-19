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
    fingerprint::{FileSnapshot, asset_id, fingerprint_open_file, validate_pdf_content},
    json_v1::{AddOutcome, ImportOutcome},
    model::AssetRecord,
    path_policy::validate_portable_relative_path,
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderScanRequest, excluded_name, scan_provider_pdfs,
    },
    registry::{CaptureRequest, capture_asset, read_asset},
};

static NEXT_IMPORT_TEMP: AtomicU64 = AtomicU64::new(0);
const IMPORT_LOCK_WAIT: Duration = Duration::from_secs(1);
const IMPORT_LOCK_RETRY: Duration = Duration::from_millis(10);
const IMPORT_TEMP_MARKER: &str = "mko-import-temp-v1\n";

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
    let (source_file, canonical_source) = open_source_nofollow(&source_path)?;
    let source_snapshot = validated_snapshot(&source_file)?;
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
    if !same_file_identity(&opened_metadata, &canonical_metadata) {
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
    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
}

#[cfg(target_os = "macos")]
fn open_path_nofollow(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    const O_NOFOLLOW: i32 = 0x100;
    OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
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
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(unix, windows)))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.created().ok() == right.created().ok()
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
        reject_non_regular_lock_path(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| MkoError::new("provider_import_locked", error.to_string()))?;
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

fn reject_non_regular_lock_path(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(MkoError::new(
            "provider_import_locked",
            "provider import lock path must be a regular non-link file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MkoError::new("provider_import_locked", error.to_string())),
    }
}

fn reset_reserved_temp_root(path: &Path) -> Result<(), MkoError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let marker = path.join(".mko-owned");
            let marker_metadata = fs::symlink_metadata(&marker).map_err(|_| {
                MkoError::new(
                    "provider_import_failed",
                    "reserved import temp directory lacks its ownership marker",
                )
            })?;
            if !marker_metadata.is_file()
                || marker_metadata.file_type().is_symlink()
                || fs::read_to_string(&marker)
                    .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?
                    != IMPORT_TEMP_MARKER
            {
                return Err(MkoError::new(
                    "provider_import_failed",
                    "reserved import temp directory has an invalid ownership marker",
                ));
            }
            fs::remove_dir_all(path)
                .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))?;
        }
        Ok(_) => {
            return Err(MkoError::new(
                "provider_import_failed",
                "reserved import temp path must be a non-link directory",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(MkoError::new("provider_import_failed", error.to_string())),
    }
    fs::create_dir(path)
        .and_then(|_| fs::write(path.join(".mko-owned"), IMPORT_TEMP_MARKER))
        .map_err(|error| MkoError::new("provider_import_failed", error.to_string()))
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
