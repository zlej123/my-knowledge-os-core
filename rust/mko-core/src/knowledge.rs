use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::error::MkoError;

pub const MAX_KNOWLEDGE_RESPONSE_BYTES: usize = 1024 * 1024;
pub const MAX_KNOWLEDGE_STRING_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConceptKind {
    Definition,
    Formula,
    Concept,
    Result,
    Theorem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Concept {
    pub id: String,
    pub name: String,
    pub kind: ConceptKind,
    pub body: String,
    pub tags: Vec<String>,
    pub locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConceptInput {
    pub name: String,
    pub kind: ConceptKind,
    pub body: String,
    pub tags: Vec<String>,
    pub locator: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeResponse {
    pub synthesis: String,
    pub concepts: Vec<ConceptInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Unreviewed,
    Reviewed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeReview {
    pub status: ReviewState,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeGeneration {
    pub processor_version: String,
    pub prompt_version: String,
    pub asset_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRecord {
    pub id: String,
    pub record_type: String,
    pub schema_version: u32,
    pub asset_id: String,
    pub title: String,
    pub review: KnowledgeReview,
    pub content_revision: String,
    pub approved_revision: Option<String>,
    pub generation: KnowledgeGeneration,
    pub concepts: Vec<Concept>,
}

pub fn parse_knowledge_response(input: &[u8]) -> Result<KnowledgeResponse, MkoError> {
    if input.len() > MAX_KNOWLEDGE_RESPONSE_BYTES {
        return Err(schema_error("knowledge response exceeds 1 MiB"));
    }
    let response: KnowledgeResponse = serde_json::from_slice(input)
        .map_err(|error| schema_error(format!("invalid knowledge response: {error}")))?;
    Ok(response)
}

pub fn normalize_and_validate_knowledge(response: &mut KnowledgeResponse) -> Result<(), MkoError> {
    normalize_string(&mut response.synthesis)?;
    if response.synthesis.trim().is_empty() {
        return Err(schema_error("synthesis must not be empty"));
    }
    for concept in &mut response.concepts {
        normalize_string(&mut concept.name)?;
        if concept.name.trim().is_empty() || concept.name.contains('\n') {
            return Err(schema_error("concept name must be a non-empty single line"));
        }
        normalize_string(&mut concept.body)?;
        if concept.body.trim().is_empty() {
            return Err(schema_error("concept body must not be empty"));
        }
        for tag in &mut concept.tags {
            normalize_string(tag)?;
        }
        if let Some(locator) = &mut concept.locator {
            normalize_string(locator)?;
        }
    }
    validate_aggregate_knowledge_limits(response)?;
    Ok(())
}

fn validate_aggregate_knowledge_limits(response: &KnowledgeResponse) -> Result<(), MkoError> {
    let names_joined = response
        .concepts
        .iter()
        .map(|concept| concept.name.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    validate_semantic_size(&names_joined)?;
    let tags_joined = response
        .concepts
        .iter()
        .flat_map(|concept| concept.tags.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    validate_semantic_size(&tags_joined)?;
    Ok(())
}

fn normalize_string(value: &mut String) -> Result<(), MkoError> {
    *value = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect();
    validate_semantic_size(value)
}

fn validate_semantic_size(value: &str) -> Result<(), MkoError> {
    if value.len() > MAX_KNOWLEDGE_STRING_BYTES {
        return Err(schema_error("normalized knowledge section exceeds 64 KiB"));
    }
    Ok(())
}

fn schema_error(message: impl Into<String>) -> MkoError {
    MkoError::new("semantic_schema_invalid", message)
}
