use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    front_matter::render_markdown,
    model_v2::{
        ReviewDecisionV2, ReviewRecordTypeV2, ReviewRecordV2, ReviewTargetTypeV2, ReviewTargetV2,
    },
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, write_projection_v2,
    },
    records_v2::{CurrentPointerV2, EvidenceBasisV2, SemanticRecordTypeV2},
    review_v2::{
        NonTtyReviewDecisionV2, NonTtyReviewRequestV2, NonTtyReviewTargetV2, ReviewDerivedStateV2,
        ReviewPublicationOutcomeV2, ReviewResolutionRequestV2, derive_review_state_v2,
        publish_non_tty_review_v2, publish_review_resolution_v2,
    },
    revision_v2::{canonical_json_bytes, canonical_json_sha256, sha256_digest},
    scaffold_v2::scaffold_personal_kb_v2,
};
use serde_json::json;
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
fn non_tty_dto_mechanically_rejects_approve() {
    let value = json!({
        "targets": [{
            "record_type": "source",
            "record_id": source_id(),
            "displayed_revision": format!("sha256:{}", "a".repeat(64)),
            "expected_review_head_id": null,
            "decision": "approve",
            "feedback": null
        }]
    });

    let error = serde_json::from_value::<NonTtyReviewRequestV2>(value)
        .expect_err("approve must not be representable by a non-TTY request");

    assert!(error.to_string().contains("unknown variant `approve`"));
}

#[test]
fn review_events_are_content_addressed_and_heads_advance_exactly() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let clock = clock("2026-07-22T01:00:00Z");

    let first = publish_non_tty_review_v2(
        root.path(),
        request_changes(&source_id(), &revision, None),
        &clock,
    )
    .unwrap();
    assert_eq!(first.outcome, ReviewPublicationOutcomeV2::Created);
    assert!(first.path.ends_with(format!("{}.md", first.record.id)));
    let first_projection = match &first.projections[0] {
        mko_core::records_v2::RecordProjectionStatusV2::Current(projection) => projection,
        other => panic!("expected current projection, got {other:?}"),
    };
    let first_projection_text = fs::read_to_string(&first_projection.path).unwrap();
    assert!(first_projection_text.contains("derived_state: changes_requested"));
    assert!(first_projection_text.contains(&first.record.id));

    let retry = publish_non_tty_review_v2(
        root.path(),
        request_changes(&source_id(), &revision, None),
        &clock,
    )
    .expect_err("the old null head is stale after the first event");
    assert_eq!(retry.code(), "review_head_stale");

    let changes_requested =
        derive_review_state_v2(root.path(), ReviewTargetTypeV2::Source, &source_id()).unwrap();
    assert_eq!(
        changes_requested.state,
        ReviewDerivedStateV2::ChangesRequested
    );
    assert_eq!(
        changes_requested.review_head_id.as_deref(),
        Some(first.record.id.as_str())
    );

    let deferred = publish_non_tty_review_v2(
        root.path(),
        defer(&source_id(), &revision, Some(first.record.id.clone())),
        &clock,
    )
    .unwrap();
    let deferred_projection = match &deferred.projections[0] {
        mko_core::records_v2::RecordProjectionStatusV2::Current(projection) => projection,
        other => panic!("expected current projection, got {other:?}"),
    };
    let deferred_projection_text = fs::read_to_string(&deferred_projection.path).unwrap();
    assert!(deferred_projection_text.contains("derived_state: deferred"));
    assert!(deferred_projection_text.contains(&deferred.record.id));
    let state =
        derive_review_state_v2(root.path(), ReviewTargetTypeV2::Source, &source_id()).unwrap();
    assert_eq!(state.state, ReviewDerivedStateV2::Deferred);
    assert_eq!(state.review_head_id, Some(deferred.record.id));
    assert_eq!(
        fs::read_dir(root.path().join("reviews")).unwrap().count(),
        2
    );
}

#[test]
fn one_multi_target_event_changes_both_states_atomically() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let knowledge_revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Knowledge,
        &knowledge_id(),
        b"knowledge-v1",
    );
    let request = NonTtyReviewRequestV2 {
        targets: vec![
            NonTtyReviewTargetV2 {
                record_type: ReviewTargetTypeV2::Source,
                record_id: source_id(),
                displayed_revision: source_revision,
                expected_review_head_id: None,
                decision: NonTtyReviewDecisionV2::RequestChanges,
                feedback: Some("Expand the exact limitation.".into()),
            },
            NonTtyReviewTargetV2 {
                record_type: ReviewTargetTypeV2::Knowledge,
                record_id: knowledge_id(),
                displayed_revision: knowledge_revision,
                expected_review_head_id: None,
                decision: NonTtyReviewDecisionV2::Defer,
                feedback: None,
            },
        ],
    };

    let published =
        publish_non_tty_review_v2(root.path(), request, &clock("2026-07-22T02:00:00Z")).unwrap();

    assert_eq!(published.record.targets.len(), 2);
    assert_eq!(
        fs::read_dir(root.path().join("reviews")).unwrap().count(),
        1
    );
    assert_eq!(
        derive_review_state_v2(root.path(), ReviewTargetTypeV2::Source, &source_id())
            .unwrap()
            .state,
        ReviewDerivedStateV2::ChangesRequested
    );
    assert_eq!(
        derive_review_state_v2(root.path(), ReviewTargetTypeV2::Knowledge, &knowledge_id())
            .unwrap()
            .state,
        ReviewDerivedStateV2::Deferred
    );
}

#[test]
fn multiple_unsuperseded_heads_are_a_blocked_conflict() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    seed_review(
        root.path(),
        &revision,
        ReviewDecisionV2::Defer,
        None,
        None,
        clock("2026-07-22T03:00:00Z").now_utc(),
    );
    seed_review(
        root.path(),
        &revision,
        ReviewDecisionV2::RequestChanges,
        Some("Concurrent feedback".into()),
        None,
        clock("2026-07-22T03:01:00Z").now_utc(),
    );

    let state =
        derive_review_state_v2(root.path(), ReviewTargetTypeV2::Source, &source_id()).unwrap();

    assert_eq!(state.state, ReviewDerivedStateV2::BlockedConflict);
    assert_eq!(state.review_head_id, None);
    assert_eq!(state.conflicting_review_head_ids.len(), 2);
    let error = publish_non_tty_review_v2(
        root.path(),
        defer(&source_id(), &revision, None),
        &clock("2026-07-22T03:02:00Z"),
    )
    .expect_err("a conflict cannot be resolved by timestamp ordering");
    assert_eq!(error.code(), "review_head_conflict");
}

#[test]
fn stale_revision_and_missing_exact_revision_are_rejected_before_publication() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let stale = format!("sha256:{}", "0".repeat(64));
    let error = publish_non_tty_review_v2(
        root.path(),
        defer(&source_id(), &stale, None),
        &clock("2026-07-22T04:00:00Z"),
    )
    .expect_err("displayed revision must still be current");
    assert_eq!(error.code(), "review_snapshot_stale");

    let revision_path = root
        .path()
        .join("sources")
        .join(source_id())
        .join("revisions")
        .join(format!("{}.md", revision.replace(':', "-")));
    fs::remove_file(revision_path).unwrap();
    let error = publish_non_tty_review_v2(
        root.path(),
        defer(&source_id(), &revision, None),
        &clock("2026-07-22T04:01:00Z"),
    )
    .expect_err("the pointer alone cannot authorize a review event");
    assert_eq!(error.code(), "review_revision_invalid");
    assert_eq!(
        fs::read_dir(root.path().join("reviews")).unwrap().count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn revision_symlink_is_rejected_without_following() {
    use std::os::unix::fs::symlink;

    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let revision_path = root
        .path()
        .join("sources")
        .join(source_id())
        .join("revisions")
        .join(format!("{}.md", revision.replace(':', "-")));
    let outside = root.path().join("outside-revision");
    fs::write(&outside, b"source-v1").unwrap();
    fs::remove_file(&revision_path).unwrap();
    symlink(&outside, &revision_path).unwrap();

    let error = publish_non_tty_review_v2(
        root.path(),
        defer(&source_id(), &revision, None),
        &clock("2026-07-22T05:00:00Z"),
    )
    .expect_err("revision symlink must fail closed");

    assert_eq!(error.code(), "review_revision_invalid");
    assert_eq!(fs::read(&outside).unwrap(), b"source-v1");
}

#[test]
fn review_resolution_is_deterministic_idempotent_and_bundle_bound() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let requested_revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let review = publish_non_tty_review_v2(
        root.path(),
        request_changes(&source_id(), &requested_revision, None),
        &clock("2026-07-22T06:00:00Z"),
    )
    .unwrap();
    let bundle_id = format!("prepared-content-sha256-{}", "c".repeat(64));
    let resulting_revision = replace_current_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v2",
        &bundle_id,
    );
    let request = resolution_request(
        &review.record.id,
        &requested_revision,
        &resulting_revision,
        &bundle_id,
    );

    let wrong_basis = publish_review_resolution_v2(
        root.path(),
        ReviewResolutionRequestV2 {
            bundle_id: format!("prepared-content-sha256-{}", "d".repeat(64)),
            ..request.clone()
        },
        &clock("2026-07-22T06:01:00Z"),
    )
    .expect_err("resolution must bind the current pointer evidence basis");
    assert_eq!(wrong_basis.code(), "review_resolution_basis_mismatch");

    let first =
        publish_review_resolution_v2(root.path(), request.clone(), &clock("2026-07-22T06:02:00Z"))
            .unwrap();
    let retry =
        publish_review_resolution_v2(root.path(), request.clone(), &clock("2026-07-22T07:00:00Z"))
            .unwrap();

    assert_eq!(first.outcome, ReviewPublicationOutcomeV2::Created);
    assert_eq!(retry.outcome, ReviewPublicationOutcomeV2::Existing);
    assert_eq!(retry.record, first.record);
    assert_eq!(retry.path, first.path);
    assert_eq!(
        fs::read_dir(root.path().join("reviews")).unwrap().count(),
        2
    );

    let mut conflicting = first.record.clone();
    conflicting.bundle_id = format!("prepared-content-sha256-{}", "d".repeat(64));
    fs::write(
        &first.path,
        render_markdown(&conflicting, "# Review resolution\n").unwrap(),
    )
    .unwrap();
    let conflict =
        publish_review_resolution_v2(root.path(), request, &clock("2026-07-22T07:01:00Z"))
            .expect_err("a different body at the deterministic ID must conflict");
    assert_eq!(conflict.code(), "review_resolution_conflict");
}

#[test]
fn review_resolution_rejects_a_superseded_request_changes_head() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let requested_revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let request_changes = publish_non_tty_review_v2(
        root.path(),
        request_changes(&source_id(), &requested_revision, None),
        &clock("2026-07-22T08:00:00Z"),
    )
    .unwrap();
    publish_non_tty_review_v2(
        root.path(),
        defer(
            &source_id(),
            &requested_revision,
            Some(request_changes.record.id.clone()),
        ),
        &clock("2026-07-22T08:01:00Z"),
    )
    .unwrap();
    let bundle_id = format!("prepared-content-sha256-{}", "c".repeat(64));
    let resulting_revision = replace_current_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v2",
        &bundle_id,
    );

    let error = publish_review_resolution_v2(
        root.path(),
        resolution_request(
            &request_changes.record.id,
            &requested_revision,
            &resulting_revision,
            &bundle_id,
        ),
        &clock("2026-07-22T08:02:00Z"),
    )
    .expect_err("only an unsuperseded request_changes head can be resolved");

    assert_eq!(error.code(), "review_resolution_stale");
}

#[test]
fn review_resolution_rejects_missing_noncurrent_and_tampered_resulting_revisions() {
    let (root, review_id, requested_revision) = resolution_environment();
    let bundle_id = format!("prepared-content-sha256-{}", "c".repeat(64));
    let existing_result = write_revision(root.path(), &source_id(), b"source-v2");
    let noncurrent = publish_review_resolution_v2(
        root.path(),
        resolution_request(
            &review_id,
            &requested_revision,
            &existing_result,
            &bundle_id,
        ),
        &clock("2026-07-22T09:00:00Z"),
    )
    .expect_err("an existing but noncurrent result is stale");
    assert_eq!(noncurrent.code(), "review_resolution_stale");

    let missing = format!("sha256:{}", "0".repeat(64));
    set_current(root.path(), &source_id(), &missing, &bundle_id);
    let missing_error = publish_review_resolution_v2(
        root.path(),
        resolution_request(&review_id, &requested_revision, &missing, &bundle_id),
        &clock("2026-07-22T09:01:00Z"),
    )
    .expect_err("a current pointer cannot resolve to a missing revision");
    assert_eq!(missing_error.code(), "review_revision_invalid");

    set_current(root.path(), &source_id(), &existing_result, &bundle_id);
    let revision_path = revision_path(root.path(), &source_id(), &existing_result);
    fs::write(&revision_path, b"tampered-result").unwrap();
    let tampered = publish_review_resolution_v2(
        root.path(),
        resolution_request(
            &review_id,
            &requested_revision,
            &existing_result,
            &bundle_id,
        ),
        &clock("2026-07-22T09:02:00Z"),
    )
    .expect_err("a tampered current immutable revision cannot be resolved");
    assert_eq!(tampered.code(), "review_revision_invalid");
}

#[cfg(unix)]
#[test]
fn review_resolution_rejects_a_resulting_revision_symlink_without_following() {
    use std::os::unix::fs::symlink;

    let (root, review_id, requested_revision) = resolution_environment();
    let bundle_id = format!("prepared-content-sha256-{}", "c".repeat(64));
    let resulting_revision = write_revision(root.path(), &source_id(), b"source-v2");
    set_current(root.path(), &source_id(), &resulting_revision, &bundle_id);
    let path = revision_path(root.path(), &source_id(), &resulting_revision);
    let outside = root.path().join("outside-resolution-revision");
    fs::write(&outside, b"source-v2").unwrap();
    fs::remove_file(&path).unwrap();
    symlink(&outside, &path).unwrap();

    let error = publish_review_resolution_v2(
        root.path(),
        resolution_request(
            &review_id,
            &requested_revision,
            &resulting_revision,
            &bundle_id,
        ),
        &clock("2026-07-22T10:00:00Z"),
    )
    .expect_err("a resolution must not follow the resulting revision symlink");

    assert_eq!(error.code(), "review_revision_invalid");
    assert_eq!(fs::read(outside).unwrap(), b"source-v2");
}

fn seed_target(
    root: &Path,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
    revision_bytes: &[u8],
) -> String {
    let collection = match record_type {
        ReviewTargetTypeV2::Source => "sources",
        ReviewTargetTypeV2::Knowledge => "knowledge",
    };
    let semantic_type = match record_type {
        ReviewTargetTypeV2::Source => SemanticRecordTypeV2::Source,
        ReviewTargetTypeV2::Knowledge => SemanticRecordTypeV2::Knowledge,
    };
    let record_directory = root.join(collection).join(record_id);
    let revisions = record_directory.join("revisions");
    fs::create_dir(&record_directory).unwrap();
    fs::create_dir(&revisions).unwrap();
    let revision = sha256_digest(revision_bytes);
    fs::write(
        revisions.join(format!("{}.md", revision.replace(':', "-"))),
        revision_bytes,
    )
    .unwrap();
    let pointer = CurrentPointerV2 {
        schema_version: 2,
        record_type: semantic_type,
        record_id: record_id.into(),
        revision: revision.clone(),
        evidence_basis: EvidenceBasisV2 {
            bundle_id: format!("prepared-content-sha256-{}", "b".repeat(64)),
            content_digest: format!("sha256:{}", "b".repeat(64)),
            asset_fingerprint: format!("sha256:{}", "a".repeat(64)),
            extractor_name: "test".into(),
            extractor_version: "1".into(),
        },
    };
    fs::write(
        record_directory.join("current.yaml"),
        canonical_json_bytes(&pointer).unwrap(),
    )
    .unwrap();
    let projection_type = match record_type {
        ReviewTargetTypeV2::Source => ProjectionRecordTypeV2::Source,
        ReviewTargetTypeV2::Knowledge => ProjectionRecordTypeV2::Knowledge,
    };
    write_projection_v2(
        root,
        &ProjectionInputV2 {
            record_type: projection_type,
            id: record_id.into(),
            title: record_id.into(),
            current_revision: revision.clone(),
            review_head_id: None,
            derived_state: ProjectionStateV2::Unreviewed,
            domain: "uncategorized".into(),
            perspectives: Vec::new(),
            tags: Vec::new(),
            record_link: format!("{collection}/{record_id}/current.yaml"),
            asset_link: format!("assets/registry/personal-asset-{}.json", "a".repeat(64)),
        },
    )
    .unwrap();
    revision
}

fn resolution_environment() -> (tempfile::TempDir, String, String) {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let requested_revision = seed_target(
        root.path(),
        ReviewTargetTypeV2::Source,
        &source_id(),
        b"source-v1",
    );
    let review = publish_non_tty_review_v2(
        root.path(),
        request_changes(&source_id(), &requested_revision, None),
        &clock("2026-07-22T08:30:00Z"),
    )
    .unwrap();
    (root, review.record.id, requested_revision)
}

fn replace_current_target(
    root: &Path,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
    bytes: &[u8],
    bundle_id: &str,
) -> String {
    assert_eq!(record_type, ReviewTargetTypeV2::Source);
    let revision = write_revision(root, record_id, bytes);
    set_current(root, record_id, &revision, bundle_id);
    revision
}

fn write_revision(root: &Path, record_id: &str, bytes: &[u8]) -> String {
    let revision = sha256_digest(bytes);
    fs::write(revision_path(root, record_id, &revision), bytes).unwrap();
    revision
}

fn set_current(root: &Path, record_id: &str, revision: &str, bundle_id: &str) {
    let pointer = CurrentPointerV2 {
        schema_version: 2,
        record_type: SemanticRecordTypeV2::Source,
        record_id: record_id.into(),
        revision: revision.into(),
        evidence_basis: EvidenceBasisV2 {
            bundle_id: bundle_id.into(),
            content_digest: format!("sha256:{}", "c".repeat(64)),
            asset_fingerprint: format!("sha256:{}", "a".repeat(64)),
            extractor_name: "test".into(),
            extractor_version: "2".into(),
        },
    };
    fs::write(
        root.join("sources").join(record_id).join("current.yaml"),
        canonical_json_bytes(&pointer).unwrap(),
    )
    .unwrap();
}

fn revision_path(root: &Path, record_id: &str, revision: &str) -> std::path::PathBuf {
    root.join("sources")
        .join(record_id)
        .join("revisions")
        .join(format!("{}.md", revision.replace(':', "-")))
}

fn resolution_request(
    review_id: &str,
    requested_revision: &str,
    resulting_revision: &str,
    bundle_id: &str,
) -> ReviewResolutionRequestV2 {
    ReviewResolutionRequestV2 {
        review_id: review_id.into(),
        target_record_id: source_id(),
        requested_revision: requested_revision.into(),
        resulting_revision: resulting_revision.into(),
        bundle_id: bundle_id.into(),
    }
}

fn seed_review(
    root: &Path,
    revision: &str,
    decision: ReviewDecisionV2,
    feedback: Option<String>,
    supersedes_review_id: Option<String>,
    created_at: DateTime<Utc>,
) -> String {
    let targets = vec![ReviewTargetV2 {
        record_type: ReviewTargetTypeV2::Source,
        record_id: source_id(),
        displayed_revision: revision.into(),
        decision,
        feedback,
        supersedes_review_id,
    }];
    let identity = json!({
        "schema_version": 2,
        "record_type": ReviewRecordTypeV2::Review,
        "targets": targets,
        "created_at": created_at,
    });
    let digest = canonical_json_sha256(&identity).unwrap();
    let id = format!(
        "personal-review-{}",
        digest.strip_prefix("sha256:").unwrap()
    );
    let event = ReviewRecordV2 {
        schema_version: 2,
        id: id.clone(),
        record_type: ReviewRecordTypeV2::Review,
        targets,
        created_at,
    };
    fs::write(
        root.join("reviews").join(format!("{id}.md")),
        render_markdown(&event, "# Review event\n").unwrap(),
    )
    .unwrap();
    id
}

fn request_changes(
    record_id: &str,
    revision: &str,
    expected_review_head_id: Option<String>,
) -> NonTtyReviewRequestV2 {
    NonTtyReviewRequestV2 {
        targets: vec![NonTtyReviewTargetV2 {
            record_type: ReviewTargetTypeV2::Source,
            record_id: record_id.into(),
            displayed_revision: revision.into(),
            expected_review_head_id,
            decision: NonTtyReviewDecisionV2::RequestChanges,
            feedback: Some("Clarify the exact limitation.".into()),
        }],
    }
}

fn defer(
    record_id: &str,
    revision: &str,
    expected_review_head_id: Option<String>,
) -> NonTtyReviewRequestV2 {
    NonTtyReviewRequestV2 {
        targets: vec![NonTtyReviewTargetV2 {
            record_type: ReviewTargetTypeV2::Source,
            record_id: record_id.into(),
            displayed_revision: revision.into(),
            expected_review_head_id,
            decision: NonTtyReviewDecisionV2::Defer,
            feedback: None,
        }],
    }
}

fn source_id() -> String {
    format!("personal-source-{}", "1".repeat(64))
}

fn knowledge_id() -> String {
    format!("personal-knowledge-{}", "2".repeat(64))
}

fn clock(timestamp: &str) -> FixedClock {
    FixedClock(timestamp.parse().unwrap())
}
