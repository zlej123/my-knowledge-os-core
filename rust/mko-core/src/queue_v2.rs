use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Read,
    path::Path,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use serde::Serialize;

use crate::{
    asset_v2::read_asset_v2,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2},
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
    tags.sort();
    tags.dedup();
    Ok(ProjectionInputV2 {
        record_type,
        id: target.record_id.clone(),
        title: target.revision.title(&target.asset).to_owned(),
        current_revision: target.pointer.revision.clone(),
        review_head_id: history.derived.review_head_id.clone(),
        derived_state: projection_state(state),
        domain: "uncategorized".into(),
        tags,
        record_link: format!("{collection}/{}/current.yaml", target.record_id),
        asset_link: format!("assets/registry/{}.json", target.asset.id),
    })
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
        state,
        domain_policy: match &target.revision {
            RevisionV2::Source(_) => None,
            RevisionV2::Knowledge(revision) => Some(revision.domain_policy.clone()),
        },
        previous_approved_revision: history.previous_approved_revision.clone(),
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
}
