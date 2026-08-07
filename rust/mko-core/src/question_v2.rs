//! What the owner asked about a piece of material.
//!
//! Answers are not kept. An answer ends in one of two states: it became a unit,
//! and is kept properly with its provenance, or it was disposable by the
//! owner's own decision. Neither needs a transcript, and a transcript would
//! rival every durable record in the knowledge base for size.
//!
//! The question is the part nothing else recovers. "이 칩의 클럭 도메인을 세
//! 번 물어봤다" is a record of what the owner was trying to understand, and no
//! note contains it.
//!
//! Attached to the Asset rather than the record: `knowledge_record_id_v2`
//! derives the record id from the asset id, so the spine is one to one, and the
//! Asset additionally exists before any record does and outlives every
//! revision.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    atomic::write_new,
    error::MkoError,
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
};

/// Long enough for a real question, short enough that the log stays a log.
pub const MAX_QUESTION_CHARS: usize = 500;
const MAX_QUESTION_RECORD_BYTES: u64 = 8 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionRecordV2 {
    pub schema_version: u32,
    pub id: String,
    pub record_type: QuestionRecordTypeV2,
    pub asset_id: String,
    pub text: String,
    pub asked_at: DateTime<Utc>,
    /// Whether the answer was kept in the note. False is the ordinary case and
    /// is not a failure: most questions are worth asking and not worth keeping.
    pub became_unit: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionRecordTypeV2 {
    Question,
}

impl QuestionRecordV2 {
    /// The identifier is filled in on write, from the record's own bytes.
    pub fn new(asset_id: &str, text: &str, asked_at: DateTime<Utc>, became_unit: bool) -> Self {
        Self {
            schema_version: 2,
            id: String::new(),
            record_type: QuestionRecordTypeV2::Question,
            asset_id: asset_id.to_owned(),
            text: text.to_owned(),
            asked_at,
            became_unit,
        }
    }
}

pub fn append_question_v2(
    repository_root: &Path,
    question: &QuestionRecordV2,
) -> Result<QuestionRecordV2, MkoError> {
    let text = question.text.trim();
    if text.is_empty()
        || text.chars().count() > MAX_QUESTION_CHARS
        || crate::asset_v2::validate_asset_id(&question.asset_id).is_err()
    {
        return Err(MkoError::new(
            "question_invalid",
            "a question needs a bounded non-empty text and a valid Asset identifier",
        ));
    }

    let mut record = QuestionRecordV2 {
        text: text.to_owned(),
        id: String::new(),
        ..question.clone()
    };
    record.schema_version = 2;
    record.record_type = QuestionRecordTypeV2::Question;
    let digest = canonical_json_sha256(&record)?;
    record.id = format!("personal-question-{}", digest.replace(':', "-"));
    let bytes = canonical_json_bytes(&record)?;
    if bytes.len() as u64 > MAX_QUESTION_RECORD_BYTES {
        return Err(MkoError::new(
            "question_invalid",
            "question record exceeds its bounded canonical representation",
        ));
    }

    // Knowledge bases scaffolded before questions existed have no such
    // directory, and an append-only log should not need a migration to start.
    let directory = repository_root.join("assets/questions");
    match std::fs::create_dir_all(&directory) {
        Ok(()) => {}
        Err(error) => return Err(MkoError::new("question_write_failed", error.to_string())),
    }
    let metadata = std::fs::symlink_metadata(&directory)
        .map_err(|error| MkoError::new("question_write_failed", error.to_string()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "question_destination_invalid",
            "assets/questions must be a real directory",
        ));
    }
    write_new(
        &directory.join(format!("{}.json", record.id)),
        &bytes,
        |_| Ok(()),
    )?;
    Ok(record)
}

/// Every question asked about one Asset, oldest first.
///
/// Ties break on the identifier so two questions recorded in the same instant
/// still come back in a stable order.
pub fn questions_for_asset_v2(
    repository_root: &Path,
    asset_id: &str,
) -> Result<Vec<QuestionRecordV2>, MkoError> {
    let directory = repository_root.join("assets/questions");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(MkoError::new("question_unreadable", error.to_string())),
    };
    let mut questions = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".json"))
        })
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<QuestionRecordV2>(&bytes).ok())
        .filter(|question| question.asset_id == asset_id)
        .collect::<Vec<_>>();
    questions.sort_by(|left, right| {
        left.asked_at
            .cmp(&right.asked_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(questions)
}
