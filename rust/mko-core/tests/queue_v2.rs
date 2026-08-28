use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    config_v2::{DomainPolicyV2, PerspectiveV2},
    front_matter::render_markdown,
    json_v2::{QueueItemStateV2, QueueItemTypeV2, QueueNextActionV2},
    model_v2::{
        KnowledgeResponseV2, PreparedContentV2, ReviewDecisionV2, ReviewRecordTypeV2,
        ReviewRecordV2, ReviewTargetTypeV2, ReviewTargetV2, SourceResponseV2,
    },
    perspective_v2::{prepare_perspective_confirmation_v2, publish_perspective_confirmation_v2},
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, write_projection_v2,
    },
    queue_v2::{
        ConfirmationLabelV2, ResurfacedKnowledgeStateV2, ReviewCardTargetStateV2,
        SearchConfirmationFilterV2, SearchLayerV2, SearchRecordTypeV2, derive_queue_v2,
        resurface_confirmed_knowledge_by_perspective_v2, resurface_confirmed_knowledge_v2,
        resurface_knowledge_by_perspective_v2, search_records_by_perspective_v2, search_records_v2,
        show_review_card_v2, summarize_home_queue_v2,
    },
    records_v2::{
        AssetRecordV2, WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2,
        read_current_knowledge_revision_v2, write_knowledge_record_v2, write_source_record_v2,
    },
    resurface_history_v2::record_resurfaced_knowledge_open_v2,
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;
use unicode_normalization::UnicodeNormalization;

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
        ProjectionStateV2::Confirmed,
    );

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert!(queue.items.is_empty());
    assert!(queue.scan_complete);
    assert_eq!(queue.remaining, 0);
    assert_eq!(queue.next_cursor, None);

    let card = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    assert_eq!(card.targets.len(), 1);
    assert_eq!(card.targets[0].state, ReviewCardTargetStateV2::Confirmed);
    assert!(
        String::from_utf8(card.card_bytes)
            .unwrap()
            .contains("State: `confirmed`")
    );
}

#[test]
fn search_includes_unconfirmed_knowledge_labelled() {
    let environment = environment();
    let knowledge = write_knowledge(&environment);

    let before = search_records_v2(environment.root.path(), "reported").unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].record_type, SearchRecordTypeV2::Knowledge);
    assert_eq!(before[0].confirmation, ConfirmationLabelV2::Unconfirmed);
    assert_eq!(
        summarize_home_queue_v2(environment.root.path())
            .unwrap()
            .review_pending,
        1
    );

    let review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Knowledge,
        &knowledge.record_id,
        &knowledge.revision,
        ReviewDecisionV2::Approve,
        None,
        None,
        "2026-07-23T01:00:00Z",
    );
    sync_projection(
        &environment,
        &knowledge,
        Some(review_id),
        ProjectionStateV2::Confirmed,
    );

    let matches = search_records_v2(environment.root.path(), "reported").unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].title, "Reported result");
    assert_eq!(matches[0].current_revision, knowledge.revision);
    assert_eq!(
        matches[0].confirmation,
        ConfirmationLabelV2::Confirmed {
            at: "2026-07-23T01:00:00Z".parse().unwrap()
        }
    );
    // An excerpt is only useful if it leads somewhere: the path a result points
    // at must be the readable document that actually exists.
    let readable =
        environment
            .root
            .path()
            .join(mko_core::projection_v2::record_projection_relative_path_v2(
                mko_core::projection_v2::ProjectionRecordTypeV2::Knowledge,
                &matches[0].record_id,
            ));
    assert!(
        readable.is_file(),
        "missing readable document: {readable:?}"
    );
    let document = fs::read_to_string(&readable).unwrap();
    assert!(document.contains(&matches[0].title));
    let summary = summarize_home_queue_v2(environment.root.path()).unwrap();
    assert_eq!(summary.review_pending, 0);
    assert_eq!(summary.confirmed_knowledge, 1);
}

#[test]
fn search_confirmed_only_filter_excludes_unconfirmed() {
    let environment = environment();
    write_knowledge(&environment);

    assert!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "reported",
            None,
            SearchConfirmationFilterV2::ConfirmedOnly,
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "reported",
            None,
            SearchConfirmationFilterV2::UnconfirmedOnly,
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn search_normalizes_nfd_query_against_nfc_stored_text() {
    let mut environment = environment();
    environment.knowledge.units[0].title = "학습".into();
    environment.knowledge.units[0].body = "학습률을 크게 개선하는 방법을 설명한다.".into();
    write_knowledge(&environment);

    let nfd_query: String = "학습률 개선".nfd().collect();
    // The literal above may already be NFC on this toolchain; force genuine
    // NFD so the assertion below actually exercises the bug this fixes
    // (D10) rather than passing by accident.
    assert_ne!(nfd_query, "학습률 개선".nfc().collect::<String>());

    let matches = search_records_v2(environment.root.path(), &nfd_query).unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].title, "학습");
}

#[test]
fn search_requires_all_whitespace_tokens_to_match() {
    let environment = environment();
    write_knowledge(&environment);

    // "Reported result" / "The document reports an example result." — both
    // tokens are present but nowhere adjacent, so substring-of-the-whole-
    // query matching would have missed this; token-AND must not.
    let matches = search_records_v2(environment.root.path(), "reported example").unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].title, "Reported result");

    assert!(
        search_records_v2(environment.root.path(), "reported nonexistentword")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn search_matches_source_records() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);

    let matches = search_records_v2(environment.root.path(), "reported example").unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].record_type, SearchRecordTypeV2::Source);
    assert_eq!(matches[0].record_id, source.record_id);
    assert_eq!(matches[0].layer, SearchLayerV2::SourceOwnWords);
    assert_eq!(matches[0].confirmation, ConfirmationLabelV2::Unconfirmed);

    // `perspective` is a human-confirmed Knowledge-only concept (§6.3): a
    // perspective filter must never match a Source hit.
    assert!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "reported example",
            Some(PerspectiveV2::Technical),
            SearchConfirmationFilterV2::Any,
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn search_tag_and_layer_filters_narrow_results() {
    let environment = environment();
    write_knowledge(&environment);

    let by_tag = search_records_by_perspective_v2(
        environment.root.path(),
        "result",
        None,
        SearchConfirmationFilterV2::Any,
        Some("example"),
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(by_tag.len(), 1);
    assert_eq!(by_tag[0].title, "Reported result");
    assert!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "result",
            None,
            SearchConfirmationFilterV2::Any,
            Some("nonexistent-tag"),
            None,
            None,
            None,
        )
        .unwrap()
        .is_empty()
    );

    let by_layer = search_records_by_perspective_v2(
        environment.root.path(),
        "document",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        Some(SearchLayerV2::CounterargumentOrUncertainty),
        None,
        None,
    )
    .unwrap();
    assert_eq!(by_layer.len(), 1);
    assert_eq!(by_layer[0].title, "External validity");
}

#[test]
fn confirmed_perspective_is_searchable_and_resurfacing_prioritizes_open_questions() {
    let environment = environment();
    let knowledge = write_knowledge(&environment);
    let prepared = prepare_perspective_confirmation_v2(
        environment.root.path(),
        &knowledge.record_id,
        vec![PerspectiveV2::Technical],
    )
    .unwrap();
    let replacement = publish_perspective_confirmation_v2(
        environment.root.path(),
        &prepared,
        &prepared.confirmation_phrase,
        &clock("2026-07-23T00:30:00Z"),
    )
    .unwrap();
    let review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Knowledge,
        &replacement.record_id,
        &replacement.revision,
        ReviewDecisionV2::Approve,
        None,
        None,
        "2026-07-23T01:00:00Z",
    );
    sync_projection(
        &environment,
        &replacement,
        Some(review_id),
        ProjectionStateV2::Confirmed,
    );

    let matches = search_records_v2(environment.root.path(), "technical").unwrap();
    assert_eq!(matches.len(), environment.knowledge.units.len());
    assert!(
        matches
            .iter()
            .all(|item| item.perspectives == vec![PerspectiveV2::Technical])
    );
    assert!(
        matches
            .iter()
            .all(|item| matches!(item.confirmation, ConfirmationLabelV2::Confirmed { .. }))
    );
    assert_eq!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "reported",
            Some(PerspectiveV2::Technical),
            SearchConfirmationFilterV2::Any,
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        search_records_by_perspective_v2(
            environment.root.path(),
            "reported",
            Some(PerspectiveV2::Investment),
            SearchConfirmationFilterV2::Any,
            None,
            None,
            None,
            None,
        )
        .unwrap()
        .is_empty()
    );
    let resurfaced = resurface_confirmed_knowledge_v2(environment.root.path(), 5).unwrap();
    assert_eq!(resurfaced.len(), 1);
    assert_eq!(resurfaced[0].perspectives, vec![PerspectiveV2::Technical]);
    assert!(resurfaced[0].has_open_questions);
    assert!(
        resurface_confirmed_knowledge_by_perspective_v2(
            environment.root.path(),
            Some(PerspectiveV2::Investment),
            5,
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn deferred_knowledge_resurfaces_and_opening_updates_only_local_history() {
    let environment = environment();
    let knowledge = write_knowledge(&environment);
    let review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Knowledge,
        &knowledge.record_id,
        &knowledge.revision,
        ReviewDecisionV2::Defer,
        None,
        None,
        "2026-07-23T03:00:00Z",
    );
    sync_projection(
        &environment,
        &knowledge,
        Some(review_id),
        ProjectionStateV2::Deferred,
    );
    let canonical_before = fs::read(&knowledge.current_path).unwrap();

    assert!(
        resurface_confirmed_knowledge_v2(environment.root.path(), 5)
            .unwrap()
            .is_empty()
    );
    let initial = resurface_knowledge_by_perspective_v2(environment.root.path(), None, 5).unwrap();
    assert_eq!(initial.len(), 1);
    assert_eq!(
        initial[0].review_state,
        ResurfacedKnowledgeStateV2::Deferred
    );
    assert_eq!(
        initial[0].reviewed_at,
        "2026-07-23T03:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    assert_eq!(initial[0].last_opened_at, None);

    record_resurfaced_knowledge_open_v2(
        environment.root.path(),
        &initial[0].knowledge_id,
        &initial[0].current_revision,
        &clock("2026-07-23T04:00:00Z"),
    )
    .unwrap();

    let reopened = resurface_knowledge_by_perspective_v2(environment.root.path(), None, 5).unwrap();
    assert_eq!(
        reopened[0].last_opened_at,
        Some("2026-07-23T04:00:00Z".parse::<DateTime<Utc>>().unwrap())
    );
    assert_eq!(fs::read(&knowledge.current_path).unwrap(), canonical_before);
    assert_eq!(
        fs::read(environment.root.path().join(".mko/.gitignore")).unwrap(),
        b"runtime/\n"
    );
    assert!(
        environment
            .root
            .path()
            .join(".mko/runtime/resurface-history.json")
            .is_file()
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
    assert_eq!(first.targets[0].domain_policy, None);
    assert_eq!(
        first.targets[1].domain_policy,
        Some(DomainPolicyV2::Standard)
    );
    let text = String::from_utf8(first.card_bytes).unwrap();
    assert!(text.contains("Source-grounded content"));
    assert!(text.contains("Knowledge analysis"));
    assert!(text.contains(&environment.source.general_summary));
    assert!(text.contains(&environment.knowledge.synthesis));
    assert!(text.contains("Domain policy requiring human confirmation: `standard`"));
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
        revised.targets[0].previous_confirmed_revision.as_deref(),
        Some(source.revision.as_str())
    );
    assert_eq!(
        revised.targets[0].state,
        ReviewCardTargetStateV2::RevisedUnconfirmed
    );
    let text = String::from_utf8(revised.card_bytes).unwrap();
    assert!(text.contains("Previous reviewed content"));
    assert!(text.contains(&environment.source.general_summary));
    assert!(text.contains(&changed_source.general_summary));
}

#[test]
fn regenerated_item_card_shows_addressed_feedback_and_bounded_diff() {
    let environment = environment();
    let feedback = "핵심 주장 1의 근거를 본문 수치가 있는 블록으로 교체";
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let review_id = seed_review(
        environment.root.path(),
        ReviewTargetTypeV2::Source,
        &source.record_id,
        &source.revision,
        ReviewDecisionV2::RequestChanges,
        Some(feedback),
        None,
        "2026-07-23T01:00:00Z",
    );
    sync_projection(
        &environment,
        &source,
        Some(review_id),
        ProjectionStateV2::ChangesRequested,
    );

    let requested = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    assert_eq!(
        requested.targets[0].state,
        ReviewCardTargetStateV2::ChangesRequested
    );
    assert_eq!(
        requested.targets[0].current_feedback.as_deref(),
        Some(feedback)
    );
    assert_eq!(requested.targets[0].addressed_feedback, None);

    let mut changed_source = environment.source.clone();
    changed_source.general_summary =
        "A regenerated summary that follows the requested change.".into();
    let replacement = write_source(
        &environment,
        &environment.bundle,
        &changed_source,
        Some(&source.revision),
    );

    let queue = derive_queue_v2(environment.root.path()).unwrap();
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].state, QueueItemStateV2::RevisedUnconfirmed);
    assert_eq!(queue.items[0].next_action, QueueNextActionV2::Display);

    let revised = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    let target = &revised.targets[0];
    assert_eq!(target.state, ReviewCardTargetStateV2::RevisedUnconfirmed);
    assert_eq!(target.current_feedback, None);
    assert_eq!(target.addressed_feedback.as_deref(), Some(feedback));
    assert_eq!(
        target.previous_reviewed_revision.as_deref(),
        Some(source.revision.as_str())
    );

    let text = String::from_utf8(revised.card_bytes.clone()).unwrap();
    assert!(text.contains("Feedback addressed by this revision for"));
    assert!(text.contains(feedback));
    assert!(text.contains("Changes since the reviewed revision for"));
    assert!(text.contains(&format!("--- reviewed {}", source.revision)));
    assert!(text.contains(&format!("+++ current {}", replacement.revision)));
    assert!(text.contains(
        "-  \"general_summary\": \"A bounded summary grounded in the prepared content.\""
    ));
    assert!(text.contains(
        "+  \"general_summary\": \"A regenerated summary that follows the requested change.\""
    ));

    let again = show_review_card_v2(environment.root.path(), &source.record_id).unwrap();
    assert_eq!(revised.card_digest, again.card_digest);
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

#[test]
fn self_consistent_projection_with_noncanonical_semantics_blocks_the_queue() {
    let environment = environment();
    let source = write_source(&environment, &environment.bundle, &environment.source, None);
    let wrong_title = "Projection-authored title that canonical Source never contained";

    write_projection_v2(
        environment.root.path(),
        &ProjectionInputV2 {
            record_type: ProjectionRecordTypeV2::Source,
            id: source.record_id.clone(),
            title: wrong_title.into(),
            current_revision: source.revision.clone(),
            review_head_id: None,
            derived_state: ProjectionStateV2::Unconfirmed,
            domain: "uncategorized".into(),
            perspectives: Vec::new(),
            tags: environment.source.tags.clone(),
            topics: Vec::new(),
            summary: String::new(),
            body_markdown: String::new(),
            record_link: format!("sources/{}/current.yaml", source.record_id),
            asset_link: format!("assets/registry/{}.json", environment.asset.id),
        },
    )
    .unwrap();

    let queue = derive_queue_v2(environment.root.path()).unwrap();

    assert_eq!(queue.items.len(), 1);
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
    let knowledge = (!is_source).then(|| {
        read_current_knowledge_revision_v2(environment.root.path(), &record.record_id).unwrap()
    });
    let mut tags = if is_source {
        environment.source.tags.clone()
    } else {
        environment
            .knowledge
            .units
            .iter()
            .flat_map(|unit| unit.tags.iter().cloned())
            .collect()
    };
    if let Some(knowledge) = &knowledge {
        tags.extend(
            knowledge
                .revision
                .perspectives
                .iter()
                .map(|perspective| format!("perspective:{}", perspective.as_str())),
        );
    }
    tags.sort();
    tags.dedup();
    let mut topics = if is_source {
        environment.source.topics.clone()
    } else {
        environment.knowledge.topics.clone()
    };
    topics.sort();
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
            domain: if let Some(knowledge) = &knowledge {
                if knowledge
                    .revision
                    .perspectives
                    .contains(&PerspectiveV2::Investment)
                {
                    "investment".into()
                } else {
                    knowledge
                        .revision
                        .perspectives
                        .first()
                        .map(PerspectiveV2::as_str)
                        .unwrap_or("uncategorized")
                        .into()
                }
            } else {
                "uncategorized".into()
            },
            perspectives: knowledge
                .as_ref()
                .map(|knowledge| knowledge.revision.perspectives.clone())
                .unwrap_or_default(),
            tags,
            topics,
            record_link: format!(
                "{}/{}/current.yaml",
                if is_source { "sources" } else { "knowledge" },
                record.record_id
            ),
            asset_link: format!("assets/registry/{}.json", environment.asset.id),
            summary: if is_source {
                mko_core::projection_v2::source_projection_summary_v2(&environment.source)
            } else {
                mko_core::projection_v2::knowledge_projection_summary_v2(
                    knowledge
                        .as_ref()
                        .map(|knowledge| &knowledge.revision.response)
                        .unwrap_or(&environment.knowledge),
                )
            },
            body_markdown: if is_source {
                mko_core::projection_v2::source_projection_body_v2(
                    &environment.source,
                    Some(environment.asset.provider.logical_locator.clone()),
                )
            } else {
                mko_core::projection_v2::knowledge_projection_body_v2(
                    knowledge
                        .as_ref()
                        .map(|knowledge| &knowledge.revision.response)
                        .unwrap_or(&environment.knowledge),
                    Some(environment.asset.provider.logical_locator.clone()),
                )
            },
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
