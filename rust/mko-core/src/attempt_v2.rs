use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    atomic::write_new,
    clock::Clock,
    error::MkoError,
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
};

/// What happened the last time Core tried to prepare an Asset.
///
/// An Asset's identity is its content, so "these exact bytes could not be
/// extracted" is an immutable fact rather than a status that a later event
/// could contradict. That is why this is an append-only observation and why
/// v0.3 assets keep carrying identity only.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationAttemptV2 {
    pub schema_version: u32,
    pub id: String,
    pub record_type: PreparationAttemptRecordTypeV2,
    pub asset_id: String,
    pub outcome: PreparationOutcomeV2,
    /// The typed failure code. The message is deliberately absent: it can quote
    /// document bytes, and a code is what every surface should branch on.
    pub code: Option<String>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationAttemptRecordTypeV2 {
    Attempt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationOutcomeV2 {
    Prepared,
    Failed,
}

/// What an owner can do about an Asset that has not become a record yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StuckReasonV2 {
    /// No attempt is on file: registered material nobody has processed yet, and
    /// also what an Asset registered before attempts existed degrades to.
    NotAttempted,
    /// The document's text layer defeated the extractor. Retrying re-runs the
    /// same failure; a different copy is the way forward.
    TextUnreadable,
    /// The provider still holds the bytes remotely.
    DownloadRequired,
    /// Something else went wrong; trying again is reasonable.
    Retryable,
}

impl StuckReasonV2 {
    pub fn from_code(code: &str) -> Self {
        match code {
            "pdf_text_unreadable" => Self::TextUnreadable,
            "hydration_confirmation_required"
            | "provider_hydration_required"
            | "provider_not_hydrated"
            | "asset_not_hydrated" => Self::DownloadRequired,
            _ => Self::Retryable,
        }
    }
}

pub fn record_preparation_attempt_v2(
    repository_root: &Path,
    asset_id: &str,
    outcome: PreparationOutcomeV2,
    code: Option<&str>,
    clock: &dyn Clock,
) -> Result<PreparationAttemptV2, MkoError> {
    let mut attempt = PreparationAttemptV2 {
        schema_version: 2,
        id: String::new(),
        record_type: PreparationAttemptRecordTypeV2::Attempt,
        asset_id: asset_id.to_owned(),
        outcome,
        code: code.map(str::to_owned),
        observed_at: clock.now_utc(),
    };
    let digest = canonical_json_sha256(&attempt)?;
    attempt.id = format!("personal-attempt-{}", digest.replace(':', "-"));
    let bytes = canonical_json_bytes(&attempt)?;
    let path = repository_root
        .join("assets/attempts")
        .join(format!("{}.json", attempt.id));
    write_new(&path, &bytes, |_| Ok(()))?;
    Ok(attempt)
}

/// The most recent attempt on file for an Asset, if any.
pub fn latest_preparation_attempt_v2(
    repository_root: &Path,
    asset_id: &str,
) -> Result<Option<PreparationAttemptV2>, MkoError> {
    Ok(read_attempts_v2(repository_root)?
        .into_iter()
        .filter(|attempt| attempt.asset_id == asset_id)
        .max_by_key(|attempt| attempt.observed_at))
}

pub(crate) fn read_attempts_v2(
    repository_root: &Path,
) -> Result<Vec<PreparationAttemptV2>, MkoError> {
    let directory = repository_root.join("assets/attempts");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(MkoError::new("attempt_unreadable", error.to_string())),
    };
    let mut attempts = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| MkoError::new("attempt_unreadable", error.to_string()))?;
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        let bytes = std::fs::read(entry.path())
            .map_err(|error| MkoError::new("attempt_unreadable", error.to_string()))?;
        let attempt: PreparationAttemptV2 = serde_json::from_slice(&bytes)
            .map_err(|error| MkoError::new("attempt_invalid", error.to_string()))?;
        attempts.push(attempt);
    }
    Ok(attempts)
}
