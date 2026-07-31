use std::path::Path;

use crate::{
    clock::Clock,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2, PerspectiveV2},
    error::MkoError,
    model_v2::KnowledgeUnitKindV2,
    records_v2::{
        CurrentKnowledgeRevisionV2, RecordWriteResultV2, read_current_knowledge_revision_v2,
        replace_knowledge_perspectives_v2,
    },
    revision_v2::{canonical_json_sha256, sha256_digest},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedPerspectiveConfirmationV2 {
    pub current: CurrentKnowledgeRevisionV2,
    pub perspectives: Vec<PerspectiveV2>,
    pub domain_policy: DomainPolicyV2,
    pub confirmation_card: Vec<u8>,
    pub card_digest: String,
    pub confirmation_phrase: String,
}

pub fn prepare_perspective_confirmation_v2(
    repository_root: &Path,
    knowledge_id: &str,
    mut perspectives: Vec<PerspectiveV2>,
) -> Result<PreparedPerspectiveConfirmationV2, MkoError> {
    let config = KnowledgeConfigV2::read(repository_root)?;
    perspectives.sort();
    perspectives.dedup();
    if perspectives.is_empty() {
        return Err(MkoError::new(
            "perspective_selection_invalid",
            "select at least one perspective",
        ));
    }
    let current = read_current_knowledge_revision_v2(repository_root, knowledge_id)?;
    let domain_policy = config.policy_for_perspectives(&perspectives);
    if domain_policy == DomainPolicyV2::HighRisk {
        let has_counterargument = current
            .revision
            .response
            .units
            .iter()
            .any(|unit| unit.kind == KnowledgeUnitKindV2::Counterargument);
        let has_open_question = current
            .revision
            .response
            .units
            .iter()
            .any(|unit| unit.kind == KnowledgeUnitKindV2::OpenQuestion);
        if !has_counterargument || !has_open_question {
            return Err(MkoError::new(
                "high_risk_knowledge_incomplete",
                "investment perspective requires a counterargument and an open question before confirmation",
            ));
        }
    }
    let effect_digest = canonical_json_sha256(&serde_json::json!({
        "knowledge_id": knowledge_id,
        "current_revision": current.pointer.revision,
        "perspectives": perspectives,
        "domain_policy": domain_policy,
        "effect": "publish_replacement_revision",
    }))?;
    let labels = perspectives
        .iter()
        .map(PerspectiveV2::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    let confirmation_card = format!(
        "# Perspective confirmation\n\n- Knowledge ID: `{knowledge_id}`\n- Current revision: `{}`\n- Perspectives: `{labels}`\n- Derived policy: `{}`\n- Effect digest: `{effect_digest}`\n\nThis publishes a new pending Knowledge revision. It does not approve, commit, push, or promote anything.\n",
        current.pointer.revision,
        match domain_policy {
            DomainPolicyV2::Standard => "standard",
            DomainPolicyV2::HighRisk => "high_risk",
        }
    )
    .into_bytes();
    let card_digest = sha256_digest(&confirmation_card);
    let confirmation_phrase = format!("confirm-perspectives {card_digest} {effect_digest}");
    Ok(PreparedPerspectiveConfirmationV2 {
        current,
        perspectives,
        domain_policy,
        confirmation_card,
        card_digest,
        confirmation_phrase,
    })
}

pub fn publish_perspective_confirmation_v2(
    repository_root: &Path,
    prepared: &PreparedPerspectiveConfirmationV2,
    confirmation: &str,
    clock: &dyn Clock,
) -> Result<RecordWriteResultV2, MkoError> {
    let canonical = prepare_perspective_confirmation_v2(
        repository_root,
        &prepared.current.revision.record_id,
        prepared.perspectives.clone(),
    )?;
    if canonical != *prepared {
        return Err(MkoError::new(
            "perspective_confirmation_stale",
            "Knowledge revision or perspective confirmation changed after display",
        ));
    }
    if confirmation != prepared.confirmation_phrase {
        return Err(MkoError::new(
            "perspective_confirmation_mismatch",
            "confirmation does not match the exact displayed perspective effect",
        ));
    }
    replace_knowledge_perspectives_v2(
        repository_root,
        &prepared.current,
        prepared.perspectives.clone(),
        clock,
    )
}
