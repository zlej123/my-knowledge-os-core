use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration as StdDuration, Instant},
};

use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions, OpenOptionsExt},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{
    asset_v2::{
        HydrationConfirmationV2, inspect_provider_file, read_asset_v2,
        require_hydration_confirmation, revalidate_provider_snapshot, validated_disjoint_roots,
    },
    attempt_v2::{PreparationOutcomeV2, record_preparation_attempt_v2},
    clock::{Clock, SystemClock},
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
    records_v2::{AssetOriginV2, AssetRecordV2},
    revision_v2::{canonical_json_bytes, canonical_json_sha256, sha256_digest},
    snapshot_v2::read_snapshot_text_v2,
};

const MAX_BLOCK_TEXT_BYTES: usize = 240 * 1024;
const MAX_PREPARED_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PREPARED_SESSION_BYTES: u64 = MAX_PREPARED_BUNDLE_BYTES + 64 * 1024;
const PREPARED_SESSION_TTL: Duration = Duration::hours(24);
const MAX_SESSION_CLEANUP_ENTRIES: usize = 128;
const MAX_SESSION_DIRECTORY_ENTRIES: usize = 4096;
const MAX_SESSION_CLEANUP_SCAN_BYTES: u64 = 512 * 1024 * 1024;
const SESSION_CLEANUP_DEADLINE: StdDuration = StdDuration::from_secs(2);
const PREPARED_SESSION_PREFIX: &str = "prepared-content-sha256-";
const PREPARED_SESSION_SUFFIX: &str = ".session.json";
const PREPARED_TEMP_PREFIX: &str = ".mko-prepared-session-";
const PREPARED_TEMP_SUFFIX: &str = ".tmp";
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
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PreparedSessionArtifactTypeV2 {
    PreparedSession,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PreparedSessionArtifactV2 {
    schema_version: u32,
    artifact_type: PreparedSessionArtifactTypeV2,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    bundle: PreparedContentV2,
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
    let repository_root = request.repository_root.to_path_buf();
    let asset_id = request.asset_id.to_owned();
    let outcome = prepare_pdf_asset_v2_with_extractor_and_clock(
        request,
        &SystemClock,
        |snapshot, expected| extract_pdf_pages_in_child(worker_executable, snapshot, expected),
    );
    // Record what happened so a later session can say why material stopped.
    // Failing to write the observation must never replace the real answer: the
    // caller still gets its result, and home simply stays as uninformative as
    // it was before attempts existed.
    let _ = record_preparation_attempt_v2(
        &repository_root,
        &asset_id,
        match &outcome {
            Ok(_) => PreparationOutcomeV2::Prepared,
            Err(_) => PreparationOutcomeV2::Failed,
        },
        outcome.as_ref().err().map(|error| error.code()),
        &SystemClock,
    );
    outcome
}

pub fn prepare_pdf_asset_v2_with_extractor<F>(
    request: PreparePdfAssetRequestV2<'_>,
    extractor: F,
) -> Result<PreparedPdfResultV2, MkoError>
where
    F: FnOnce(File, &FileSnapshot) -> Result<Vec<String>, MkoError>,
{
    prepare_pdf_asset_v2_with_extractor_and_clock(request, &SystemClock, extractor)
}

pub fn prepare_pdf_asset_v2_with_extractor_and_clock<F>(
    request: PreparePdfAssetRequestV2<'_>,
    clock: &dyn Clock,
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
    let bundle_bytes = canonical_json_bytes(&bundle)?;
    if bundle_bytes.len() as u64 > MAX_PREPARED_BUNDLE_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local runtime representation",
        ));
    }

    let _mutation_lock = RepositoryMutationLock::acquire(
        &repository_root,
        "v2 PDF prepare",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    cleanup_expired_sessions(&runtime.prepared, clock)?;
    revalidate_provider_snapshot(&provider_root, &asset.provider.logical_locator, &before)?;
    let filename = published_session_filename(&bundle)?;
    let bundle_path = runtime.prepared_path.join(&filename);
    let created_at = clock.now_utc();
    let session = PreparedSessionArtifactV2 {
        schema_version: 2,
        artifact_type: PreparedSessionArtifactTypeV2::PreparedSession,
        created_at,
        expires_at: created_at + PREPARED_SESSION_TTL,
        bundle: bundle.clone(),
    };
    let bytes = canonical_json_bytes(&session)?;
    if bytes.len() as u64 > MAX_PREPARED_SESSION_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local session representation",
        ));
    }
    let outcome = write_session_immutable(
        &runtime.prepared,
        Path::new(&filename),
        &session,
        &bytes,
        clock,
    )?;
    let persisted = read_cap_prepared_session_v2(&runtime.prepared, Path::new(&filename), clock)?;
    if persisted.bundle != bundle {
        return Err(MkoError::new(
            "prepared_bundle_invalid",
            "prepared plaintext session failed exact canonical validation",
        ));
    }
    Ok(PreparedPdfResultV2 {
        bundle,
        bundle_path,
        outcome,
        created_at: persisted.created_at,
        expires_at: persisted.expires_at,
    })
}

/// Prepares a web snapshot for drafting.
///
/// The PDF path exists to get trustworthy text out of a file the owner holds:
/// it inspects the provider, re-fingerprints the bytes, snapshots them, and
/// runs an extractor in a child process. A snapshot needs none of that. The
/// text is already in the knowledge base, its hash *is* the Asset's identity,
/// and there is no provider file that could have changed underneath.
///
/// What remains is the integrity check that matters: the stored text must still
/// hash to the identity it was registered under, or the evidence a note would
/// cite is not the evidence that was read.
pub fn prepare_snapshot_asset_v2(
    repository_root: &Path,
    asset_id: &str,
    metadata: PreparedMetadataV2,
) -> Result<PreparedPdfResultV2, MkoError> {
    prepare_snapshot_asset_v2_with_clock(repository_root, asset_id, metadata, &SystemClock)
}

pub fn prepare_snapshot_asset_v2_with_clock(
    repository_root: &Path,
    asset_id: &str,
    metadata: PreparedMetadataV2,
    clock: &dyn Clock,
) -> Result<PreparedPdfResultV2, MkoError> {
    let outcome = prepare_snapshot_inner(repository_root, asset_id, metadata, clock);
    // Same observation the PDF path records, for the same reason: a failure
    // that is reported once and discarded leaves material registered, stalled,
    // and unexplained.
    let _ = record_preparation_attempt_v2(
        repository_root,
        asset_id,
        match &outcome {
            Ok(_) => PreparationOutcomeV2::Prepared,
            Err(_) => PreparationOutcomeV2::Failed,
        },
        outcome.as_ref().err().map(|error| error.code()),
        clock,
    );
    outcome
}

fn prepare_snapshot_inner(
    repository_root: &Path,
    asset_id: &str,
    metadata: PreparedMetadataV2,
    clock: &dyn Clock,
) -> Result<PreparedPdfResultV2, MkoError> {
    let config = KnowledgeConfigV2::read(repository_root)?;
    if config.derived_artifacts == DerivedArtifactsPolicyV2::Provider {
        return Err(MkoError::new(
            "derived_artifacts_policy_unsupported",
            "v0.3.0 refuses provider persistence because prepared plaintext cannot be stored safely there",
        ));
    }
    let asset = read_asset_v2(repository_root, asset_id)?;
    if asset.origin != AssetOriginV2::WebSnapshot {
        return Err(MkoError::new(
            "asset_binding_invalid",
            "this Asset is not a web snapshot",
        ));
    }
    let text = read_snapshot_text_v2(repository_root, asset_id)?;
    if sha256_digest(text.as_bytes()) != asset.fingerprint {
        return Err(MkoError::new(
            "registered_asset_changed",
            "the stored snapshot no longer matches the identity it was registered under",
        ));
    }

    let bundle = build_pdf_prepared_content_v2(&asset, std::slice::from_ref(&text), metadata)?;
    let runtime = LocalRuntimeV2::open(repository_root)?;
    let bundle_bytes = canonical_json_bytes(&bundle)?;
    if bundle_bytes.len() as u64 > MAX_PREPARED_BUNDLE_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local runtime representation",
        ));
    }

    let _mutation_lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 snapshot prepare",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    cleanup_expired_sessions(&runtime.prepared, clock)?;
    let filename = published_session_filename(&bundle)?;
    let bundle_path = runtime.prepared_path.join(&filename);
    let created_at = clock.now_utc();
    let session = PreparedSessionArtifactV2 {
        schema_version: 2,
        artifact_type: PreparedSessionArtifactTypeV2::PreparedSession,
        created_at,
        expires_at: created_at + PREPARED_SESSION_TTL,
        bundle: bundle.clone(),
    };
    let bytes = canonical_json_bytes(&session)?;
    if bytes.len() as u64 > MAX_PREPARED_SESSION_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local session representation",
        ));
    }
    let outcome = write_session_immutable(
        &runtime.prepared,
        Path::new(&filename),
        &session,
        &bytes,
        clock,
    )?;
    let persisted = read_cap_prepared_session_v2(&runtime.prepared, Path::new(&filename), clock)?;
    if persisted.bundle != bundle {
        return Err(MkoError::new(
            "prepared_bundle_invalid",
            "prepared plaintext session failed exact canonical validation",
        ));
    }
    Ok(PreparedPdfResultV2 {
        bundle,
        bundle_path,
        outcome,
        created_at: persisted.created_at,
        expires_at: persisted.expires_at,
    })
}

pub fn read_prepared_content_v2(path: &Path) -> Result<PreparedContentV2, MkoError> {
    read_prepared_content_v2_with_clock(path, &SystemClock)
}

pub fn read_prepared_content_v2_with_clock(
    path: &Path,
    clock: &dyn Clock,
) -> Result<PreparedContentV2, MkoError> {
    let bytes = read_bounded_session_nofollow(path)?;
    parse_prepared_session(&bytes, clock).map(|session| session.bundle)
}

/// Cleans Core-owned prepared plaintext sessions during ordinary repository use.
///
/// A repository without a prepared-session runtime is a no-op. Once that
/// runtime exists, cleanup is deliberately fail-closed: an unmanaged name,
/// link, special file, invalid session, or unsafe permission blocks the caller
/// without deleting the suspicious entry. This makes queue/show/dashboard
/// surface local tampering instead of silently stepping around it.
pub fn cleanup_prepared_sessions_v2(
    repository_root: &Path,
    clock: &dyn Clock,
) -> Result<(), MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let repository_root = fs::canonicalize(repository_root)
        .map_err(|error| MkoError::new("repository_root_invalid", error.to_string()))?;
    if !prepared_session_runtime_exists(&repository_root)? {
        return Ok(());
    }
    let _mutation_lock = RepositoryMutationLock::acquire(
        &repository_root,
        "v2 prepared session cleanup",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let Some(prepared) = open_existing_prepared_session_directory(&repository_root)? else {
        return Ok(());
    };
    cleanup_expired_sessions(&prepared, clock)
}

fn parse_prepared_session(
    bytes: &[u8],
    clock: &dyn Clock,
) -> Result<PreparedSessionArtifactV2, MkoError> {
    let session = parse_prepared_session_without_expiry(bytes)?;
    if clock.now_utc() >= session.expires_at {
        return Err(MkoError::new(
            "prepared_session_expired",
            "the prepared plaintext session expired; prepare the Asset again before writing Source or Knowledge",
        ));
    }
    Ok(session)
}

fn parse_prepared_session_without_expiry(
    bytes: &[u8],
) -> Result<PreparedSessionArtifactV2, MkoError> {
    let session: PreparedSessionArtifactV2 = serde_json::from_slice(bytes)
        .map_err(|error| MkoError::new("prepared_session_invalid", error.to_string()))?;
    if canonical_json_bytes(&session)? != bytes {
        return Err(MkoError::new(
            "prepared_session_invalid",
            "prepared plaintext session is not canonical JSON",
        ));
    }
    if session.schema_version != 2
        || session.expires_at != session.created_at + PREPARED_SESSION_TTL
        || session.expires_at <= session.created_at
    {
        return Err(MkoError::new(
            "prepared_session_invalid",
            "prepared plaintext session metadata violates the Core-owned lifetime contract",
        ));
    }
    validate_bundle_integrity(&session.bundle)?;
    Ok(session)
}

fn validate_bundle_integrity(bundle: &PreparedContentV2) -> Result<(), MkoError> {
    let digest = semantic_bundle_digest(bundle)?;
    if bundle.content_digest != digest
        || bundle.bundle_id != format!("prepared-content-{}", digest.replace(':', "-"))
    {
        Err(MkoError::new(
            "prepared_bundle_digest_mismatch",
            "prepared-content-v2 does not match its canonical digest",
        ))
    } else {
        Ok(())
    }
}

/// Builds the canonical schema-v2 prepared-content artifact for a PDF.
///
/// The PDF extractor currently supplies page text rather than stable paragraph
/// geometry, so every locator explicitly advertises `granularity:coarse`.
/// Chunks already-extracted text into the bundle every downstream surface
/// consumes. Named for the PDF path it was written for; a web snapshot arrives
/// here as one page, having never needed an extractor.
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
    // A snapshot's media type is its own; what must hold for either origin is
    // that the identity is derived from the fingerprint of what was stored.
    let media_type_ok = match asset.origin {
        AssetOriginV2::ProviderPdf => asset.media_type == "application/pdf",
        AssetOriginV2::WebSnapshot => asset.media_type == "text/plain",
    };
    if asset.schema_version != 2
        || !media_type_ok
        || expected_id.as_deref() != Some(asset.id.as_str())
    {
        return Err(MkoError::new(
            "asset_binding_invalid",
            "prepared input requires an exact schema-v2 Asset identity",
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
    let value = strip_ambiguous_controls(value);
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
    let value = strip_ambiguous_controls(value);
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

/// Drop control characters that carry no text, keeping the ones that do.
///
/// Extractors emit occasional stray control bytes from font and encoding
/// quirks — a few dozen in a long book. Rejecting the document over them
/// discarded hundreds of readable pages and left the owner nothing to do, while
/// the property that mattered was only ever that canonical text contains no
/// ambiguous controls. Removing them keeps that property by construction, and
/// belongs to the same normalization pass that already collapses whitespace and
/// applies NFC: the bundle is a normalized projection, never a byte-exact copy
/// of the original, which stays preserved and fingerprinted on its own.
fn strip_ambiguous_controls(value: &str) -> String {
    value
        .chars()
        .filter(|character| {
            !character.is_control() || matches!(character, '\n' | '\r' | '\t' | '\u{000c}')
        })
        .collect()
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
        let sessions = ensure_private_directory(&runtime.join("sessions"))?;
        let prepared_path = ensure_private_directory(&sessions.join("prepared"))?;
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

fn prepared_session_runtime_exists(repository_root: &Path) -> Result<bool, MkoError> {
    let path = repository_root.join(".mko/runtime/sessions/prepared");
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MkoError::new("local_runtime_invalid", error.to_string())),
    }
}

fn open_existing_prepared_session_directory(
    repository_root: &Path,
) -> Result<Option<Dir>, MkoError> {
    let mko = repository_root.join(".mko");
    require_real_directory(&mko)?;
    if read_small_regular_nofollow(&mko.join(".gitignore"), 1024)? != b"runtime/\n" {
        return Err(MkoError::new(
            "local_runtime_policy_invalid",
            ".mko/.gitignore must exclude runtime before prepared plaintext is cleaned",
        ));
    }
    for path in [
        mko.join("runtime"),
        mko.join("runtime/sessions"),
        mko.join("runtime/sessions/prepared"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                require_real_directory(&path)?;
                ensure_private_directory_permissions(&path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(MkoError::new("local_runtime_invalid", error.to_string()));
            }
        }
    }
    Dir::open_ambient_dir(
        repository_root.join(".mko/runtime/sessions/prepared"),
        ambient_authority(),
    )
    .map(Some)
    .map_err(|error| MkoError::new("local_runtime_invalid", error.to_string()))
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

fn read_bounded_session_nofollow(path: &Path) -> Result<Vec<u8>, MkoError> {
    let bytes = read_regular_nofollow(path, MAX_PREPARED_SESSION_BYTES)?;
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
            "prepared plaintext session file must be owner-only",
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

fn cleanup_expired_sessions(directory: &Dir, clock: &dyn Clock) -> Result<(), MkoError> {
    let deadline = Instant::now() + SESSION_CLEANUP_DEADLINE;
    let entries = directory
        .entries()
        .map_err(|error| MkoError::new("prepared_session_cleanup_failed", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("prepared_session_cleanup_failed", error.to_string()))?;
    if entries.len() > MAX_SESSION_DIRECTORY_ENTRIES {
        return Err(session_cleanup_limit());
    }

    let mut entries = entries
        .into_iter()
        .map(|entry| {
            let name = entry.file_name();
            let name_text = name
                .to_str()
                .ok_or_else(|| {
                    MkoError::new(
                        "prepared_session_directory_invalid",
                        "prepared session filenames must be Unicode",
                    )
                })?
                .to_owned();
            let kind = managed_session_entry_kind(&name_text)?;
            let path = PathBuf::from(name);
            let metadata = directory.symlink_metadata(&path).map_err(|error| {
                MkoError::new("prepared_session_cleanup_failed", error.to_string())
            })?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > MAX_PREPARED_SESSION_BYTES
            {
                return Err(MkoError::new(
                    "prepared_session_directory_invalid",
                    "prepared session directory contains a link, special file, or oversized entry; inspect and relocate it before retrying",
                ));
            }
            Ok((name_text, path, kind, metadata.len()))
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut scanned_bytes = 0_u64;
    let mut removable = Vec::new();
    for (name, path, kind, size) in entries {
        if Instant::now() >= deadline {
            return Err(session_cleanup_limit());
        }
        scanned_bytes = scanned_bytes
            .checked_add(size)
            .ok_or_else(session_cleanup_limit)?;
        if scanned_bytes > MAX_SESSION_CLEANUP_SCAN_BYTES {
            return Err(session_cleanup_limit());
        }
        let bytes = read_cap_regular_nofollow(directory, &path, MAX_PREPARED_SESSION_BYTES)?;
        if kind == ManagedSessionEntryKind::Temporary {
            // A crash can leave the staged file partially written. Its exact
            // Core-only name, private regular-file checks above, and the held
            // repository mutation lock are sufficient to identify it as an
            // unpublished artifact; parsing it would make crash recovery
            // depend on the write having completed.
            removable.push(path);
            continue;
        }
        let session = parse_prepared_session_without_expiry(&bytes)?;
        validate_session_entry_binding(&name, &kind, &session)?;
        if clock.now_utc() >= session.expires_at {
            removable.push(path);
        }
    }

    let mut removed = false;
    for path in removable.into_iter().take(MAX_SESSION_CLEANUP_ENTRIES) {
        directory
            .remove_file(&path)
            .map_err(|error| MkoError::new("prepared_session_cleanup_failed", error.to_string()))?;
        removed = true;
    }
    if removed {
        sync_cap_directory(directory)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagedSessionEntryKind {
    Published,
    Temporary,
}

fn managed_session_entry_kind(name: &str) -> Result<ManagedSessionEntryKind, MkoError> {
    if let Some(digest) = name
        .strip_prefix(PREPARED_SESSION_PREFIX)
        .and_then(|name| name.strip_suffix(PREPARED_SESSION_SUFFIX))
        && valid_lower_hex_digest(digest)
    {
        return Ok(ManagedSessionEntryKind::Published);
    }
    if let Some(body) = name
        .strip_prefix(PREPARED_TEMP_PREFIX)
        .and_then(|name| name.strip_suffix(PREPARED_TEMP_SUFFIX))
        && let Some((digest_and_pid, counter)) = body.rsplit_once('-')
        && let Some((digest, pid)) = digest_and_pid.rsplit_once('-')
        && valid_lower_hex_digest(digest)
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && !counter.is_empty()
        && counter.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(ManagedSessionEntryKind::Temporary);
    }
    Err(MkoError::new(
        "prepared_session_directory_invalid",
        "prepared session directory contains an unmanaged entry; inspect and relocate it before retrying",
    ))
}

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn validate_session_entry_binding(
    name: &str,
    kind: &ManagedSessionEntryKind,
    session: &PreparedSessionArtifactV2,
) -> Result<(), MkoError> {
    let digest = session
        .bundle
        .bundle_id
        .strip_prefix(PREPARED_SESSION_PREFIX)
        .ok_or_else(|| {
            MkoError::new(
                "prepared_session_directory_invalid",
                "prepared session bundle ID is not canonical",
            )
        })?;
    let matches = match kind {
        ManagedSessionEntryKind::Published => {
            name == format!("{PREPARED_SESSION_PREFIX}{digest}{PREPARED_SESSION_SUFFIX}")
        }
        ManagedSessionEntryKind::Temporary => name
            .strip_prefix(PREPARED_TEMP_PREFIX)
            .and_then(|name| name.strip_suffix(PREPARED_TEMP_SUFFIX))
            .is_some_and(|body| body.starts_with(&format!("{digest}-"))),
    };
    if matches {
        Ok(())
    } else {
        Err(MkoError::new(
            "prepared_session_directory_invalid",
            "prepared session filename does not match its content-addressed bundle",
        ))
    }
}

fn session_cleanup_limit() -> MkoError {
    MkoError::new(
        "prepared_session_cleanup_limit",
        "prepared session cleanup exceeded its deterministic entry, byte, or time bound",
    )
}

fn published_session_filename(bundle: &PreparedContentV2) -> Result<String, MkoError> {
    let digest = bundle
        .bundle_id
        .strip_prefix(PREPARED_SESSION_PREFIX)
        .filter(|digest| valid_lower_hex_digest(digest))
        .ok_or_else(|| {
            MkoError::new(
                "prepared_bundle_invalid",
                "prepared bundle ID is not a canonical content digest",
            )
        })?;
    Ok(format!(
        "{PREPARED_SESSION_PREFIX}{digest}{PREPARED_SESSION_SUFFIX}"
    ))
}

fn write_session_immutable(
    directory: &Dir,
    name: &Path,
    session: &PreparedSessionArtifactV2,
    bytes: &[u8],
    clock: &dyn Clock,
) -> Result<PreparedPersistenceOutcomeV2, MkoError> {
    if bytes.len() as u64 > MAX_PREPARED_SESSION_BYTES {
        return Err(MkoError::new(
            "prepared_bundle_too_large",
            "prepared-content-v2 exceeds its bounded local session representation",
        ));
    }
    match directory.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            let existing_bytes =
                read_cap_regular_nofollow(directory, name, MAX_PREPARED_SESSION_BYTES)?;
            let existing = parse_prepared_session_without_expiry(&existing_bytes)?;
            if clock.now_utc() < existing.expires_at {
                if existing.bundle == session.bundle {
                    return Ok(PreparedPersistenceOutcomeV2::Existing);
                }
                return Err(MkoError::new(
                    "prepared_bundle_conflict",
                    "content-addressed prepared session contains different bundle bytes",
                ));
            }
            directory
                .remove_file(name)
                .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))?;
            sync_cap_directory(directory)?;
        }
        Ok(_) => {
            return Err(MkoError::new(
                "prepared_bundle_destination_invalid",
                "prepared session destination must be a regular non-link file",
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
    let digest = session
        .bundle
        .bundle_id
        .strip_prefix(PREPARED_SESSION_PREFIX)
        .ok_or_else(|| {
            MkoError::new(
                "prepared_bundle_invalid",
                "prepared bundle ID is not a canonical content digest",
            )
        })?;
    let temporary = PathBuf::from(format!(
        "{PREPARED_TEMP_PREFIX}{digest}-{}-{}{PREPARED_TEMP_SUFFIX}",
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
            .take(MAX_PREPARED_SESSION_BYTES + 1)
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
                let existing = read_cap_prepared_session_v2(directory, name, clock)?;
                if existing.bundle == session.bundle {
                    Ok(PreparedPersistenceOutcomeV2::Existing)
                } else {
                    Err(MkoError::new(
                        "prepared_bundle_conflict",
                        "content-addressed prepared session contains different bundle bytes",
                    ))
                }
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

fn read_cap_prepared_session_v2(
    directory: &Dir,
    name: &Path,
    clock: &dyn Clock,
) -> Result<PreparedSessionArtifactV2, MkoError> {
    let bytes = read_cap_regular_nofollow(directory, name, MAX_PREPARED_SESSION_BYTES)?;
    parse_prepared_session(&bytes, clock)
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
            "prepared session must be a bounded regular non-link file",
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
            "prepared session exceeds its bounded input size",
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
            "prepared plaintext session file must be owner-only",
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
            "prepared plaintext session ACL must be owner-only",
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

#[cfg(unix)]
fn sync_cap_directory(directory: &Dir) -> Result<(), MkoError> {
    directory
        .open(".")
        .and_then(|file| file.sync_all())
        .map_err(|error| MkoError::new("local_runtime_write_failed", error.to_string()))
}

#[cfg(windows)]
fn sync_cap_directory(_directory: &Dir) -> Result<(), MkoError> {
    // Windows has no supported POSIX-equivalent parent-directory fsync in this safe API layer.
    // Session file content is flushed before linking the published entry, but parent-entry crash
    // durability is not claimed. This matches the shared capability publisher's platform contract.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_cap_directory(_directory: &Dir) -> Result<(), MkoError> {
    Ok(())
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
