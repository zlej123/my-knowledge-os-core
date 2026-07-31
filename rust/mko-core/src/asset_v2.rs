use std::{
    collections::{BTreeMap, btree_map::Entry},
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions, OpenOptionsExt},
};
use chrono::{DateTime, Utc};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new},
    clock::SystemClock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    fingerprint::{
        FileSnapshot, MAX_ASSET_BYTES, asset_id, fingerprint_open_file, validate_pdf_content,
    },
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    path_policy::validate_portable_relative_path,
    provider_scan::{
        DEFAULT_SCAN_LIMITS, ElapsedClock, ProviderCatalogEntry, ProviderCatalogScan,
        ProviderScanRequest, ProviderScanWarning, scan_provider_catalog,
    },
    records_v2::{AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2},
    revision_v2::canonical_json_bytes,
};

const MAX_ASSET_RECORD_BYTES: u64 = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HydrationConfirmationV2 {
    NotConfirmed,
    Confirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetRegistrationOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRegistrationResultV2 {
    pub asset: AssetRecordV2,
    pub registry_path: PathBuf,
    pub outcome: AssetRegistrationOutcomeV2,
}

pub struct RegisterAssetRequestV2<'a> {
    pub repository_root: &'a Path,
    pub provider_root: &'a Path,
    pub logical_locator: &'a str,
    pub hydration_confirmation: HydrationConfirmationV2,
}

pub struct RegisterInboxAssetsRequestV2<'a> {
    pub repository_root: &'a Path,
    pub provider_root: &'a Path,
    pub hydration_confirmation: HydrationConfirmationV2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboxAssetRegistrationItemV2 {
    pub logical_locator: String,
    pub registration: Option<AssetRegistrationResultV2>,
    pub error: Option<MkoError>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboxAssetRegistrationResultV2 {
    /// False when provider discovery was incomplete or more known candidates
    /// remain outside this bounded response.
    pub scan_complete: bool,
    pub items: Vec<InboxAssetRegistrationItemV2>,
    pub warnings: Vec<ProviderScanWarning>,
    /// Number of already-discovered actionable locators omitted by the batch
    /// ceiling. If `scan_complete` is false, additional undiscovered entries may
    /// also exist and are intentionally not guessed here.
    pub remaining: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InboxAssetInspectionV2 {
    pub new_count: u64,
    pub registered_count: u64,
    pub blocked_count: u64,
    pub scan_complete: bool,
}

#[derive(Clone, Debug)]
enum InboxCandidateV2 {
    Readable,
    Blocked(MkoError),
}

/// Registers one deterministic, bounded page of PDFs already present below the
/// configured provider Inbox. Individual failures are returned beside successful
/// immutable registrations and never roll back earlier successes.
pub fn register_inbox_pdf_assets_v2(
    request: RegisterInboxAssetsRequestV2<'_>,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<InboxAssetRegistrationResultV2, MkoError> {
    KnowledgeConfigV2::read(request.repository_root)?;
    let (repository_root, provider_root) =
        validated_disjoint_roots(request.repository_root, request.provider_root)?;
    let scan = scan_provider_catalog(
        ProviderScanRequest::new(&provider_root).with_limits(DEFAULT_SCAN_LIMITS),
        elapsed_clock,
    )?;
    Ok(apply_inbox_catalog_v2(
        request,
        &repository_root,
        &provider_root,
        scan,
    ))
}

/// Inspects the provider Inbox without registering or changing any Asset.
pub fn inspect_inbox_pdf_assets_v2(
    repository_root: &Path,
    provider_root: &Path,
    elapsed_clock: &dyn ElapsedClock,
) -> Result<InboxAssetInspectionV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let (repository_root, provider_root) =
        validated_disjoint_roots(repository_root, provider_root)?;
    let scan = scan_provider_catalog(
        ProviderScanRequest::new(&provider_root).with_limits(DEFAULT_SCAN_LIMITS),
        elapsed_clock,
    )?;
    let mut result = InboxAssetInspectionV2 {
        scan_complete: scan.scan_complete,
        ..InboxAssetInspectionV2::default()
    };
    for entry in scan.entries {
        match entry {
            ProviderCatalogEntry::Placeholder { .. } => {
                result.blocked_count += 1;
            }
            ProviderCatalogEntry::Readable(pdf) => {
                let id = asset_id(&pdf.fingerprint)?;
                let registry_path = repository_root
                    .join("assets/registry")
                    .join(format!("{id}.json"));
                match fs::symlink_metadata(&registry_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        result.new_count += 1;
                    }
                    Err(_) => {
                        result.blocked_count += 1;
                    }
                    Ok(_) if read_asset_v2(&repository_root, &id).is_ok() => {
                        result.registered_count += 1;
                    }
                    Ok(_) => {
                        result.blocked_count += 1;
                    }
                }
            }
        }
    }
    result.blocked_count = result
        .blocked_count
        .saturating_add(scan.warnings.len().try_into().unwrap_or(u64::MAX));
    Ok(result)
}

fn apply_inbox_catalog_v2(
    request: RegisterInboxAssetsRequestV2<'_>,
    repository_root: &Path,
    provider_root: &Path,
    scan: ProviderCatalogScan,
) -> InboxAssetRegistrationResultV2 {
    // The catalog's legacy `scan_complete` is false for every warning, including
    // a fully enumerated invalid PDF. Batch callers need the narrower discovery
    // meaning: known item-local content failures do not imply hidden entries.
    let provider_scan_complete = scan.scan_complete
        || scan
            .warnings
            .iter()
            .all(item_warning_preserves_scan_completeness);
    let mut candidates = BTreeMap::<String, InboxCandidateV2>::new();
    for entry in scan.entries {
        match entry {
            ProviderCatalogEntry::Readable(pdf) => {
                candidates.insert(pdf.provider_locator, InboxCandidateV2::Readable);
            }
            ProviderCatalogEntry::Placeholder {
                provider_locator, ..
            } => {
                candidates.insert(
                    provider_locator,
                    InboxCandidateV2::Blocked(MkoError::new(
                        "asset_not_hydrated",
                        "provider PDF is not available as a fully local readable snapshot",
                    )),
                );
            }
        }
    }

    let mut warnings = Vec::new();
    for warning in scan.warnings {
        let Some(locator) = warning.provider_locator.clone() else {
            warnings.push(warning);
            continue;
        };
        match candidates.entry(locator) {
            Entry::Vacant(entry) => {
                entry.insert(InboxCandidateV2::Blocked(MkoError::new(
                    warning.code,
                    warning.message,
                )));
            }
            Entry::Occupied(_) => warnings.push(warning),
        }
    }

    let batch_limit = usize::try_from(DEFAULT_SCAN_LIMITS.max_batch_items).unwrap_or(usize::MAX);
    let remaining = candidates.len().saturating_sub(batch_limit) as u64;
    if remaining > 0 {
        warnings.push(ProviderScanWarning {
            code: "batch_item_limit".into(),
            message: format!(
                "{remaining} additional discovered Inbox item(s) remain outside this bounded batch"
            ),
            provider_locator: None,
        });
    }

    let mut items = Vec::with_capacity(candidates.len().min(batch_limit));
    for (logical_locator, candidate) in candidates.into_iter().take(batch_limit) {
        let (registration, error) = match candidate {
            InboxCandidateV2::Readable => match register_pdf_asset_v2(RegisterAssetRequestV2 {
                repository_root,
                provider_root,
                logical_locator: &logical_locator,
                hydration_confirmation: request.hydration_confirmation,
            }) {
                Ok(result) => (Some(result), None),
                Err(error) => (None, Some(error)),
            },
            InboxCandidateV2::Blocked(error) => (None, Some(error)),
        };
        items.push(InboxAssetRegistrationItemV2 {
            logical_locator,
            registration,
            error,
        });
    }
    warnings.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then(left.provider_locator.cmp(&right.provider_locator))
    });
    InboxAssetRegistrationResultV2 {
        scan_complete: provider_scan_complete && remaining == 0,
        items,
        warnings,
        remaining,
    }
}

fn item_warning_preserves_scan_completeness(warning: &ProviderScanWarning) -> bool {
    warning.provider_locator.is_some()
        && matches!(
            warning.code.as_str(),
            "invalid_pdf"
                | "file_too_large"
                | "pdf_too_large"
                | "fingerprint_changed"
                | "scan_file_unreadable"
        )
}

pub fn register_pdf_asset_v2(
    request: RegisterAssetRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    let config = KnowledgeConfigV2::read(request.repository_root)?;
    let (repository_root, provider_root) =
        validated_disjoint_roots(request.repository_root, request.provider_root)?;
    let inspected = inspect_provider_file(&provider_root, request.logical_locator)?;
    require_hydration_confirmation(
        inspected.size_bytes,
        config.provider.hydration_warning_threshold_bytes,
        request.hydration_confirmation,
    )?;
    let mut provider = inspected.open_readonly()?;
    validate_pdf_content(&mut provider)?;
    let before = fingerprint_open_file(&mut provider)?;
    if before.size_bytes != inspected.size_bytes {
        return Err(provider_changed_error());
    }

    let id = asset_id(&before.fingerprint)?;
    let record = AssetRecordV2 {
        schema_version: 2,
        id: id.clone(),
        record_type: AssetRecordTypeV2::Asset,
        fingerprint: before.fingerprint.value.clone(),
        title_fallback: title_fallback(request.logical_locator)?,
        media_type: "application/pdf".into(),
        provider: AssetProviderBindingV2 {
            provider_type: config.provider.r#type,
            logical_locator: request.logical_locator.nfc().collect(),
            size_bytes: before.size_bytes,
            modified_at: Some(DateTime::<Utc>::from(before.modified_at.into_std())),
        },
    };
    validate_asset_record_v2(&record)?;
    let bytes = canonical_json_bytes(&record)?;
    if bytes.len() as u64 > MAX_ASSET_RECORD_BYTES {
        return Err(MkoError::new(
            "asset_record_invalid",
            "Asset registry record exceeds its bounded canonical representation",
        ));
    }

    let _mutation_lock = RepositoryMutationLock::acquire(
        &repository_root,
        "v2 asset register",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    revalidate_provider_snapshot(&provider_root, request.logical_locator, &before)?;
    let registry_directory = repository_root.join("assets/registry");
    require_real_directory(&registry_directory, "asset_registry_invalid")?;
    let registry_path = registry_directory.join(format!("{id}.json"));
    let outcome = write_new(&registry_path, &bytes, |path| {
        let existing = read_bounded_nofollow(path, MAX_ASSET_RECORD_BYTES, "asset_registry")?;
        let existing_record: AssetRecordV2 = serde_json::from_slice(&existing).map_err(|_| {
            MkoError::new(
                "asset_registry_conflict",
                "existing Asset registry bytes are not a schema-v2 Asset",
            )
        })?;
        validate_asset_record_v2(&existing_record)?;
        if existing == bytes {
            Ok(())
        } else if existing_record.id == record.id
            && existing_record.fingerprint == record.fingerprint
            && existing_record.media_type == record.media_type
        {
            // An identical fingerprint is one Asset even if it is rediscovered at another
            // locator. Its first immutable provider binding remains authoritative.
            Ok(())
        } else {
            Err(MkoError::new(
                "asset_registry_conflict",
                "content-addressed Asset registry path contains different identity bytes",
            ))
        }
    })?;
    let (asset, outcome) = match outcome {
        AtomicWriteResult::Created => (record, AssetRegistrationOutcomeV2::Created),
        AtomicWriteResult::Existing => (
            read_asset_record_path_v2(&registry_path, &id)?,
            AssetRegistrationOutcomeV2::Existing,
        ),
    };
    Ok(AssetRegistrationResultV2 {
        asset,
        registry_path,
        outcome,
    })
}

pub fn read_asset_v2(repository_root: &Path, asset_id: &str) -> Result<AssetRecordV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    validate_asset_id(asset_id)?;
    read_asset_record_path_v2(
        &repository_root
            .join("assets/registry")
            .join(format!("{asset_id}.json")),
        asset_id,
    )
}

fn read_asset_record_path_v2(path: &Path, expected_id: &str) -> Result<AssetRecordV2, MkoError> {
    let bytes = read_bounded_nofollow(path, MAX_ASSET_RECORD_BYTES, "asset_registry")?;
    let record: AssetRecordV2 = serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("asset_registry_invalid", error.to_string()))?;
    validate_asset_record_v2(&record)?;
    if record.id != expected_id || canonical_json_bytes(&record)? != bytes {
        return Err(MkoError::new(
            "asset_registry_invalid",
            "Asset registry bytes are not the expected canonical immutable record",
        ));
    }
    Ok(record)
}

pub(crate) fn validate_asset_record_v2(asset: &AssetRecordV2) -> Result<(), MkoError> {
    validate_asset_id(&asset.id)?;
    let expected = asset
        .fingerprint
        .strip_prefix("sha256:")
        .map(|hash| format!("personal-asset-{hash}"));
    if asset.schema_version != 2
        || asset.record_type != AssetRecordTypeV2::Asset
        || expected.as_deref() != Some(asset.id.as_str())
        || asset.title_fallback.is_empty()
        || asset.title_fallback.len() > 4096
        || asset.media_type != "application/pdf"
        || asset.provider.provider_type != "google-drive-filesystem"
        || asset.provider.size_bytes > MAX_ASSET_BYTES
        || validate_portable_relative_path(&asset.provider.logical_locator).is_err()
    {
        return Err(MkoError::new(
            "asset_record_invalid",
            "Asset registry record violates the schema-v2 PDF identity contract",
        ));
    }
    Ok(())
}

pub(crate) fn require_hydration_confirmation(
    size_bytes: u64,
    threshold_bytes: u64,
    confirmation: HydrationConfirmationV2,
) -> Result<(), MkoError> {
    if size_bytes > MAX_ASSET_BYTES {
        return Err(MkoError::new(
            "file_too_large",
            "PDF exceeds 50 MiB; use the documented manual processing path",
        ));
    }
    if size_bytes > threshold_bytes && confirmation != HydrationConfirmationV2::Confirmed {
        return Err(MkoError::new(
            "hydration_confirmation_required",
            format!(
                "provider reports {size_bytes} bytes; exact identity requires a complete read or download"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn validated_disjoint_roots(
    repository_root: &Path,
    provider_root: &Path,
) -> Result<(PathBuf, PathBuf), MkoError> {
    let repository_root = canonical_real_directory(repository_root, "repository_root_invalid")?;
    let provider_root = canonical_real_directory(provider_root, "provider_root_invalid")?;
    if repository_root.starts_with(&provider_root) || provider_root.starts_with(&repository_root) {
        return Err(MkoError::new(
            "storage_roots_overlap",
            "the Git KB and provider roots must be disjoint so local plaintext cannot enter the provider",
        ));
    }
    Ok((repository_root, provider_root))
}

fn canonical_real_directory(path: &Path, code: &str) -> Result<PathBuf, MkoError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| MkoError::new(code, error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(code, "root must be a real directory"));
    }
    fs::canonicalize(path).map_err(|error| MkoError::new(code, error.to_string()))
}

pub(crate) struct InspectedProviderFileV2 {
    parent: Dir,
    name: PathBuf,
    pub size_bytes: u64,
}

impl InspectedProviderFileV2 {
    pub fn open_readonly(&self) -> Result<File, MkoError> {
        let mut options = OpenOptions::new();
        options.read(true);
        configure_nofollow(&mut options, false);
        let file = self.parent.open_with(&self.name, &options).map_err(|_| {
            MkoError::new(
                "asset_not_hydrated",
                "provider did not expose a fully local readable Asset snapshot",
            )
        })?;
        let metadata = file
            .metadata()
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != self.size_bytes
        {
            return Err(provider_changed_error());
        }
        Ok(file)
    }
}

pub(crate) fn inspect_provider_file(
    provider_root: &Path,
    logical_locator: &str,
) -> Result<InspectedProviderFileV2, MkoError> {
    validate_portable_relative_path(logical_locator)?;
    let normalized: String = logical_locator.nfc().collect();
    if normalized != logical_locator {
        return Err(MkoError::new(
            "path_not_portable",
            "provider locator must already use canonical NFC spelling",
        ));
    }
    let mut root_options = OpenOptions::new();
    root_options.read(true);
    configure_nofollow(&mut root_options, true);
    let root = File::open_ambient_with(provider_root, &root_options, ambient_authority())
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
    let metadata = root
        .metadata()
        .map_err(|error| MkoError::new("provider_root_invalid", error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "provider_root_invalid",
            "provider root must be a real non-link directory",
        ));
    }
    let mut parent = Dir::from_std_file(root.into_std());
    let path = Path::new(logical_locator);
    let components = path.components().collect::<Vec<_>>();
    for component in &components[..components.len().saturating_sub(1)] {
        let Component::Normal(name) = component else {
            return Err(MkoError::new(
                "path_not_portable",
                "provider locator contains traversal",
            ));
        };
        let mut options = OpenOptions::new();
        options.read(true);
        configure_nofollow(&mut options, true);
        let directory = parent.open_with(name, &options).map_err(|_| {
            MkoError::new(
                "outside_allowed_root",
                "provider locator contains an unreadable link or non-directory",
            )
        })?;
        let metadata = directory
            .metadata()
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(MkoError::new(
                "outside_allowed_root",
                "provider locator contains a link or non-directory",
            ));
        }
        parent = Dir::from_std_file(directory.into_std());
    }
    let Some(Component::Normal(name)) = components.last() else {
        return Err(MkoError::new(
            "path_not_portable",
            "provider locator must name a file",
        ));
    };
    let name = PathBuf::from(name);
    let metadata = parent.symlink_metadata(&name).map_err(|error| {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "asset_not_hydrated"
        } else {
            "file_unreadable"
        };
        MkoError::new(code, error.to_string())
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "file_unreadable",
            "provider Asset must be a regular non-link file",
        ));
    }
    Ok(InspectedProviderFileV2 {
        parent,
        name,
        size_bytes: metadata.len(),
    })
}

pub(crate) fn revalidate_provider_snapshot(
    provider_root: &Path,
    logical_locator: &str,
    expected: &FileSnapshot,
) -> Result<(), MkoError> {
    let inspected = inspect_provider_file(provider_root, logical_locator)?;
    if inspected.size_bytes != expected.size_bytes {
        return Err(provider_changed_error());
    }
    let mut file = inspected.open_readonly()?;
    let actual = fingerprint_open_file(&mut file)?;
    if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
        return Err(provider_changed_error());
    }
    Ok(())
}

fn title_fallback(locator: &str) -> Result<String, MkoError> {
    let title = Path::new(locator)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MkoError::new(
                "path_not_portable",
                "provider locator has no UTF-8 filename",
            )
        })?
        .nfc()
        .collect::<String>();
    if title.is_empty() || title.len() > 4096 {
        return Err(MkoError::new(
            "asset_record_invalid",
            "Asset title fallback is invalid",
        ));
    }
    Ok(title)
}

fn validate_asset_id(id: &str) -> Result<(), MkoError> {
    let hash = id.strip_prefix("personal-asset-").unwrap_or_default();
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(MkoError::new(
            "asset_id_invalid",
            "Asset ID must contain a full lowercase SHA-256 fingerprint",
        ));
    }
    Ok(())
}

fn require_real_directory(path: &Path, code: &str) -> Result<(), MkoError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| MkoError::new(code, error.to_string()))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            code,
            "managed directory must be a real directory",
        ))
    }
}

fn read_bounded_nofollow(path: &Path, limit: u64, subject: &str) -> Result<Vec<u8>, MkoError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_std_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new(format!("{subject}_unreadable"), error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new(format!("{subject}_unreadable"), error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(MkoError::new(
            format!("{subject}_invalid"),
            format!("{subject} must be a bounded regular non-link file"),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(format!("{subject}_unreadable"), error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            format!("{subject}_invalid"),
            format!("{subject} exceeds its bounded input size"),
        ));
    }
    Ok(bytes)
}

fn provider_changed_error() -> MkoError {
    MkoError::new(
        "registered_asset_changed",
        "provider content no longer matches the exact registered Asset snapshot",
    )
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_DIRECTORY: i32 = 0x10_000;
    options.custom_flags(O_NONBLOCK | O_NOFOLLOW | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    const O_DIRECTORY: i32 = 0x10_0000;
    options.custom_flags(O_NONBLOCK | O_NOFOLLOW | if directory { O_DIRECTORY } else { 0 });
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions, directory: bool) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    options.custom_flags(
        FILE_FLAG_OPEN_REPARSE_POINT
            | if directory {
                FILE_FLAG_BACKUP_SEMANTICS
            } else {
                0
            },
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions, _directory: bool) {}

#[cfg(target_os = "linux")]
fn configure_std_nofollow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(0x20_000 | 0x800);
}

#[cfg(target_os = "macos")]
fn configure_std_nofollow(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(0x100 | 0x4);
}

#[cfg(windows)]
fn configure_std_nofollow(options: &mut fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(0x0020_0000);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_std_nofollow(_options: &mut fs::OpenOptions) {}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use crate::{
        provider_scan::{ProviderCatalogEntry, ProviderCatalogScan, ProviderScanWarning},
        scaffold_v2::scaffold_personal_kb_v2,
    };

    use super::{HydrationConfirmationV2, RegisterInboxAssetsRequestV2, apply_inbox_catalog_v2};

    #[test]
    fn placeholder_is_an_item_and_time_limit_stays_a_global_warning() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let provider = root.path().join("provider");
        scaffold_personal_kb_v2(&repository).unwrap();
        fs::create_dir(&provider).unwrap();
        let scan = ProviderCatalogScan {
            scan_complete: false,
            mutation_safe: false,
            entries: vec![ProviderCatalogEntry::Placeholder {
                provider_locator: "offline.pdf".into(),
                relative_path: PathBuf::from("offline.pdf"),
            }],
            warnings: vec![ProviderScanWarning {
                code: "scan_time_limit".into(),
                message: "provider scan reached the time limit".into(),
                provider_locator: None,
            }],
            entries_seen: 1,
            total_pdf_bytes: 0,
        };

        let result = apply_inbox_catalog_v2(
            RegisterInboxAssetsRequestV2 {
                repository_root: &repository,
                provider_root: &provider,
                hydration_confirmation: HydrationConfirmationV2::Confirmed,
            },
            &repository,
            &provider,
            scan,
        );

        assert!(!result.scan_complete);
        assert_eq!(result.remaining, 0);
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].logical_locator, "offline.pdf");
        assert_eq!(
            result.items[0].error.as_ref().unwrap().code(),
            "asset_not_hydrated"
        );
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].code, "scan_time_limit");
        assert!(
            fs::read_dir(repository.join("assets/registry"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}
