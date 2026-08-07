use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == 2 {
        Ok(version)
    } else {
        Err(serde::de::Error::custom("schema_version must be 2"))
    }
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedArtifactTypeV2 {
    PreparedContent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparedTrustV2 {
    UntrustedDocumentContent,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractorIdentityV2 {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMetadataV2 {
    #[serde(deserialize_with = "deserialize_required_option")]
    pub title: Option<String>,
    pub authors: Vec<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TableScalarV2 {
    String(String),
    Number(serde_json::Number),
    Boolean(bool),
    Null(()),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "block_type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlockV2 {
    Text {
        id: String,
        locator: String,
        text: String,
    },
    Table {
        id: String,
        locator: String,
        columns: Vec<String>,
        rows: Vec<Vec<TableScalarV2>>,
    },
    Image {
        id: String,
        locator: String,
        #[serde(deserialize_with = "deserialize_required_option")]
        alt_text: Option<String>,
        artifact_id: String,
    },
    Transcript {
        id: String,
        locator: String,
        #[serde(deserialize_with = "deserialize_required_option")]
        speaker: Option<String>,
        text: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedArtifactV2 {
    pub id: String,
    pub media_type: String,
    pub content_digest: String,
    pub size_bytes: u64,
    pub provider_locator: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedContentV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub artifact_type: PreparedArtifactTypeV2,
    pub bundle_id: String,
    pub content_digest: String,
    pub asset_id: String,
    pub asset_fingerprint: String,
    pub media_type: String,
    pub trust: PreparedTrustV2,
    pub extractor: ExtractorIdentityV2,
    pub metadata: PreparedMetadataV2,
    pub content_blocks: Vec<ContentBlockV2>,
    pub artifacts: Vec<PreparedArtifactV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TextSpanUtf8V2 {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TableRangeV2 {
    pub row_start: u64,
    pub row_end: u64,
    pub column_start: u64,
    pub column_end: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvidenceRefV2 {
    pub block_id: String,
    pub locator: String,
    pub text_span_utf8: Option<TextSpanUtf8V2>,
    pub table_range: Option<TableRangeV2>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceRefWireV2 {
    block_id: String,
    locator: String,
    #[serde(default)]
    text_span_utf8: Option<TextSpanUtf8V2>,
    #[serde(default)]
    table_range: Option<TableRangeV2>,
}

impl<'de> Deserialize<'de> for EvidenceRefV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = EvidenceRefWireV2::deserialize(deserializer)?;
        if wire.text_span_utf8.is_some() && wire.table_range.is_some() {
            return Err(serde::de::Error::custom(
                "evidence reference may use at most one narrowing form",
            ));
        }
        Ok(Self {
            block_id: wire.block_id,
            locator: wire.locator,
            text_span_utf8: wire.text_span_utf8,
            table_range: wire.table_range,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceClaimV2 {
    pub text: String,
    pub evidence_refs: Vec<EvidenceRefV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitationBasisV2 {
    Stated,
    ObservedMissingEvidence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLimitationV2 {
    pub text: String,
    pub basis: LimitationBasisV2,
    pub evidence_refs: Vec<EvidenceRefV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRecommendationOutcomeV2 {
    Recommend,
    ReferenceOnly,
    Archive,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRecommendationV2 {
    pub outcome: KnowledgeRecommendationOutcomeV2,
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceResponseV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub title: String,
    pub authors: Vec<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub publication_date: Option<NaiveDate>,
    pub one_sentence_summary: String,
    pub general_summary: String,
    pub key_claims: Vec<SourceClaimV2>,
    pub limitations: Vec<SourceLimitationV2>,
    pub tags: Vec<String>,
    pub knowledge_recommendation: KnowledgeRecommendationV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeUnitKindV2 {
    Fact,
    Definition,
    Formula,
    Result,
    Interpretation,
    Hypothesis,
    Counterargument,
    Uncertainty,
    OpenQuestion,
    /// What the model knows that the document does not say.
    ///
    /// Kept apart from the grounded kinds so a reader can always tell the
    /// document's own words from what was supplied around them.
    Background,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceV2 {
    High,
    Medium,
    Low,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeBasisV2 {
    Evidence,
    MissingEvidence,
    ConflictingEvidence,
    /// The model supplied this, and the document does not support it.
    ModelKnowledge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeUnitV2 {
    pub kind: KnowledgeUnitKindV2,
    pub title: String,
    pub body: String,
    pub confidence: ConfidenceV2,
    pub basis: KnowledgeBasisV2,
    pub evidence_refs: Vec<EvidenceRefV2>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeResponseV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub synthesis: String,
    pub units: Vec<KnowledgeUnitV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRecordTypeV2 {
    Review,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTargetTypeV2 {
    Source,
    Knowledge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecisionV2 {
    Approve,
    RequestChanges,
    Defer,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTargetV2 {
    pub record_type: ReviewTargetTypeV2,
    pub record_id: String,
    pub displayed_revision: String,
    pub decision: ReviewDecisionV2,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub feedback: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub supersedes_review_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecordV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub record_type: ReviewRecordTypeV2,
    pub targets: Vec<ReviewTargetV2>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewResolutionRecordTypeV2 {
    ReviewResolution,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewResolutionV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub record_type: ReviewResolutionRecordTypeV2,
    pub review_id: String,
    pub target_record_id: String,
    pub requested_revision: String,
    pub resulting_revision: String,
    pub bundle_id: String,
    pub created_at: DateTime<Utc>,
}
