use serde::Serialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::{
    error::MkoError,
    model::{SourceMetadata, SourceRecord},
};

pub fn calculate_source_revision(source: &SourceRecord, body: &str) -> Result<String, MkoError> {
    let canonical = CanonicalSourceRevision {
        title: normalize_string(&source.title),
        tags: normalize_set(&source.tags),
        domain: normalize_set(&source.domain),
        asset_ids: normalize_set(&source.relations.asset_ids),
        source_metadata: CanonicalSourceMetadata::from(&source.source_metadata),
        body: normalize_body(body),
    };
    let serialized = serde_json::to_vec(&canonical)
        .map_err(|error| MkoError::new("revision_invalid", error.to_string()))?;
    let digest = Sha256::digest(serialized);
    Ok(format!("sha256:{}", hex::encode(digest)))
}

#[derive(Serialize)]
struct CanonicalSourceRevision {
    title: String,
    tags: Vec<String>,
    domain: Vec<String>,
    asset_ids: Vec<String>,
    source_metadata: CanonicalSourceMetadata,
    body: String,
}

#[derive(Serialize)]
struct CanonicalSourceMetadata {
    authors: Vec<String>,
    publication_date: Option<String>,
    doi: Option<String>,
}

impl From<&SourceMetadata> for CanonicalSourceMetadata {
    fn from(metadata: &SourceMetadata) -> Self {
        Self {
            authors: metadata
                .authors
                .iter()
                .map(|author| normalize_string(author))
                .collect(),
            publication_date: metadata.publication_date.map(|date| date.to_string()),
            doi: metadata.doi.as_deref().map(normalize_string),
        }
    }
}

fn normalize_set(values: &[String]) -> Vec<String> {
    let mut normalized: Vec<_> = values.iter().map(|value| normalize_string(value)).collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_string(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect()
}

fn normalize_body(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<_> = normalized.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n")).nfc().collect()
    }
}
