//! A local Markdown/text file the owner already holds, kept as immutable
//! evidence alongside its original bytes (§6.1, §6.2).
//!
//! Unlike a web snapshot, a paste, or a captured conversation — none of which
//! have an original file — a local file's identity is the **original bytes'**
//! fingerprint, not the fingerprint of any agent-extracted text. Since
//! md/txt originals are themselves small text files, the original is stored
//! verbatim, content-addressed, in a minimal text-originals store
//! (`assets/originals/<hash>.<ext>`). Phase 3 extends this store to binaries
//! after a separate check-budget decision (§8); Phase 2 covers text only, and
//! the store shares the snapshot-scale bound (~2 MiB).
//!
//! Registration deliberately does not route through the Google Drive Inbox
//! provider machinery (`inspect_provider_file`/`validated_disjoint_roots`):
//! those functions are Inbox-specific, and a local file is read directly from
//! an absolute local path via the existing bounded no-follow reader.

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::{
    asset_v2::{
        AssetRegistrationOutcomeV2, AssetRegistrationResultV2, read_bounded_nofollow,
        validate_asset_record_v2, write_asset_registry_record_v2,
    },
    atomic::{write_new, write_replace},
    clock::SystemClock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    records_v2::{AssetOriginV2, AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2},
    revision_v2::{canonical_json_bytes, sha256_digest},
};

/// Same scale as a snapshot (§6): generous for a note, bounded so a runaway
/// file cannot fill the knowledge base.
pub const MAX_LOCAL_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LOCAL_FILE_TITLE_CHARS: usize = 200;

pub struct RegisterLocalFileRequestV2<'a> {
    pub repository_root: &'a Path,
    /// The absolute local path the file was read from. Recorded as the
    /// locator; the original bytes' fingerprint, not this path, is the
    /// identity.
    pub path: &'a Path,
    pub title: &'a str,
    pub modified_at: DateTime<Utc>,
}

pub fn register_local_file_asset_v2(
    request: RegisterLocalFileRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    KnowledgeConfigV2::read(request.repository_root)?;
    if !request.path.is_absolute() {
        return Err(MkoError::new(
            "local_file_path_invalid",
            "a local file must be registered by its absolute path",
        ));
    }
    reject_path_inside_repository(request.repository_root, request.path)?;
    let bytes = read_bounded_nofollow(request.path, MAX_LOCAL_FILE_BYTES, "local_file")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        MkoError::new(
            "local_file_not_text",
            "only UTF-8 Markdown/text local files may be registered in this Core version",
        )
    })?;
    if text.trim().is_empty() {
        return Err(MkoError::new(
            "local_file_empty",
            "the file has no readable text",
        ));
    }

    let locator = request
        .path
        .to_str()
        .ok_or_else(|| MkoError::new("local_file_path_invalid", "path must be valid UTF-8"))?;
    let extension = original_extension(request.path);
    let fingerprint = sha256_digest(&bytes);
    let hash = fingerprint
        .strip_prefix("sha256:")
        .ok_or_else(|| MkoError::new("local_file_write_failed", "unexpected digest form"))?
        .to_owned();
    let record = AssetRecordV2 {
        schema_version: 2,
        id: format!("personal-asset-{hash}"),
        record_type: AssetRecordTypeV2::Asset,
        origin: AssetOriginV2::LocalFile,
        fingerprint,
        title_fallback: bounded_title(request.title, request.path),
        media_type: "text/plain".into(),
        provider: AssetProviderBindingV2 {
            provider_type: "local-file".into(),
            logical_locator: locator.into(),
            size_bytes: bytes.len() as u64,
            modified_at: Some(request.modified_at),
        },
    };
    // Refuse an unusable path here rather than writing a registry record that
    // could only ever fail on read.
    validate_asset_record_v2(&record)?;
    let record_bytes = canonical_json_bytes(&record)?;

    let _mutation_lock = RepositoryMutationLock::acquire(
        request.repository_root,
        "v2 local-file register",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    // Decide new-vs-existing on the content-addressed registry entry before
    // touching the originals store. The identity (`id`/`fingerprint`) comes
    // from the file's bytes alone, not its extension, so byte-identical
    // content re-registered under a different extension is the same Asset —
    // writing a second `assets/originals/<hash>.<ext>` file for it first and
    // deciding afterwards left that second file orphaned (the registry keeps
    // pointing at the first-registered extension, and nothing ever reads the
    // second one back).
    let result = write_asset_registry_record_v2(request.repository_root, record, &record_bytes)?;
    if result.outcome == AssetRegistrationOutcomeV2::Created {
        write_original_bytes(request.repository_root, &hash, &extension, &bytes)?;
    }
    Ok(result)
}

/// Refuses to register a file whose canonical (symlink-resolved) path lives
/// inside the KB repository itself. Nothing stops `--local-file` from naming
/// a path such as `$KB/README.md`: without this check that would silently
/// write a duplicate into `assets/originals/` and dirty the tree with a
/// second copy of a file the repository already tracks.
fn reject_path_inside_repository(repository_root: &Path, path: &Path) -> Result<(), MkoError> {
    let repository_root = std::fs::canonicalize(repository_root)
        .map_err(|error| MkoError::new("local_file_write_failed", error.to_string()))?;
    let canonical_path = std::fs::canonicalize(path)
        .map_err(|error| MkoError::new("local_file_unreadable", error.to_string()))?;
    if canonical_path == repository_root || canonical_path.starts_with(&repository_root) {
        return Err(MkoError::new(
            "local_file_inside_repository",
            "등록하려는 파일이 지식 베이스 저장소 안에 있습니다 — 저장소 밖의 원본 파일만 --local-file로 등록할 수 있습니다",
        ));
    }
    Ok(())
}

/// The original bytes an Asset was built from, decoded as the UTF-8 text the
/// prepare step consumes. Reads back the store rather than the owner's
/// filesystem again: the original is now content-addressed evidence in the
/// KB, and re-reading the live path would defeat the point of storing it.
pub fn read_original_text_v2(
    repository_root: &Path,
    asset: &AssetRecordV2,
) -> Result<String, MkoError> {
    let hash = asset.fingerprint.strip_prefix("sha256:").ok_or_else(|| {
        MkoError::new(
            "local_file_unreadable",
            "Asset fingerprint is not canonical",
        )
    })?;
    let extension = extension_from_locator(&asset.provider.logical_locator);
    let bytes = read_bounded_nofollow(
        &original_path(repository_root, hash, &extension),
        MAX_LOCAL_FILE_BYTES,
        "local_file_original",
    )?;
    String::from_utf8(bytes)
        .map_err(|error| MkoError::new("local_file_unreadable", error.to_string()))
}

fn write_original_bytes(
    repository_root: &Path,
    hash: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<(), MkoError> {
    // Knowledge bases scaffolded before this store existed have no such
    // directory, and evidence should not need a migration to start being
    // stored: create it on first write, refusing anything that is not a real
    // directory. Mirrors `snapshot_v2::write_snapshot_text`.
    let directory = repository_root.join("assets/originals");
    match std::fs::create_dir(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(&directory)
                .map_err(|error| MkoError::new("local_file_write_failed", error.to_string()))?;
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err(MkoError::new(
                    "local_file_destination_invalid",
                    "assets/originals must be a real directory",
                ));
            }
        }
        Err(error) => return Err(MkoError::new("local_file_write_failed", error.to_string())),
    }
    let path = original_path(repository_root, hash, extension);
    let outcome = write_new(&path, bytes, |existing| {
        let stored = read_bounded_nofollow(existing, MAX_LOCAL_FILE_BYTES, "local_file_original")?;
        if stored == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "local_file_damaged",
                "stored original bytes do not match the identity they are filed under",
            ))
        }
    });
    match outcome {
        Ok(_) => Ok(()),
        // The hash is the identity: bytes that disagree with it are provably
        // damaged, and these bytes are provably what belongs there.
        Err(error) if error.code() == "local_file_damaged" => write_replace(&path, bytes),
        Err(error) => Err(error),
    }
    .map(|_| ())
}

fn original_path(repository_root: &Path, hash: &str, extension: &str) -> std::path::PathBuf {
    repository_root
        .join("assets/originals")
        .join(format!("{hash}.{extension}"))
}

/// Bounds the stored filename extension to a small, known-safe set. The
/// extension is cosmetic — media type is always `text/plain` for this
/// origin — so a file with any other extension, or none, is still accepted
/// and simply filed as `.txt`.
fn original_extension(path: &Path) -> String {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("md") => "md".into(),
        Some(extension) if extension.eq_ignore_ascii_case("markdown") => "markdown".into(),
        _ => "txt".into(),
    }
}

fn extension_from_locator(locator: &str) -> String {
    original_extension(Path::new(locator))
}

/// A local file always has a name to show: its own path. `Asset::title_fallback`
/// mirrors `AssetOriginV2::ProviderPdf`'s use of the file name alone rather
/// than the full path, which is often long and never what the owner meant to
/// name the material.
fn bounded_title(title: &str, path: &Path) -> String {
    let fallback = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("(제목 없는 파일)");
    let candidate = if title.trim().is_empty() {
        fallback
    } else {
        title.trim()
    };
    candidate.chars().take(MAX_LOCAL_FILE_TITLE_CHARS).collect()
}
