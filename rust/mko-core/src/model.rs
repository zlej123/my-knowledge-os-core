use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    Registered,
    Extracted,
    ReviewPending,
    Processed,
    Changed,
    Missing,
    Superseded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Pending,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Public,
    Personal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    ReviewPending,
    Approved,
    Rejected,
    Stale,
    Archived,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LastSuccessfulStep {
    Registered,
    Extracted,
    Drafted,
    Reviewed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub r#type: String,
    pub locator: String,
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub method: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LastError {
    pub code: Option<String>,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRecord {
    pub id: String,
    pub record_type: String,
    pub schema_version: u32,
    pub scope: String,
    pub title: String,
    pub classification: Classification,
    pub asset_class: String,
    pub media_type: String,
    pub provider: Provider,
    pub size_bytes: u64,
    pub modified_at: DateTime<Utc>,
    pub fingerprint: Fingerprint,
    pub asset_status: AssetStatus,
    pub durable_state_history: Vec<AssetStatus>,
    pub supersedes: Option<String>,
    pub last_successful_step: LastSuccessfulStep,
    pub last_error: LastError,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Relations {
    pub asset_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub extractor_name: String,
    pub extractor_version: String,
    pub core_version: String,
    pub processor_version: String,
    pub prompt_version: String,
    pub asset_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub status: ReviewStatus,
    pub approved_revision: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub authors: Vec<String>,
    pub publication_date: Option<NaiveDate>,
    pub doi: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRecord {
    pub id: String,
    pub record_type: String,
    pub schema_version: u32,
    pub scope: String,
    pub title: String,
    pub status: SourceStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
    pub domain: Vec<String>,
    pub ai_assisted: bool,
    pub relations: Relations,
    pub generation: Generation,
    pub content_revision: String,
    pub review: Review,
    pub source_metadata: SourceMetadata,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticResponse {
    pub title: String,
    pub source_metadata: SourceMetadata,
    pub tags: Vec<String>,
    pub domain: Vec<String>,
    pub one_sentence_summary: String,
    pub problem: String,
    pub method: String,
    pub contributions: String,
    pub reported_evidence: String,
    pub stated_limitations: String,
    pub domain_perspective: String,
    pub implementation_considerations: String,
    pub questions_and_unknowns: String,
    pub related_knowledge: String,
}
