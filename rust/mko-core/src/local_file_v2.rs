//! A local file the owner already holds, kept as immutable evidence alongside
//! its original bytes (§6.1, §6.2). Covers Markdown/text (Phase 2), and
//! images and docx/hwpx documents (Phase 3).
//!
//! Unlike a web snapshot, a paste, or a captured conversation — none of which
//! have an original file — a local file's identity is the **original bytes'**
//! fingerprint, not the fingerprint of any agent-extracted text. The original
//! is stored verbatim, content-addressed, in a minimal originals store
//! (`assets/originals/<hash>.<ext>`), bounded per media type (§8's named
//! check-budget decision, resolved in `check.rs`).
//!
//! **[DECIDED, Phase 3] Enum shape.** `AssetOriginV2::LocalFile` is not split
//! into per-media-type variants; instead `media_type` discriminates text,
//! image, and document forms behind the single `LocalFile` origin. Every
//! extension-, signature-, and size-specific rule below lives in one small
//! table (`LOCAL_FILE_MEDIA_TYPES`) rather than scattered `if` chains, so a
//! fifth form is one table row plus one signature function, not a new code
//! path threaded through registration, validation, and `check`.
//!
//! **Text is registered with its own bytes as the evidence** (unchanged from
//! Phase 2): the file must be valid UTF-8, and that text is both the stored
//! original and the extracted text a prepared bundle is built from.
//!
//! **Image and document originals carry no extractable text of their own.**
//! The Core never parses them (D2): registration stores only the original
//! bytes, signature-validated. The agent-read text (OCR output for an image,
//! a converted document body) is supplied later, at prepare time, exactly
//! once per prepare call — see `prepared_v2::prepare_local_file_asset_v2`.
//! This mirrors the PDF path's shape (an extractor supplies pages at prepare
//! time, never at registration) rather than inventing a second persistent
//! text store: re-extraction (a better OCR pass) is simply another prepare
//! call with different supplied text, landing as a new Source/Knowledge
//! revision of the same immutable Asset — never a duplicate Asset, and no
//! extra Asset-level bookkeeping is required for it.
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

/// Text ceiling: generous for a note, bounded so a runaway file cannot fill
/// the knowledge base. Unchanged from Phase 2.
pub const MAX_LOCAL_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// **[DECIDED, Phase 3]** Image originals (screenshots, photos of pages).
pub const MAX_LOCAL_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
/// **[DECIDED, Phase 3]** docx/hwpx originals.
pub const MAX_LOCAL_DOCUMENT_BYTES: u64 = 15 * 1024 * 1024;
/// The largest of the three ceilings above — the bound a caller uses before
/// it knows which of them applies (e.g. `check`'s originals walk, which must
/// read a file before its extension tells it which specific ceiling to check
/// it against).
pub const MAX_LOCAL_ORIGINAL_BYTES: u64 = MAX_LOCAL_DOCUMENT_BYTES;

const MAX_LOCAL_FILE_TITLE_CHARS: usize = 200;

pub const DOCX_MEDIA_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub const HWPX_MEDIA_TYPE: &str = "application/vnd.hancom.hwpx";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFileMediaKindV2 {
    Text,
    Image,
    Document,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LocalFileMediaSpecV2 {
    pub extension: &'static str,
    pub media_type: &'static str,
    pub max_bytes: u64,
    pub kind: LocalFileMediaKindV2,
}

/// One row per recognized extension. Order does not matter — extensions are
/// unique. An extension absent from this table (or a path with none) falls
/// back to the Text/`.txt` entry, exactly as Phase 2 did: the extension is
/// cosmetic for text, so an unrecognized one is still accepted and simply
/// filed as `.txt`. For an image or document, that same fallback means an
/// unrecognized binary extension is treated as text and rejected by the
/// UTF-8 check — a clear, if generic, refusal rather than a silent guess at
/// a binary format this Core version does not support.
const LOCAL_FILE_MEDIA_TYPES: &[(&str, LocalFileMediaSpecV2)] = &[
    (
        "md",
        LocalFileMediaSpecV2 {
            extension: "md",
            media_type: "text/plain",
            max_bytes: MAX_LOCAL_FILE_BYTES,
            kind: LocalFileMediaKindV2::Text,
        },
    ),
    (
        "markdown",
        LocalFileMediaSpecV2 {
            extension: "markdown",
            media_type: "text/plain",
            max_bytes: MAX_LOCAL_FILE_BYTES,
            kind: LocalFileMediaKindV2::Text,
        },
    ),
    (
        "txt",
        LocalFileMediaSpecV2 {
            extension: "txt",
            media_type: "text/plain",
            max_bytes: MAX_LOCAL_FILE_BYTES,
            kind: LocalFileMediaKindV2::Text,
        },
    ),
    (
        "png",
        LocalFileMediaSpecV2 {
            extension: "png",
            media_type: "image/png",
            max_bytes: MAX_LOCAL_IMAGE_BYTES,
            kind: LocalFileMediaKindV2::Image,
        },
    ),
    (
        "jpg",
        LocalFileMediaSpecV2 {
            extension: "jpg",
            media_type: "image/jpeg",
            max_bytes: MAX_LOCAL_IMAGE_BYTES,
            kind: LocalFileMediaKindV2::Image,
        },
    ),
    (
        "jpeg",
        LocalFileMediaSpecV2 {
            extension: "jpeg",
            media_type: "image/jpeg",
            max_bytes: MAX_LOCAL_IMAGE_BYTES,
            kind: LocalFileMediaKindV2::Image,
        },
    ),
    (
        "webp",
        LocalFileMediaSpecV2 {
            extension: "webp",
            media_type: "image/webp",
            max_bytes: MAX_LOCAL_IMAGE_BYTES,
            kind: LocalFileMediaKindV2::Image,
        },
    ),
    (
        "heic",
        LocalFileMediaSpecV2 {
            extension: "heic",
            media_type: "image/heic",
            max_bytes: MAX_LOCAL_IMAGE_BYTES,
            kind: LocalFileMediaKindV2::Image,
        },
    ),
    (
        "docx",
        LocalFileMediaSpecV2 {
            extension: "docx",
            media_type: DOCX_MEDIA_TYPE,
            max_bytes: MAX_LOCAL_DOCUMENT_BYTES,
            kind: LocalFileMediaKindV2::Document,
        },
    ),
    (
        "hwpx",
        LocalFileMediaSpecV2 {
            extension: "hwpx",
            media_type: HWPX_MEDIA_TYPE,
            max_bytes: MAX_LOCAL_DOCUMENT_BYTES,
            kind: LocalFileMediaKindV2::Document,
        },
    ),
];

const TEXT_FALLBACK_SPEC: LocalFileMediaSpecV2 = LocalFileMediaSpecV2 {
    extension: "txt",
    media_type: "text/plain",
    max_bytes: MAX_LOCAL_FILE_BYTES,
    kind: LocalFileMediaKindV2::Text,
};

pub(crate) fn media_spec_for_extension(extension: &str) -> LocalFileMediaSpecV2 {
    let needle = extension.to_ascii_lowercase();
    LOCAL_FILE_MEDIA_TYPES
        .iter()
        .find(|(key, _)| *key == needle)
        .map(|(_, spec)| *spec)
        .unwrap_or(TEXT_FALLBACK_SPEC)
}

pub(crate) fn media_spec_for_path(path: &Path) -> LocalFileMediaSpecV2 {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => media_spec_for_extension(extension),
        None => TEXT_FALLBACK_SPEC,
    }
}

/// The table row a registered Asset's `media_type` corresponds to, or `None`
/// for a media type this Core version has never registered under this
/// origin — a defensive case, not a reachable one, given `media_type` is
/// always drawn from this same table at registration time.
pub(crate) fn media_spec_for_media_type(media_type: &str) -> Option<LocalFileMediaSpecV2> {
    LOCAL_FILE_MEDIA_TYPES
        .iter()
        .map(|(_, spec)| *spec)
        .find(|spec| spec.media_type == media_type)
}

pub fn is_known_local_file_media_type(media_type: &str) -> bool {
    media_spec_for_media_type(media_type).is_some()
}

pub fn local_file_media_kind(media_type: &str) -> Option<LocalFileMediaKindV2> {
    media_spec_for_media_type(media_type).map(|spec| spec.kind)
}

/// Validates a binary original's magic bytes against the signature its
/// extension claims, following `fingerprint::validate_pdf_content`'s
/// precedent. Text carries no signature check — its evidence is that it
/// decodes as UTF-8, checked separately.
fn validate_signature(
    kind: LocalFileMediaKindV2,
    media_type: &str,
    bytes: &[u8],
) -> Result<(), MkoError> {
    let valid = match (kind, media_type) {
        (LocalFileMediaKindV2::Text, _) => true,
        (LocalFileMediaKindV2::Image, "image/png") => bytes.starts_with(b"\x89PNG"),
        (LocalFileMediaKindV2::Image, "image/jpeg") => bytes.starts_with(b"\xFF\xD8\xFF"),
        (LocalFileMediaKindV2::Image, "image/webp") => {
            bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
        }
        (LocalFileMediaKindV2::Image, "image/heic") => valid_heic_ftyp(bytes),
        (LocalFileMediaKindV2::Document, _) => bytes.starts_with(b"PK\x03\x04"),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(MkoError::new(
            "local_file_signature_invalid",
            "file content does not match the signature its extension claims",
        ))
    }
}

/// ISO base media file format `ftyp` box check: bytes 4..8 spell `ftyp`, and
/// the brand at bytes 8..12 is a HEIC/HEIF brand this Core recognizes.
fn valid_heic_ftyp(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    const KNOWN_BRANDS: &[&[u8; 4]] = &[
        b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs", b"mif1", b"msf1",
    ];
    KNOWN_BRANDS.iter().any(|brand| &&bytes[8..12] == brand)
}

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
    let spec = media_spec_for_path(request.path);
    let bytes = read_bounded_nofollow(request.path, spec.max_bytes, "local_file")?;
    match spec.kind {
        LocalFileMediaKindV2::Text => {
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                MkoError::new(
                    "local_file_not_text",
                    "only UTF-8 Markdown/text local files may be registered under this extension",
                )
            })?;
            if text.trim().is_empty() {
                return Err(MkoError::new(
                    "local_file_empty",
                    "the file has no readable text",
                ));
            }
        }
        LocalFileMediaKindV2::Image | LocalFileMediaKindV2::Document => {
            validate_signature(spec.kind, spec.media_type, &bytes)?;
        }
    }

    let locator = request
        .path
        .to_str()
        .ok_or_else(|| MkoError::new("local_file_path_invalid", "path must be valid UTF-8"))?;
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
        media_type: spec.media_type.into(),
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
        write_original_bytes(request.repository_root, &hash, spec.extension, &bytes)?;
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
/// prepare step consumes. Text-kind Assets only: an image or document
/// original has no text of its own to decode (D2) — see
/// `prepared_v2::prepare_local_file_asset_v2`, which takes the agent-read
/// text as an explicit argument for those kinds instead.
pub fn read_original_text_v2(
    repository_root: &Path,
    asset: &AssetRecordV2,
) -> Result<String, MkoError> {
    if local_file_media_kind(&asset.media_type) != Some(LocalFileMediaKindV2::Text) {
        return Err(MkoError::new(
            "local_file_not_text",
            "this Asset's original is not text; its evidence is agent-read text supplied at prepare time",
        ));
    }
    let bytes = read_original_bytes_v2(repository_root, asset)?;
    String::from_utf8(bytes)
        .map_err(|error| MkoError::new("local_file_unreadable", error.to_string()))
}

/// The exact original bytes an Asset was built from, of any local-file media
/// kind, read back from the content-addressed originals store rather than
/// the owner's filesystem again: the original is now committed evidence in
/// the KB, and re-reading the live path would defeat the point of storing
/// it.
pub fn read_original_bytes_v2(
    repository_root: &Path,
    asset: &AssetRecordV2,
) -> Result<Vec<u8>, MkoError> {
    let hash = asset.fingerprint.strip_prefix("sha256:").ok_or_else(|| {
        MkoError::new(
            "local_file_unreadable",
            "Asset fingerprint is not canonical",
        )
    })?;
    // The stored extension is derived from the locator's own extension, not
    // reconstructed from `media_type` alone: several extensions share
    // `text/plain` (`.md`, `.markdown`, `.txt`), so `media_type` cannot
    // uniquely recover which one the original was filed under — the locator
    // (recorded verbatim at registration) can.
    let spec = media_spec_for_path(Path::new(&asset.provider.logical_locator));
    if spec.media_type != asset.media_type {
        return Err(MkoError::new(
            "local_file_unreadable",
            "Asset media type does not match the extension of its own locator",
        ));
    }
    read_bounded_nofollow(
        &original_path(repository_root, hash, spec.extension),
        spec.max_bytes,
        "local_file_original",
    )
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
        let stored =
            read_bounded_nofollow(existing, MAX_LOCAL_ORIGINAL_BYTES, "local_file_original")?;
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
