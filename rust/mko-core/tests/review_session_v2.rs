use std::fs;

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    model_v2::{PreparedContentV2, SourceResponseV2},
    records_v2::{AssetRecordV2, WriteSourceRecordRequestV2, write_source_record_v2},
    review_session_v2::{
        ReviewSessionDecisionInputV2, ReviewSessionTargetDecisionV2,
        apply_review_session_decision_v2, open_review_session_v2,
    },
    review_v2::{NonTtyReviewDecisionV2, ReviewDerivedStateV2, derive_review_state_v2},
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
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

struct Environment {
    root: tempfile::TempDir,
    asset: AssetRecordV2,
    bundle: PreparedContentV2,
    source: SourceResponseV2,
}

#[test]
fn exact_session_publishes_feedback_once_and_replay_is_consumed() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let opened = open_review_session_v2(
        environment.root.path(),
        &source.record_id,
        &clock("2026-07-23T01:00:00Z"),
    )
    .unwrap();

    assert!(opened.single_use);
    assert_eq!(opened.targets.len(), 1);
    assert_eq!(opened.targets[0].displayed_revision, source.revision);
    let input = request_changes(&opened, "Clarify the exact limitation.");
    let publication = apply_review_session_decision_v2(
        environment.root.path(),
        input.clone(),
        &clock("2026-07-23T01:01:00Z"),
    )
    .unwrap();
    assert_eq!(publication.record.targets.len(), 1);
    assert_eq!(
        publication.record.targets[0].feedback.as_deref(),
        Some("Clarify the exact limitation.")
    );
    assert_eq!(
        derive_review_state_v2(
            environment.root.path(),
            opened.targets[0].record_type.clone(),
            &opened.targets[0].record_id,
        )
        .unwrap()
        .state,
        ReviewDerivedStateV2::ChangesRequested
    );

    let error = apply_review_session_decision_v2(
        environment.root.path(),
        input,
        &clock("2026-07-23T01:02:00Z"),
    )
    .expect_err("a consumed session must never replay");
    assert_eq!(error.code(), "review_session_consumed");
}

#[test]
fn expired_session_has_a_stable_error_and_changes_nothing() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let opened = open_review_session_v2(
        environment.root.path(),
        &source.record_id,
        &clock("2026-07-23T02:00:00Z"),
    )
    .unwrap();
    let input = defer(&opened);

    for timestamp in ["2026-07-23T02:15:00Z", "2026-07-24T00:00:00Z"] {
        let error = apply_review_session_decision_v2(
            environment.root.path(),
            input.clone(),
            &clock(timestamp),
        )
        .expect_err("expired sessions remain expired");
        assert_eq!(error.code(), "review_session_expired");
    }
    assert_eq!(
        fs::read_dir(environment.root.path().join("reviews"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn changed_revision_or_card_digest_is_rejected_as_stale() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let opened = open_review_session_v2(
        environment.root.path(),
        &source.record_id,
        &clock("2026-07-23T03:00:00Z"),
    )
    .unwrap();

    let mut forged_digest = defer(&opened);
    forged_digest.card_digest = format!("sha256:{}", "0".repeat(64));
    let error = apply_review_session_decision_v2(
        environment.root.path(),
        forged_digest,
        &clock("2026-07-23T03:01:00Z"),
    )
    .expect_err("caller-provided card digest cannot replace the displayed card");
    assert_eq!(error.code(), "review_snapshot_stale");

    let mut revised = environment.source.clone();
    revised.general_summary = "A new exact current summary.".into();
    write_source(
        &environment,
        &environment.bundle,
        &revised,
        Some(&source.revision),
    );
    let error = apply_review_session_decision_v2(
        environment.root.path(),
        defer(&opened),
        &clock("2026-07-23T03:02:00Z"),
    )
    .expect_err("a changed current revision invalidates the display session");
    assert_eq!(error.code(), "review_snapshot_stale");
}

#[test]
fn decision_dto_cannot_encode_approve() {
    let value = json!({
        "session_id": format!("mko-review-session-{}", "a".repeat(64)),
        "card_digest": format!("sha256:{}", "b".repeat(64)),
        "target_decisions": [{
            "record_id": format!("personal-source-{}", "c".repeat(64)),
            "decision": "approve",
            "feedback": null
        }]
    });

    let error = serde_json::from_value::<ReviewSessionDecisionInputV2>(value)
        .expect_err("non-TTY session input must not represent approve");
    assert!(error.to_string().contains("unknown variant `approve`"));
}

#[cfg(unix)]
#[test]
fn session_file_symlink_is_not_followed() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let opened = open_review_session_v2(
        environment.root.path(),
        &source.record_id,
        &clock("2026-07-23T04:00:00Z"),
    )
    .unwrap();
    let open_directory = environment
        .root
        .path()
        .join(".mko/runtime/review-sessions/open");
    assert_eq!(
        fs::metadata(&open_directory).unwrap().permissions().mode() & 0o077,
        0
    );
    let session_path = open_directory.join(format!("{}.json", opened.session_id));
    assert_eq!(
        fs::metadata(&session_path).unwrap().permissions().mode() & 0o077,
        0
    );
    let outside = environment.root.path().join("outside-session.json");
    fs::write(&outside, b"not a session").unwrap();
    fs::remove_file(&session_path).unwrap();
    symlink(&outside, &session_path).unwrap();

    let error = apply_review_session_decision_v2(
        environment.root.path(),
        defer(&opened),
        &clock("2026-07-23T04:01:00Z"),
    )
    .expect_err("session symlink must not be followed");
    assert_eq!(error.code(), "review_session_invalid");
    assert_eq!(fs::read(&outside).unwrap(), b"not a session");
}

#[cfg(unix)]
#[test]
fn session_directory_symlink_is_rejected_before_writing_outside() {
    use std::os::unix::fs::symlink;

    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let runtime = environment.root.path().join(".mko/runtime");
    let outside = environment.root.path().join("outside-sessions");
    fs::create_dir(&runtime).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, runtime.join("review-sessions")).unwrap();

    let error = open_review_session_v2(
        environment.root.path(),
        &source.record_id,
        &clock("2026-07-23T05:00:00Z"),
    )
    .expect_err("runtime symlink must be rejected");
    assert_eq!(error.code(), "review_session_invalid");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
}

fn request_changes(
    opened: &mko_core::review_session_v2::ReviewOpenDataV2,
    feedback: &str,
) -> ReviewSessionDecisionInputV2 {
    ReviewSessionDecisionInputV2 {
        session_id: opened.session_id.clone(),
        card_digest: opened.card_digest.clone(),
        target_decisions: vec![ReviewSessionTargetDecisionV2 {
            record_id: opened.targets[0].record_id.clone(),
            decision: NonTtyReviewDecisionV2::RequestChanges,
            feedback: Some(feedback.into()),
        }],
    }
}

fn defer(opened: &mko_core::review_session_v2::ReviewOpenDataV2) -> ReviewSessionDecisionInputV2 {
    ReviewSessionDecisionInputV2 {
        session_id: opened.session_id.clone(),
        card_digest: opened.card_digest.clone(),
        target_decisions: vec![ReviewSessionTargetDecisionV2 {
            record_id: opened.targets[0].record_id.clone(),
            decision: NonTtyReviewDecisionV2::Defer,
            feedback: None,
        }],
    }
}

fn environment() -> Environment {
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
    seal_bundle(&mut bundle);
    Environment {
        root,
        asset,
        bundle,
        source: serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/source-response.json"
        ))
        .unwrap(),
    }
}

fn write_source(
    environment: &Environment,
    bundle: &PreparedContentV2,
    response: &SourceResponseV2,
    expected_revision: Option<&str>,
) -> mko_core::records_v2::RecordWriteResultV2 {
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle,
            response,
            expected_revision,
        },
        &clock("2026-07-23T00:00:00Z"),
    )
    .unwrap()
}

fn seal_bundle(bundle: &mut PreparedContentV2) {
    let mut value = serde_json::to_value(&*bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
}

fn clock(timestamp: &str) -> FixedClock {
    FixedClock(timestamp.parse().unwrap())
}
