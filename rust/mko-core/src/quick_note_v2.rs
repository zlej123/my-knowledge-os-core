use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new},
    clock::Clock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    revision_v2::{canonical_json_sha256, sha256_digest},
};

const MAX_NOTE_TEXT_BYTES: usize = 32 * 1024;
const MAX_NOTE_FILE_BYTES: u64 = 96 * 1024;
const MAX_NOTES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickNoteAuthorshipV2 {
    UserConfirmedViaTty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuickNoteV2 {
    pub schema_version: u32,
    pub record_type: String,
    pub id: String,
    pub text: String,
    pub text_digest: String,
    pub authorship: QuickNoteAuthorshipV2,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedQuickNoteV2 {
    pub note: QuickNoteV2,
    pub markdown: Vec<u8>,
    pub confirmation_card: Vec<u8>,
    pub card_digest: String,
    pub confirmation_phrase: String,
    pub input_changed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuickNotePublicationOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickNotePublicationV2 {
    pub path: PathBuf,
    pub outcome: QuickNotePublicationOutcomeV2,
}

pub fn prepare_quick_note_v2(
    text: &str,
    created_at: DateTime<Utc>,
) -> Result<PreparedQuickNoteV2, MkoError> {
    let normalized = normalize_exact_text(text)?;
    let text_digest = sha256_digest(normalized.as_bytes());
    let authorship = QuickNoteAuthorshipV2::UserConfirmedViaTty;
    let identity = canonical_json_sha256(&serde_json::json!({
        "authorship": authorship,
        "created_at": created_at,
        "text_digest": text_digest,
    }))?;
    let note = QuickNoteV2 {
        schema_version: 2,
        record_type: "quick_note".into(),
        id: format!(
            "personal-note-{}",
            identity.strip_prefix("sha256:").unwrap_or_default()
        ),
        text: normalized.clone(),
        text_digest,
        authorship,
        created_at,
    };
    let markdown = render_markdown(&note, &format!("# Quick note\n\n{normalized}\n"))?.into_bytes();
    let confirmation_card = format!(
        "# Quick note confirmation\n\n- Note ID: `{}`\n- Text digest: `{}`\n\n## Exact text\n\n{}\n",
        note.id, note.text_digest, normalized
    )
    .into_bytes();
    let card_digest = sha256_digest(&confirmation_card);
    let confirmation_phrase = format!("save-note {card_digest}");
    Ok(PreparedQuickNoteV2 {
        note,
        markdown,
        confirmation_card,
        card_digest,
        confirmation_phrase,
        input_changed: text.as_bytes() != normalized.as_bytes(),
    })
}

pub fn publish_quick_note_v2(
    repository_root: &Path,
    prepared: &PreparedQuickNoteV2,
    confirmation: &str,
    clock: &dyn Clock,
) -> Result<QuickNotePublicationV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let canonical = prepare_quick_note_v2(&prepared.note.text, prepared.note.created_at)?;
    if canonical.note != prepared.note
        || canonical.markdown != prepared.markdown
        || canonical.confirmation_card != prepared.confirmation_card
        || canonical.card_digest != prepared.card_digest
        || canonical.confirmation_phrase != prepared.confirmation_phrase
    {
        return Err(MkoError::new(
            "quick_note_invalid",
            "prepared quick note does not match its canonical exact-text record",
        ));
    }
    if confirmation != prepared.confirmation_phrase {
        return Err(MkoError::new(
            "quick_note_confirmation_mismatch",
            "quick note confirmation does not match the exact displayed text",
        ));
    }

    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 quick note publish",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let directory = repository_root.join("notes");
    ensure_real_directory(&directory)?;
    let path = directory.join(format!("{}.md", prepared.note.id));
    let result = write_new(&path, &prepared.markdown, |existing| {
        let bytes = read_regular_nofollow(existing)?;
        if bytes == prepared.markdown {
            Ok(())
        } else {
            Err(MkoError::new(
                "quick_note_conflict",
                "immutable quick note path contains different bytes",
            ))
        }
    })
    .map_err(|error| MkoError::new("quick_note_publication_failed", error.message()))?;
    Ok(QuickNotePublicationV2 {
        path,
        outcome: match result {
            AtomicWriteResult::Created => QuickNotePublicationOutcomeV2::Created,
            AtomicWriteResult::Existing => QuickNotePublicationOutcomeV2::Existing,
        },
    })
}

pub fn list_quick_notes_v2(repository_root: &Path) -> Result<Vec<QuickNoteV2>, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let directory = repository_root.join("notes");
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(MkoError::new("quick_note_scan_failed", error.to_string())),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "quick_note_scan_failed",
            "notes must be a real directory",
        ));
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| MkoError::new("quick_note_scan_failed", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("quick_note_scan_failed", error.to_string()))?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_NOTES {
        return Err(MkoError::new(
            "quick_note_scan_limit",
            "quick note scan exceeded its bounded entry limit",
        ));
    }
    let mut notes = Vec::with_capacity(entries.len());
    for entry in entries {
        let bytes = read_regular_nofollow(&entry.path())?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| MkoError::new("quick_note_invalid", "quick note must be UTF-8"))?;
        let parsed = parse_markdown::<QuickNoteV2>(text)
            .map_err(|_| MkoError::new("quick_note_invalid", "quick note is not canonical"))?;
        let prepared = prepare_quick_note_v2(&parsed.metadata.text, parsed.metadata.created_at)?;
        if prepared.note != parsed.metadata
            || prepared.markdown != bytes
            || entry.file_name().to_string_lossy() != format!("{}.md", prepared.note.id)
        {
            return Err(MkoError::new(
                "quick_note_invalid",
                "quick note identity or canonical bytes are inconsistent",
            ));
        }
        notes.push(prepared.note);
    }
    notes.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(left.id.cmp(&right.id))
    });
    Ok(notes)
}

pub fn search_quick_notes_v2(
    repository_root: &Path,
    term: &str,
) -> Result<Vec<QuickNoteV2>, MkoError> {
    let needle = term.trim().to_lowercase();
    if needle.is_empty() {
        return Err(MkoError::new(
            "quick_note_search_invalid",
            "search term must not be empty",
        ));
    }
    Ok(list_quick_notes_v2(repository_root)?
        .into_iter()
        .filter(|note| note.text.to_lowercase().contains(&needle))
        .collect())
}

fn normalize_exact_text(text: &str) -> Result<String, MkoError> {
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect::<String>();
    let normalized = normalized.trim_matches('\n').to_owned();
    if normalized.is_empty() || normalized.len() > MAX_NOTE_TEXT_BYTES {
        return Err(MkoError::new(
            "quick_note_text_invalid",
            "quick note must contain 1 to 32768 UTF-8 bytes",
        ));
    }
    if normalized.chars().any(disallowed_character) {
        return Err(MkoError::new(
            "quick_note_text_invalid",
            "quick note contains control or bidirectional formatting characters",
        ));
    }
    Ok(normalized)
}

fn disallowed_character(character: char) -> bool {
    (character.is_control() && character != '\n' && character != '\t')
        || matches!(
            character,
            '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn ensure_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)
                .map_err(|error| MkoError::new("quick_note_path_invalid", error.to_string()))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(MkoError::new(
                    "quick_note_path_invalid",
                    "notes must be a real directory",
                ))
            }
        }
        Err(error) => Err(MkoError::new(
            "quick_note_publication_failed",
            error.to_string(),
        )),
    }
}

fn read_regular_nofollow(path: &Path) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new("quick_note_unreadable", error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("quick_note_unreadable", error.to_string()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_NOTE_FILE_BYTES
    {
        return Err(MkoError::new(
            "quick_note_invalid",
            "quick note must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_NOTE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("quick_note_unreadable", error.to_string()))?;
    if bytes.len() as u64 > MAX_NOTE_FILE_BYTES {
        return Err(MkoError::new(
            "quick_note_invalid",
            "quick note exceeds its byte limit",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NOFOLLOW: i32 = 0x20_000;
    const O_NONBLOCK: i32 = 0x800;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_nofollow(options: &mut OpenOptions) {
    const O_NOFOLLOW: i32 = 0x100;
    const O_NONBLOCK: i32 = 0x4;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_nofollow(options: &mut OpenOptions) {
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_nofollow(_options: &mut OpenOptions) {}
