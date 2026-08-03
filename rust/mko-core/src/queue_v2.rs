use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    asset_v2::read_asset_v2,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2, PerspectiveV2},
    error::MkoError,
    front_matter::parse_markdown,
    json_v2::{QueueDataV2, QueueItemStateV2, QueueItemTypeV2, QueueItemV2, QueueNextActionV2},
    judgment_v2::{JudgmentAnnotationV2, prepare_judgment_v2},
    model_v2::{KnowledgeUnitKindV2, ReviewTargetTypeV2},
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionSnapshotStatusV2, ProjectionStateV2,
        projection_relative_path_v2, projection_snapshot_status_v2,
    },
    records_v2::{
        AssetRecordV2, CurrentPointerV2, KnowledgeRevisionV2, SemanticRecordTypeV2,
        SourceRevisionV2,
    },
    resurface_history_v2::read_resurface_opened_at_v2,
    review_v2::{
        ReviewDerivedStateV2, ReviewTargetHistoryV2, ReviewTargetSnapshotV2,
        derive_review_histories_v2,
    },
    revision_v2::{canonical_json_bytes, canonical_json_sha256, sha256_digest},
};

const MAX_RECORDS: usize = 4096;
const MAX_CURRENT_POINTER_BYTES: u64 = 64 * 1024;
const MAX_REVISION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CARD_BYTES: usize = 64 * 1024 * 1024;
const MAX_JUDGMENTS_PER_RECORD: usize = 256;
const MAX_JUDGMENT_FILE_BYTES: u64 = 128 * 1024;
const RECORD_SCAN_DEADLINE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewCardTargetStateV2 {
    Unreviewed,
    Deferred,
    ChangesRequested,
    RevisedUnreviewed,
    Approved,
    Blocked,
}

impl ReviewCardTargetStateV2 {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Deferred => "deferred",
            Self::ChangesRequested => "changes_requested",
            Self::RevisedUnreviewed => "revised_unreviewed",
            Self::Approved => "approved",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewCardTargetV2 {
    pub snapshot: ReviewTargetSnapshotV2,
    pub state: ReviewCardTargetStateV2,
    /// Core-owned policy embedded in the exact immutable Knowledge revision.
    ///
    /// Approval requires the human to type this value back for every pending
    /// Knowledge target. Source targets have no domain policy.
    pub domain_policy: Option<DomainPolicyV2>,
    pub previous_approved_revision: Option<String>,
    pub previous_reviewed_revision: Option<String>,
    pub current_feedback: Option<String>,
    /// Feedback the displayed replacement revision claims to address; present
    /// only in the revised-unreviewed state.
    pub addressed_feedback: Option<String>,
    pub conflicting_review_head_ids: Vec<String>,
    pub effects: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedReviewCardV2 {
    pub item_id: String,
    pub asset_id: String,
    pub targets: Vec<ReviewCardTargetV2>,
    pub effect_digest: String,
    pub card_bytes: Vec<u8>,
    pub card_digest: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HomeQueueSummaryV2 {
    pub review_pending: u64,
    pub changes_requested: u64,
    pub blocked: u64,
    pub approved_knowledge: u64,
    /// Assets that already have a Source or Knowledge record, in any state.
    pub recorded_asset_ids: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeSearchMatchV2 {
    pub knowledge_id: String,
    pub asset_id: String,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub perspectives: Vec<PerspectiveV2>,
    pub locators: Vec<String>,
    pub layer: KnowledgeSearchLayerV2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnowledgeSearchLayerV2 {
    GroundedEvidence,
    LlmAnalysis,
    CounterargumentOrUncertainty,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResurfacedKnowledgeV2 {
    pub knowledge_id: String,
    pub current_revision: String,
    pub title: String,
    pub synthesis: String,
    pub perspectives: Vec<PerspectiveV2>,
    pub has_open_questions: bool,
    pub review_state: ResurfacedKnowledgeStateV2,
    pub reviewed_at: DateTime<Utc>,
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResurfacedKnowledgeStateV2 {
    Deferred,
    Approved,
}

#[derive(Clone)]
enum RevisionV2 {
    Source(SourceRevisionV2),
    Knowledge(KnowledgeRevisionV2),
}

impl RevisionV2 {
    fn asset_id(&self) -> &str {
        match self {
            Self::Source(revision) => &revision.asset_id,
            Self::Knowledge(revision) => &revision.asset_id,
        }
    }

    fn title<'a>(&'a self, asset: &'a AssetRecordV2) -> &'a str {
        match self {
            Self::Source(revision) => &revision.response.title,
            Self::Knowledge(_) => &asset.title_fallback,
        }
    }

    fn asset_fingerprint(&self) -> &str {
        match self {
            Self::Source(revision) => &revision.asset_fingerprint,
            Self::Knowledge(revision) => &revision.asset_fingerprint,
        }
    }
}

#[derive(Clone)]
struct ScannedTarget {
    record_type: ReviewTargetTypeV2,
    record_id: String,
    pointer: CurrentPointerV2,
    revision: RevisionV2,
    asset: AssetRecordV2,
    history: Option<ReviewTargetHistoryV2>,
    state: Option<ReviewCardTargetStateV2>,
    projection_stale: bool,
    expected_projection: Option<ProjectionInputV2>,
    domain_policy_gate_satisfied: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CanonicalProjectionHealthV2 {
    pub path: String,
    pub stale: bool,
    pub expected: ProjectionInputV2,
}

pub fn derive_queue_v2(repository_root: &Path) -> Result<QueueDataV2, MkoError> {
    let groups = derive_groups(repository_root)?;
    queue_from_groups(&groups)
}

pub fn summarize_home_queue_v2(repository_root: &Path) -> Result<HomeQueueSummaryV2, MkoError> {
    let groups = derive_groups(repository_root)?;
    let queue = queue_from_groups(&groups)?;
    let mut summary = HomeQueueSummaryV2::default();
    for item in queue.items {
        match item.state {
            crate::json_v2::QueueItemStateV2::Unreviewed
            | crate::json_v2::QueueItemStateV2::Deferred
            | crate::json_v2::QueueItemStateV2::RevisedUnreviewed => {
                summary.review_pending += 1;
            }
            crate::json_v2::QueueItemStateV2::ChangesRequested => {
                summary.changes_requested += 1;
            }
            crate::json_v2::QueueItemStateV2::Blocked => {
                summary.blocked += 1;
            }
        }
    }
    summary.recorded_asset_ids = groups
        .values()
        .flatten()
        .map(|target| target.asset.id.clone())
        .collect();
    summary.approved_knowledge = groups
        .values()
        .flatten()
        .filter(|target| {
            target.record_type == ReviewTargetTypeV2::Knowledge
                && target.state == Some(ReviewCardTargetStateV2::Approved)
        })
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(summary)
}

pub fn search_approved_knowledge_v2(
    repository_root: &Path,
    term: &str,
) -> Result<Vec<KnowledgeSearchMatchV2>, MkoError> {
    search_approved_knowledge_by_perspective_v2(repository_root, term, None)
}

pub fn search_approved_knowledge_by_perspective_v2(
    repository_root: &Path,
    term: &str,
    perspective: Option<PerspectiveV2>,
) -> Result<Vec<KnowledgeSearchMatchV2>, MkoError> {
    let needle = term.trim().to_lowercase();
    if needle.is_empty() {
        return Err(MkoError::new(
            "knowledge_search_invalid",
            "search term must not be empty",
        ));
    }
    let groups = derive_groups(repository_root)?;
    let mut matches = groups
        .values()
        .flatten()
        .filter(|target| {
            target.record_type == ReviewTargetTypeV2::Knowledge
                && target.state == Some(ReviewCardTargetStateV2::Approved)
        })
        .flat_map(|target| {
            let RevisionV2::Knowledge(revision) = &target.revision else {
                return Vec::new();
            };
            if perspective
                .as_ref()
                .is_some_and(|selected| !revision.perspectives.contains(selected))
            {
                return Vec::new();
            }
            revision
                .response
                .units
                .iter()
                .filter(|unit| {
                    unit.title.to_lowercase().contains(&needle)
                        || unit.body.to_lowercase().contains(&needle)
                        || unit
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains(&needle))
                        || revision
                            .perspectives
                            .iter()
                            .any(|perspective| perspective.as_str().contains(&needle))
                })
                .map(|unit| KnowledgeSearchMatchV2 {
                    knowledge_id: target.record_id.clone(),
                    asset_id: target.asset.id.clone(),
                    title: unit.title.clone(),
                    body: unit.body.clone(),
                    tags: unit.tags.clone(),
                    perspectives: revision.perspectives.clone(),
                    locators: unit
                        .evidence_refs
                        .iter()
                        .map(|evidence| evidence.locator.clone())
                        .collect(),
                    layer: search_layer(&unit.kind),
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.title
            .cmp(&right.title)
            .then(left.knowledge_id.cmp(&right.knowledge_id))
            .then(left.body.cmp(&right.body))
    });
    Ok(matches)
}

fn search_layer(kind: &KnowledgeUnitKindV2) -> KnowledgeSearchLayerV2 {
    match kind {
        KnowledgeUnitKindV2::Fact
        | KnowledgeUnitKindV2::Definition
        | KnowledgeUnitKindV2::Formula
        | KnowledgeUnitKindV2::Result => KnowledgeSearchLayerV2::GroundedEvidence,
        KnowledgeUnitKindV2::Interpretation | KnowledgeUnitKindV2::Hypothesis => {
            KnowledgeSearchLayerV2::LlmAnalysis
        }
        KnowledgeUnitKindV2::Counterargument
        | KnowledgeUnitKindV2::Uncertainty
        | KnowledgeUnitKindV2::OpenQuestion => KnowledgeSearchLayerV2::CounterargumentOrUncertainty,
    }
}

pub fn resurface_approved_knowledge_v2(
    repository_root: &Path,
    limit: usize,
) -> Result<Vec<ResurfacedKnowledgeV2>, MkoError> {
    resurface_approved_knowledge_by_perspective_v2(repository_root, None, limit)
}

pub fn resurface_approved_knowledge_by_perspective_v2(
    repository_root: &Path,
    perspective: Option<PerspectiveV2>,
    limit: usize,
) -> Result<Vec<ResurfacedKnowledgeV2>, MkoError> {
    resurface_knowledge_internal(repository_root, perspective, limit, false)
}

pub fn resurface_knowledge_by_perspective_v2(
    repository_root: &Path,
    perspective: Option<PerspectiveV2>,
    limit: usize,
) -> Result<Vec<ResurfacedKnowledgeV2>, MkoError> {
    resurface_knowledge_internal(repository_root, perspective, limit, true)
}

fn resurface_knowledge_internal(
    repository_root: &Path,
    perspective: Option<PerspectiveV2>,
    limit: usize,
    include_deferred: bool,
) -> Result<Vec<ResurfacedKnowledgeV2>, MkoError> {
    let groups = derive_groups(repository_root)?;
    let opened_at = read_resurface_opened_at_v2(repository_root)?;
    let mut items = groups
        .values()
        .flatten()
        .filter(|target| {
            target.record_type == ReviewTargetTypeV2::Knowledge
                && (target.state == Some(ReviewCardTargetStateV2::Approved)
                    || (include_deferred
                        && target.state == Some(ReviewCardTargetStateV2::Deferred)))
        })
        .filter_map(|target| {
            let RevisionV2::Knowledge(revision) = &target.revision else {
                return None;
            };
            let history = target.history.as_ref()?;
            let reviewed_at = history.current_reviewed_at?;
            if perspective
                .as_ref()
                .is_some_and(|selected| !revision.perspectives.contains(selected))
            {
                return None;
            }
            Some(ResurfacedKnowledgeV2 {
                knowledge_id: target.record_id.clone(),
                current_revision: target.pointer.revision.clone(),
                title: target.revision.title(&target.asset).to_owned(),
                synthesis: revision.response.synthesis.clone(),
                perspectives: revision.perspectives.clone(),
                has_open_questions: revision
                    .response
                    .units
                    .iter()
                    .any(|unit| unit.kind == KnowledgeUnitKindV2::OpenQuestion),
                review_state: if target.state == Some(ReviewCardTargetStateV2::Deferred) {
                    ResurfacedKnowledgeStateV2::Deferred
                } else {
                    ResurfacedKnowledgeStateV2::Approved
                },
                reviewed_at,
                last_opened_at: opened_at
                    .get(&(target.record_id.clone(), target.pointer.revision.clone()))
                    .copied(),
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(compare_resurfaced_knowledge);
    items.truncate(limit);
    Ok(items)
}

impl ResurfacedKnowledgeV2 {
    fn is_deferred(&self) -> bool {
        self.review_state == ResurfacedKnowledgeStateV2::Deferred
    }
}

fn compare_resurfaced_knowledge(
    left: &ResurfacedKnowledgeV2,
    right: &ResurfacedKnowledgeV2,
) -> std::cmp::Ordering {
    right
        .is_deferred()
        .cmp(&left.is_deferred())
        .then(
            left.last_opened_at
                .is_some()
                .cmp(&right.last_opened_at.is_some()),
        )
        .then(left.last_opened_at.cmp(&right.last_opened_at))
        .then(right.has_open_questions.cmp(&left.has_open_questions))
        .then(right.reviewed_at.cmp(&left.reviewed_at))
        .then(left.knowledge_id.cmp(&right.knowledge_id))
}

pub(crate) fn derive_queue_with_projection_health_v2(
    repository_root: &Path,
) -> Result<(QueueDataV2, Vec<CanonicalProjectionHealthV2>), MkoError> {
    let groups = derive_groups(repository_root)?;
    let queue = queue_from_groups(&groups)?;
    let mut projections = groups
        .values()
        .flatten()
        .map(|target| {
            let expected = target.expected_projection.clone().ok_or_else(|| {
                MkoError::new(
                    "projection_state_invalid",
                    "canonical expected projection is missing",
                )
            })?;
            Ok(CanonicalProjectionHealthV2 {
                path: projection_relative_path_v2(&expected)?,
                stale: target.projection_stale,
                expected,
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    projections.sort_by(|left, right| left.path.cmp(&right.path));
    Ok((queue, projections))
}

fn queue_from_groups(
    groups: &BTreeMap<String, Vec<ScannedTarget>>,
) -> Result<QueueDataV2, MkoError> {
    let items = groups
        .values()
        .filter(|targets| {
            targets
                .iter()
                .any(|target| target.state.as_ref() != Some(&ReviewCardTargetStateV2::Approved))
        })
        .map(queue_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(QueueDataV2 {
        items,
        scan_complete: true,
        remaining: 0,
        next_cursor: None,
    })
}

pub fn show_review_card_v2(
    repository_root: &Path,
    stable_id: &str,
) -> Result<RenderedReviewCardV2, MkoError> {
    validate_show_id(stable_id)?;
    let groups = derive_groups(repository_root)?;
    let selected = groups.values().find(|targets| {
        item_id(targets).is_ok_and(|item_id| item_id == stable_id)
            || targets.iter().any(|target| target.record_id == stable_id)
    });
    let targets = selected.ok_or_else(|| {
        MkoError::new(
            "review_card_not_found",
            "no current Source or Knowledge record matches the stable ID",
        )
    })?;
    render_card(repository_root, targets)
}

fn derive_groups(repository_root: &Path) -> Result<BTreeMap<String, Vec<ScannedTarget>>, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    validate_real_directory(repository_root, "queue_repository_invalid")?;
    let deadline = Instant::now() + RECORD_SCAN_DEADLINE;
    let mut targets = scan_collection(
        repository_root,
        ReviewTargetTypeV2::Source,
        "sources",
        deadline,
    )?;
    targets.extend(scan_collection(
        repository_root,
        ReviewTargetTypeV2::Knowledge,
        "knowledge",
        deadline,
    )?);
    if targets.len() > MAX_RECORDS {
        return Err(queue_scan_limit());
    }

    let review_inputs = targets
        .iter()
        .map(|target| {
            (
                target.record_type.clone(),
                target.record_id.clone(),
                target.pointer.revision.clone(),
            )
        })
        .collect::<Vec<_>>();
    let histories = derive_review_histories_v2(repository_root, &review_inputs)?;
    for (target, history) in targets.iter_mut().zip(histories) {
        let mut state = target_state(&history);
        target.history = Some(history);
        target.state = Some(state.clone());
        let expected_projection = canonical_projection_input(target)?;
        let projection_status =
            projection_snapshot_status_v2(repository_root, &expected_projection)?;
        target.expected_projection = Some(expected_projection);
        target.projection_stale = projection_status != ProjectionSnapshotStatusV2::Current;
        if target.projection_stale || !target.domain_policy_gate_satisfied {
            state = ReviewCardTargetStateV2::Blocked;
        }
        target.state = Some(state);
    }

    let mut groups = BTreeMap::<String, Vec<ScannedTarget>>::new();
    for target in targets {
        groups
            .entry(target.revision.asset_id().to_owned())
            .or_default()
            .push(target);
    }
    for targets in groups.values_mut() {
        targets.sort_by_key(|target| match target.record_type {
            ReviewTargetTypeV2::Source => 0,
            ReviewTargetTypeV2::Knowledge => 1,
        });
        if targets.len() > 2
            || targets
                .windows(2)
                .any(|pair| pair[0].record_type == pair[1].record_type)
        {
            return Err(MkoError::new(
                "queue_record_conflict",
                "an Asset has multiple current records of the same semantic type",
            ));
        }
    }
    Ok(groups)
}

fn canonical_projection_input(target: &ScannedTarget) -> Result<ProjectionInputV2, MkoError> {
    let history = target
        .history
        .as_ref()
        .ok_or_else(|| MkoError::new("review_state_invalid", "review history is missing"))?;
    let state = target
        .state
        .as_ref()
        .ok_or_else(|| MkoError::new("review_state_invalid", "review state is missing"))?;
    let (record_type, collection) = match target.record_type {
        ReviewTargetTypeV2::Source => (ProjectionRecordTypeV2::Source, "sources"),
        ReviewTargetTypeV2::Knowledge => (ProjectionRecordTypeV2::Knowledge, "knowledge"),
    };
    let mut tags = match &target.revision {
        RevisionV2::Source(revision) => revision.response.tags.clone(),
        RevisionV2::Knowledge(revision) => revision
            .response
            .units
            .iter()
            .flat_map(|unit| unit.tags.iter().cloned())
            .collect(),
    };
    let perspectives = match &target.revision {
        RevisionV2::Source(_) => Vec::new(),
        RevisionV2::Knowledge(revision) => revision.perspectives.clone(),
    };
    tags.extend(
        perspectives
            .iter()
            .map(|perspective| format!("perspective:{}", perspective.as_str())),
    );
    tags.sort();
    tags.dedup();
    let body = match &target.revision {
        RevisionV2::Source(revision) => crate::projection_v2::source_projection_body_v2(
            &revision.response,
            Some(target.asset.provider.logical_locator.clone()),
        ),
        RevisionV2::Knowledge(revision) => crate::projection_v2::knowledge_projection_body_v2(
            &revision.response,
            Some(target.asset.provider.logical_locator.clone()),
        ),
    };
    Ok(ProjectionInputV2 {
        record_type,
        id: target.record_id.clone(),
        title: target.revision.title(&target.asset).to_owned(),
        current_revision: target.pointer.revision.clone(),
        review_head_id: history.derived.review_head_id.clone(),
        derived_state: projection_state(state),
        domain: primary_perspective(&perspectives),
        perspectives,
        tags,
        body,
        record_link: format!("{collection}/{}/current.yaml", target.record_id),
        asset_link: format!("assets/registry/{}.json", target.asset.id),
    })
}

fn primary_perspective(perspectives: &[PerspectiveV2]) -> String {
    if perspectives.contains(&PerspectiveV2::Investment) {
        PerspectiveV2::Investment.as_str().into()
    } else {
        perspectives
            .first()
            .map(PerspectiveV2::as_str)
            .unwrap_or("uncategorized")
            .into()
    }
}

fn scan_collection(
    repository_root: &Path,
    record_type: ReviewTargetTypeV2,
    collection: &str,
    deadline: Instant,
) -> Result<Vec<ScannedTarget>, MkoError> {
    let directory = repository_root.join(collection);
    validate_real_directory(&directory, "queue_repository_invalid")?;
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| MkoError::new("queue_scan_failed", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("queue_scan_failed", error.to_string()))?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_RECORDS {
        return Err(queue_scan_limit());
    }
    let mut targets = Vec::with_capacity(entries.len());
    for entry in entries {
        if Instant::now() >= deadline {
            return Err(queue_scan_limit());
        }
        let record_id = entry
            .file_name()
            .into_string()
            .map_err(|_| MkoError::new("queue_record_invalid", "record ID must be valid UTF-8"))?;
        validate_record_id(&record_type, &record_id)?;
        validate_real_directory(&entry.path(), "queue_record_invalid")?;
        let pointer_bytes = read_regular_nofollow(
            &entry.path().join("current.yaml"),
            MAX_CURRENT_POINTER_BYTES,
            "queue_pointer_invalid",
        )?;
        let pointer: CurrentPointerV2 = serde_json::from_slice(&pointer_bytes).map_err(|_| {
            MkoError::new(
                "queue_pointer_invalid",
                "current pointer is not canonical schema-v2 JSON",
            )
        })?;
        if canonical_json_bytes(&pointer)? != pointer_bytes
            || pointer.record_id != record_id
            || pointer.record_type != semantic_type(&record_type)
            || !valid_digest(&pointer.revision)
        {
            return Err(MkoError::new(
                "queue_pointer_invalid",
                "current pointer does not canonically identify its record",
            ));
        }
        let revision_path = entry
            .path()
            .join("revisions")
            .join(format!("{}.md", pointer.revision.replace(':', "-")));
        let revision_bytes =
            read_regular_nofollow(&revision_path, MAX_REVISION_BYTES, "queue_revision_invalid")?;
        if sha256_digest(&revision_bytes) != pointer.revision {
            return Err(MkoError::new(
                "queue_revision_invalid",
                "current immutable revision bytes do not match their digest",
            ));
        }
        let revision = parse_revision(&record_type, &record_id, Some(&pointer), &revision_bytes)?;
        let asset = read_asset_v2(repository_root, revision.asset_id())?;
        if revision.asset_fingerprint() != asset.fingerprint {
            return Err(MkoError::new(
                "queue_revision_invalid",
                "current revision does not match its immutable Asset fingerprint",
            ));
        }
        let domain_policy_gate_satisfied = knowledge_policy_gate_satisfied(&revision);
        targets.push(ScannedTarget {
            record_type: record_type.clone(),
            record_id,
            pointer,
            revision,
            asset,
            history: None,
            state: None,
            projection_stale: false,
            expected_projection: None,
            domain_policy_gate_satisfied,
        });
    }
    Ok(targets)
}

fn parse_revision(
    record_type: &ReviewTargetTypeV2,
    record_id: &str,
    pointer: Option<&CurrentPointerV2>,
    bytes: &[u8],
) -> Result<RevisionV2, MkoError> {
    let (heading, json_bytes) = match record_type {
        ReviewTargetTypeV2::Source => (
            b"# Source revision\n\n    ".as_slice(),
            revision_json(bytes, b"# Source revision\n\n    ")?,
        ),
        ReviewTargetTypeV2::Knowledge => (
            b"# Knowledge revision\n\n    ".as_slice(),
            revision_json(bytes, b"# Knowledge revision\n\n    ")?,
        ),
    };
    let revision = match record_type {
        ReviewTargetTypeV2::Source => {
            let revision: SourceRevisionV2 = serde_json::from_slice(json_bytes)
                .map_err(|_| revision_invalid("Source revision JSON is invalid"))?;
            if revision.record_id != record_id
                || revision.asset_fingerprint != revision.evidence_basis.asset_fingerprint
                || pointer.is_some_and(|pointer| {
                    revision.evidence_basis != pointer.evidence_basis
                        || revision.asset_fingerprint != pointer.evidence_basis.asset_fingerprint
                })
                || canonical_json_bytes(&revision)? != json_bytes
            {
                return Err(revision_invalid("Source revision identity is inconsistent"));
            }
            RevisionV2::Source(revision)
        }
        ReviewTargetTypeV2::Knowledge => {
            let revision: KnowledgeRevisionV2 = serde_json::from_slice(json_bytes)
                .map_err(|_| revision_invalid("Knowledge revision JSON is invalid"))?;
            if revision.record_id != record_id
                || revision.asset_fingerprint != revision.evidence_basis.asset_fingerprint
                || pointer.is_some_and(|pointer| {
                    revision.evidence_basis != pointer.evidence_basis
                        || revision.asset_fingerprint != pointer.evidence_basis.asset_fingerprint
                })
                || canonical_json_bytes(&revision)? != json_bytes
            {
                return Err(revision_invalid(
                    "Knowledge revision identity is inconsistent",
                ));
            }
            RevisionV2::Knowledge(revision)
        }
    };
    let mut expected = Vec::with_capacity(bytes.len());
    expected.extend_from_slice(heading);
    expected.extend_from_slice(json_bytes);
    expected.push(b'\n');
    if expected != bytes {
        return Err(revision_invalid("revision Markdown is not canonical"));
    }
    Ok(revision)
}

fn revision_json<'a>(bytes: &'a [u8], heading: &[u8]) -> Result<&'a [u8], MkoError> {
    bytes
        .strip_prefix(heading)
        .and_then(|remaining| remaining.strip_suffix(b"\n"))
        .ok_or_else(|| revision_invalid("revision Markdown wrapper is invalid"))
}

fn target_state(history: &ReviewTargetHistoryV2) -> ReviewCardTargetStateV2 {
    match history.derived.state {
        ReviewDerivedStateV2::Unreviewed if history.previous_reviewed_revision.is_some() => {
            ReviewCardTargetStateV2::RevisedUnreviewed
        }
        ReviewDerivedStateV2::Unreviewed => ReviewCardTargetStateV2::Unreviewed,
        ReviewDerivedStateV2::Deferred => ReviewCardTargetStateV2::Deferred,
        ReviewDerivedStateV2::ChangesRequested => ReviewCardTargetStateV2::ChangesRequested,
        ReviewDerivedStateV2::Approved => ReviewCardTargetStateV2::Approved,
        ReviewDerivedStateV2::BlockedConflict => ReviewCardTargetStateV2::Blocked,
    }
}

fn projection_state(state: &ReviewCardTargetStateV2) -> ProjectionStateV2 {
    match state {
        ReviewCardTargetStateV2::Unreviewed => ProjectionStateV2::Unreviewed,
        ReviewCardTargetStateV2::Deferred => ProjectionStateV2::Deferred,
        ReviewCardTargetStateV2::ChangesRequested => ProjectionStateV2::ChangesRequested,
        ReviewCardTargetStateV2::RevisedUnreviewed => ProjectionStateV2::RevisedUnreviewed,
        ReviewCardTargetStateV2::Approved => ProjectionStateV2::Approved,
        ReviewCardTargetStateV2::Blocked => ProjectionStateV2::Blocked,
    }
}

fn queue_item(targets: &Vec<ScannedTarget>) -> Result<QueueItemV2, MkoError> {
    let state = aggregate_queue_state(targets);
    Ok(QueueItemV2 {
        item_id: item_id(targets)?,
        target_ids: targets
            .iter()
            .map(|target| target.record_id.clone())
            .collect(),
        title: group_title(targets).to_owned(),
        item_type: match targets.as_slice() {
            [target] if target.record_type == ReviewTargetTypeV2::Source => QueueItemTypeV2::Source,
            [target] if target.record_type == ReviewTargetTypeV2::Knowledge => {
                QueueItemTypeV2::Knowledge
            }
            _ => QueueItemTypeV2::Combined,
        },
        state,
        revisions: targets
            .iter()
            .map(|target| target.pointer.revision.clone())
            .collect(),
        next_action: match aggregate_queue_state(targets) {
            QueueItemStateV2::ChangesRequested => QueueNextActionV2::Regenerate,
            QueueItemStateV2::Blocked => QueueNextActionV2::Diagnose,
            QueueItemStateV2::Unreviewed
            | QueueItemStateV2::Deferred
            | QueueItemStateV2::RevisedUnreviewed => QueueNextActionV2::Display,
        },
    })
}

fn aggregate_queue_state(targets: &[ScannedTarget]) -> QueueItemStateV2 {
    let states = targets.iter().filter_map(|target| target.state.as_ref());
    let states = states.collect::<Vec<_>>();
    if states.contains(&&ReviewCardTargetStateV2::Blocked) {
        QueueItemStateV2::Blocked
    } else if states.contains(&&ReviewCardTargetStateV2::ChangesRequested) {
        QueueItemStateV2::ChangesRequested
    } else if states.contains(&&ReviewCardTargetStateV2::RevisedUnreviewed) {
        QueueItemStateV2::RevisedUnreviewed
    } else if states.contains(&&ReviewCardTargetStateV2::Unreviewed) {
        QueueItemStateV2::Unreviewed
    } else {
        QueueItemStateV2::Deferred
    }
}

fn item_id(targets: &[ScannedTarget]) -> Result<String, MkoError> {
    let asset_id = targets
        .first()
        .ok_or_else(|| MkoError::new("queue_record_invalid", "empty Asset group"))?
        .revision
        .asset_id();
    let digest = canonical_json_sha256(&serde_json::json!({
        "schema_version": 2,
        "view": "review_queue",
        "asset_id": asset_id,
    }))?;
    Ok(format!(
        "personal-queue-{}",
        digest.trim_start_matches("sha256:")
    ))
}

fn group_title(targets: &[ScannedTarget]) -> &str {
    targets
        .iter()
        .find_map(|target| match &target.revision {
            RevisionV2::Source(revision) => Some(revision.response.title.as_str()),
            RevisionV2::Knowledge(_) => None,
        })
        .unwrap_or_else(|| targets[0].revision.title(&targets[0].asset))
}

#[derive(Serialize)]
struct EffectInputV2<'a> {
    operation: &'static str,
    targets: Vec<EffectTargetV2<'a>>,
}

#[derive(Serialize)]
struct EffectTargetV2<'a> {
    record_type: &'a ReviewTargetTypeV2,
    record_id: &'a str,
    displayed_revision: &'a str,
    expected_review_head_id: Option<&'a str>,
    domain_policy: Option<&'a DomainPolicyV2>,
    effects: &'a [String],
}

fn render_card(
    repository_root: &Path,
    targets: &[ScannedTarget],
) -> Result<RenderedReviewCardV2, MkoError> {
    let item_id = item_id(targets)?;
    let asset_id = targets[0].revision.asset_id().to_owned();
    let card_targets = targets.iter().map(card_target).collect::<Vec<_>>();
    let effect = EffectInputV2 {
        operation: "review_current_targets",
        targets: targets
            .iter()
            .zip(&card_targets)
            .map(|(target, card_target)| EffectTargetV2 {
                record_type: &target.record_type,
                record_id: &target.record_id,
                displayed_revision: &target.pointer.revision,
                expected_review_head_id: target
                    .history
                    .as_ref()
                    .and_then(|history| history.derived.review_head_id.as_deref()),
                domain_policy: card_target.domain_policy.as_ref(),
                effects: &card_target.effects,
            })
            .collect(),
    };
    let effect_digest = canonical_json_sha256(&effect)?;
    let mut card = String::new();
    card.push_str("# Review card\n\n");
    card.push_str(&format!("- Item ID: `{item_id}`\n"));
    card.push_str(&format!("- Asset ID: `{asset_id}`\n"));
    card.push_str(&format!("- Effect digest: `{effect_digest}`\n"));
    card.push_str("\n## Exact targets and effects\n");
    for card_target in &card_targets {
        let snapshot = &card_target.snapshot;
        card.push_str(&format!(
            "\n### {}\n\n- Record ID: `{}`\n- Current revision: `{}`\n- Review head: `{}`\n- State: `{}`\n- Previous approved revision: `{}`\n- Effects: `{}`\n",
            target_type_name(&snapshot.record_type),
            snapshot.record_id,
            snapshot.displayed_revision,
            snapshot.expected_review_head_id.as_deref().unwrap_or("none"),
            card_target.state.as_str(),
            card_target
                .previous_approved_revision
                .as_deref()
                .unwrap_or("none"),
            card_target.effects.join(", "),
        ));
        if let Some(domain_policy) = &card_target.domain_policy {
            card.push_str(&format!(
                "- Domain policy requiring human confirmation: `{}`\n",
                domain_policy_name(domain_policy)
            ));
        }
        if !card_target.conflicting_review_head_ids.is_empty() {
            card.push_str(&format!(
                "- Conflicting review heads: `{}`\n",
                card_target.conflicting_review_head_ids.join("`, `")
            ));
        }
    }
    append_json_section(&mut card, "Provenance", &targets[0].asset)?;
    for target in targets {
        match &target.revision {
            RevisionV2::Source(revision) => {
                append_json_section(&mut card, "Source-grounded content", &revision.response)?;
            }
            RevisionV2::Knowledge(revision) => {
                append_json_section(&mut card, "Knowledge analysis", &revision.response)?;
                let judgments = read_current_judgments(repository_root, target)?;
                if !judgments.is_empty() {
                    append_json_section(&mut card, "User judgments", &judgments)?;
                }
            }
        }
        if let Some(history) = &target.history {
            if let Some(feedback) = &history.current_feedback {
                append_json_section(
                    &mut card,
                    &format!("Current feedback for {}", target.record_id),
                    feedback,
                )?;
            }
            if let Some(previous) = &history.previous_reviewed_revision {
                let previous_revision = read_revision_by_digest(
                    repository_root,
                    target,
                    previous,
                    "review_card_previous_revision_invalid",
                )?;
                let heading = format!("Previous reviewed content for {}", target.record_id);
                match &previous_revision {
                    RevisionV2::Source(revision) => {
                        append_json_section(&mut card, &heading, &revision.response)?;
                    }
                    RevisionV2::Knowledge(revision) => {
                        append_json_section(&mut card, &heading, &revision.response)?;
                    }
                }
                if target.state == Some(ReviewCardTargetStateV2::RevisedUnreviewed) {
                    if let Some(feedback) = &history.previous_feedback {
                        append_json_section(
                            &mut card,
                            &format!(
                                "Feedback addressed by this revision for {}",
                                target.record_id
                            ),
                            feedback,
                        )?;
                    }
                    append_diff_section(
                        &mut card,
                        &target.record_id,
                        previous,
                        &target.pointer.revision,
                        &revision_pretty_json(&previous_revision)?,
                        &revision_pretty_json(&target.revision)?,
                    );
                }
            }
        }
        if target.projection_stale {
            card.push_str(&format!(
                "\n## Diagnostic for {}\n\nThe generated projection is stale or non-canonical for the exact current pointer and review head.\n",
                target.record_id
            ));
        }
        if !target.domain_policy_gate_satisfied {
            card.push_str(&format!(
                "\n## Diagnostic for {}\n\nThe high-risk Knowledge revision is missing a counterargument or open question and cannot be approved.\n",
                target.record_id
            ));
        }
    }
    if card.len() > MAX_CARD_BYTES {
        return Err(MkoError::new(
            "review_card_too_large",
            "canonical review card exceeds its bounded byte limit",
        ));
    }
    let card_bytes = card.into_bytes();
    let card_digest = sha256_digest(&card_bytes);
    Ok(RenderedReviewCardV2 {
        item_id,
        asset_id,
        targets: card_targets,
        effect_digest,
        card_bytes,
        card_digest,
    })
}

fn card_target(target: &ScannedTarget) -> ReviewCardTargetV2 {
    let history = target.history.as_ref().expect("queue history derived");
    let state = target.state.clone().expect("queue state derived");
    let effects = match state {
        ReviewCardTargetStateV2::Blocked => vec!["diagnose".into()],
        ReviewCardTargetStateV2::ChangesRequested => vec![
            "regenerate_current_revision".into(),
            "defer_current_revision".into(),
            "approve_current_revision_via_tty".into(),
        ],
        ReviewCardTargetStateV2::Approved => vec!["none".into()],
        ReviewCardTargetStateV2::Unreviewed
        | ReviewCardTargetStateV2::Deferred
        | ReviewCardTargetStateV2::RevisedUnreviewed => vec![
            "request_changes_current_revision".into(),
            "defer_current_revision".into(),
            "approve_current_revision_via_tty".into(),
        ],
    };
    ReviewCardTargetV2 {
        snapshot: ReviewTargetSnapshotV2 {
            record_type: target.record_type.clone(),
            record_id: target.record_id.clone(),
            displayed_revision: target.pointer.revision.clone(),
            expected_review_head_id: history.derived.review_head_id.clone(),
        },
        domain_policy: match &target.revision {
            RevisionV2::Source(_) => None,
            RevisionV2::Knowledge(revision) => Some(revision.domain_policy.clone()),
        },
        previous_approved_revision: history.previous_approved_revision.clone(),
        previous_reviewed_revision: history.previous_reviewed_revision.clone(),
        current_feedback: history.current_feedback.clone(),
        addressed_feedback: if state == ReviewCardTargetStateV2::RevisedUnreviewed {
            history.previous_feedback.clone()
        } else {
            None
        },
        state,
        conflicting_review_head_ids: history.derived.conflicting_review_head_ids.clone(),
        effects,
    }
}

fn knowledge_policy_gate_satisfied(revision: &RevisionV2) -> bool {
    let RevisionV2::Knowledge(revision) = revision else {
        return true;
    };
    if revision.domain_policy != DomainPolicyV2::HighRisk {
        return true;
    }
    let has_counterargument = revision
        .response
        .units
        .iter()
        .any(|unit| unit.kind == KnowledgeUnitKindV2::Counterargument);
    let has_open_question = revision
        .response
        .units
        .iter()
        .any(|unit| unit.kind == KnowledgeUnitKindV2::OpenQuestion);
    has_counterargument && has_open_question
}

fn domain_policy_name(policy: &DomainPolicyV2) -> &'static str {
    match policy {
        DomainPolicyV2::Standard => "standard",
        DomainPolicyV2::HighRisk => "high_risk",
    }
}

fn read_revision_by_digest(
    repository_root: &Path,
    target: &ScannedTarget,
    revision: &str,
    code: &str,
) -> Result<RevisionV2, MkoError> {
    let collection = match target.record_type {
        ReviewTargetTypeV2::Source => "sources",
        ReviewTargetTypeV2::Knowledge => "knowledge",
    };
    if !valid_digest(revision) {
        return Err(MkoError::new(code, "previous revision digest is invalid"));
    }
    let path = repository_root
        .join(collection)
        .join(&target.record_id)
        .join("revisions")
        .join(format!("{}.md", revision.replace(':', "-")));
    let bytes = read_regular_nofollow(&path, MAX_REVISION_BYTES, code)?;
    if sha256_digest(&bytes) != revision {
        return Err(MkoError::new(
            code,
            "previous immutable revision bytes do not match their digest",
        ));
    }
    let parsed = parse_revision(&target.record_type, &target.record_id, None, &bytes)
        .map_err(|error| MkoError::new(code, error.message()))?;
    if parsed.asset_id() != target.asset.id
        || parsed.asset_fingerprint() != target.asset.fingerprint
    {
        return Err(MkoError::new(
            code,
            "previous revision does not match the record's immutable Asset",
        ));
    }
    Ok(parsed)
}

const MAX_DIFF_SECTION_BYTES: usize = 256 * 1024;
const MAX_DIFF_LCS_LINES: usize = 512;
const DIFF_CONTEXT_LINES: usize = 3;
const DIFF_KEEP_COLLAPSE_THRESHOLD: usize = 2 * DIFF_CONTEXT_LINES + 1;

enum DiffOpV2<'a> {
    Keep(&'a str),
    Remove(&'a str),
    Add(&'a str),
}

/// Deterministic, dependency-free, line-based diff for the re-review card.
///
/// Common prefix and suffix lines are trimmed first; the remaining middle is
/// diffed exactly (LCS) when both sides fit `MAX_DIFF_LCS_LINES`, and emitted
/// as one remove-then-add block otherwise. Long unchanged runs collapse to a
/// count marker and the whole section is byte-bounded, so the card limit can
/// never be reached through a pathological revision pair.
pub(crate) fn bounded_revision_diff(previous: &str, current: &str) -> String {
    let previous_lines: Vec<&str> = previous.lines().collect();
    let current_lines: Vec<&str> = current.lines().collect();
    let mut prefix = 0;
    while prefix < previous_lines.len()
        && prefix < current_lines.len()
        && previous_lines[prefix] == current_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < previous_lines.len() - prefix
        && suffix < current_lines.len() - prefix
        && previous_lines[previous_lines.len() - 1 - suffix]
            == current_lines[current_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let removed = &previous_lines[prefix..previous_lines.len() - suffix];
    let added = &current_lines[prefix..current_lines.len() - suffix];
    if removed.is_empty() && added.is_empty() {
        return "No line-level changes.\n".into();
    }

    let middle = if removed.len() <= MAX_DIFF_LCS_LINES && added.len() <= MAX_DIFF_LCS_LINES {
        lcs_diff_ops(removed, added)
    } else {
        removed
            .iter()
            .map(|line| DiffOpV2::Remove(line))
            .chain(added.iter().map(|line| DiffOpV2::Add(line)))
            .collect()
    };

    let mut lines = Vec::new();
    for line in &previous_lines[prefix.saturating_sub(DIFF_CONTEXT_LINES)..prefix] {
        lines.push(format!(" {line}"));
    }
    let mut index = 0;
    while index < middle.len() {
        match &middle[index] {
            DiffOpV2::Keep(_) => {
                let run_start = index;
                while index < middle.len() && matches!(middle[index], DiffOpV2::Keep(_)) {
                    index += 1;
                }
                let run = &middle[run_start..index];
                if run.len() > DIFF_KEEP_COLLAPSE_THRESHOLD {
                    for op in &run[..DIFF_CONTEXT_LINES] {
                        if let DiffOpV2::Keep(line) = op {
                            lines.push(format!(" {line}"));
                        }
                    }
                    lines.push(format!(
                        "… {} unchanged lines …",
                        run.len() - 2 * DIFF_CONTEXT_LINES
                    ));
                    for op in &run[run.len() - DIFF_CONTEXT_LINES..] {
                        if let DiffOpV2::Keep(line) = op {
                            lines.push(format!(" {line}"));
                        }
                    }
                } else {
                    for op in run {
                        if let DiffOpV2::Keep(line) = op {
                            lines.push(format!(" {line}"));
                        }
                    }
                }
            }
            DiffOpV2::Remove(line) => {
                lines.push(format!("-{line}"));
                index += 1;
            }
            DiffOpV2::Add(line) => {
                lines.push(format!("+{line}"));
                index += 1;
            }
        }
    }
    let after_start = previous_lines.len() - suffix;
    for line in
        &previous_lines[after_start..(after_start + DIFF_CONTEXT_LINES).min(previous_lines.len())]
    {
        lines.push(format!(" {line}"));
    }

    let total = lines.len();
    let mut output = String::new();
    for (shown, line) in lines.iter().enumerate() {
        if output.len() + line.len() + 1 > MAX_DIFF_SECTION_BYTES {
            output.push_str(&format!(
                "… diff truncated: {shown} of {total} lines shown …\n"
            ));
            return output;
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn lcs_diff_ops<'a>(removed: &[&'a str], added: &[&'a str]) -> Vec<DiffOpV2<'a>> {
    let rows = removed.len();
    let columns = added.len();
    let width = columns + 1;
    let mut table = vec![0u16; (rows + 1) * width];
    for i in (0..rows).rev() {
        for j in (0..columns).rev() {
            table[i * width + j] = if removed[i] == added[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < rows && j < columns {
        if removed[i] == added[j] {
            ops.push(DiffOpV2::Keep(removed[i]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push(DiffOpV2::Remove(removed[i]));
            i += 1;
        } else {
            ops.push(DiffOpV2::Add(added[j]));
            j += 1;
        }
    }
    while i < rows {
        ops.push(DiffOpV2::Remove(removed[i]));
        i += 1;
    }
    while j < columns {
        ops.push(DiffOpV2::Add(added[j]));
        j += 1;
    }
    ops
}

fn revision_pretty_json(revision: &RevisionV2) -> Result<String, MkoError> {
    match revision {
        RevisionV2::Source(revision) => serde_json::to_string_pretty(&revision.response),
        RevisionV2::Knowledge(revision) => serde_json::to_string_pretty(&revision.response),
    }
    .map_err(|error| MkoError::new("review_card_invalid", error.to_string()))
}

fn append_diff_section(
    card: &mut String,
    record_id: &str,
    previous_digest: &str,
    current_digest: &str,
    previous_json: &str,
    current_json: &str,
) {
    card.push_str(&format!(
        "\n## Changes since the reviewed revision for {record_id}\n\n"
    ));
    let body = format!(
        "--- reviewed {previous_digest}\n+++ current {current_digest}\n{}",
        bounded_revision_diff(previous_json, current_json)
    );
    for line in body.lines() {
        card.push_str("    ");
        card.push_str(line);
        card.push('\n');
    }
}

fn append_json_section<T: Serialize>(
    card: &mut String,
    heading: &str,
    value: &T,
) -> Result<(), MkoError> {
    let bytes = canonical_json_bytes(value)?;
    let json = std::str::from_utf8(&bytes)
        .map_err(|error| MkoError::new("review_card_invalid", error.to_string()))?;
    card.push_str(&format!("\n## {heading}\n\n    {json}\n"));
    Ok(())
}

fn read_current_judgments(
    repository_root: &Path,
    target: &ScannedTarget,
) -> Result<Vec<JudgmentAnnotationV2>, MkoError> {
    if target.record_type != ReviewTargetTypeV2::Knowledge {
        return Ok(Vec::new());
    }
    let directory = repository_root
        .join("knowledge")
        .join(&target.record_id)
        .join("judgments");
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(MkoError::new(
                "review_card_judgment_invalid",
                error.to_string(),
            ));
        }
        Ok(_) => validate_real_directory(&directory, "review_card_judgment_invalid")?,
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| MkoError::new("review_card_judgment_invalid", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("review_card_judgment_invalid", error.to_string()))?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_JUDGMENTS_PER_RECORD {
        return Err(MkoError::new(
            "review_card_judgment_limit",
            "judgment scan exceeded its bounded entry limit",
        ));
    }
    let mut judgments = Vec::new();
    for entry in entries {
        let name = entry.file_name().into_string().map_err(|_| {
            MkoError::new(
                "review_card_judgment_invalid",
                "judgment filename must be valid UTF-8",
            )
        })?;
        let id = name.strip_suffix(".md").ok_or_else(|| {
            MkoError::new(
                "review_card_judgment_invalid",
                "judgment directory contains a non-Markdown entry",
            )
        })?;
        if !valid_prefixed_hash(id, "personal-judgment-") {
            return Err(MkoError::new(
                "review_card_judgment_invalid",
                "judgment filename is not a canonical schema-v2 ID",
            ));
        }
        let bytes = read_regular_nofollow(
            &entry.path(),
            MAX_JUDGMENT_FILE_BYTES,
            "review_card_judgment_invalid",
        )?;
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            MkoError::new(
                "review_card_judgment_invalid",
                "judgment record must be UTF-8",
            )
        })?;
        let parsed = parse_markdown::<JudgmentAnnotationV2>(text).map_err(|_| {
            MkoError::new(
                "review_card_judgment_invalid",
                "judgment record is not canonical schema-v2 Markdown",
            )
        })?;
        let annotation = parsed.metadata;
        let canonical = prepare_judgment_v2(
            &annotation.knowledge_id,
            &annotation.knowledge_revision,
            &annotation.text,
            annotation.authorship.clone(),
            annotation.created_at,
        )?;
        if annotation.id != id
            || annotation.knowledge_id != target.record_id
            || canonical.annotation != annotation
            || canonical.markdown != bytes
        {
            return Err(MkoError::new(
                "review_card_judgment_invalid",
                "judgment identity or canonical bytes are inconsistent",
            ));
        }
        if annotation.knowledge_revision == target.pointer.revision {
            judgments.push(annotation);
        }
    }
    Ok(judgments)
}

fn target_type_name(record_type: &ReviewTargetTypeV2) -> &'static str {
    match record_type {
        ReviewTargetTypeV2::Source => "Source",
        ReviewTargetTypeV2::Knowledge => "Knowledge",
    }
}

fn validate_show_id(value: &str) -> Result<(), MkoError> {
    if ["personal-queue-", "personal-source-", "personal-knowledge-"]
        .into_iter()
        .any(|prefix| valid_prefixed_hash(value, prefix))
    {
        Ok(())
    } else {
        Err(MkoError::new(
            "review_card_id_invalid",
            "show requires a canonical queue, Source, or Knowledge stable ID",
        ))
    }
}

fn validate_record_id(record_type: &ReviewTargetTypeV2, record_id: &str) -> Result<(), MkoError> {
    let prefix = match record_type {
        ReviewTargetTypeV2::Source => "personal-source-",
        ReviewTargetTypeV2::Knowledge => "personal-knowledge-",
    };
    if valid_prefixed_hash(record_id, prefix) {
        Ok(())
    } else {
        Err(MkoError::new(
            "queue_record_invalid",
            "record directory name is not a canonical schema-v2 ID",
        ))
    }
}

fn semantic_type(record_type: &ReviewTargetTypeV2) -> SemanticRecordTypeV2 {
    match record_type {
        ReviewTargetTypeV2::Source => SemanticRecordTypeV2::Source,
        ReviewTargetTypeV2::Knowledge => SemanticRecordTypeV2::Knowledge,
    }
}

fn valid_digest(value: &str) -> bool {
    valid_prefixed_hash(value, "sha256:")
}

fn valid_prefixed_hash(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn validate_real_directory(path: &Path, code: &str) -> Result<(), MkoError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| MkoError::new(code, error.to_string()))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            code,
            "managed queue path must be a real non-symlink directory",
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
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > limit
    {
        return Err(MkoError::new(
            code,
            "managed queue input must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(MkoError::new(
            code,
            "managed queue input exceeds its byte limit",
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

fn revision_invalid(message: &str) -> MkoError {
    MkoError::new("queue_revision_invalid", message)
}

fn queue_scan_limit() -> MkoError {
    MkoError::new(
        "queue_scan_limit",
        "record scan exceeded its entry or elapsed-time bound",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model_v2::{KnowledgeResponseV2, KnowledgeUnitV2},
        records_v2::{EvidenceBasisV2, KnowledgeRevisionRecordTypeV2},
    };

    #[test]
    fn revision_diff_marks_changed_lines_with_context() {
        let previous = "a\nb\nc\nd\ne";
        let current = "a\nb\nC\nd\ne";

        let diff = bounded_revision_diff(previous, current);

        assert_eq!(diff, " a\n b\n-c\n+C\n d\n e\n");
    }

    #[test]
    fn revision_diff_keeps_interleaved_changes_exact() {
        let previous = "one\ntwo\nthree\nfour";
        let current = "one\ntwo changed\nthree\nfive";

        let diff = bounded_revision_diff(previous, current);

        assert_eq!(diff, " one\n-two\n+two changed\n three\n-four\n+five\n");
    }

    #[test]
    fn revision_diff_collapses_long_unchanged_runs() {
        let unchanged = (0..40).map(|i| format!("line {i}")).collect::<Vec<_>>();
        let previous = format!("start old\n{}\nend old", unchanged.join("\n"));
        let current = format!("start new\n{}\nend new", unchanged.join("\n"));

        let diff = bounded_revision_diff(&previous, &current);

        assert!(diff.contains("-start old\n+start new\n"));
        assert!(diff.contains("… 34 unchanged lines …"));
        assert!(diff.contains("-end old\n+end new\n"));
        assert!(!diff.contains("line 20"));
    }

    #[test]
    fn revision_diff_is_deterministic_and_byte_bounded() {
        let previous = (0..2_000)
            .map(|i| format!("previous unique line {i} {}", "x".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        let current = (0..2_000)
            .map(|i| format!("current unique line {i} {}", "y".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");

        let first = bounded_revision_diff(&previous, &current);
        let second = bounded_revision_diff(&previous, &current);

        assert_eq!(first, second);
        assert!(first.len() <= MAX_DIFF_SECTION_BYTES + 128);
        assert!(first.contains("… diff truncated:"));
    }

    #[test]
    fn identical_middles_report_no_line_changes() {
        assert_eq!(
            bounded_revision_diff("same", "same"),
            "No line-level changes.\n"
        );
    }

    #[test]
    fn high_risk_policy_gate_requires_counterargument_and_open_question() {
        let response: KnowledgeResponseV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/knowledge-response.json"
        ))
        .unwrap();
        let mut revision = KnowledgeRevisionV2 {
            schema_version: 2,
            record_type: KnowledgeRevisionRecordTypeV2::Knowledge,
            record_id: format!("personal-knowledge-{}", "1".repeat(64)),
            asset_id: format!("personal-asset-{}", "2".repeat(64)),
            asset_fingerprint: format!("sha256:{}", "2".repeat(64)),
            evidence_basis: EvidenceBasisV2 {
                bundle_id: format!("prepared-content-sha256-{}", "3".repeat(64)),
                content_digest: format!("sha256:{}", "3".repeat(64)),
                asset_fingerprint: format!("sha256:{}", "2".repeat(64)),
                extractor_name: "test".into(),
                extractor_version: "1".into(),
            },
            domain_policy: DomainPolicyV2::HighRisk,
            perspectives: Vec::new(),
            response,
        };

        assert!(!knowledge_policy_gate_satisfied(&RevisionV2::Knowledge(
            revision.clone()
        )));

        for unit in [
            serde_json::json!({
                "kind": "counterargument",
                "title": "Alternative",
                "body": "An alternative explanation remains possible.",
                "confidence": "low",
                "basis": "conflicting_evidence",
                "evidence_refs": [],
                "tags": []
            }),
            serde_json::json!({
                "kind": "open_question",
                "title": "Verification",
                "body": "What independent evidence would verify this?",
                "confidence": "low",
                "basis": "missing_evidence",
                "evidence_refs": [],
                "tags": []
            }),
        ] {
            revision
                .response
                .units
                .push(serde_json::from_value::<KnowledgeUnitV2>(unit).unwrap());
        }

        assert!(knowledge_policy_gate_satisfied(&RevisionV2::Knowledge(
            revision
        )));
    }

    #[test]
    fn resurfacing_order_is_deferred_then_least_opened_then_questions_then_recency() {
        let timestamp = |value: &str| value.parse::<DateTime<Utc>>().unwrap();
        let item = |id: &str,
                    state: ResurfacedKnowledgeStateV2,
                    reviewed_at: &str,
                    last_opened_at: Option<&str>,
                    has_open_questions: bool| ResurfacedKnowledgeV2 {
            knowledge_id: id.into(),
            current_revision: format!("sha256:{}", "1".repeat(64)),
            title: id.into(),
            synthesis: "synthesis".into(),
            perspectives: Vec::new(),
            has_open_questions,
            review_state: state,
            reviewed_at: timestamp(reviewed_at),
            last_opened_at: last_opened_at.map(timestamp),
        };
        let mut items = [
            item(
                "approved-recently-opened",
                ResurfacedKnowledgeStateV2::Approved,
                "2026-07-23T05:00:00Z",
                Some("2026-07-23T04:00:00Z"),
                true,
            ),
            item(
                "approved-never-opened-newer",
                ResurfacedKnowledgeStateV2::Approved,
                "2026-07-23T03:00:00Z",
                None,
                true,
            ),
            item(
                "approved-never-opened-older",
                ResurfacedKnowledgeStateV2::Approved,
                "2026-07-23T02:00:00Z",
                None,
                true,
            ),
            item(
                "deferred",
                ResurfacedKnowledgeStateV2::Deferred,
                "2026-07-23T01:00:00Z",
                Some("2026-07-23T06:00:00Z"),
                false,
            ),
        ];

        items.sort_by(compare_resurfaced_knowledge);

        assert_eq!(
            items
                .iter()
                .map(|item| item.knowledge_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "deferred",
                "approved-never-opened-newer",
                "approved-never-opened-older",
                "approved-recently-opened",
            ]
        );
    }
}
