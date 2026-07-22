use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    front_matter::render_markdown,
    json_v2::{QueueItemStateV2, QueueItemTypeV2, QueueNextActionV2},
    model_v2::{
        KnowledgeResponseV2, PreparedContentV2, ReviewDecisionV2, ReviewRecordTypeV2,
        ReviewRecordV2, ReviewTargetTypeV2, ReviewTargetV2, SourceResponseV2,
    },
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, write_projection_v2,
    },
    queue_v2::{ReviewCardTargetStateV2, derive_queue_v2, show_review_card_v2},
    records_v2::{
        AssetRecordV2, WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2,
        write_knowledge_record_v2, write_source_record_v2,
    },
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
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
    knowledge: KnowledgeResponseV2,
}

#[test]
fn approved_records_are_excluded_from_the_default_queue_but_remain_showable() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::Approve,
        None,
        None,
        "2026-07-23T01:00:00Z",
    );
    sync_projection(
        &environment,
        &source,
        Some(review_id),
        ProjectionStateV2::Approved,
    );

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert!(queue.items.is_empty());
    assert!(queue.scan_complete);
    assert_eq!(queue.remaining, 0);
    assert_eq!(queue.next_cursor, None);

    let card = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    assert_eq!(card.targets.len(), 1);
    assert_eq!(card.targets[0].state, ReviewCardTargetStateV2::Approved);
    assert!(
        String::from_utf8(card.card_bytes)
            .unwrap()
            .contains("State: `approved`")
    );
}

#[test]
fn request_changes_and_deferred_targets_derive_one_combined_queue_item() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let knowledge = write_knowledge(&environment);
    let source_review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::RequestChanges,
        Some("Clarify the limitation."),
        None,
        "2026-07-23T02:00:00Z",
    );
    let knowledge_review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Knowledge,
        &knowledge.record_id,
        &knowledge.revision,
        ReviewDecisionV2::Defer,
        None,
        None,
        "2026-07-23T02:01:00Z",
    );
    sync_projection(
        &environment,
        &source,
        Some(source_review_id),
        ProjectionStateV2::ChangesRequested,
    );
    sync_projection(
        &environment,
        &knowledge,
        Some(knowledge_review_id),
        ProjectionStateV2::Deferred,
    );

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert_eq!(queue.items.len(), 1);
    let item = &queue.items[0];
    assert_eq!(item.item_type, QueueItemTypeV2::Combined);
    assert_eq!(item.state, QueueItemStateV2::ChangesRequested);
    assert_eq!(item.next_action, QueueNextActionV2::Regenerate);
    assert_eq!(item.target_ids, vec![source.record_id, knowledge.record_id]);
}

#[test]
fn concurrent_review_heads_are_a_blocked_queue_item_and_card() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::Defer,
        None,
        None,
        "2026-07-23T03:00:00Z",
    );
    seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::RequestChanges,
        Some("Concurrent feedback."),
        None,
        "2026-07-23T03:01:00Z",
    );

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert_eq!(queue.items[0].state, QueueItemStateV2::Blocked);
    assert_eq!(queue.items[0].next_action, QueueNextActionV2::Diagnose);
    let card = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    assert_eq!(card.targets[0].state, ReviewCardTargetStateV2::Blocked);
    assert_eq!(card.targets[0].conflicting_review_head_ids.len(), 2);
    assert_eq!(card.targets[0].effects, vec!["diagnose"]);
}

#[test]
fn source_and_knowledge_share_one_full_canonical_card_with_a_stable_digest() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let knowledge = write_knowledge(&environment);

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].item_type, QueueItemTypeV2::Combined);
    let first = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    let second = show_review_card_v2(environment.root.path(), &queue.items[0].item_id).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        first.card_digest,
        mko_core::revision_v2::sha256_digest(&first.card_bytes)
    );
    assert_eq!(first.targets.len(), 2);
    assert_eq!(first.targets[0].snapshot.record_id, source.record_id);
    assert_eq!(first.targets[1].snapshot.record_id, knowledge.record_id);
    let text = String::from_utf8(first.card_bytes).unwrap();
    assert!(text.contains("Source-grounded content"));
    assert!(text.contains("Knowledge analysis"));
    assert!(text.contains(&environment.source.general_summary));
    assert!(text.contains(&environment.knowledge.synthesis));
    assert!(text.contains(&first.effect_digest));
}

#[test]
fn changed_pointer_changes_card_digest_and_historical_evidence_basis_remains_readable() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::Approve,
        None,
        None,
        "2026-07-23T04:00:00Z",
    );
    let approved = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();

    let mut changed_bundle = environment.bundle.clone();
    changed_bundle.extractor.version = "2.0.0".into();
    seal_bundle(&mut changed_bundle);
    let mut changed_source = environment.source.clone();
    changed_source.general_summary = "A revised exact summary.".into();
    let changed = write_source(
        &environment,
        &changed_bundle,
        &changed_source,
        Some(&source.revision),
    );
    let revised = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();

    assert_ne!(approved.card_digest, revised.card_digest);
    assert_eq!(
        revised.targets[0].snapshot.displayed_revision,
        changed.revision
    );
    assert_eq!(
        revised.targets[0].previous_approved_revision.as_deref(),
        Some(source.revision.as_str())
    );
    assert_eq!(
        revised.targets[0].state,
        ReviewCardTargetStateV2::RevisedUnreviewed
    );
    let text = String::from_utf8(revised.card_bytes).unwrap();
    assert!(text.contains("Previous reviewed content"));
    assert!(text.contains(&environment.source.general_summary));
    assert!(text.contains(&changed_source.general_summary));
}

#[test]
fn missing_projection_blocks_the_queue() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let projection = match &source.projection {
        mko_core::records_v2::RecordProjectionStatusV2::Current(projection) => projection,
        other => panic!("expected current projection, got {other:?}"),
    };
    fs::remove_file(&projection.path).unwrap();

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert_eq!(queue.items[0].state, QueueItemStateV2::Blocked);
    assert_eq!(queue.items[0].next_action, QueueNextActionV2::Diagnose);
}

fn sync_projection(
    environment: &Environment,
    record: &mko_core::records_v2::RecordWriteResultV2,
    review_head_id: Option<String>,
    derived_state: ProjectionStateV2,
) {
    let is_source = record.record_id.starts_with("personal-source-");
    write_projection_v2(
        environment.root.path(),
        &ProjectionInputV2 {
            record_type: if is_source {
                ProjectionRecordTypeV2::Source
            } else {
                ProjectionRecordTypeV2::Knowledge
            },
            id: record.record_id.clone(),
            title: if is_source {
                environment.source.title.clone()
            } else {
                environment.asset.title_fallback.clone()
            },
            current_revision: record.revision.clone(),
            review_head_id,
            derived_state,
            domain: "uncategorized".into(),
            tags: if is_source {
                environment.source.tags.clone()
            } else {
                environment
                    .knowledge
                    .units
                    .iter()
                    .flat_map(|unit| unit.tags.iter().cloned())
                    .collect()
            },
            record_link: format!(
                "{}/{}/current.yaml",
                if is_source { "sources" } else { "knowledge" },
                record.record_id
            ),
            asset_link: format!("assets/registry/{}.json", environment.asset.id),
        },
    )
    .unwrap();
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
        knowledge: serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/knowledge-response.json"
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

fn write_knowledge(environment: &Environment) -> mko_core::records_v2::RecordWriteResultV2 {
    write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle: &environment.bundle,
            response: &environment.knowledge,
            expected_revision: None,
        },
        &clock("2026-07-23T00:00:00Z"),
    )
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn seed_review(
    root: &Path,
    record_type: ReviewTargetTypeV2,
    record_id: &str,
    revision: &str,
    decision: ReviewDecisionV2,
    feedback: Option<&str>,
    supersedes_review_id: Option<String>,
    created_at: &str,
) -> String {
    let targets = vec![ReviewTargetV2 {
        record_type,
        record_id: record_id.into(),
        displayed_revision: revision.into(),
        decision,
        feedback: feedback.map(str::to_owned),
        supersedes_review_id,
    }];
    let created_at: DateTime<Utc> = created_at.parse().unwrap();
    let identity = serde_json::json!({
        "schema_version": 2,
        "record_type": ReviewRecordTypeV2::Review,
        "targets": targets,
        "created_at": created_at,
    });
    let digest = canonical_json_sha256(&identity).unwrap();
    let id = format!("personal-review-{}", digest.trim_start_matches("sha256:"));
    let record = ReviewRecordV2 {
        schema_version: 2,
        id: id.clone(),
        record_type: ReviewRecordTypeV2::Review,
        targets,
        created_at,
    };
    fs::write(
        root.join("reviews").join(format!("{id}.md")),
        render_markdown(&record, "# Review event\n").unwrap(),
    )
    .unwrap();
    id
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
