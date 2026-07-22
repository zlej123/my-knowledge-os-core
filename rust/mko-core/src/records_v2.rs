use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    clock::Clock,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2},
    error::MkoError,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    model_v2::{
        ContentBlockV2, EvidenceRefV2, KnowledgeBasisV2, KnowledgeResponseV2, KnowledgeUnitKindV2,
        LimitationBasisV2, PreparedContentV2, ReviewTargetTypeV2, SourceResponseV2,
    },
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, ProjectionWriteOutcomeV2,
        ProjectionWriteResultV2, projection_relative_path_v2, render_projection_v2,
        write_projection_locked,
    },
    review_v2::{ReviewDerivedStateV2, derive_review_histories_v2},
    revision_v2::{
        PublicationOutcomeV2, canonical_json_bytes, canonical_json_sha256,
        compare_and_swap_current_pointer_v2, create_current_pointer_v2, publish_revision_v2,
        sha256_digest,
    },
};

const MAX_CURRENT_POINTER_BYTES: u64 = 64 * 1024;

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetRecordTypeV2 {
    Asset,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetProviderBindingV2 {
    pub provider_type: String,
    pub logical_locator: String,
    pub size_bytes: u64,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub modified_at: Option<DateTime<Utc>>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetRecordV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub record_type: AssetRecordTypeV2,
    pub fingerprint: String,
    pub title_fallback: String,
    pub media_type: String,
    pub provider: AssetProviderBindingV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticRecordTypeV2 {
    Source,
    Knowledge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRevisionRecordTypeV2 {
    Source,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeRevisionRecordTypeV2 {
    Knowledge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBasisV2 {
    pub bundle_id: String,
    pub content_digest: String,
    pub asset_fingerprint: String,
    pub extractor_name: String,
    pub extractor_version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentPointerV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub record_type: SemanticRecordTypeV2,
    pub record_id: String,
    pub revision: String,
    pub evidence_basis: EvidenceBasisV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRevisionV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub record_type: SourceRevisionRecordTypeV2,
    pub record_id: String,
    pub asset_id: String,
    pub asset_fingerprint: String,
    pub evidence_basis: EvidenceBasisV2,
    pub response: SourceResponseV2,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRevisionV2 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub record_type: KnowledgeRevisionRecordTypeV2,
    pub record_id: String,
    pub asset_id: String,
    pub asset_fingerprint: String,
    pub evidence_basis: EvidenceBasisV2,
    pub domain_policy: DomainPolicyV2,
    pub response: KnowledgeResponseV2,
}

pub struct WriteSourceRecordRequestV2<'a> {
    pub repository_root: &'a Path,
    pub asset: &'a AssetRecordV2,
    pub bundle: &'a PreparedContentV2,
    pub response: &'a SourceResponseV2,
    pub expected_revision: Option<&'a str>,
}

pub struct WriteKnowledgeRecordRequestV2<'a> {
    pub repository_root: &'a Path,
    pub asset: &'a AssetRecordV2,
    pub bundle: &'a PreparedContentV2,
    pub response: &'a KnowledgeResponseV2,
    pub expected_revision: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordWriteOutcomeV2 {
    Created,
    Existing,
    Replaced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordWriteResultV2 {
    pub record_id: String,
    pub revision: String,
    pub revision_path: PathBuf,
    pub current_path: PathBuf,
    pub outcome: RecordWriteOutcomeV2,
    pub projection: RecordProjectionStatusV2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordProjectionStatusV2 {
    Current(ProjectionWriteResultV2),
    RepairRequired(ProjectionWriteResultV2),
    Stale { path: PathBuf, error: MkoError },
}

pub fn source_record_id_v2(asset_id: &str) -> Result<String, MkoError> {
    deterministic_record_id("source", asset_id)
}

pub fn knowledge_record_id_v2(asset_id: &str) -> Result<String, MkoError> {
    deterministic_record_id("knowledge", asset_id)
}

pub fn write_source_record_v2(
    request: WriteSourceRecordRequestV2<'_>,
    clock: &dyn Clock,
) -> Result<RecordWriteResultV2, MkoError> {
    KnowledgeConfigV2::read(request.repository_root)?;
    validate_asset_and_bundle(request.asset, request.bundle)?;
    validate_source_response(request.bundle, request.response)?;

    let record_id = source_record_id_v2(&request.asset.id)?;
    let evidence_basis = evidence_basis(request.bundle);
    let revision = SourceRevisionV2 {
        schema_version: 2,
        record_type: SourceRevisionRecordTypeV2::Source,
        record_id: record_id.clone(),
        asset_id: request.asset.id.clone(),
        asset_fingerprint: request.asset.fingerprint.clone(),
        evidence_basis: evidence_basis.clone(),
        response: request.response.clone(),
    };
    let bytes = render_revision_markdown("Source", &revision)?;
    publish_record_and_projection(
        request.repository_root,
        "sources",
        SemanticRecordTypeV2::Source,
        record_id,
        evidence_basis,
        &bytes,
        request.expected_revision,
        "v2 source write",
        request.response.title.clone(),
        request.response.tags.clone(),
        request.asset.id.clone(),
        clock,
    )
}

pub fn write_knowledge_record_v2(
    request: WriteKnowledgeRecordRequestV2<'_>,
    clock: &dyn Clock,
) -> Result<RecordWriteResultV2, MkoError> {
    let config = KnowledgeConfigV2::read(request.repository_root)?;
    validate_asset_and_bundle(request.asset, request.bundle)?;
    let domain_policy = config.domain_policies.default.clone();
    validate_knowledge_response(request.bundle, request.response, &domain_policy)?;

    let record_id = knowledge_record_id_v2(&request.asset.id)?;
    let evidence_basis = evidence_basis(request.bundle);
    let revision = KnowledgeRevisionV2 {
        schema_version: 2,
        record_type: KnowledgeRevisionRecordTypeV2::Knowledge,
        record_id: record_id.clone(),
        asset_id: request.asset.id.clone(),
        asset_fingerprint: request.asset.fingerprint.clone(),
        evidence_basis: evidence_basis.clone(),
        domain_policy,
        response: request.response.clone(),
    };
    let bytes = render_revision_markdown("Knowledge", &revision)?;
    let tags = request
        .response
        .units
        .iter()
        .flat_map(|unit| unit.tags.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    publish_record_and_projection(
        request.repository_root,
        "knowledge",
        SemanticRecordTypeV2::Knowledge,
        record_id,
        evidence_basis,
        &bytes,
        request.expected_revision,
        "v2 knowledge write",
        request.asset.title_fallback.clone(),
        tags,
        request.asset.id.clone(),
        clock,
    )
}

fn deterministic_record_id(record_type: &str, asset_id: &str) -> Result<String, MkoError> {
    if !valid_prefixed_hash(asset_id, "personal-asset-") {
        return Err(MkoError::new(
            "asset_binding_invalid",
            "Asset ID is not a canonical schema-v2 ID",
        ));
    }
    let input = serde_json::json!({"asset_id": asset_id, "record_type": record_type});
    let digest = canonical_json_sha256(&input)?;
    Ok(format!(
        "personal-{record_type}-{}",
        digest.trim_start_matches("sha256:")
    ))
}

fn validate_asset_and_bundle(
    asset: &AssetRecordV2,
    bundle: &PreparedContentV2,
) -> Result<(), MkoError> {
    if asset.schema_version != 2
        || !valid_digest(&asset.fingerprint)
        || asset.id
            != format!(
                "personal-asset-{}",
                asset.fingerprint.trim_start_matches("sha256:")
            )
        || asset.media_type != bundle.media_type
        || bundle.asset_id != asset.id
        || bundle.asset_fingerprint != asset.fingerprint
    {
        return Err(MkoError::new(
            "asset_binding_invalid",
            "the prepared bundle does not match the exact Asset identity and fingerprint",
        ));
    }
    validate_bundle_self_digest(bundle)
}

fn validate_bundle_self_digest(bundle: &PreparedContentV2) -> Result<(), MkoError> {
    let mut semantic = serde_json::to_value(bundle)
        .map_err(|error| MkoError::new("prepared_bundle_invalid", error.to_string()))?;
    let object = semantic.as_object_mut().ok_or_else(|| {
        MkoError::new(
            "prepared_bundle_invalid",
            "prepared bundle must be an object",
        )
    })?;
    object.remove("bundle_id");
    object.remove("content_digest");
    let digest = canonical_json_sha256(&semantic)?;
    let bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
    if bundle.content_digest != digest || bundle.bundle_id != bundle_id {
        return Err(MkoError::new(
            "prepared_bundle_digest_mismatch",
            "prepared bundle ID or self-digest does not match its canonical semantic fields",
        ));
    }

    let mut block_ids = HashSet::new();
    for block in &bundle.content_blocks {
        if !block_ids.insert(block_id(block)) {
            return Err(MkoError::new(
                "prepared_bundle_invalid",
                "prepared bundle contains duplicate block IDs",
            ));
        }
    }
    let mut artifact_ids = HashSet::new();
    for artifact in &bundle.artifacts {
        if !artifact_ids.insert(artifact.id.as_str()) || !valid_digest(&artifact.content_digest) {
            return Err(MkoError::new(
                "prepared_bundle_invalid",
                "prepared bundle contains invalid or duplicate artifacts",
            ));
        }
    }
    for block in &bundle.content_blocks {
        if let ContentBlockV2::Image { artifact_id, .. } = block
            && !artifact_ids.contains(artifact_id.as_str())
        {
            return Err(MkoError::new(
                "prepared_bundle_invalid",
                "prepared bundle contains an image with a dangling artifact ID",
            ));
        }
    }
    Ok(())
}

fn validate_source_response(
    bundle: &PreparedContentV2,
    response: &SourceResponseV2,
) -> Result<(), MkoError> {
    if response.schema_version != 2 {
        return Err(MkoError::new(
            "source_response_invalid",
            "Source response schema_version must be 2",
        ));
    }
    for claim in &response.key_claims {
        if claim.evidence_refs.is_empty() {
            return Err(MkoError::new(
                "source_grounding_invalid",
                "every Source key claim requires evidence",
            ));
        }
        validate_evidence_refs(bundle, &claim.evidence_refs)?;
    }
    for limitation in &response.limitations {
        if matches!(limitation.basis, LimitationBasisV2::Stated)
            && limitation.evidence_refs.is_empty()
        {
            return Err(MkoError::new(
                "source_grounding_invalid",
                "a stated Source limitation requires evidence",
            ));
        }
        validate_evidence_refs(bundle, &limitation.evidence_refs)?;
    }
    Ok(())
}

fn validate_knowledge_response(
    bundle: &PreparedContentV2,
    response: &KnowledgeResponseV2,
    domain_policy: &DomainPolicyV2,
) -> Result<(), MkoError> {
    if response.schema_version != 2 {
        return Err(MkoError::new(
            "knowledge_response_invalid",
            "Knowledge response schema_version must be 2",
        ));
    }
    let mut has_counterargument = false;
    let mut has_open_question = false;
    for unit in &response.units {
        let grounded_kind = matches!(
            unit.kind,
            KnowledgeUnitKindV2::Fact
                | KnowledgeUnitKindV2::Definition
                | KnowledgeUnitKindV2::Formula
                | KnowledgeUnitKindV2::Result
        );
        let missing_or_conflicting = matches!(
            unit.basis,
            KnowledgeBasisV2::MissingEvidence | KnowledgeBasisV2::ConflictingEvidence
        );
        let uncertainty_kind = matches!(
            unit.kind,
            KnowledgeUnitKindV2::Counterargument
                | KnowledgeUnitKindV2::Uncertainty
                | KnowledgeUnitKindV2::OpenQuestion
        );

        if grounded_kind
            && (!matches!(unit.basis, KnowledgeBasisV2::Evidence) || unit.evidence_refs.is_empty())
        {
            return Err(MkoError::new(
                "knowledge_grounding_invalid",
                "grounded Knowledge units require evidence basis and evidence references",
            ));
        }
        if missing_or_conflicting && !uncertainty_kind {
            return Err(MkoError::new(
                "knowledge_grounding_invalid",
                "missing or conflicting evidence is restricted to uncertainty units",
            ));
        }
        if unit.evidence_refs.is_empty() && !(missing_or_conflicting && uncertainty_kind) {
            return Err(MkoError::new(
                "knowledge_grounding_invalid",
                "an empty evidence list requires an uncertainty kind and explicit basis",
            ));
        }
        validate_evidence_refs(bundle, &unit.evidence_refs)?;
        has_counterargument |= matches!(unit.kind, KnowledgeUnitKindV2::Counterargument);
        has_open_question |= matches!(unit.kind, KnowledgeUnitKindV2::OpenQuestion);
    }
    if matches!(domain_policy, DomainPolicyV2::HighRisk)
        && (!has_counterargument || !has_open_question)
    {
        return Err(MkoError::new(
            "high_risk_knowledge_incomplete",
            "high-risk Knowledge requires a counterargument and an open question",
        ));
    }
    Ok(())
}

fn validate_evidence_refs(
    bundle: &PreparedContentV2,
    evidence_refs: &[EvidenceRefV2],
) -> Result<(), MkoError> {
    let blocks = bundle
        .content_blocks
        .iter()
        .map(|block| (block_id(block), block))
        .collect::<HashMap<_, _>>();
    for evidence in evidence_refs {
        let block = blocks.get(evidence.block_id.as_str()).ok_or_else(|| {
            MkoError::new(
                "evidence_reference_invalid",
                "evidence references an unknown prepared-content block",
            )
        })?;
        if block_locator(block) != evidence.locator {
            return Err(MkoError::new(
                "evidence_reference_invalid",
                "evidence locator does not exactly match its prepared-content block",
            ));
        }
        if evidence.text_span_utf8.is_some() && evidence.table_range.is_some() {
            return Err(MkoError::new(
                "evidence_reference_invalid",
                "evidence may use at most one narrowing form",
            ));
        }
        if let Some(span) = &evidence.text_span_utf8 {
            let text = match block {
                ContentBlockV2::Text { text, .. } | ContentBlockV2::Transcript { text, .. } => text,
                _ => {
                    return Err(MkoError::new(
                        "evidence_reference_invalid",
                        "UTF-8 spans may narrow only text or transcript blocks",
                    ));
                }
            };
            let start = usize::try_from(span.start).map_err(|_| evidence_bounds_error())?;
            let end = usize::try_from(span.end).map_err(|_| evidence_bounds_error())?;
            if start >= end
                || end > text.len()
                || !text.is_char_boundary(start)
                || !text.is_char_boundary(end)
            {
                return Err(evidence_bounds_error());
            }
        }
        if let Some(range) = &evidence.table_range {
            let ContentBlockV2::Table { columns, rows, .. } = block else {
                return Err(MkoError::new(
                    "evidence_reference_invalid",
                    "table ranges may narrow only table blocks",
                ));
            };
            let row_start =
                usize::try_from(range.row_start).map_err(|_| evidence_bounds_error())?;
            let row_end = usize::try_from(range.row_end).map_err(|_| evidence_bounds_error())?;
            let column_start =
                usize::try_from(range.column_start).map_err(|_| evidence_bounds_error())?;
            let column_end =
                usize::try_from(range.column_end).map_err(|_| evidence_bounds_error())?;
            if row_start >= row_end
                || row_end > rows.len()
                || column_start >= column_end
                || column_end > columns.len()
                || rows[row_start..row_end]
                    .iter()
                    .any(|row| column_end > row.len())
            {
                return Err(evidence_bounds_error());
            }
        }
    }
    Ok(())
}

fn evidence_bounds_error() -> MkoError {
    MkoError::new(
        "evidence_reference_invalid",
        "evidence narrowing is outside block bounds or a UTF-8 boundary",
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_record_and_projection(
    repository_root: &Path,
    collection: &str,
    record_type: SemanticRecordTypeV2,
    record_id: String,
    evidence_basis: EvidenceBasisV2,
    bytes: &[u8],
    expected_revision: Option<&str>,
    command: &str,
    title: String,
    mut tags: Vec<String>,
    asset_id: String,
    clock: &dyn Clock,
) -> Result<RecordWriteResultV2, MkoError> {
    let candidate_revision = sha256_digest(bytes);
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        command,
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    tags.sort();
    tags.dedup();
    let projection_input = expected_projection_input(
        repository_root,
        &record_type,
        &record_id,
        &candidate_revision,
        title,
        tags,
        &asset_id,
    )?;
    // Rendering is deliberately completed before canonical publication. This
    // guarantees that deterministic projection-shape errors cannot leave a
    // newly published canonical pointer behind. The later view write is still
    // derived state: if it fails, the canonical revision remains authoritative
    // and the typed result reports a stale projection for repair.
    let _ = render_projection_v2(&projection_input)?;
    let mut result = publish_record_locked(
        repository_root,
        collection,
        record_type,
        record_id,
        evidence_basis,
        bytes,
        expected_revision,
    )?;
    let projection_path = repository_root.join(projection_relative_path_v2(&projection_input)?);
    result.projection = match write_projection_locked(repository_root, &projection_input) {
        Ok(projection) if projection.outcome == ProjectionWriteOutcomeV2::RepairRequired => {
            RecordProjectionStatusV2::RepairRequired(projection)
        }
        Ok(projection) => RecordProjectionStatusV2::Current(projection),
        Err(error) => RecordProjectionStatusV2::Stale {
            path: projection_path,
            error,
        },
    };
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn publish_record_locked(
    repository_root: &Path,
    collection: &str,
    record_type: SemanticRecordTypeV2,
    record_id: String,
    evidence_basis: EvidenceBasisV2,
    bytes: &[u8],
    expected_revision: Option<&str>,
) -> Result<RecordWriteResultV2, MkoError> {
    let candidate_revision = sha256_digest(bytes);
    let collection_directory = repository_root.join(collection);
    validate_real_directory(&collection_directory)?;
    let record_directory = collection_directory.join(&record_id);
    let current_path = record_directory.join("current.yaml");

    let existing = read_current_pointer_if_present(&current_path)?;
    if let Some(current) = &existing {
        if current.record_type != record_type || current.record_id != record_id {
            return Err(MkoError::new(
                "current_pointer_invalid",
                "current pointer does not identify its containing record",
            ));
        }
        if current.revision == candidate_revision {
            let revision_path = record_directory
                .join("revisions")
                .join(format!("{}.md", candidate_revision.replace(':', "-")));
            require_existing_revision(&revision_path, bytes)?;
            return Ok(RecordWriteResultV2 {
                record_id,
                revision: candidate_revision.clone(),
                revision_path,
                current_path,
                outcome: RecordWriteOutcomeV2::Existing,
                projection: projection_placeholder(),
            });
        }
        let Some(expected) = expected_revision else {
            return Err(MkoError::new(
                "replacement_revision_required",
                "replacing a current record requires its expected revision",
            ));
        };
        if current.revision != expected {
            return Err(MkoError::new(
                "record_revision_stale",
                "the expected revision is not the current revision",
            ));
        }
    } else if expected_revision.is_some() {
        return Err(MkoError::new(
            "record_revision_stale",
            "an expected revision was supplied for a record without a current pointer",
        ));
    }

    ensure_real_directory(&record_directory)?;
    let revisions_directory = record_directory.join("revisions");
    ensure_real_directory(&revisions_directory)?;
    let publication = publish_revision_v2(&revisions_directory, bytes)?;
    let replacement = CurrentPointerV2 {
        schema_version: 2,
        record_type,
        record_id: record_id.clone(),
        revision: publication.revision.clone(),
        evidence_basis,
    };
    let outcome = if let Some(current) = existing {
        compare_and_swap_current_pointer_v2(&current_path, &current, &replacement)?;
        RecordWriteOutcomeV2::Replaced
    } else {
        create_current_pointer_v2(&current_path, &replacement)?;
        RecordWriteOutcomeV2::Created
    };
    debug_assert!(matches!(
        publication.outcome,
        PublicationOutcomeV2::Created | PublicationOutcomeV2::Existing
    ));
    Ok(RecordWriteResultV2 {
        record_id,
        revision: publication.revision,
        revision_path: publication.path,
        current_path,
        outcome,
        projection: projection_placeholder(),
    })
}

fn projection_placeholder() -> RecordProjectionStatusV2 {
    RecordProjectionStatusV2::Stale {
        path: PathBuf::new(),
        error: MkoError::new(
            "projection_not_attempted",
            "projection synchronization has not run",
        ),
    }
}

fn expected_projection_input(
    repository_root: &Path,
    record_type: &SemanticRecordTypeV2,
    record_id: &str,
    candidate_revision: &str,
    title: String,
    tags: Vec<String>,
    asset_id: &str,
) -> Result<ProjectionInputV2, MkoError> {
    let (review_type, projection_type, collection) = match record_type {
        SemanticRecordTypeV2::Source => (
            ReviewTargetTypeV2::Source,
            ProjectionRecordTypeV2::Source,
            "sources",
        ),
        SemanticRecordTypeV2::Knowledge => (
            ReviewTargetTypeV2::Knowledge,
            ProjectionRecordTypeV2::Knowledge,
            "knowledge",
        ),
    };
    let history = derive_review_histories_v2(
        repository_root,
        &[(
            review_type,
            record_id.to_owned(),
            candidate_revision.to_owned(),
        )],
    )?
    .into_iter()
    .next()
    .ok_or_else(|| MkoError::new("review_state_invalid", "review history is missing"))?;
    let derived_state = match history.derived.state {
        ReviewDerivedStateV2::Unreviewed if history.previous_reviewed_revision.is_some() => {
            ProjectionStateV2::RevisedUnreviewed
        }
        ReviewDerivedStateV2::Unreviewed => ProjectionStateV2::Unreviewed,
        ReviewDerivedStateV2::Deferred => ProjectionStateV2::Deferred,
        ReviewDerivedStateV2::ChangesRequested => ProjectionStateV2::ChangesRequested,
        ReviewDerivedStateV2::Approved => ProjectionStateV2::Approved,
        ReviewDerivedStateV2::BlockedConflict => ProjectionStateV2::Blocked,
    };
    Ok(ProjectionInputV2 {
        record_type: projection_type,
        id: record_id.to_owned(),
        title,
        current_revision: candidate_revision.to_owned(),
        review_head_id: history.derived.review_head_id,
        derived_state,
        domain: "uncategorized".into(),
        tags,
        record_link: format!("{collection}/{record_id}/current.yaml"),
        asset_link: format!("assets/registry/{asset_id}.json"),
    })
}

fn render_revision_markdown<T>(title: &str, revision: &T) -> Result<Vec<u8>, MkoError>
where
    T: Serialize,
{
    let canonical = canonical_json_bytes(revision)?;
    let canonical = String::from_utf8(canonical)
        .map_err(|error| MkoError::new("revision_invalid", error.to_string()))?;
    Ok(format!("# {title} revision\n\n    {canonical}\n").into_bytes())
}

fn read_current_pointer_if_present(path: &Path) -> Result<Option<CurrentPointerV2>, MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_record_nofollow(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(MkoError::new(
                "current_pointer_invalid",
                "current pointer cannot be opened without following links",
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("current_pointer_unreadable", error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "current_pointer_invalid",
            "current pointer must be a regular non-symlink file",
        ));
    }
    if metadata.len() > MAX_CURRENT_POINTER_BYTES {
        return Err(MkoError::new(
            "current_pointer_invalid",
            "current pointer exceeds the bounded input size",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CURRENT_POINTER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new("current_pointer_unreadable", error.to_string()))?;
    let pointer: CurrentPointerV2 = serde_json::from_slice(&bytes).map_err(|_| {
        MkoError::new(
            "current_pointer_invalid",
            "current pointer is not canonical v2 JSON",
        )
    })?;
    if canonical_json_bytes(&pointer)? != bytes {
        return Err(MkoError::new(
            "current_pointer_invalid",
            "current pointer bytes are not canonical v2 JSON",
        ));
    }
    Ok(Some(pointer))
}

fn require_existing_revision(path: &Path, expected: &[u8]) -> Result<(), MkoError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_record_nofollow(&mut options);
    let file = options
        .open(path)
        .map_err(|_| MkoError::new("revision_not_found", "current revision is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|error| MkoError::new("revision_unreadable", error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(MkoError::new(
            "revision_destination_invalid",
            "current revision is not a regular non-symlink file",
        ));
    }
    let expected_len = u64::try_from(expected.len())
        .map_err(|_| MkoError::new("revision_invalid", "revision is too large"))?;
    if metadata.len() != expected_len {
        return Err(MkoError::new(
            "revision_conflict",
            "current immutable revision bytes do not match their digest",
        ));
    }
    let mut actual = Vec::with_capacity(expected.len());
    file.take(expected_len.saturating_add(1))
        .read_to_end(&mut actual)
        .map_err(|error| MkoError::new("revision_unreadable", error.to_string()))?;
    if actual == expected {
        Ok(())
    } else {
        Err(MkoError::new(
            "revision_conflict",
            "current immutable revision bytes do not match their digest",
        ))
    }
}

#[cfg(target_os = "linux")]
fn configure_record_nofollow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    const O_NONBLOCK: i32 = 0x800;
    const O_NOFOLLOW: i32 = 0x20_000;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(target_os = "macos")]
fn configure_record_nofollow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    const O_NONBLOCK: i32 = 0x4;
    const O_NOFOLLOW: i32 = 0x100;
    options.custom_flags(O_NOFOLLOW | O_NONBLOCK);
}

#[cfg(windows)]
fn configure_record_nofollow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn configure_record_nofollow(_options: &mut OpenOptions) {}

fn ensure_real_directory(path: &Path) -> Result<(), MkoError> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_real_directory(path)
        }
        Err(error) => Err(MkoError::new("record_write_failed", error.to_string())),
    }
}

fn validate_real_directory(path: &Path) -> Result<(), MkoError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| MkoError::new("record_write_failed", error.to_string()))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            "record_destination_invalid",
            "managed record path must be a real directory",
        ))
    }
}

fn evidence_basis(bundle: &PreparedContentV2) -> EvidenceBasisV2 {
    EvidenceBasisV2 {
        bundle_id: bundle.bundle_id.clone(),
        content_digest: bundle.content_digest.clone(),
        asset_fingerprint: bundle.asset_fingerprint.clone(),
        extractor_name: bundle.extractor.name.clone(),
        extractor_version: bundle.extractor.version.clone(),
    }
}

fn block_id(block: &ContentBlockV2) -> &str {
    match block {
        ContentBlockV2::Text { id, .. }
        | ContentBlockV2::Table { id, .. }
        | ContentBlockV2::Image { id, .. }
        | ContentBlockV2::Transcript { id, .. } => id,
    }
}

fn block_locator(block: &ContentBlockV2) -> &str {
    match block {
        ContentBlockV2::Text { locator, .. }
        | ContentBlockV2::Table { locator, .. }
        | ContentBlockV2::Image { locator, .. }
        | ContentBlockV2::Transcript { locator, .. } => locator,
    }
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn valid_prefixed_hash(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}
