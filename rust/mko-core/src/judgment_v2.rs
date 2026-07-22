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
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::{
    atomic::{AtomicWriteResult, write_new},
    clock::Clock,
    config_v2::KnowledgeConfigV2,
    error::MkoError,
    front_matter::render_markdown,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    revision_v2::{canonical_json_bytes, sha256_digest},
};

const MAX_JUDGMENT_BYTES: usize = 32 * 1024;
const MAX_KNOWLEDGE_REVISION_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgmentAuthorshipV2 {
    UserConfirmedViaConversation,
    UserConfirmedViaTty,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JudgmentAnnotationV2 {
    pub schema_version: u32,
    pub record_type: String,
    pub id: String,
    pub knowledge_id: String,
    pub knowledge_revision: String,
    pub text: String,
    pub text_digest: String,
    pub authorship: JudgmentAuthorshipV2,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedJudgmentV2 {
    pub annotation: JudgmentAnnotationV2,
    pub markdown: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JudgmentPublicationOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JudgmentPublicationV2 {
    pub path: PathBuf,
    pub outcome: JudgmentPublicationOutcomeV2,
}

pub fn prepare_judgment_v2(
    knowledge_id: &str,
    knowledge_revision: &str,
    text: &str,
    authorship: JudgmentAuthorshipV2,
    created_at: DateTime<Utc>,
) -> Result<PreparedJudgmentV2, MkoError> {
    validate_knowledge_id(knowledge_id)?;
    validate_sha256(knowledge_revision, "knowledge_revision_invalid")?;
    let normalized = normalize_exact_text(text)?;
    let text_digest = digest(normalized.as_bytes());
    let identity_input = serde_json::json!({
        "knowledge_id": knowledge_id,
        "knowledge_revision": knowledge_revision,
        "text_digest": text_digest,
        "authorship": authorship,
    });
    let identity = digest(&canonical_json_bytes(&identity_input)?);
    let annotation = JudgmentAnnotationV2 {
        schema_version: 2,
        record_type: "judgment".into(),
        id: format!(
            "personal-judgment-{}",
            identity.strip_prefix("sha256:").unwrap_or_default()
        ),
        knowledge_id: knowledge_id.into(),
        knowledge_revision: knowledge_revision.into(),
        text: normalized.clone(),
        text_digest,
        authorship,
        created_at,
    };
    let body = format!("# User judgment\n\n{normalized}\n");
    let markdown = render_markdown(&annotation, &body)?.into_bytes();
    Ok(PreparedJudgmentV2 {
        annotation,
        markdown,
    })
}

pub fn publish_judgment_v2(
    repository_root: &Path,
    prepared: &PreparedJudgmentV2,
    clock: &dyn Clock,
) -> Result<JudgmentPublicationV2, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    let canonical = prepare_judgment_v2(
        &prepared.annotation.knowledge_id,
        &prepared.annotation.knowledge_revision,
        &prepared.annotation.text,
        prepared.annotation.authorship.clone(),
        prepared.annotation.created_at,
    )?;
    if canonical != *prepared {
        return Err(MkoError::new(
            "judgment_annotation_invalid",
            "judgment annotation does not match its canonical identity and exact text",
        ));
    }

    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 judgment publish",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let record_directory = repository_root
        .join("knowledge")
        .join(&prepared.annotation.knowledge_id);
    validate_real_directory(&record_directory)?;
    let revision_path = record_directory.join("revisions").join(format!(
        "{}.md",
        prepared.annotation.knowledge_revision.replace(':', "-")
    ));
    let revision = read_regular_nofollow(
        &revision_path,
        MAX_KNOWLEDGE_REVISION_BYTES,
        "judgment_revision_invalid",
    )?;
    if sha256_digest(&revision) != prepared.annotation.knowledge_revision {
        return Err(MkoError::new(
            "judgment_revision_invalid",
            "the linked Knowledge revision bytes do not match their identity",
        ));
    }
    let judgments = record_directory.join("judgments");
    ensure_real_directory(&judgments)?;
    let path = judgments.join(format!("{}.md", prepared.annotation.id));
    let result = write_new(&path, &prepared.markdown, |existing| {
        let bytes = read_regular_nofollow(
            existing,
            MAX_JUDGMENT_BYTES as u64 * 2,
            "judgment_annotation_invalid",
        )?;
        if bytes == prepared.markdown {
            Ok(())
        } else {
            Err(MkoError::new(
                "judgment_annotation_conflict",
                "the immutable judgment path contains different bytes",
            ))
        }
    })
    .map_err(|error| MkoError::new("judgment_publication_failed", error.message()))?;
    Ok(JudgmentPublicationV2 {
        path,
        outcome: match result {
            AtomicWriteResult::Created => JudgmentPublicationOutcomeV2::Created,
            AtomicWriteResult::Existing => JudgmentPublicationOutcomeV2::Existing,
        },
    })
}

fn normalize_exact_text(text: &str) -> Result<String, MkoError> {
    let normalized: String = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    let trimmed = normalized.trim_matches('\n');
    if trimmed.is_empty() || trimmed.len() > MAX_JUDGMENT_BYTES {
        return Err(MkoError::new(
            "judgment_text_invalid",
            "judgment text must contain 1 to 32768 UTF-8 bytes",
        ));
    }
    if trimmed.chars().any(disallowed_character) {
        return Err(MkoError::new(
            "judgment_text_invalid",
            "judgment text contains control or bidirectional formatting characters",
        ));
    }
    Ok(trimmed.to_owned())
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

fn validate_knowledge_id(value: &str) -> Result<(), MkoError> {
    let valid = value
        .strip_prefix("personal-knowledge-")
        .is_some_and(|hash| is_lower_hex(hash, 64));
    if valid {
        Ok(())
    } else {
        Err(MkoError::new(
            "knowledge_id_invalid",
            "knowledge ID must use the v2 content identity form",
        ))
    }
}

fn validate_sha256(value: &str, code: &str) -> Result<(), MkoError> {
    if value
        .strip_prefix("sha256:")
        .is_some_and(|hash| is_lower_hex(hash, 64))
    {
        Ok(())
    } else {
        Err(MkoError::new(code, "expected a lowercase SHA-256 digest"))
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn ensure_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_real_directory(path)
        }
        Err(error) => Err(MkoError::new(
            "judgment_publication_failed",
            error.to_string(),
        )),
    }
}

fn validate_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("judgment_destination_invalid", error.to_string()))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            "judgment_destination_invalid",
            "managed judgment path must be a real non-symlink directory",
        ))
    }
}

fn read_regular_nofollow(path: &Path, limit: u64, code: &str) -> Result<Vec<u8>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(MkoError::new(
            code,
            "managed judgment input must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            code,
            "managed judgment input exceeds its byte limit",
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
