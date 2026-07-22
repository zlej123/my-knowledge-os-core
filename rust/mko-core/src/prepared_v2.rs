use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions, OpenOptionsExt},
};
use unicode_normalization::UnicodeNormalization;

use crate::{
    asset_v2::{
        HydrationConfirmationV2, inspect_provider_file, read_asset_v2,
        require_hydration_confirmation, revalidate_provider_snapshot, validated_disjoint_roots,
    },
    clock::SystemClock,
    config_v2::{DerivedArtifactsPolicyV2, KnowledgeConfigV2},
    error::MkoError,
    fingerprint::{FileSnapshot, fingerprint_open_file, validate_pdf_content},
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    model_v2::{
        ContentBlockV2, ExtractorIdentityV2, PreparedArtifactTypeV2, PreparedContentV2,
        PreparedMetadataV2, PreparedTrustV2,
    },
    pdf::{
        EXTRACTOR_NAME, EXTRACTOR_VERSION, extract_pdf_pages_in_child, validate_extracted_pages,
    },
    records_v2::AssetRecordV2,
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
};

const MAX_BLOCK_TEXT_BYTES: usize = 240 * 1024;
const MAX_PREPARED_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreparedPersistenceOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedPdfResultV2 {
    pub bundle: PreparedContentV2,
    pub bundle_path: PathBuf,
    pub outcome: PreparedPersistenceOutcomeV2,
}

pub struct PreparePdfAssetRequestV2<'a> {
    pub repository_root: &'a Path,
    pub provider_root: &'a Path,
    pub asset_id: &'a str,
    pub metadata: PreparedMetadataV2,
    pub hydration_confirmation: HydrationConfirmationV2,
}

pub fn prepare_pdf_asset_v2(
    request: PreparePdfAssetRequestV2<'_>,
    worker_executable: &Path,
) -> Result<PreparedPdfResultV2, MkoError> {
    prepare_pdf_asset_v2_with_extractor(request, |snapshot, expected| {
        extract_pdf_pages_in_child(worker_executable, snapshot, expected)
    })
}

pub fn prepare_pdf_asset_v2_with_extractor<F>(
    request: PreparePdfAssetRequestV2<'_>,
    extractor: F,
) -> Result<PreparedPdfResultV2, MkoError>
where
    F: FnOnce(File, &FileSnapshot) -> Result<Vec<String>, MkoError>,
{
    let config = KnowledgeConfigV2::read(request.repository_root)?;
    if config.derived_artifacts == DerivedArtifactsPolicyV2::Provider {
        return Err(MkoError::new(
            "derived_artifacts_policy_unsupported",
            "v0.3.0 refuses provider persistence because prepared plaintext cannot be stored safely there",
        ));
    }
    let (repository_root, provider_root) =
        validated_disjoint_roots(request.repository_root, request.provider_root)?;
    let asset = read_asset_v2(&repository_root, request.asset_id)?;
    let inspected = inspect_provider_file(&provider_root, &asset.provider.logical_locator)?;
    if inspected.size_bytes != asset.provider.size_bytes {
        return Err(registered_asset_changed_error());
    }
    require_hydration_confirmation(
        inspected.size_bytes,
        config.provider.hydration_warning_threshold_bytes,
        request.hydration_confirmation,
    )?;
    let mut provider = inspected.open_readonly()?;
    validate_pdf_content(&mut provider)?;
    let before = fingerprint_open_file(&mut provider)?;
    if before.fingerprint.value != asset.fingerprint
        || before.size_bytes != asset.provider.size_bytes
    {
        return Err(registered_asset_changed_error());
    }

    let runtime = LocalRuntimeV2::open(&repository_root)?;
    let snapshot = PdfSnapshotV2::copy_from(&runtime.snapshots, &asset.id, &mut provider, &before)?;
    let pages = extractor(snapshot.clone_file()?, &before)?;
    validate_extracted_pages(&pages)?;

    let retained_after = fingerprint_open_file(&mut provider)?;
    if retained_after.fingerprint != before.fingerprint
        || retained_after.size_bytes != before.size_bytes
    {
        return Err(registered_asset_changed_error());
    }
    revalidate_provider_snapshot(&provider_root, &asset.provider.logical_locator, &before)?;
    let bundle = build_pdf_prepared_content_v2(&asset, &pages, request.metadata)?;
    let bytes = canonical_json_bytes(&bundle)?;
    if bytes.len() as u64 > MAX_PREPARED_BUNDLE_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local runtime representation",
        ));
    }

    let _mutation_lock = RepositoryMutationLock::acquire(
        &repository_root,
        "v2 PDF prepare",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    revalidate_provider_snapshot(&provider_root, &asset.provider.logical_locator, &before)?;
    let filename = format!("{}.json", bundle.bundle_id);
    let bundle_path = runtime.prepared_path.join(&filename);
    let outcome = write_bundle_immutable(&runtime.prepared, Path::new(&filename), &bytes)?;
    let persisted = read_cap_prepared_content_v2(&runtime.prepared, Path::new(&filename))?;
    if persisted != bundle {
        return Err(MkoError::new(
            "prepared_bundle_invalid",
            "persisted prepared bundle failed exact canonical validation",
        ));
    }
    Ok(PreparedPdfResultV2 {
        bundle,
        bundle_path,
        outcome,
    })
}

pub fn read_prepared_content_v2(path: &Path) -> Result<PreparedContentV2, MkoError> {
    let bytes = read_bounded_bundle_nofollow(path)?;
    let bundle: PreparedContentV2 = serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("prepared_bundle_invalid", error.to_string()))?;
    if canonical_json_bytes(&bundle)? != bytes {
        return Err(MkoError::new(
            "prepared_bundle_invalid",
            "prepared-content-v2 cache entry is not canonical JSON",
        ));
    }
    let digest = semantic_bundle_digest(&bundle)?;
    if bundle.content_digest != digest
        || bundle.bundle_id != format!("prepared-content-{}", digest.replace(':', "-"))
    {
        return Err(MkoError::new(
            "prepared_bundle_digest_mismatch",
            "prepared-content-v2 cache entry does not match its canonical digest",
        ));
    }
    Ok(bundle)
}

/// Builds the canonical schema-v2 prepared-content artifact for a PDF.
///
/// The PDF extractor currently supplies page text rather than stable paragraph
/// geometry, so every locator explicitly advertises `granularity:coarse`.
pub fn build_pdf_prepared_content_v2(
    asset: &AssetRecordV2,
    pages: &[String],
    metadata: PreparedMetadataV2,
) -> Result<PreparedContentV2, MkoError> {
    validate_asset(asset)?;
    validate_extracted_pages(pages)?;

    let mut content_blocks = Vec::new();
    let mut sequence = 0_u64;
    for (page_index, page) in pages.iter().enumerate() {
        let normalized = normalize_document_text(page)?;
        for (chunk_index, chunk) in bounded_chunks(&normalized).into_iter().enumerate() {
            sequence += 1;
            content_blocks.push(ContentBlockV2::Text {
                id: format!("block-{sequence:06}"),
                locator: format!(
                    "page:{};chunk:{};granularity:coarse",
                    page_index + 1,
                    chunk_index + 1
                ),
                text: chunk,
            });
        }
    }

    let mut bundle = PreparedContentV2 {
        schema_version: 2,
        artifact_type: PreparedArtifactTypeV2::PreparedContent,
        bundle_id: String::new(),
        content_digest: String::new(),
        asset_id: asset.id.clone(),
        asset_fingerprint: asset.fingerprint.clone(),
        media_type: asset.media_type.clone(),
        trust: PreparedTrustV2::UntrustedDocumentContent,
        extractor: ExtractorIdentityV2 {
            name: EXTRACTOR_NAME.into(),
            version: EXTRACTOR_VERSION.into(),
        },
        metadata: normalize_metadata(metadata)?,
        content_blocks,
        artifacts: Vec::new(),
    };
    let digest = semantic_bundle_digest(&bundle)?;
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
    bundle.content_digest = digest;
    Ok(bundle)
}

fn semantic_bundle_digest(bundle: &PreparedContentV2) -> Result<String, MkoError> {
    let mut value = serde_json::to_value(bundle)
        .map_err(|error| MkoError::new("prepared_bundle_invalid", error.to_string()))?;
    let object = value.as_object_mut().ok_or_else(|| {
        MkoError::new(
            "prepared_bundle_invalid",
            "prepared content must be an object",
        )
    })?;
    object.remove("bundle_id");
    object.remove("content_digest");
    canonical_json_sha256(&value)
}

fn validate_asset(asset: &AssetRecordV2) -> Result<(), MkoError> {
    let expected_id = asset
        .fingerprint
        .strip_prefix("sha256:")
        .map(|digest| format!("personal-asset-{digest}"));
    if asset.schema_version != 2
        || asset.media_type != "application/pdf"
        || expected_id.as_deref() != Some(asset.id.as_str())
    {
        return Err(MkoError::new(
            "asset_binding_invalid",
            "prepared PDF input requires an exact schema-v2 PDF Asset identity",
        ));
    }
    Ok(())
}

fn normalize_metadata(mut metadata: PreparedMetadataV2) -> Result<PreparedMetadataV2, MkoError> {
    metadata.title = metadata
        .title
        .map(|value| normalize_single_line(&value))
        .transpose()?;
    metadata.authors = metadata
        .authors
        .into_iter()
        .map(|value| normalize_single_line(&value))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(metadata)
}

fn normalize_single_line(value: &str) -> Result<String, MkoError> {
    reject_ambiguous_controls(value)?;
    Ok(value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect())
}

fn normalize_document_text(value: &str) -> Result<String, MkoError> {
    reject_ambiguous_controls(value)?;
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut output = Vec::new();
    let mut previous_blank = true;
    for line in normalized.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let blank = line.is_empty();
        if blank && previous_blank {
            continue;
        }
        output.push(line);
        previous_blank = blank;
    }
    while output.last().is_some_and(String::is_empty) {
        output.pop();
    }
    Ok(output.join("\n").nfc().collect())
}

fn reject_ambiguous_controls(value: &str) -> Result<(), MkoError> {
    if value.chars().any(|character| {
        character.is_control() && !matches!(character, '\n' | '\r' | '\t' | '\u{000c}')
    }) {
        return Err(MkoError::new(
            "prepared_text_invalid",
            "extracted content contains unsupported control characters",
        ));
    }
    Ok(())
}

fn bounded_chunks(value: &str) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < value.len() {
        let mut end = value.len().min(start + MAX_BLOCK_TEXT_BYTES);
        while end > start && !value.is_char_boundary(end) {
            end -= 1;
        }
        if end < value.len()
            && let Some(newline) = value[start..end].rfind('\n')
            && newline > 0
        {
            end = start + newline;
        }
        chunks.push(value[start..end].to_owned());
        start = end;
        if value.as_bytes().get(start) == Some(&b'\n') {
            start += 1;
        }
    }
    chunks
}

struct LocalRuntimeV2 {
    snapshots: Dir,
    prepared: Dir,
    prepared_path: PathBuf,
}

impl LocalRuntimeV2 {
    fn open(repository_root: &Path) -> Result<Self, MkoError> {
        let mko = repository_root.join(".mko");
        require_real_directory(&mko)?;
        let ignore = mko.join(".gitignore");
        let ignore_bytes = read_small_regular_nofollow(&ignore, 1024)?;
        if ignore_bytes != b"runtime/\n" {
            return Err(MkoError::new(
                "local_runtime_policy_invalid",
                ".mko/.gitignore must exclude runtime before prepared plaintext is persisted",
            ));
        }
        let runtime = ensure_private_directory(&mko.join("runtime"))?;
        let cache = ensure_private_directory(&runtime.join("cache"))?;
        let prepared_path = ensure_private_directory(&cache.join("prepared"))?;
        let snapshots_path = ensure_private_directory(&runtime.join("snapshots"))?;
        let prepared = Dir::open_ambient_dir(&prepared_path, ambient_authority())
            .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
        let snapshots = Dir::open_ambient_dir(&snapshots_path, ambient_authority())
            .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
        Ok(Self {
            snapshots,
            prepared,
            prepared_path,
        })
    }
}

struct PdfSnapshotV2 {
    directory: Dir,
    name: PathBuf,
    file: Option<File>,
}

impl PdfSnapshotV2 {
    fn copy_from(
        directory: &Dir,
        asset_id: &str,
        provider: &mut File,
        expected: &FileSnapshot,
    ) -> Result<Self, MkoError> {
        let name = PathBuf::from(format!(
            ".{asset_id}.{}.{}.pdf",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        let mut snapshot = directory
            .open_with(&name, &options)
            .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
        protect_private_cap_file(&snapshot)?;
        provider
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        let copy_result = (|| {
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = provider
                    .read(&mut buffer)
                    .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
                if read == 0 {
                    break;
                }
                snapshot.write_all(&buffer[..read]).map_err(|error| {
                    MkoError::new("local_runtime_write_failed", error.to_string())
                })?;
            }
            snapshot
                .sync_all()
                .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))
        })();
        provider
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("file_unreadable", error.to_string()))?;
        if let Err(error) = copy_result {
            drop(snapshot);
            let _ = directory.remove_file(&name);
            return Err(error);
        }
        let retained = directory
            .try_clone()
            .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
        let result = Self {
            directory: retained,
            name,
            file: Some(snapshot),
        };
        result.verify(expected)?;
        Ok(result)
    }

    fn verify(&self, expected: &FileSnapshot) -> Result<(), MkoError> {
        let mut file = self.clone_file()?;
        let actual = fingerprint_open_file(&mut file)?;
        if actual.fingerprint != expected.fingerprint || actual.size_bytes != expected.size_bytes {
            return Err(registered_asset_changed_error());
        }
        Ok(())
    }

    fn clone_file(&self) -> Result<File, MkoError> {
        self.file
            .as_ref()
            .ok_or_else(|| {
                MkoError::new("local_runtime_invalid", "PDF snapshot handle is missing")
            })?
            .try_clone()
            .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))
    }
}

impl Drop for PdfSnapshotV2 {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = self.directory.remove_file(&self.name);
    }
}

fn ensure_private_directory(path: &Path) -> Result<PathBuf, MkoError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(MkoError::new(
                "local_runtime_write_failed",
                error.to_string(),
            ));
        }
    }
    require_real_directory(path)?;
    protect_private_directory(path)?;
    ensure_private_directory_permissions(path)?;
    Ok(path.to_path_buf())
}

fn require_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime path must be a real directory",
        ))
    }
}

fn read_bounded_bundle_nofollow(path: &Path) -> Result<Vec<u8>, MkoError> {
    let bytes = read_regular_nofollow(path, MAX_PREPARED_BUNDLE_BYTES)?;
    ensure_private_file_permissions(path)?;
    Ok(bytes)
}

fn read_small_regular_nofollow(path: &Path, limit: u64) -> Result<Vec<u8>, MkoError> {
    read_regular_nofollow(path, limit)
}

fn read_regular_nofollow(path: &Path, limit: u64) -> Result<Vec<u8>, MkoError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    configure_std_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime entry must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "local runtime entry exceeds its bounded input size",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_directory(path: &Path) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_path(
        path,
        mko_windows_acl::Inheritance::ContainersAndObjects,
    )
    .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_directory(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local runtime directory must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_directory_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_directory_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

fn protect_private_cap_file(file: &File) -> Result<(), MkoError> {
    let std_file = file
        .try_clone()
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?
        .into_std();
    protect_private_std_file(&std_file)
}

#[cfg(unix)]
fn protect_private_std_file(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(windows)]
fn protect_private_std_file(file: &fs::File) -> Result<(), MkoError> {
    mko_windows_acl::apply_owner_only_to_file(file)
        .map(|_| ())
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))
}

#[cfg(not(any(unix, windows)))]
fn protect_private_std_file(_file: &fs::File) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

#[cfg(unix)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    if metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "prepared plaintext cache file must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_file_permissions(path: &Path) -> Result<(), MkoError> {
    validate_windows_acl(path)
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_file_permissions(_path: &Path) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

#[cfg(windows)]
fn validate_windows_acl(path: &Path) -> Result<(), MkoError> {
    const FULL_CONTROL_MASK: u32 = 0x001f_01ff;
    let inspection = mko_windows_acl::inspect_path(path)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    if inspection.owner_is_current_user
        && inspection.dacl_is_protected
        && inspection.entries.len() == 1
        && inspection.entries[0].allows_current_user
        && inspection.entries[0].access_mask == FULL_CONTROL_MASK
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "local runtime ACL must grant full control only to the current user",
        ))
    }
}

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

fn registered_asset_changed_error() -> MkoError {
    MkoError::new(
        "registered_asset_changed",
        "provider content changed before or during extraction; prepared output was discarded",
    )
}

fn write_bundle_immutable(
    directory: &Dir,
    name: &Path,
    bytes: &[u8],
) -> Result<PreparedPersistenceOutcomeV2, MkoError> {
    if bytes.len() as u64 > MAX_PREPARED_BUNDLE_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local runtime representation",
        ));
    }
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            require_cap_bundle_bytes(directory, name, bytes)?;
            return Ok(PreparedPersistenceOutcomeV2::Existing);
        }
        Ok(_) => {
            return Err(MkoError::new(
                "prepared_bundle_destination_invalid",
                "prepared bundle destination must be a regular non-link file",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MkoError::new(
                "local_runtime_write_failed",
                error.to_string(),
            ));
        }
    }
    let temporary = PathBuf::from(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    let mut staged = directory
        .open_with(&temporary, &options)
        .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
    protect_private_cap_file(&staged)?;
    let result = (|| {
        staged
            .write_all(bytes)
            .and_then(|_| staged.sync_all())
            .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
        staged
            .seek(SeekFrom::Start(0))
            .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
        let mut actual = Vec::with_capacity(bytes.len());
        (&mut staged)
            .take(MAX_PREPARED_BUNDLE_BYTES + 1)
            .read_to_end(&mut actual)
            .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
        if actual != bytes {
            return Err(MkoError::new(
                "local_runtime_write_failed",
                "staged prepared bundle failed byte-for-byte verification",
            ));
        }
        drop(staged);
        match directory.hard_link(&temporary, directory, name) {
            Ok(()) => {
                directory.remove_file(&temporary).map_err(|error| {
                    MkoError::new("local_runtime_write_failed", error.to_string())
                })?;
                sync_cap_directory(directory)?;
                Ok(PreparedPersistenceOutcomeV2::Created)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                directory.remove_file(&temporary).map_err(|error| {
                    MkoError::new("local_runtime_write_failed", error.to_string())
                })?;
                require_cap_bundle_bytes(directory, name, bytes)?;
                Ok(PreparedPersistenceOutcomeV2::Existing)
            }
            Err(error) => Err(MkoError::new(
                "local_runtime_write_failed",
                error.to_string(),
            )),
        }
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result
}

fn require_cap_bundle_bytes(directory: &Dir, name: &Path, expected: &[u8]) -> Result<(), MkoError> {
    let bytes = read_cap_regular_nofollow(directory, name, MAX_PREPARED_BUNDLE_BYTES)?;
    if bytes == expected {
        Ok(())
    } else {
        Err(MkoError::new(
            "prepared_bundle_conflict",
            "content-addressed prepared bundle path contains different bytes",
        ))
    }
}

fn read_cap_prepared_content_v2(
    directory: &Dir,
    name: &Path,
) -> Result<PreparedContentV2, MkoError> {
    let bytes = read_cap_regular_nofollow(directory, name, MAX_PREPARED_BUNDLE_BYTES)?;
    let bundle: PreparedContentV2 = serde_json::from_slice(&bytes)
        .map_err(|error| MkoError::new("prepared_bundle_invalid", error.to_string()))?;
    if canonical_json_bytes(&bundle)? != bytes {
        return Err(MkoError::new(
            "prepared_bundle_invalid",
            "persisted prepared bundle is not canonical JSON",
        ));
    }
    Ok(bundle)
}

fn read_cap_regular_nofollow(
    directory: &Dir,
    name: &Path,
    limit: u64,
) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_cap_nofollow(&mut options);
    let file = directory
        .open_with(name, &options)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "prepared bundle must be a bounded regular non-link file",
        ));
    }
    let std_file = file
        .try_clone()
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?
        .into_std();
    ensure_private_std_file_permissions(&std_file)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            "local_runtime_invalid",
            "prepared bundle exceeds its bounded input size",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn ensure_private_std_file_permissions(file: &fs::File) -> Result<(), MkoError> {
    use std::os::unix::fs::PermissionsExt;
    if file
        .metadata()
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?
        .permissions()
        .mode()
        & 0o077
        == 0
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "prepared plaintext cache file must be owner-only",
        ))
    }
}

#[cfg(windows)]
fn ensure_private_std_file_permissions(file: &fs::File) -> Result<(), MkoError> {
    let inspection = mko_windows_acl::apply_owner_only_to_file(file)
        .map_err(|error| MkoError::new("local_runtime_permissions_invalid", error.to_string()))?;
    const FULL_CONTROL_MASK: u32 = 0x001f_01ff;
    if inspection.owner_is_current_user
        && inspection.dacl_is_protected
        && inspection.entries.len() == 1
        && inspection.entries[0].allows_current_user
        && inspection.entries[0].access_mask == FULL_CONTROL_MASK
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_runtime_permissions_invalid",
            "prepared plaintext cache ACL must be owner-only",
        ))
    }
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_std_file_permissions(_file: &fs::File) -> Result<(), MkoError> {
    Err(MkoError::new(
        "local_runtime_permissions_unsupported",
        "owner-only local runtime permissions cannot be verified on this platform",
    ))
}

fn sync_cap_directory(directory: &Dir) -> Result<(), MkoError> {
    directory
        .try_clone()
        .and_then(|directory| directory.into_std_file().sync_all())
        .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))
}

#[cfg(target_os = "linux")]
fn configure_cap_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x20_000 | 0x800);
}

#[cfg(target_os = "macos")]
fn configure_cap_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x100 | 0x4);
}

#[cfg(windows)]
fn configure_cap_nofollow(options: &mut OpenOptions) {
    options.custom_flags(0x0020_0000);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_cap_nofollow(_options: &mut OpenOptions) {}
