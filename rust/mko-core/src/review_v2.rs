use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    atomic::{AtomicWriteResult, write_new},
    clock::Clock,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2},
    error::MkoError,
    front_matter::{parse_markdown, render_markdown},
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    model_v2::{
        ReviewDecisionV2, ReviewRecordTypeV2, ReviewRecordV2, ReviewResolutionRecordTypeV2,
        ReviewResolutionV2, ReviewTargetTypeV2, ReviewTargetV2,
    },
    projection_v2::{
        ProjectionRecordTypeV2, ProjectionStateV2, ProjectionWriteOutcomeV2,
        projection_relative_path_v2, read_current_projection_input_v2, render_projection_v2,
        write_projection_locked,
    },
    queue_v2::{RenderedReviewCardV2, ReviewCardTargetStateV2, show_review_card_v2},
    records_v2::{CurrentPointerV2, RecordProjectionStatusV2, SemanticRecordTypeV2},
    revision_v2::{canonical_json_sha256, sha256_digest},
};

const MAX_REVIEW_EVENTS: usize = 4096;
const MAX_REVIEW_EVENT_BYTES: u64 = 1024 * 1024;
const MAX_CURRENT_POINTER_BYTES: u64 = 64 * 1024;
const MAX_REVISION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_FEEDBACK_BYTES: usize = 256 * 1024;
const REVIEW_SCAN_DEADLINE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonTtyReviewDecisionV2 {
    RequestChanges,
    Defer,
}

impl From<NonTtyReviewDecisionV2> for ReviewDecisionV2 {
    fn from(value: NonTtyReviewDecisionV2) -> Self {
        match value {
            NonTtyReviewDecisionV2::RequestChanges => Self::RequestChanges,
            NonTtyReviewDecisionV2::Defer => Self::Defer,
        }
    }
}

/// A revision-bound decision accepted by the non-interactive Core API.
///
/// The decision enum deliberately cannot represent `approve`; serde therefore
/// rejects an attempted `"approve"` before publication code is reached.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NonTtyReviewTargetV2 {
    pub record_type: ReviewTargetTypeV2,
    pub record_id: String,
    pub displayed_revision: String,
    #[serde(default)]
    pub expected_review_head_id: Option<String>,
    pub decision: NonTtyReviewDecisionV2,
    #[serde(default)]
    pub feedback: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NonTtyReviewRequestV2 {
    pub targets: Vec<NonTtyReviewTargetV2>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTargetSnapshotV2 {
    pub record_type: ReviewTargetTypeV2,
    pub record_id: String,
    pub displayed_revision: String,
    pub expected_review_head_id: Option<String>,
}

/// Core-computed effect prepared for direct terminal display.
#[derive(Debug)]
struct TtyApprovalEffectV2 {
    card: RenderedReviewCardV2,
    selection: TtyApprovalSelectionV2,
    effect_digest: String,
    targets: Vec<ReviewTargetSnapshotV2>,
    selected_effects: Vec<SelectedTargetEffectV2>,
    domain_confirmations: Vec<DomainConfirmationV2>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum TtyApprovalSelectionV2 {
    All,
    Record(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SelectedTargetEffectV2 {
    record_id: String,
    displayed_revision: String,
    effects: Vec<String>,
}

/// Per-document classification that a human must explicitly type back before
/// a Knowledge revision can be approved. The value comes from the immutable
/// revision through the Core-rendered card, never from agent text or tags.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DomainConfirmationV2 {
    record_id: String,
    displayed_revision: String,
    domain_policy: DomainPolicyV2,
}

/// Sealed approval authority created only by exact real-TTY confirmation.
///
/// It has no public constructor or serialization, is not a host attestation,
/// and never crosses a process boundary.
#[derive(Debug)]
struct ConfirmedTtyApprovalV2(TtyApprovalEffectV2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewPublicationOutcomeV2 {
    Created,
    Existing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewPublicationV2 {
    pub record: ReviewRecordV2,
    pub path: PathBuf,
    pub outcome: ReviewPublicationOutcomeV2,
    pub projections: Vec<RecordProjectionStatusV2>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewResolutionRequestV2 {
    pub review_id: String,
    pub target_record_id: String,
    pub requested_revision: String,
    pub resulting_revision: String,
    pub bundle_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewResolutionPublicationV2 {
    pub record: ReviewResolutionV2,
    pub path: PathBuf,
    pub outcome: ReviewPublicationOutcomeV2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReviewDerivedStateV2 {
    Unreviewed,
    Deferred,
    ChangesRequested,
    Approved,
    BlockedConflict,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedReviewStateV2 {
    pub record_type: ReviewTargetTypeV2,
    pub record_id: String,
    pub revision: String,
    pub state: ReviewDerivedStateV2,
    pub review_head_id: Option<String>,
    pub conflicting_review_head_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewTargetHistoryV2 {
    pub derived: DerivedReviewStateV2,
    pub previous_reviewed_revision: Option<String>,
    pub previous_approved_revision: Option<String>,
    pub current_feedback: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetKey {
    record_type: ReviewTargetTypeV2,
    record_id: String,
    revision: String,
}

impl Hash for TargetKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.record_type {
            ReviewTargetTypeV2::Source => 0_u8.hash(state),
            ReviewTargetTypeV2::Knowledge => 1_u8.hash(state),
        }
        self.record_id.hash(state);
        self.revision.hash(state);
    }
}

#[derive(Default)]
struct ReviewGraph {
    events: HashMap<String, ReviewRecordV2>,
    resolutions: HashMap<String, ReviewResolutionV2>,
    targets: HashMap<TargetKey, Vec<(String, ReviewTargetV2)>>,
}

pub fn publish_non_tty_review_v2(
    repository_root: &Path,
    request: NonTtyReviewRequestV2,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    let targets = pending_targets_from_request(request)?;
    publish_review(repository_root, targets, clock)
}

pub(crate) fn publish_non_tty_review_locked_v2(
    repository_root: &Path,
    request: NonTtyReviewRequestV2,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    let targets = pending_targets_from_request(request)?;
    publish_review_locked(repository_root, targets, clock)
}

fn pending_targets_from_request(
    request: NonTtyReviewRequestV2,
) -> Result<Vec<PendingTarget>, MkoError> {
    let targets = request
        .targets
        .into_iter()
        .map(|target| {
            validate_feedback(&target.decision, target.feedback.as_deref())?;
            Ok(PendingTarget {
                snapshot: ReviewTargetSnapshotV2 {
                    record_type: target.record_type,
                    record_id: target.record_id,
                    displayed_revision: target.displayed_revision,
                    expected_review_head_id: target.expected_review_head_id,
                },
                decision: target.decision.into(),
                feedback: target.feedback,
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    Ok(targets)
}

pub fn publish_tty_approval_review_v2(
    repository_root: &Path,
    stable_id: &str,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    let mut terminal = ProcessTty;
    publish_tty_approval_with_terminal(repository_root, stable_id, clock, &mut terminal)
}

fn publish_tty_approval_with_terminal(
    repository_root: &Path,
    stable_id: &str,
    clock: &dyn Clock,
    terminal: &mut dyn TtyInteraction,
) -> Result<ReviewPublicationV2, MkoError> {
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 TTY review approval",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    let effect = prepare_tty_approval_locked(repository_root, stable_id)?;
    let confirmed = confirm_tty_approval(effect, terminal)?;
    validate_confirmed_tty_approval_locked(repository_root, &confirmed)?;
    publish_confirmed_tty_approval_locked(repository_root, confirmed, clock)
}

pub fn publish_review_resolution_v2(
    repository_root: &Path,
    request: ReviewResolutionRequestV2,
    clock: &dyn Clock,
) -> Result<ReviewResolutionPublicationV2, MkoError> {
    validate_resolution_request(&request)?;
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 review resolution publish",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    KnowledgeConfigV2::read(repository_root)?;
    let reviews_directory = repository_root.join("reviews");
    validate_real_directory(&reviews_directory, "review_destination_invalid")?;
    let graph = read_review_graph_from(&reviews_directory)?;
    let (record_type, review_target) = resolution_review_target(&graph, &request)?;
    let key = TargetKey {
        record_type: record_type.clone(),
        record_id: request.target_record_id.clone(),
        revision: request.requested_revision.clone(),
    };
    let heads = heads_for(&graph, &key)?;
    if heads.len() != 1 || heads[0] != request.review_id {
        return Err(MkoError::new(
            "review_resolution_stale",
            "the request_changes event is no longer the unsuperseded review head",
        ));
    }
    debug_assert_eq!(review_target.decision, ReviewDecisionV2::RequestChanges);

    let pointer =
        read_and_validate_current(repository_root, &record_type, &request.target_record_id)?;
    if pointer.revision != request.resulting_revision {
        return Err(MkoError::new(
            "review_resolution_stale",
            "the resulting revision is not the exact current target revision",
        ));
    }
    validate_exact_revision(
        repository_root,
        &record_type,
        &request.target_record_id,
        &request.resulting_revision,
    )?;
    if pointer.evidence_basis.bundle_id != request.bundle_id {
        return Err(MkoError::new(
            "review_resolution_basis_mismatch",
            "the requested prepared bundle is not the current revision's evidence basis",
        ));
    }

    let id = review_resolution_id(
        &request.review_id,
        &request.target_record_id,
        &request.resulting_revision,
    )?;
    let path = reviews_directory.join(format!("{id}.md"));
    if let Some(existing) = graph.resolutions.get(&id) {
        if resolution_matches_request(existing, &request) {
            return Ok(ReviewResolutionPublicationV2 {
                record: existing.clone(),
                path,
                outcome: ReviewPublicationOutcomeV2::Existing,
            });
        }
        return Err(MkoError::new(
            "review_resolution_conflict",
            "the deterministic Review resolution ID contains a different body",
        ));
    }

    let record = ReviewResolutionV2 {
        schema_version: 2,
        id,
        record_type: ReviewResolutionRecordTypeV2::ReviewResolution,
        review_id: request.review_id,
        target_record_id: request.target_record_id,
        requested_revision: request.requested_revision,
        resulting_revision: request.resulting_revision,
        bundle_id: request.bundle_id,
        created_at: clock.now_utc(),
    };
    let bytes = render_resolution(&record)?;
    let outcome = write_new(&path, &bytes, |existing| {
        let actual = read_regular_nofollow(
            existing,
            MAX_REVIEW_EVENT_BYTES,
            "review_resolution_invalid",
        )?;
        if actual == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "review_resolution_conflict",
                "the deterministic Review resolution path contains different bytes",
            ))
        }
    })
    .map_err(map_review_atomic_error)?;
    Ok(ReviewResolutionPublicationV2 {
        record,
        path,
        outcome: match outcome {
            AtomicWriteResult::Created => ReviewPublicationOutcomeV2::Created,
            AtomicWriteResult::Existing => ReviewPublicationOutcomeV2::Existing,
        },
    })
}

fn publish_confirmed_tty_approval_locked(
    repository_root: &Path,
    artifact: ConfirmedTtyApprovalV2,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    debug_assert!(
        validate_digest(&artifact.0.effect_digest, "review_effect_digest_invalid").is_ok()
    );
    let targets = artifact
        .0
        .targets
        .into_iter()
        .map(|snapshot| PendingTarget {
            snapshot,
            decision: ReviewDecisionV2::Approve,
            feedback: None,
        })
        .collect();
    publish_review_locked(repository_root, targets, clock)
}

fn prepare_tty_approval_locked(
    repository_root: &Path,
    stable_id: &str,
) -> Result<TtyApprovalEffectV2, MkoError> {
    let card = show_review_card_v2(repository_root, stable_id)?;
    let selection = if stable_id == card.item_id {
        TtyApprovalSelectionV2::All
    } else if card
        .targets
        .iter()
        .any(|target| target.snapshot.record_id == stable_id)
    {
        TtyApprovalSelectionV2::Record(stable_id.to_owned())
    } else {
        return Err(MkoError::new(
            "review_selection_invalid",
            "the approval selection is not an exact target from the canonical review card",
        ));
    };
    let selected = card
        .targets
        .iter()
        .filter(|target| match &selection {
            TtyApprovalSelectionV2::All => true,
            TtyApprovalSelectionV2::Record(record_id) => target.snapshot.record_id == *record_id,
        })
        .collect::<Vec<_>>();
    if selected
        .iter()
        .any(|target| target.state == ReviewCardTargetStateV2::Blocked)
    {
        return Err(MkoError::new(
            "review_target_blocked",
            "the canonical review card contains a blocked target; diagnose it before approval",
        ));
    }
    let targets = selected
        .iter()
        .filter(|target| target.state != ReviewCardTargetStateV2::Approved)
        .map(|target| target.snapshot.clone())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(MkoError::new(
            "review_nothing_to_approve",
            "every target in the canonical review card is already approved",
        ));
    }
    validate_snapshot_targets(&targets)?;
    reject_duplicate_snapshots(&targets)?;
    let selected_effects = selected
        .iter()
        .filter(|target| target.state != ReviewCardTargetStateV2::Approved)
        .map(|target| {
            if !target
                .effects
                .iter()
                .any(|effect| effect == "approve_current_revision_via_tty")
            {
                return Err(MkoError::new(
                    "review_effect_invalid",
                    "the selected canonical card target has no TTY approval effect",
                ));
            }
            Ok(SelectedTargetEffectV2 {
                record_id: target.snapshot.record_id.clone(),
                displayed_revision: target.snapshot.displayed_revision.clone(),
                effects: vec!["approve_current_revision_via_tty".into()],
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    let domain_confirmations = selected
        .iter()
        .filter(|target| target.state != ReviewCardTargetStateV2::Approved)
        .filter_map(|target| {
            target
                .domain_policy
                .clone()
                .map(|domain_policy| DomainConfirmationV2 {
                    record_id: target.snapshot.record_id.clone(),
                    displayed_revision: target.snapshot.displayed_revision.clone(),
                    domain_policy,
                })
        })
        .collect::<Vec<_>>();
    let effect = serde_json::json!({
        "schema_version": 2,
        "operation": "approve",
        "card_digest": card.card_digest,
        "displayed_effect_digest": card.effect_digest,
        "selection": selection,
        "targets": targets,
        "selected_effects": selected_effects,
        "domain_confirmations": domain_confirmations,
    });
    let effect_digest = canonical_json_sha256(&effect)?;
    Ok(TtyApprovalEffectV2 {
        card,
        selection,
        effect_digest,
        targets,
        selected_effects,
        domain_confirmations,
    })
}

fn validate_confirmed_tty_approval_locked(
    repository_root: &Path,
    confirmed: &ConfirmedTtyApprovalV2,
) -> Result<(), MkoError> {
    let stable_id = match &confirmed.0.selection {
        TtyApprovalSelectionV2::All => &confirmed.0.card.item_id,
        TtyApprovalSelectionV2::Record(record_id) => record_id,
    };
    let current = prepare_tty_approval_locked(repository_root, stable_id)
        .map_err(|_| review_snapshot_stale())?;
    if current.card.card_bytes != confirmed.0.card.card_bytes
        || current.card.card_digest != confirmed.0.card.card_digest
        || current.card.effect_digest != confirmed.0.card.effect_digest
        || current.card.targets != confirmed.0.card.targets
        || current.selection != confirmed.0.selection
        || current.targets != confirmed.0.targets
        || current.selected_effects != confirmed.0.selected_effects
        || current.domain_confirmations != confirmed.0.domain_confirmations
        || current.effect_digest != confirmed.0.effect_digest
    {
        return Err(review_snapshot_stale());
    }
    Ok(())
}

fn review_snapshot_stale() -> MkoError {
    MkoError::new(
        "review_snapshot_stale",
        "the canonical review card changed after display; display and confirm the current card again",
    )
}

trait TtyInteraction {
    fn is_real_tty(&self) -> bool;
    fn display(&mut self, bytes: &[u8]) -> std::io::Result<()>;
    fn read_confirmation(&mut self, byte_limit: u64) -> std::io::Result<String>;
}

struct ProcessTty;

impl TtyInteraction for ProcessTty {
    fn is_real_tty(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }

    fn display(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let mut stderr = std::io::stderr().lock();
        stderr.write_all(bytes)?;
        stderr.flush()
    }

    fn read_confirmation(&mut self, byte_limit: u64) -> std::io::Result<String> {
        let stdin = std::io::stdin().lock();
        let mut bounded = BufReader::new(stdin.take(byte_limit));
        let mut input = String::new();
        bounded.read_line(&mut input)?;
        Ok(input)
    }
}

fn confirm_tty_approval(
    effect: TtyApprovalEffectV2,
    terminal: &mut dyn TtyInteraction,
) -> Result<ConfirmedTtyApprovalV2, MkoError> {
    let mut phrase = format!(
        "approve {} {}",
        effect.card.card_digest, effect.effect_digest
    );
    for confirmation in &effect.domain_confirmations {
        write!(
            phrase,
            " confirm-domain {} {} {}",
            confirmation.record_id,
            confirmation.displayed_revision,
            domain_policy_name(&confirmation.domain_policy)
        )
        .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
    }
    let mut display = Vec::with_capacity(effect.card.card_bytes.len() + 2048);
    display.extend_from_slice(b"\nMy Knowledge OS final approval\n\n");
    display.extend_from_slice(&effect.card.card_bytes);
    display.extend_from_slice(b"\n## Exact approval effect\n\n");
    match &effect.selection {
        TtyApprovalSelectionV2::All => {
            display.extend_from_slice(b"Approval selection: all actionable displayed targets\n");
        }
        TtyApprovalSelectionV2::Record(record_id) => {
            writeln!(
                display,
                "Approval selection: selected record only (`{record_id}`)"
            )
            .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
        }
    }
    for target in &effect.targets {
        let selected_effects = effect
            .selected_effects
            .iter()
            .find(|selected| selected.record_id == target.record_id)
            .map(|selected| selected.effects.join(", "))
            .unwrap_or_else(|| "none".into());
        writeln!(
            display,
            "- {:?} `{}` at revision `{}` (review head: `{}`; effects: `{}`)",
            target.record_type,
            target.record_id,
            target.displayed_revision,
            target.expected_review_head_id.as_deref().unwrap_or("none"),
            selected_effects,
        )
        .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
    }
    if !effect.domain_confirmations.is_empty() {
        display.extend_from_slice(b"\n## Required per-document domain confirmation\n\n");
        for confirmation in &effect.domain_confirmations {
            writeln!(
                display,
                "- Knowledge `{}` at revision `{}` is classified `{}`",
                confirmation.record_id,
                confirmation.displayed_revision,
                domain_policy_name(&confirmation.domain_policy)
            )
            .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
        }
        display.extend_from_slice(
            b"The final phrase explicitly confirms each classification. Request changes instead if any classification is wrong.\n",
        );
    }
    write!(
        display,
        "\nEffect: approve every exact target listed above\nCard digest: {}\nDisplayed effect digest: {}\nApproval effect digest: {}\n\nType exactly:\n{}\n> ",
        effect.card.card_digest,
        effect.card.effect_digest,
        effect.effect_digest,
        phrase
    )
    .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
    terminal
        .display(&display)
        .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
    if !terminal.is_real_tty() {
        return Err(MkoError::new(
            "review_tty_required",
            "final approval requires Core-owned display and confirmation on a real TTY",
        ));
    }
    let input = terminal
        .read_confirmation(512)
        .map_err(|error| MkoError::new("review_tty_failed", error.to_string()))?;
    let input = input
        .strip_suffix("\r\n")
        .or_else(|| input.strip_suffix('\n'));
    if input != Some(phrase.as_str()) {
        return Err(MkoError::new(
            "review_confirmation_mismatch",
            "the exact Core-rendered approval phrase was not entered",
        ));
    }
    Ok(ConfirmedTtyApprovalV2(effect))
}

fn domain_policy_name(policy: &DomainPolicyV2) -> &'static str {
    match policy {
        DomainPolicyV2::Standard => "standard",
        DomainPolicyV2::HighRisk => "high_risk",
    }
}

pub fn derive_review_state_v2(
    repository_root: &Path,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
) -> Result<DerivedReviewStateV2, MkoError> {
    validate_target_identity(&record_type, record_id)?;
    KnowledgeConfigV2::read(repository_root)?;
    let pointer = read_and_validate_current(repository_root, &record_type, record_id)?;
    validate_exact_revision(repository_root, &record_type, record_id, &pointer.revision)?;
    let graph = read_review_graph(repository_root)?;
    Ok(derive_state_from_graph(
        &graph,
        record_type,
        record_id,
        &pointer.revision,
    ))
}

pub(crate) fn derive_review_histories_v2(
    repository_root: &Path,
    targets: &[(ReviewTargetTypeV2, String, String)],
) -> Result<Vec<ReviewTargetHistoryV2>, MkoError> {
    KnowledgeConfigV2::read(repository_root)?;
    for (record_type, record_id, revision) in targets {
        validate_target_identity(record_type, record_id)?;
        validate_digest(revision, "review_revision_invalid")?;
    }
    let graph = read_review_graph(repository_root)?;
    targets
        .iter()
        .map(|(record_type, record_id, revision)| {
            review_history_from_graph(&graph, record_type.clone(), record_id, revision)
        })
        .collect()
}

struct PendingTarget {
    snapshot: ReviewTargetSnapshotV2,
    decision: ReviewDecisionV2,
    feedback: Option<String>,
}

fn publish_review(
    repository_root: &Path,
    targets: Vec<PendingTarget>,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    validate_pending_targets(&targets)?;
    let _lock = RepositoryMutationLock::acquire(
        repository_root,
        "v2 review event publish",
        clock,
        StaleRepositoryLockPolicy::Preserve,
    )?;
    publish_review_locked(repository_root, targets, clock)
}

/// Publishes a Review while the caller holds the repository mutation lock.
///
/// This is crate-private so the review-session capability can revalidate the
/// exact displayed card and publish without releasing the lock between those
/// two operations. Public callers must use one of the lock-owning entrypoints.
fn publish_review_locked(
    repository_root: &Path,
    targets: Vec<PendingTarget>,
    clock: &dyn Clock,
) -> Result<ReviewPublicationV2, MkoError> {
    validate_pending_targets(&targets)?;
    KnowledgeConfigV2::read(repository_root)?;
    let reviews_directory = repository_root.join("reviews");
    validate_real_directory(&reviews_directory, "review_destination_invalid")?;
    let graph = read_review_graph_from(&reviews_directory)?;
    let snapshots = targets
        .iter()
        .map(|target| target.snapshot.clone())
        .collect::<Vec<_>>();
    validate_target_snapshots_at_commit(repository_root, &snapshots, &graph)?;

    let mut projection_inputs = targets
        .iter()
        .map(|target| {
            let record_type = match target.snapshot.record_type {
                ReviewTargetTypeV2::Source => ProjectionRecordTypeV2::Source,
                ReviewTargetTypeV2::Knowledge => ProjectionRecordTypeV2::Knowledge,
            };
            let input = read_current_projection_input_v2(
                repository_root,
                record_type,
                &target.snapshot.record_id,
            )?;
            if input.current_revision != target.snapshot.displayed_revision
                || input.review_head_id != target.snapshot.expected_review_head_id
            {
                return Err(MkoError::new(
                    "projection_snapshot_changed",
                    "the displayed target projection is not the exact current review snapshot",
                ));
            }
            Ok(input)
        })
        .collect::<Result<Vec<_>, MkoError>>()?;

    let created_at = clock.now_utc();
    let review_targets = targets
        .into_iter()
        .map(|target| ReviewTargetV2 {
            record_type: target.snapshot.record_type,
            record_id: target.snapshot.record_id,
            displayed_revision: target.snapshot.displayed_revision,
            decision: target.decision,
            feedback: target.feedback,
            supersedes_review_id: target.snapshot.expected_review_head_id,
        })
        .collect::<Vec<_>>();
    let id = review_id(&review_targets, created_at)?;
    for (input, target) in projection_inputs.iter_mut().zip(&review_targets) {
        input.review_head_id = Some(id.clone());
        input.derived_state = match target.decision {
            ReviewDecisionV2::Approve => ProjectionStateV2::Approved,
            ReviewDecisionV2::RequestChanges => ProjectionStateV2::ChangesRequested,
            ReviewDecisionV2::Defer => ProjectionStateV2::Deferred,
        };
        // Complete deterministic rendering before the authoritative Review is
        // published. Later filesystem failures remain derived-view failures.
        let _ = render_projection_v2(input)?;
    }
    let record = ReviewRecordV2 {
        schema_version: 2,
        id: id.clone(),
        record_type: ReviewRecordTypeV2::Review,
        targets: review_targets,
        created_at,
    };
    let bytes = render_review(&record)?;
    let path = reviews_directory.join(format!("{id}.md"));
    let outcome = write_new(&path, &bytes, |existing| {
        let actual =
            read_regular_nofollow(existing, MAX_REVIEW_EVENT_BYTES, "review_event_invalid")?;
        if actual == bytes {
            Ok(())
        } else {
            Err(MkoError::new(
                "review_event_conflict",
                "the content-addressed Review path contains different bytes",
            ))
        }
    })
    .map_err(map_review_atomic_error)?;
    let projections = projection_inputs
        .iter()
        .map(|input| {
            let path = repository_root.join(projection_relative_path_v2(input)?);
            Ok(match write_projection_locked(repository_root, input) {
                Ok(projection)
                    if projection.outcome == ProjectionWriteOutcomeV2::RepairRequired =>
                {
                    RecordProjectionStatusV2::RepairRequired(projection)
                }
                Ok(projection) => RecordProjectionStatusV2::Current(projection),
                Err(error) => RecordProjectionStatusV2::Stale { path, error },
            })
        })
        .collect::<Result<Vec<_>, MkoError>>()?;
    Ok(ReviewPublicationV2 {
        record,
        path,
        outcome: match outcome {
            AtomicWriteResult::Created => ReviewPublicationOutcomeV2::Created,
            AtomicWriteResult::Existing => ReviewPublicationOutcomeV2::Existing,
        },
        projections,
    })
}

fn validate_pending_targets(targets: &[PendingTarget]) -> Result<(), MkoError> {
    if targets.is_empty() || targets.len() > 2 {
        return Err(MkoError::new(
            "review_targets_invalid",
            "a Review event must contain one or two targets",
        ));
    }
    validate_snapshot_targets(
        &targets
            .iter()
            .map(|target| target.snapshot.clone())
            .collect::<Vec<_>>(),
    )?;
    reject_duplicate_snapshots(
        &targets
            .iter()
            .map(|target| target.snapshot.clone())
            .collect::<Vec<_>>(),
    )
}

fn validate_snapshot_targets(targets: &[ReviewTargetSnapshotV2]) -> Result<(), MkoError> {
    if targets.is_empty() || targets.len() > 2 {
        return Err(MkoError::new(
            "review_targets_invalid",
            "a Review event must contain one or two targets",
        ));
    }
    for target in targets {
        validate_target_identity(&target.record_type, &target.record_id)?;
        validate_digest(&target.displayed_revision, "review_revision_invalid")?;
        if let Some(head) = &target.expected_review_head_id {
            validate_prefixed_hash(head, "personal-review-", "review_head_invalid")?;
        }
    }
    Ok(())
}

fn reject_duplicate_snapshots(targets: &[ReviewTargetSnapshotV2]) -> Result<(), MkoError> {
    let mut unique = HashSet::new();
    for target in targets {
        let key = TargetKey {
            record_type: target.record_type.clone(),
            record_id: target.record_id.clone(),
            revision: target.displayed_revision.clone(),
        };
        if !unique.insert(key) {
            return Err(MkoError::new(
                "review_targets_invalid",
                "a Review event cannot contain a duplicate target revision",
            ));
        }
    }
    Ok(())
}

fn validate_target_snapshots_at_commit(
    repository_root: &Path,
    targets: &[ReviewTargetSnapshotV2],
    graph: &ReviewGraph,
) -> Result<(), MkoError> {
    for snapshot in targets {
        let pointer =
            read_and_validate_current(repository_root, &snapshot.record_type, &snapshot.record_id)?;
        if pointer.revision != snapshot.displayed_revision {
            return Err(MkoError::new(
                "review_snapshot_stale",
                "the displayed revision is no longer the target's current revision",
            ));
        }
        validate_exact_revision(
            repository_root,
            &snapshot.record_type,
            &snapshot.record_id,
            &snapshot.displayed_revision,
        )?;
        let heads = heads_for(
            graph,
            &TargetKey {
                record_type: snapshot.record_type.clone(),
                record_id: snapshot.record_id.clone(),
                revision: snapshot.displayed_revision.clone(),
            },
        )?;
        if heads.len() > 1 {
            return Err(MkoError::new(
                "review_head_conflict",
                "the target revision has multiple unsuperseded review heads",
            ));
        }
        if heads.first().map(String::as_str) != snapshot.expected_review_head_id.as_deref() {
            return Err(MkoError::new(
                "review_head_stale",
                "the expected review head is no longer authoritative",
            ));
        }
    }
    Ok(())
}

fn validate_feedback(
    decision: &NonTtyReviewDecisionV2,
    feedback: Option<&str>,
) -> Result<(), MkoError> {
    match decision {
        NonTtyReviewDecisionV2::RequestChanges => {
            let Some(feedback) = feedback else {
                return Err(MkoError::new(
                    "review_feedback_invalid",
                    "request_changes requires explicit feedback",
                ));
            };
            if feedback.trim().is_empty() || feedback.len() > MAX_FEEDBACK_BYTES {
                return Err(MkoError::new(
                    "review_feedback_invalid",
                    "review feedback must contain 1 to 262144 UTF-8 bytes",
                ));
            }
        }
        NonTtyReviewDecisionV2::Defer if feedback.is_some() => {
            return Err(MkoError::new(
                "review_feedback_invalid",
                "defer does not accept feedback",
            ));
        }
        NonTtyReviewDecisionV2::Defer => {}
    }
    Ok(())
}

fn derive_state_from_graph(
    graph: &ReviewGraph,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
    revision: &str,
) -> DerivedReviewStateV2 {
    let key = TargetKey {
        record_type: record_type.clone(),
        record_id: record_id.to_owned(),
        revision: revision.to_owned(),
    };
    let mut heads = heads_for(graph, &key).unwrap_or_default();
    heads.sort();
    if heads.len() > 1 {
        return DerivedReviewStateV2 {
            record_type,
            record_id: record_id.to_owned(),
            revision: revision.to_owned(),
            state: ReviewDerivedStateV2::BlockedConflict,
            review_head_id: None,
            conflicting_review_head_ids: heads,
        };
    }
    let head = heads.first().cloned();
    let state = head
        .as_ref()
        .and_then(|id| graph.events.get(id))
        .and_then(|event| {
            event.targets.iter().find(|target| {
                target.record_type == record_type
                    && target.record_id == record_id
                    && target.displayed_revision == revision
            })
        })
        .map_or(ReviewDerivedStateV2::Unreviewed, |target| {
            match target.decision {
                ReviewDecisionV2::Approve => ReviewDerivedStateV2::Approved,
                ReviewDecisionV2::RequestChanges => ReviewDerivedStateV2::ChangesRequested,
                ReviewDecisionV2::Defer => ReviewDerivedStateV2::Deferred,
            }
        });
    DerivedReviewStateV2 {
        record_type,
        record_id: record_id.to_owned(),
        revision: revision.to_owned(),
        state,
        review_head_id: head,
        conflicting_review_head_ids: Vec::new(),
    }
}

fn review_history_from_graph(
    graph: &ReviewGraph,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
    revision: &str,
) -> Result<ReviewTargetHistoryV2, MkoError> {
    let derived = derive_state_from_graph(graph, record_type.clone(), record_id, revision);
    let current_feedback = derived
        .review_head_id
        .as_deref()
        .and_then(|review_id| review_target(graph, review_id, &record_type, record_id, revision))
        .and_then(|target| target.feedback.clone());

    let mut reviewed = Vec::new();
    let mut approved = Vec::new();
    for key in graph.targets.keys() {
        if key.record_type != record_type || key.record_id != record_id || key.revision == revision
        {
            continue;
        }
        let heads = heads_for(graph, key)?;
        if heads.len() != 1 {
            continue;
        }
        let review_id = &heads[0];
        let Some(event) = graph.events.get(review_id) else {
            continue;
        };
        let Some(target) = review_target(graph, review_id, &record_type, record_id, &key.revision)
        else {
            continue;
        };
        let candidate = (event.created_at, review_id.clone(), key.revision.clone());
        reviewed.push(candidate.clone());
        if target.decision == ReviewDecisionV2::Approve {
            approved.push(candidate);
        }
    }
    reviewed.sort();
    approved.sort();

    Ok(ReviewTargetHistoryV2 {
        derived,
        previous_reviewed_revision: reviewed.pop().map(|(_, _, revision)| revision),
        previous_approved_revision: approved.pop().map(|(_, _, revision)| revision),
        current_feedback,
    })
}

fn review_target<'a>(
    graph: &'a ReviewGraph,
    review_id: &str,
    record_type: &ReviewTargetTypeV2,
    record_id: &str,
    revision: &str,
) -> Option<&'a ReviewTargetV2> {
    graph.events.get(review_id)?.targets.iter().find(|target| {
        &target.record_type == record_type
            && target.record_id == record_id
            && target.displayed_revision == revision
    })
}

fn read_review_graph(repository_root: &Path) -> Result<ReviewGraph, MkoError> {
    read_review_graph_from(&repository_root.join("reviews"))
}

fn read_review_graph_from(reviews_directory: &Path) -> Result<ReviewGraph, MkoError> {
    validate_real_directory(reviews_directory, "review_destination_invalid")?;
    let deadline = Instant::now() + REVIEW_SCAN_DEADLINE;
    let mut entries = fs::read_dir(reviews_directory)
        .map_err(|error| MkoError::new("review_scan_failed", error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MkoError::new("review_scan_failed", error.to_string()))?;
    entries.sort_by_key(|entry| entry.file_name());
    if entries.len() > MAX_REVIEW_EVENTS {
        return Err(review_scan_limit_error());
    }

    let mut graph = ReviewGraph::default();
    for entry in entries {
        if Instant::now() >= deadline {
            return Err(review_scan_limit_error());
        }
        let name = entry.file_name().into_string().map_err(|_| {
            MkoError::new(
                "review_event_invalid",
                "Review filename must be valid UTF-8",
            )
        })?;
        let Some(id) = name.strip_suffix(".md") else {
            return Err(MkoError::new(
                "review_event_invalid",
                "reviews/ may contain only canonical Review event files",
            ));
        };
        let is_resolution = id.starts_with("personal-review-resolution-");
        validate_prefixed_hash(
            id,
            if is_resolution {
                "personal-review-resolution-"
            } else {
                "personal-review-"
            },
            "review_event_invalid",
        )?;
        let bytes = read_regular_nofollow(
            &entry.path(),
            MAX_REVIEW_EVENT_BYTES,
            "review_event_invalid",
        )?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| MkoError::new("review_event_invalid", "Review event must be UTF-8"))?;
        if is_resolution {
            let parsed = parse_markdown::<ReviewResolutionV2>(text).map_err(|_| {
                MkoError::new(
                    "review_resolution_invalid",
                    "Review resolution is not canonical v2 Markdown",
                )
            })?;
            let resolution = parsed.metadata;
            validate_stored_resolution(&resolution, id, &bytes)?;
            if graph
                .resolutions
                .insert(id.to_owned(), resolution)
                .is_some()
            {
                return Err(MkoError::new(
                    "review_resolution_invalid",
                    "duplicate Review resolution identity",
                ));
            }
            continue;
        }
        let parsed = parse_markdown::<ReviewRecordV2>(text).map_err(|_| {
            MkoError::new(
                "review_event_invalid",
                "Review event is not canonical v2 Markdown",
            )
        })?;
        let event = parsed.metadata;
        validate_stored_event(&event, id, &bytes)?;
        if graph.events.insert(id.to_owned(), event.clone()).is_some() {
            return Err(MkoError::new(
                "review_event_invalid",
                "duplicate Review event identity",
            ));
        }
        for target in &event.targets {
            let key = TargetKey {
                record_type: target.record_type.clone(),
                record_id: target.record_id.clone(),
                revision: target.displayed_revision.clone(),
            };
            graph
                .targets
                .entry(key)
                .or_default()
                .push((event.id.clone(), target.clone()));
        }
    }
    validate_review_edges(&graph)?;
    validate_resolution_edges(&graph)?;
    Ok(graph)
}

fn validate_stored_event(
    event: &ReviewRecordV2,
    filename_id: &str,
    actual_bytes: &[u8],
) -> Result<(), MkoError> {
    if event.schema_version != 2
        || event.record_type != ReviewRecordTypeV2::Review
        || event.id != filename_id
        || event.targets.is_empty()
        || event.targets.len() > 2
    {
        return Err(MkoError::new(
            "review_event_invalid",
            "Review event identity or cardinality is invalid",
        ));
    }
    let mut seen = HashSet::new();
    for target in &event.targets {
        validate_target_identity(&target.record_type, &target.record_id)?;
        validate_digest(&target.displayed_revision, "review_event_invalid")?;
        if let Some(head) = &target.supersedes_review_id {
            validate_prefixed_hash(head, "personal-review-", "review_event_invalid")?;
        }
        match target.decision {
            ReviewDecisionV2::RequestChanges => {
                let feedback = target.feedback.as_deref().ok_or_else(|| {
                    MkoError::new("review_event_invalid", "request_changes requires feedback")
                })?;
                if feedback.trim().is_empty() || feedback.len() > MAX_FEEDBACK_BYTES {
                    return Err(MkoError::new(
                        "review_event_invalid",
                        "stored review feedback is invalid",
                    ));
                }
            }
            ReviewDecisionV2::Approve | ReviewDecisionV2::Defer if target.feedback.is_some() => {
                return Err(MkoError::new(
                    "review_event_invalid",
                    "approve and defer events cannot contain feedback",
                ));
            }
            ReviewDecisionV2::Approve | ReviewDecisionV2::Defer => {}
        }
        let key = TargetKey {
            record_type: target.record_type.clone(),
            record_id: target.record_id.clone(),
            revision: target.displayed_revision.clone(),
        };
        if !seen.insert(key) {
            return Err(MkoError::new(
                "review_event_invalid",
                "Review event contains a duplicate target revision",
            ));
        }
    }
    if review_id(&event.targets, event.created_at)? != event.id
        || render_review(event)? != actual_bytes
    {
        return Err(MkoError::new(
            "review_event_invalid",
            "Review event content does not match its content address",
        ));
    }
    Ok(())
}

fn validate_stored_resolution(
    resolution: &ReviewResolutionV2,
    filename_id: &str,
    actual_bytes: &[u8],
) -> Result<(), MkoError> {
    if resolution.schema_version != 2
        || resolution.record_type != ReviewResolutionRecordTypeV2::ReviewResolution
        || resolution.id != filename_id
    {
        return Err(MkoError::new(
            "review_resolution_invalid",
            "Review resolution identity is invalid",
        ));
    }
    validate_prefixed_hash(
        &resolution.review_id,
        "personal-review-",
        "review_resolution_invalid",
    )?;
    validate_record_id_without_type(&resolution.target_record_id, "review_resolution_invalid")?;
    validate_digest(&resolution.requested_revision, "review_resolution_invalid")?;
    validate_digest(&resolution.resulting_revision, "review_resolution_invalid")?;
    validate_prefixed_hash(
        &resolution.bundle_id,
        "prepared-content-sha256-",
        "review_resolution_invalid",
    )?;
    if review_resolution_id(
        &resolution.review_id,
        &resolution.target_record_id,
        &resolution.resulting_revision,
    )? != resolution.id
        || render_resolution(resolution)? != actual_bytes
    {
        return Err(MkoError::new(
            "review_resolution_invalid",
            "Review resolution content does not match its deterministic identity",
        ));
    }
    Ok(())
}

fn validate_review_edges(graph: &ReviewGraph) -> Result<(), MkoError> {
    for (key, targets) in &graph.targets {
        let ids = targets
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<HashSet<_>>();
        for (id, target) in targets {
            if let Some(superseded) = target.supersedes_review_id.as_deref()
                && (!ids.contains(superseded) || superseded == id)
            {
                return Err(MkoError::new(
                    "review_history_invalid",
                    format!(
                        "Review {id} supersedes a missing or mismatched head for {}",
                        key.record_id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_resolution_edges(graph: &ReviewGraph) -> Result<(), MkoError> {
    for resolution in graph.resolutions.values() {
        let event = graph.events.get(&resolution.review_id).ok_or_else(|| {
            MkoError::new(
                "review_resolution_invalid",
                "Review resolution references a missing Review event",
            )
        })?;
        let matching = event.targets.iter().filter(|target| {
            target.record_id == resolution.target_record_id
                && target.displayed_revision == resolution.requested_revision
                && target.decision == ReviewDecisionV2::RequestChanges
        });
        if matching.count() != 1 {
            return Err(MkoError::new(
                "review_resolution_invalid",
                "Review resolution does not reference one request_changes target",
            ));
        }
    }
    Ok(())
}

fn heads_for(graph: &ReviewGraph, key: &TargetKey) -> Result<Vec<String>, MkoError> {
    let Some(targets) = graph.targets.get(key) else {
        return Ok(Vec::new());
    };
    let mut heads = targets
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<HashSet<_>>();
    for (_, target) in targets {
        if let Some(superseded) = &target.supersedes_review_id {
            // Two events may race from the same previously authoritative head
            // outside the cooperating lock protocol (for example after a Git
            // merge). Both children remain unsuperseded heads. This is the
            // explicit blocked-conflict state, not an ordering opportunity.
            heads.remove(superseded);
        }
    }
    Ok(heads.into_iter().collect())
}

fn validate_resolution_request(request: &ReviewResolutionRequestV2) -> Result<(), MkoError> {
    validate_prefixed_hash(
        &request.review_id,
        "personal-review-",
        "review_resolution_invalid",
    )?;
    validate_record_id_without_type(&request.target_record_id, "review_resolution_invalid")?;
    validate_digest(&request.requested_revision, "review_resolution_invalid")?;
    validate_digest(&request.resulting_revision, "review_resolution_invalid")?;
    validate_prefixed_hash(
        &request.bundle_id,
        "prepared-content-sha256-",
        "review_resolution_invalid",
    )
}

fn resolution_review_target<'a>(
    graph: &'a ReviewGraph,
    request: &ReviewResolutionRequestV2,
) -> Result<(ReviewTargetTypeV2, &'a ReviewTargetV2), MkoError> {
    let event = graph.events.get(&request.review_id).ok_or_else(|| {
        MkoError::new(
            "review_resolution_invalid",
            "Review resolution references a missing Review event",
        )
    })?;
    let mut matches = event.targets.iter().filter(|target| {
        target.record_id == request.target_record_id
            && target.displayed_revision == request.requested_revision
            && target.decision == ReviewDecisionV2::RequestChanges
    });
    let target = matches.next().ok_or_else(|| {
        MkoError::new(
            "review_resolution_invalid",
            "Review resolution must reference the exact request_changes target and revision",
        )
    })?;
    if matches.next().is_some() {
        return Err(MkoError::new(
            "review_resolution_invalid",
            "Review resolution target is ambiguous",
        ));
    }
    Ok((target.record_type.clone(), target))
}

fn resolution_matches_request(
    resolution: &ReviewResolutionV2,
    request: &ReviewResolutionRequestV2,
) -> bool {
    resolution.review_id == request.review_id
        && resolution.target_record_id == request.target_record_id
        && resolution.requested_revision == request.requested_revision
        && resolution.resulting_revision == request.resulting_revision
        && resolution.bundle_id == request.bundle_id
}

fn review_resolution_id(
    review_id: &str,
    target_record_id: &str,
    resulting_revision: &str,
) -> Result<String, MkoError> {
    let identity = serde_json::json!({
        "review_id": review_id,
        "target_record_id": target_record_id,
        "resulting_revision": resulting_revision,
    });
    let digest = canonical_json_sha256(&identity)?;
    Ok(format!(
        "personal-review-resolution-{}",
        digest.strip_prefix("sha256:").unwrap_or_default()
    ))
}

fn review_id(targets: &[ReviewTargetV2], created_at: DateTime<Utc>) -> Result<String, MkoError> {
    let identity = serde_json::json!({
        "schema_version": 2,
        "record_type": ReviewRecordTypeV2::Review,
        "targets": targets,
        "created_at": created_at,
    });
    let digest = canonical_json_sha256(&identity)?;
    Ok(format!(
        "personal-review-{}",
        digest.strip_prefix("sha256:").unwrap_or_default()
    ))
}

fn render_review(record: &ReviewRecordV2) -> Result<Vec<u8>, MkoError> {
    render_markdown(record, "# Review event\n")
        .map(String::into_bytes)
        .map_err(|error| MkoError::new("review_event_invalid", error.message()))
}

fn render_resolution(record: &ReviewResolutionV2) -> Result<Vec<u8>, MkoError> {
    render_markdown(record, "# Review resolution\n")
        .map(String::into_bytes)
        .map_err(|error| MkoError::new("review_resolution_invalid", error.message()))
}

fn read_and_validate_current(
    repository_root: &Path,
    record_type: &ReviewTargetTypeV2,
    record_id: &str,
) -> Result<CurrentPointerV2, MkoError> {
    validate_target_identity(record_type, record_id)?;
    let collection = target_collection(record_type);
    let collection_directory = repository_root.join(collection);
    validate_real_directory(&collection_directory, "review_snapshot_invalid")?;
    let record_directory = collection_directory.join(record_id);
    validate_real_directory(&record_directory, "review_snapshot_invalid")?;
    let bytes = read_regular_nofollow(
        &record_directory.join("current.yaml"),
        MAX_CURRENT_POINTER_BYTES,
        "review_snapshot_invalid",
    )?;
    let pointer: CurrentPointerV2 = serde_json::from_slice(&bytes).map_err(|_| {
        MkoError::new(
            "review_snapshot_invalid",
            "target current pointer is not canonical schema-v2 JSON",
        )
    })?;
    let expected_type = match record_type {
        ReviewTargetTypeV2::Source => SemanticRecordTypeV2::Source,
        ReviewTargetTypeV2::Knowledge => SemanticRecordTypeV2::Knowledge,
    };
    if pointer.record_type != expected_type || pointer.record_id != record_id {
        return Err(MkoError::new(
            "review_snapshot_invalid",
            "target current pointer does not identify its containing record",
        ));
    }
    validate_digest(&pointer.revision, "review_snapshot_invalid")?;
    Ok(pointer)
}

fn validate_exact_revision(
    repository_root: &Path,
    record_type: &ReviewTargetTypeV2,
    record_id: &str,
    revision: &str,
) -> Result<(), MkoError> {
    validate_digest(revision, "review_revision_invalid")?;
    let record_directory = repository_root
        .join(target_collection(record_type))
        .join(record_id);
    let revisions = record_directory.join("revisions");
    validate_real_directory(&revisions, "review_revision_invalid")?;
    let path = revisions.join(format!("{}.md", revision.replace(':', "-")));
    let bytes = read_regular_nofollow(&path, MAX_REVISION_BYTES, "review_revision_invalid")?;
    if sha256_digest(&bytes) != revision {
        return Err(MkoError::new(
            "review_revision_invalid",
            "the immutable revision bytes do not match the displayed revision",
        ));
    }
    Ok(())
}

fn validate_target_identity(
    record_type: &ReviewTargetTypeV2,
    record_id: &str,
) -> Result<(), MkoError> {
    let prefix = match record_type {
        ReviewTargetTypeV2::Source => "personal-source-",
        ReviewTargetTypeV2::Knowledge => "personal-knowledge-",
    };
    validate_prefixed_hash(record_id, prefix, "review_target_invalid")
}

fn validate_record_id_without_type(value: &str, code: &str) -> Result<(), MkoError> {
    if value.starts_with("personal-source-") {
        validate_prefixed_hash(value, "personal-source-", code)
    } else if value.starts_with("personal-knowledge-") {
        validate_prefixed_hash(value, "personal-knowledge-", code)
    } else {
        Err(MkoError::new(
            code,
            "expected a canonical Source or Knowledge record ID",
        ))
    }
}

fn target_collection(record_type: &ReviewTargetTypeV2) -> &'static str {
    match record_type {
        ReviewTargetTypeV2::Source => "sources",
        ReviewTargetTypeV2::Knowledge => "knowledge",
    }
}

fn validate_digest(value: &str, code: &str) -> Result<(), MkoError> {
    validate_prefixed_hash(value, "sha256:", code)
}

fn validate_prefixed_hash(value: &str, prefix: &str, code: &str) -> Result<(), MkoError> {
    if value.strip_prefix(prefix).is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }) {
        Ok(())
    } else {
        Err(MkoError::new(
            code,
            "expected a canonical lowercase SHA-256 identity",
        ))
    }
}

fn validate_real_directory(path: &Path, code: &str) -> Result<(), MkoError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| MkoError::new(code, error.to_string()))?;
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(MkoError::new(
            code,
            "managed review path must be a real non-symlink directory",
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
    if !metadata.is_file() || metadata.len() > limit {
        return Err(MkoError::new(
            code,
            "managed review input must be a bounded regular non-link file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| MkoError::new(code, error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(MkoError::new(
            code,
            "managed review input exceeds its byte limit",
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

fn map_review_atomic_error(error: MkoError) -> MkoError {
    let code = match error.code() {
        "registry_destination_invalid" => "review_destination_invalid",
        "registry_locked" => "review_publication_locked",
        _ => return error,
    };
    MkoError::new(code, error.message())
}

fn review_scan_limit_error() -> MkoError {
    MkoError::new(
        "review_scan_limit",
        "Review event scan exceeded its entry or elapsed-time bound",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        judgment_v2::{JudgmentAuthorshipV2, prepare_judgment_v2},
        model_v2::{KnowledgeResponseV2, PreparedContentV2, SourceResponseV2},
        records_v2::{
            AssetRecordV2, RecordWriteResultV2, WriteKnowledgeRecordRequestV2,
            WriteSourceRecordRequestV2, write_knowledge_record_v2, write_source_record_v2,
        },
        revision_v2::canonical_json_bytes,
        scaffold_v2::scaffold_personal_kb_v2,
    };
    use tempfile::tempdir;

    struct FakeTty {
        real: bool,
        input: String,
        displayed: Vec<u8>,
        reads: usize,
    }

    impl TtyInteraction for FakeTty {
        fn is_real_tty(&self) -> bool {
            self.real
        }

        fn display(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.displayed.extend_from_slice(bytes);
            Ok(())
        }

        fn read_confirmation(&mut self, _byte_limit: u64) -> std::io::Result<String> {
            self.reads += 1;
            Ok(self.input.clone())
        }
    }

    struct MutatingTty<F: FnOnce()> {
        displayed: Vec<u8>,
        mutation: Option<F>,
    }

    impl<F: FnOnce()> TtyInteraction for MutatingTty<F> {
        fn is_real_tty(&self) -> bool {
            true
        }

        fn display(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.displayed.extend_from_slice(bytes);
            Ok(())
        }

        fn read_confirmation(&mut self, _byte_limit: u64) -> std::io::Result<String> {
            if let Some(mutation) = self.mutation.take() {
                mutation();
            }
            let displayed = std::str::from_utf8(&self.displayed).unwrap();
            let phrase = displayed
                .split("Type exactly:\n")
                .nth(1)
                .and_then(|remaining| remaining.lines().next())
                .unwrap();
            Ok(format!("{phrase}\n"))
        }
    }

    #[derive(Clone, Copy)]
    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct CombinedEnvironment {
        root: tempfile::TempDir,
        source: RecordWriteResultV2,
        knowledge: RecordWriteResultV2,
        clock: FixedClock,
    }

    #[test]
    fn tty_confirmation_renders_the_full_card_and_binds_both_digests() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let effect = test_effect(digest.clone(), ReviewTargetTypeV2::Source);
        let phrase = format!(
            "approve {} {}",
            effect.card.card_digest, effect.effect_digest
        );
        let mut terminal = FakeTty {
            real: true,
            input: format!("{phrase}\n"),
            displayed: Vec::new(),
            reads: 0,
        };
        let record_id = effect.targets[0].record_id.clone();
        let revision = effect.targets[0].displayed_revision.clone();
        let card_bytes = effect.card.card_bytes.clone();
        let card_digest = effect.card.card_digest.clone();

        confirm_tty_approval(effect, &mut terminal).unwrap();

        let displayed = String::from_utf8(terminal.displayed).unwrap();
        assert!(displayed.contains(std::str::from_utf8(&card_bytes).unwrap()));
        assert!(displayed.contains(&record_id));
        assert!(displayed.contains(&revision));
        assert!(displayed.contains(&card_digest));
        assert!(displayed.contains(&digest));
        assert!(displayed.contains(&phrase));
        assert_eq!(terminal.reads, 1);
    }

    #[test]
    fn tty_confirmation_rejects_wrong_input_and_non_tty_without_reading() {
        let effect = || {
            test_effect(
                format!("sha256:{}", "d".repeat(64)),
                ReviewTargetTypeV2::Knowledge,
            )
        };
        let mut wrong = FakeTty {
            real: true,
            input: "approve\n".into(),
            displayed: Vec::new(),
            reads: 0,
        };
        assert_eq!(
            confirm_tty_approval(effect(), &mut wrong)
                .unwrap_err()
                .code(),
            "review_confirmation_mismatch"
        );

        let mut non_tty = FakeTty {
            real: false,
            input: "unused\n".into(),
            displayed: Vec::new(),
            reads: 0,
        };
        assert_eq!(
            confirm_tty_approval(effect(), &mut non_tty)
                .unwrap_err()
                .code(),
            "review_tty_required"
        );
        assert!(
            String::from_utf8(non_tty.displayed)
                .unwrap()
                .contains("# Review card")
        );
        assert_eq!(non_tty.reads, 0);
    }

    #[test]
    fn knowledge_approval_requires_explicit_revision_bound_domain_confirmation() {
        let effect = test_effect(
            format!("sha256:{}", "d".repeat(64)),
            ReviewTargetTypeV2::Knowledge,
        );
        let confirmation = &effect.domain_confirmations[0];
        let phrase = format!(
            "approve {} {} confirm-domain {} {} standard",
            effect.card.card_digest,
            effect.effect_digest,
            confirmation.record_id,
            confirmation.displayed_revision,
        );
        let mut terminal = FakeTty {
            real: true,
            input: format!("{phrase}\n"),
            displayed: Vec::new(),
            reads: 0,
        };

        confirm_tty_approval(effect, &mut terminal).unwrap();

        let displayed = String::from_utf8(terminal.displayed).unwrap();
        assert!(displayed.contains("Required per-document domain confirmation"));
        assert!(displayed.contains("classified `standard`"));
        assert!(displayed.contains(&phrase));
        assert_eq!(terminal.reads, 1);
    }

    #[test]
    fn source_record_id_selects_only_source_from_the_full_combined_card() {
        let environment = combined_environment();
        let mut terminal = MutatingTty {
            displayed: Vec::new(),
            mutation: Some(|| {}),
        };

        let publication = publish_tty_approval_with_terminal(
            environment.root.path(),
            &environment.source.record_id,
            &environment.clock,
            &mut terminal,
        )
        .unwrap();

        assert_eq!(publication.record.targets.len(), 1);
        assert_eq!(
            publication.record.targets[0].record_id,
            environment.source.record_id
        );
        let displayed = String::from_utf8(terminal.displayed).unwrap();
        assert!(displayed.contains(&environment.knowledge.record_id));
        assert!(displayed.contains("Approval selection: selected record only"));
        assert_eq!(
            derive_review_state_v2(
                environment.root.path(),
                ReviewTargetTypeV2::Source,
                &environment.source.record_id,
            )
            .unwrap()
            .state,
            ReviewDerivedStateV2::Approved
        );
        assert_eq!(
            derive_review_state_v2(
                environment.root.path(),
                ReviewTargetTypeV2::Knowledge,
                &environment.knowledge.record_id,
            )
            .unwrap()
            .state,
            ReviewDerivedStateV2::Unreviewed
        );
    }

    #[test]
    fn knowledge_record_id_selects_only_knowledge_and_its_domain_confirmation() {
        let environment = combined_environment();
        let mut terminal = MutatingTty {
            displayed: Vec::new(),
            mutation: Some(|| {}),
        };

        let publication = publish_tty_approval_with_terminal(
            environment.root.path(),
            &environment.knowledge.record_id,
            &environment.clock,
            &mut terminal,
        )
        .unwrap();

        assert_eq!(publication.record.targets.len(), 1);
        assert_eq!(
            publication.record.targets[0].record_id,
            environment.knowledge.record_id
        );
        let displayed = String::from_utf8(terminal.displayed).unwrap();
        assert!(displayed.contains(&environment.source.record_id));
        assert!(displayed.contains("Required per-document domain confirmation"));
        assert!(displayed.contains("Approval selection: selected record only"));
        assert_eq!(
            derive_review_state_v2(
                environment.root.path(),
                ReviewTargetTypeV2::Source,
                &environment.source.record_id,
            )
            .unwrap()
            .state,
            ReviewDerivedStateV2::Unreviewed
        );
        assert_eq!(
            derive_review_state_v2(
                environment.root.path(),
                ReviewTargetTypeV2::Knowledge,
                &environment.knowledge.record_id,
            )
            .unwrap()
            .state,
            ReviewDerivedStateV2::Approved
        );
    }

    #[test]
    fn queue_item_id_approves_all_actionable_targets_in_one_event() {
        let environment = combined_environment();
        let item_id = show_review_card_v2(environment.root.path(), &environment.source.record_id)
            .unwrap()
            .item_id;
        let mut terminal = MutatingTty {
            displayed: Vec::new(),
            mutation: Some(|| {}),
        };

        let publication = publish_tty_approval_with_terminal(
            environment.root.path(),
            &item_id,
            &environment.clock,
            &mut terminal,
        )
        .unwrap();

        assert_eq!(publication.record.targets.len(), 2);
        assert_eq!(
            publication
                .record
                .targets
                .iter()
                .map(|target| target.record_id.as_str())
                .collect::<Vec<_>>(),
            vec![
                environment.source.record_id.as_str(),
                environment.knowledge.record_id.as_str(),
            ]
        );
        assert!(
            String::from_utf8(terminal.displayed)
                .unwrap()
                .contains("Approval selection: all actionable displayed targets")
        );
    }

    #[test]
    fn invalid_selective_target_never_reaches_confirmation_or_publication() {
        let environment = combined_environment();
        let mut terminal = FakeTty {
            real: true,
            input: String::new(),
            displayed: Vec::new(),
            reads: 0,
        };

        let error = publish_tty_approval_with_terminal(
            environment.root.path(),
            &format!("personal-source-{}", "0".repeat(64)),
            &environment.clock,
            &mut terminal,
        )
        .unwrap_err();

        assert_eq!(error.code(), "review_card_not_found");
        assert!(terminal.displayed.is_empty());
        assert_eq!(terminal.reads, 0);
        assert_eq!(
            fs::read_dir(environment.root.path().join("reviews"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn judgment_added_after_display_makes_the_canonical_card_stale() {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();
        let asset: AssetRecordV2 =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
                .unwrap();
        fs::write(
            root.path()
                .join("assets/registry")
                .join(format!("{}.json", asset.id)),
            canonical_json_bytes(&asset).unwrap(),
        )
        .unwrap();
        let mut bundle: PreparedContentV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/prepared-content.json"
        ))
        .unwrap();
        seal_test_bundle(&mut bundle);
        let response: KnowledgeResponseV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/knowledge-response.json"
        ))
        .unwrap();
        let clock = FixedClock("2026-07-23T12:00:00Z".parse().unwrap());
        let knowledge = write_knowledge_record_v2(
            WriteKnowledgeRecordRequestV2 {
                repository_root: root.path(),
                asset: &asset,
                bundle: &bundle,
                response: &response,
                expected_revision: None,
            },
            &clock,
        )
        .unwrap();
        let judgment = prepare_judgment_v2(
            &knowledge.record_id,
            &knowledge.revision,
            "This exact judgment appeared after display.",
            JudgmentAuthorshipV2::UserConfirmedViaTty,
            clock.now_utc(),
        )
        .unwrap();
        let judgment_directory = root
            .path()
            .join("knowledge")
            .join(&knowledge.record_id)
            .join("judgments");
        let judgment_path = judgment_directory.join(format!("{}.md", judgment.annotation.id));
        let judgment_bytes = judgment.markdown;
        let mut terminal = MutatingTty {
            displayed: Vec::new(),
            mutation: Some(move || {
                fs::create_dir(&judgment_directory).unwrap();
                fs::write(&judgment_path, judgment_bytes).unwrap();
            }),
        };

        let error = publish_tty_approval_with_terminal(
            root.path(),
            &knowledge.record_id,
            &clock,
            &mut terminal,
        )
        .expect_err("a judgment that changes displayed card bytes must invalidate approval");

        assert_eq!(error.code(), "review_snapshot_stale");
        assert_eq!(
            fs::read_dir(root.path().join("reviews")).unwrap().count(),
            0
        );

        let mut current_terminal = MutatingTty {
            displayed: Vec::new(),
            mutation: Some(|| {}),
        };
        let publication = publish_tty_approval_with_terminal(
            root.path(),
            &knowledge.record_id,
            &clock,
            &mut current_terminal,
        )
        .expect("the newly displayed current card remains approvable");
        assert_eq!(publication.record.targets.len(), 1);
        assert_eq!(
            publication.record.targets[0].displayed_revision,
            knowledge.revision
        );
        assert_eq!(
            publication.record.targets[0].decision,
            ReviewDecisionV2::Approve
        );
    }

    fn test_effect(effect_digest: String, record_type: ReviewTargetTypeV2) -> TtyApprovalEffectV2 {
        let record_id = match record_type {
            ReviewTargetTypeV2::Source => format!("personal-source-{}", "b".repeat(64)),
            ReviewTargetTypeV2::Knowledge => format!("personal-knowledge-{}", "e".repeat(64)),
        };
        let domain_policy = match record_type {
            ReviewTargetTypeV2::Source => None,
            ReviewTargetTypeV2::Knowledge => Some(DomainPolicyV2::Standard),
        };
        let snapshot = ReviewTargetSnapshotV2 {
            record_type,
            record_id: record_id.clone(),
            displayed_revision: format!("sha256:{}", "c".repeat(64)),
            expected_review_head_id: None,
        };
        let card_bytes = b"# Review card\n\ncanonical body and exact effects\n".to_vec();
        TtyApprovalEffectV2 {
            card: RenderedReviewCardV2 {
                item_id: format!("personal-queue-{}", "1".repeat(64)),
                asset_id: format!("personal-asset-{}", "2".repeat(64)),
                targets: vec![crate::queue_v2::ReviewCardTargetV2 {
                    snapshot: snapshot.clone(),
                    state: ReviewCardTargetStateV2::Unreviewed,
                    domain_policy: domain_policy.clone(),
                    previous_approved_revision: None,
                    conflicting_review_head_ids: Vec::new(),
                    effects: vec!["approve_current_revision_via_tty".into()],
                }],
                effect_digest: format!("sha256:{}", "3".repeat(64)),
                card_digest: sha256_digest(&card_bytes),
                card_bytes,
            },
            selection: TtyApprovalSelectionV2::All,
            effect_digest,
            targets: vec![snapshot],
            selected_effects: vec![SelectedTargetEffectV2 {
                record_id: record_id.clone(),
                displayed_revision: format!("sha256:{}", "c".repeat(64)),
                effects: vec!["approve_current_revision_via_tty".into()],
            }],
            domain_confirmations: domain_policy
                .map(|domain_policy| DomainConfirmationV2 {
                    record_id: record_id.clone(),
                    displayed_revision: format!("sha256:{}", "c".repeat(64)),
                    domain_policy,
                })
                .into_iter()
                .collect(),
        }
    }

    fn seal_test_bundle(bundle: &mut PreparedContentV2) {
        let mut value = serde_json::to_value(&*bundle).unwrap();
        value.as_object_mut().unwrap().remove("bundle_id");
        value.as_object_mut().unwrap().remove("content_digest");
        let digest = canonical_json_sha256(&value).unwrap();
        bundle.content_digest = digest.clone();
        bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
    }

    fn combined_environment() -> CombinedEnvironment {
        let root = tempdir().unwrap();
        scaffold_personal_kb_v2(root.path()).unwrap();
        let asset: AssetRecordV2 =
            serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
                .unwrap();
        fs::write(
            root.path()
                .join("assets/registry")
                .join(format!("{}.json", asset.id)),
            canonical_json_bytes(&asset).unwrap(),
        )
        .unwrap();
        let mut bundle: PreparedContentV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/prepared-content.json"
        ))
        .unwrap();
        seal_test_bundle(&mut bundle);
        let clock = FixedClock("2026-07-23T13:00:00Z".parse().unwrap());
        let source_response: SourceResponseV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/source-response.json"
        ))
        .unwrap();
        let knowledge_response: KnowledgeResponseV2 = serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/knowledge-response.json"
        ))
        .unwrap();
        let source = write_source_record_v2(
            WriteSourceRecordRequestV2 {
                repository_root: root.path(),
                asset: &asset,
                bundle: &bundle,
                response: &source_response,
                expected_revision: None,
            },
            &clock,
        )
        .unwrap();
        let knowledge = write_knowledge_record_v2(
            WriteKnowledgeRecordRequestV2 {
                repository_root: root.path(),
                asset: &asset,
                bundle: &bundle,
                response: &knowledge_response,
                expected_revision: None,
            },
            &clock,
        )
        .unwrap();
        CombinedEnvironment {
            root,
            source,
            knowledge,
            clock,
        }
    }
}
