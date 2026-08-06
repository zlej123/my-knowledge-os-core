//! Text an agent read from the web, kept as immutable evidence.
//!
//! A PDF can be returned to: the file is in the provider and its fingerprint
//! identifies it. A web page cannot — it changes, and it dies. So the text
//! itself is stored, and the Asset is identified by that text rather than by
//! the address it came from. A note approved today can still be checked against
//! what was actually read, a year after the page stops existing.
//!
//! The Core does not fetch. The workspace has no network dependency and pins
//! every crate exactly; the agent performs the request and hands the extracted
//! text here, the same boundary the semantic path already uses.

use std::{fs, path::Path};

use chrono::{DateTime, Utc};

use crate::{
    asset_v2::{
        AssetRegistrationResultV2, validate_asset_record_v2, write_asset_registry_record_v2,
    },
    clock::SystemClock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    records_v2::{AssetOriginV2, AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2},
    revision_v2::{canonical_json_bytes, sha256_digest},
};

/// Generous for an article, small enough that a runaway page cannot fill the
/// knowledge base. A page beyond it is refused, never truncated: half a page of
/// evidence is worse than none, because nothing marks it as half.
pub const MAX_SNAPSHOT_BYTES: u64 = 2 * 1024 * 1024;

/// The owner reads this in the waiting list and the vault.
const MAX_SNAPSHOT_TITLE_CHARS: usize = 200;

pub struct RegisterSnapshotRequestV2<'a> {
    pub repository_root: &'a Path,
    /// The address the text was read from. Recorded, but not the identity.
    pub url: &'a str,
    pub title: &'a str,
    /// The extracted text, as the agent read it.
    pub text: &'a str,
    pub fetched_at: DateTime<Utc>,
}

/// Reads the moment a page was fetched, defaulting to now.
///
/// Parsing lives here rather than in the caller so that what a snapshot's
/// timestamp may be is decided in one place, beside the rest of its contract.
pub fn parse_fetched_at_v2(value: Option<&str>) -> Result<DateTime<Utc>, MkoError> {
    match value {
        Some(value) => DateTime::parse_from_rfc3339(value)
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|error| MkoError::new("snapshot_timestamp_invalid", error.to_string())),
        None => Ok(Utc::now()),
    }
}

pub fn register_web_snapshot_v2(
    request: RegisterSnapshotRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    KnowledgeConfigV2::read(request.repository_root)?;
    let bytes = request.text.as_bytes();
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(MkoError::new(
            "snapshot_too_large",
            "the page text is larger than a snapshot may be",
        ));
    }
    if request.text.trim().is_empty() {
        return Err(MkoError::new(
            "snapshot_text_empty",
            "the page produced no readable text",
        ));
    }

    let fingerprint = sha256_digest(bytes);
    let hash = fingerprint
        .strip_prefix("sha256:")
        .ok_or_else(|| MkoError::new("snapshot_write_failed", "unexpected digest form"))?
        .to_owned();
    let record = AssetRecordV2 {
        schema_version: 2,
        id: format!("personal-asset-{hash}"),
        record_type: AssetRecordTypeV2::Asset,
        origin: AssetOriginV2::WebSnapshot,
        fingerprint,
        title_fallback: bounded_title(request.title, request.url),
        media_type: "text/plain".into(),
        provider: AssetProviderBindingV2 {
            provider_type: "web-snapshot".into(),
            logical_locator: request.url.into(),
            size_bytes: bytes.len() as u64,
            modified_at: Some(request.fetched_at),
        },
    };
    // Refuse an unusable address here rather than writing a registry record
    // that could only ever fail on read.
    validate_asset_record_v2(&record)?;
    let record_bytes = canonical_json_bytes(&record)?;

    let _mutation_lock = RepositoryMutationLock::acquire(
        request.repository_root,
        "v2 snapshot register",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    write_snapshot_text(request.repository_root, &hash, bytes)?;
    write_asset_registry_record_v2(request.repository_root, record, &record_bytes)
}

/// The text an Asset was built from, for a caller that wants to read the
/// evidence rather than the record about it.
pub fn read_snapshot_text_v2(repository_root: &Path, asset_id: &str) -> Result<String, MkoError> {
    let hash = asset_id.strip_prefix("personal-asset-").ok_or_else(|| {
        MkoError::new(
            "snapshot_unreadable",
            "not an Asset identifier, so it names no snapshot",
        )
    })?;
    fs::read_to_string(snapshot_path(repository_root, hash))
        .map_err(|error| MkoError::new("snapshot_unreadable", error.to_string()))
}

fn write_snapshot_text(repository_root: &Path, hash: &str, bytes: &[u8]) -> Result<(), MkoError> {
    // Knowledge bases scaffolded before snapshots existed have no such
    // directory, and evidence should not need a migration to start being
    // stored: create it on first write, refusing anything that is not a real
    // directory.
    let directory = repository_root.join("assets/snapshots");
    match fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&directory)
                .map_err(|error| MkoError::new("snapshot_write_failed", error.to_string()))?;
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(MkoError::new(
                    "snapshot_destination_invalid",
                    "assets/snapshots must be a real directory",
                ));
            }
        }
        Err(error) => return Err(MkoError::new("snapshot_write_failed", error.to_string())),
    }
    let path = directory.join(format!("{hash}.txt"));
    // Content-addressed: identical text is already this file, byte for byte.
    if path.exists() {
        return Ok(());
    }
    fs::write(&path, bytes)
        .map_err(|error| MkoError::new("snapshot_write_failed", error.to_string()))
}

fn snapshot_path(repository_root: &Path, hash: &str) -> std::path::PathBuf {
    repository_root
        .join("assets/snapshots")
        .join(format!("{hash}.txt"))
}

/// A snapshot always has a name to show. A page that supplied none is named by
/// its address, which is at least what the owner asked for.
fn bounded_title(title: &str, url: &str) -> String {
    let candidate = if title.trim().is_empty() {
        url.trim()
    } else {
        title.trim()
    };
    candidate.chars().take(MAX_SNAPSHOT_TITLE_CHARS).collect()
}
