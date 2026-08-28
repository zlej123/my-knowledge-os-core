//! Text with no original file behind it, kept as immutable evidence: a web
//! page the agent read, text the owner pasted, a conversation captured on a
//! recall miss (§6.1, §6.3), or a video transcript the agent read or
//! transcribed (Phase 4, §6).
//!
//! A PDF can be returned to: the file is in the provider and its fingerprint
//! identifies it. None of these can — a web page changes and dies, a paste and
//! a conversation never had a file at all. So the text itself is stored, and
//! the Asset is identified by that text rather than by any address it came
//! from. A note approved today can still be checked against what was actually
//! read, a year after the page stops existing.
//!
//! The Core does not fetch. The workspace has no network dependency and pins
//! every crate exactly; the agent performs the request (or the owner pastes,
//! or the conversation is captured) and hands the extracted text here, the
//! same boundary the semantic path already uses.

use std::{fs, path::Path};

use chrono::{DateTime, Utc};

use crate::{
    asset_v2::{
        AssetRegistrationResultV2, read_bounded_nofollow, validate_asset_record_v2,
        write_asset_registry_record_v2,
    },
    atomic::{write_new, write_replace},
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

/// Text the owner pasted directly, or text captured from a conversation
/// (store-on-miss, §6.3). Neither has an address to return to, so both use
/// the empty-string locator convention (§6.1, decided) rather than widening
/// `logical_locator` to `Option`.
pub struct RegisterPastedTextRequestV2<'a> {
    pub repository_root: &'a Path,
    pub title: &'a str,
    /// The pasted text, verbatim.
    pub text: &'a str,
    pub captured_at: DateTime<Utc>,
}

pub struct RegisterConversationRequestV2<'a> {
    pub repository_root: &'a Path,
    pub title: &'a str,
    /// The conversation content the agent captured, verbatim.
    pub text: &'a str,
    pub captured_at: DateTime<Utc>,
}

/// A video (e.g. YouTube) the agent read or transcribed. Same model as a web
/// page (Phase 4, §6): the Core does not fetch or transcribe, has an address
/// to record, and keeps no original bytes — only the transcript text the
/// agent supplies.
pub struct RegisterVideoTranscriptRequestV2<'a> {
    pub repository_root: &'a Path,
    /// The video's address. Recorded, but not the identity.
    pub url: &'a str,
    pub title: &'a str,
    /// The transcript, as the agent read or transcribed it.
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
    register_text_evidence_v2(TextEvidenceRequestV2 {
        repository_root: request.repository_root,
        origin: AssetOriginV2::WebSnapshot,
        provider_type: "web-snapshot",
        locator: request.url,
        title: request.title,
        text: request.text,
        captured_at: request.fetched_at,
        default_title: "",
    })
}

/// Registers text the owner pasted directly. Same TEXT-fingerprint identity
/// as a web snapshot (§6.1): the pasted text itself is the evidence, and
/// pasting the same text twice returns the same Asset.
pub fn register_pasted_text_v2(
    request: RegisterPastedTextRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    register_text_evidence_v2(TextEvidenceRequestV2 {
        repository_root: request.repository_root,
        origin: AssetOriginV2::PastedText,
        provider_type: "pasted-text",
        locator: "",
        title: request.title,
        text: request.text,
        captured_at: request.captured_at,
        default_title: "(제목 없는 붙여넣기)",
    })
}

/// Registers text captured from a conversation (store-on-miss, §6.3). Same
/// TEXT-fingerprint identity as a web snapshot (§6.1).
pub fn register_conversation_v2(
    request: RegisterConversationRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    register_text_evidence_v2(TextEvidenceRequestV2 {
        repository_root: request.repository_root,
        origin: AssetOriginV2::Conversation,
        provider_type: "conversation",
        locator: "",
        title: request.title,
        text: request.text,
        captured_at: request.captured_at,
        default_title: "(제목 없는 대화)",
    })
}

/// Registers a video transcript the agent read or transcribed (Phase 4,
/// §6). Same TEXT-fingerprint identity and locator contract as a web
/// snapshot (§6.1): the transcript text is the evidence, no original video
/// bytes are ever fetched or stored, and the dormant
/// `ContentBlockV2::Transcript` blocks are deliberately not used here — this
/// is snapshot-model registration, not a structured transcript (§10, D2).
pub fn register_video_transcript_v2(
    request: RegisterVideoTranscriptRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    register_text_evidence_v2(TextEvidenceRequestV2 {
        repository_root: request.repository_root,
        origin: AssetOriginV2::VideoTranscript,
        provider_type: "video-transcript",
        locator: request.url,
        title: request.title,
        text: request.text,
        captured_at: request.fetched_at,
        default_title: "",
    })
}

/// The shared shape behind every text-fingerprint origin (§6.1): the text
/// itself is the evidence and its hash is the identity, so registration only
/// ever needs to validate, fingerprint, and store it — no provider file, no
/// extractor.
struct TextEvidenceRequestV2<'a> {
    repository_root: &'a Path,
    origin: AssetOriginV2,
    provider_type: &'static str,
    locator: &'a str,
    title: &'a str,
    text: &'a str,
    captured_at: DateTime<Utc>,
    /// Shown when both `title` and `locator` are empty.
    default_title: &'static str,
}

fn register_text_evidence_v2(
    request: TextEvidenceRequestV2<'_>,
) -> Result<AssetRegistrationResultV2, MkoError> {
    KnowledgeConfigV2::read(request.repository_root)?;
    let bytes = request.text.as_bytes();
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(MkoError::new(
            "snapshot_too_large",
            "the text is larger than a snapshot may be",
        ));
    }
    if request.text.trim().is_empty() {
        return Err(MkoError::new(
            "snapshot_text_empty",
            "no readable text was supplied",
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
        origin: request.origin,
        fingerprint,
        title_fallback: bounded_title(request.title, request.locator, request.default_title),
        media_type: "text/plain".into(),
        provider: AssetProviderBindingV2 {
            provider_type: request.provider_type.into(),
            logical_locator: request.locator.into(),
            size_bytes: bytes.len() as u64,
            modified_at: Some(request.captured_at),
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
    // Evidence goes through the same writer as every other v2 record: a
    // capability-scoped parent, a destination that must be a regular file, and
    // a temporary-write-fsync-rename that cannot leave a half-written file
    // behind. This once used a bare `exists()` guard, which follows links —
    // a dangling symlink here reported "nothing there" and the write went
    // through it, out of the knowledge base.
    let outcome = write_new(&path, bytes, |existing| {
        let stored = read_bounded_nofollow(existing, MAX_SNAPSHOT_BYTES, "snapshot")?;
        if stored == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "snapshot_damaged",
                "stored snapshot text does not match the identity it is filed under",
            ))
        }
    });
    match outcome {
        Ok(_) => Ok(()),
        // The hash is the identity, so bytes that disagree with it are
        // provably damaged and these bytes are provably what belongs there.
        // Registering the page again is the repair; before, the exists()
        // short-circuit reported success and left the damage in place, which
        // made preparation fail as registered_asset_changed forever.
        Err(error) if error.code() == "snapshot_damaged" => write_replace(&path, bytes),
        Err(error) => Err(error),
    }
}

fn snapshot_path(repository_root: &Path, hash: &str) -> std::path::PathBuf {
    repository_root
        .join("assets/snapshots")
        .join(format!("{hash}.txt"))
}

/// A snapshot always has a name to show. A page that supplied none is named by
/// its address, which is at least what the owner asked for; a paste or a
/// captured conversation has no address, so it falls back to a fixed label
/// instead.
fn bounded_title(title: &str, fallback_locator: &str, default_title: &str) -> String {
    let candidate = if !title.trim().is_empty() {
        title.trim()
    } else if !fallback_locator.trim().is_empty() {
        fallback_locator.trim()
    } else {
        default_title
    };
    candidate.chars().take(MAX_SNAPSHOT_TITLE_CHARS).collect()
}
