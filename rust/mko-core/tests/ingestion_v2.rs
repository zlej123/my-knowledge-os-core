//! Phase 2 unified ingestion (§6, §6.1, §6.3): registration identity for the
//! new text-fingerprint and original-bytes-fingerprint origins, topics
//! normalization, `mko topics`, the `--topic` filter, and the
//! backward-compat guarantee that a revision written before `topics` existed
//! still parses and scans cleanly.

use std::fs;

use chrono::{DateTime, Utc};
use mko_core::{
    clock::{Clock, SystemClock},
    local_file_v2::{
        RegisterLocalFileRequestV2, read_original_text_v2, register_local_file_asset_v2,
    },
    model_v2::{
        ConfidenceV2, EvidenceRefV2, KnowledgeBasisV2, KnowledgeRecommendationOutcomeV2,
        KnowledgeRecommendationV2, KnowledgeResponseV2, KnowledgeUnitKindV2, KnowledgeUnitV2,
        PreparedMetadataV2, SourceResponseV2,
    },
    prepared_v2::{prepare_local_file_asset_v2, prepare_snapshot_asset_v2},
    queue_v2::{
        SearchConfirmationFilterV2, derive_queue_v2, list_topics_v2,
        search_records_by_perspective_v2,
    },
    records_v2::{
        AssetRecordV2, WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2,
        knowledge_record_id_v2, read_current_knowledge_revision_v2, write_knowledge_record_v2,
        write_source_record_v2,
    },
    revision_v2::{canonical_json_bytes, sha256_digest},
    scaffold_v2::scaffold_personal_kb_v2,
    snapshot_v2::{
        RegisterConversationRequestV2, RegisterPastedTextRequestV2, register_conversation_v2,
        register_pasted_text_v2,
    },
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

fn clock(rfc3339: &str) -> FixedClock {
    FixedClock(rfc3339.parse().unwrap())
}

fn no_metadata() -> PreparedMetadataV2 {
    PreparedMetadataV2 {
        title: None,
        authors: Vec::new(),
        created_at: None,
    }
}

#[test]
fn pasted_text_registered_twice_converges_to_one_asset() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();

    let first = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "My paste",
        text: "the owner pasted this exact text",
        captured_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        first.outcome,
        mko_core::asset_v2::AssetRegistrationOutcomeV2::Created
    );
    assert_eq!(
        first.asset.origin,
        mko_core::records_v2::AssetOriginV2::PastedText
    );
    assert_eq!(first.asset.provider.provider_type, "pasted-text");
    assert_eq!(first.asset.provider.logical_locator, "");

    let second = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "A different title does not change identity",
        text: "the owner pasted this exact text",
        captured_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        second.outcome,
        mko_core::asset_v2::AssetRegistrationOutcomeV2::Existing
    );
    assert_eq!(second.asset.id, first.asset.id);
    // The first registration's title is authoritative; a second registration
    // never rewrites the immutable Asset record.
    assert_eq!(second.asset.title_fallback, "My paste");
}

#[test]
fn conversation_capture_registers_with_its_own_origin_and_locator_convention() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();

    let result = register_conversation_v2(RegisterConversationRequestV2 {
        repository_root: root.path(),
        title: "",
        text: "owner: what did I read about X?\nagent: nothing found, here is what I know...",
        captured_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        result.asset.origin,
        mko_core::records_v2::AssetOriginV2::Conversation
    );
    assert_eq!(result.asset.provider.provider_type, "conversation");
    // No locator exists for a captured conversation (§6.1, decided): the
    // empty-string convention, not `Option`.
    assert_eq!(result.asset.provider.logical_locator, "");
    // An empty title falls back to a fixed label rather than failing.
    assert_eq!(result.asset.title_fallback, "(제목 없는 대화)");
}

#[test]
fn local_file_registered_twice_is_idempotent_and_prepares_the_same_bundle() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("note.md");
    fs::write(
        &path,
        "# A note\n\nSomething the owner already wrote down.\n",
    )
    .unwrap();

    let first = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        first.outcome,
        mko_core::asset_v2::AssetRegistrationOutcomeV2::Created
    );
    assert_eq!(
        first.asset.origin,
        mko_core::records_v2::AssetOriginV2::LocalFile
    );
    assert_eq!(first.asset.provider.provider_type, "local-file");
    assert_eq!(first.asset.title_fallback, "note.md");
    // The original-bytes fingerprint is the identity (§6.1) — not a text
    // fingerprint derived some other way.
    let bytes = fs::read(&path).unwrap();
    assert_eq!(first.asset.fingerprint, sha256_digest(&bytes));

    // Re-registering the same file at the same path is the same Asset —
    // "new extraction revision path", never a duplicate Asset.
    let second = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        second.outcome,
        mko_core::asset_v2::AssetRegistrationOutcomeV2::Existing
    );
    assert_eq!(second.asset.id, first.asset.id);

    let prepared_once =
        prepare_local_file_asset_v2(root.path(), &first.asset.id, no_metadata()).unwrap();
    let prepared_twice =
        prepare_local_file_asset_v2(root.path(), &first.asset.id, no_metadata()).unwrap();
    assert_eq!(prepared_once.bundle, prepared_twice.bundle);
    assert_eq!(prepared_once.bundle.media_type, "text/plain");
    assert_eq!(prepared_once.bundle.asset_id, first.asset.id);
}

#[test]
fn originals_store_round_trips_the_exact_bytes() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("owner-notes.txt");
    let content = "line one\nline two\n한글도 포함됩니다\n";
    fs::write(&path, content).unwrap();

    let registered = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();

    let hash = registered
        .asset
        .fingerprint
        .strip_prefix("sha256:")
        .unwrap();
    let stored = fs::read_to_string(
        root.path()
            .join("assets/originals")
            .join(format!("{hash}.txt")),
    )
    .unwrap();
    assert_eq!(stored, content);

    let read_back = read_original_text_v2(root.path(), &registered.asset).unwrap();
    assert_eq!(read_back, content);
}

/// §6.1's literal identity rule ("original bytes' fingerprint" for a local
/// file; "text fingerprint" for a paste) has a consequence worth documenting
/// rather than discovering by accident: for a plain-text local file, the
/// original bytes *are* the text, so identical content converges to one
/// Asset regardless of which form registered it first. Divergent bytes (here,
/// a trailing newline the file has and the paste does not) are a different
/// Asset, as expected.
#[test]
fn local_file_and_pasted_text_converge_when_bytes_match_and_diverge_when_they_do_not() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("shared.txt");
    let content = "identical content, byte for byte";
    fs::write(&path, content).unwrap();

    let local_file = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "From disk",
        modified_at: Utc::now(),
    })
    .unwrap();

    let pasted_same_bytes = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "Pasted",
        text: content,
        captured_at: Utc::now(),
    })
    .unwrap();
    // Same Asset — the first registration's origin remains authoritative
    // (write_asset_registry_record_v2 keeps the first immutable binding).
    assert_eq!(pasted_same_bytes.asset.id, local_file.asset.id);
    assert_eq!(
        pasted_same_bytes.asset.origin,
        mko_core::records_v2::AssetOriginV2::LocalFile
    );
    assert_eq!(
        pasted_same_bytes.outcome,
        mko_core::asset_v2::AssetRegistrationOutcomeV2::Existing
    );

    let pasted_different_bytes = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "Pasted with a trailing newline",
        text: "identical content, byte for byte\n",
        captured_at: Utc::now(),
    })
    .unwrap();
    assert_ne!(pasted_different_bytes.asset.id, local_file.asset.id);
}

fn source_response(topics: Vec<String>) -> SourceResponseV2 {
    SourceResponseV2 {
        schema_version: 2,
        title: "Example paste".into(),
        authors: Vec::new(),
        publication_date: None,
        one_sentence_summary: "A bounded summary.".into(),
        general_summary: "A grounded general summary.".into(),
        key_claims: Vec::new(),
        limitations: Vec::new(),
        tags: Vec::new(),
        knowledge_recommendation: KnowledgeRecommendationV2 {
            outcome: KnowledgeRecommendationOutcomeV2::ReferenceOnly,
            reasons: Vec::new(),
        },
        topics,
    }
}

fn knowledge_response(evidence: EvidenceRefV2, topics: Vec<String>) -> KnowledgeResponseV2 {
    KnowledgeResponseV2 {
        schema_version: 2,
        synthesis: "A grounded synthesis.".into(),
        units: vec![KnowledgeUnitV2 {
            kind: KnowledgeUnitKindV2::Fact,
            title: "Evidence fact".into(),
            body: "The evidence text exists.".into(),
            confidence: ConfidenceV2::High,
            basis: KnowledgeBasisV2::Evidence,
            evidence_refs: vec![evidence],
            tags: Vec::new(),
        }],
        topics,
    }
}

/// Registers a paste and prepares it, returning the real Asset and bundle a
/// topics test can write a Source/Knowledge revision against — exercising
/// the actual Phase 2 pipeline rather than a hand-built fixture.
fn prepared_pasted_asset(
    root: &std::path::Path,
    text: &str,
) -> (AssetRecordV2, mko_core::model_v2::PreparedContentV2) {
    let registration = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root,
        title: "Topics fixture",
        text,
        captured_at: Utc::now(),
    })
    .unwrap();
    let prepared = prepare_snapshot_asset_v2(root, &registration.asset.id, no_metadata()).unwrap();
    (registration.asset, prepared.bundle)
}

#[test]
fn topics_are_normalized_case_preserving_and_deduped_case_insensitively() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let (asset, bundle) = prepared_pasted_asset(root.path(), "text for the topics test");

    let response = source_response(vec![
        "  투자>반도체  ".into(),
        "투자>반도체".into(),
        "TOPIC".into(),
        "topic".into(),
        "개발>Rust".into(),
    ]);
    let result = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision: None,
        },
        &clock("2026-08-29T00:00:00Z"),
    )
    .unwrap();

    let bytes = fs::read(&result.revision_path).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let json = text
        .strip_prefix("# Source revision\n\n    ")
        .and_then(|remaining| remaining.strip_suffix('\n'))
        .unwrap();
    let stored: serde_json::Value = serde_json::from_str(json).unwrap();
    let topics = stored["response"]["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    // Whitespace-trimmed, first-seen casing kept, case-insensitive duplicates
    // collapsed: four inputs collapse to two topics ("투자>반도체" once,
    // "TOPIC" once — its lowercase repeat dropped), plus the untouched third.
    assert_eq!(topics, vec!["투자>반도체", "TOPIC", "개발>Rust"]);
}

#[test]
fn an_empty_or_oversized_topic_is_rejected_with_a_clear_error() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let (asset, bundle) = prepared_pasted_asset(root.path(), "text for the rejection test");

    let empty = source_response(vec!["   ".into()]);
    let error = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &empty,
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "topic_invalid");

    let oversized = source_response(vec!["x".repeat(257)]);
    let error = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &oversized,
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "topic_invalid");

    let too_many = source_response((0..257).map(|index| format!("topic-{index}")).collect());
    let error = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &too_many,
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap_err();
    assert_eq!(error.code(), "topics_too_many");
}

#[test]
fn mko_topics_is_flat_case_insensitively_deduped_and_deterministically_sorted() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let (source_asset, source_bundle) =
        prepared_pasted_asset(root.path(), "first piece of pasted text");
    let (knowledge_asset, knowledge_bundle) = prepared_pasted_asset(
        root.path(),
        "second piece of pasted text, distinct evidence",
    );

    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &source_asset,
            bundle: &source_bundle,
            response: &source_response(vec!["투자>반도체".into(), "개발>Rust".into()]),
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap();

    let evidence = evidence_ref_for(&knowledge_bundle);
    write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: root.path(),
            asset: &knowledge_asset,
            bundle: &knowledge_bundle,
            response: &knowledge_response(evidence, vec!["투자>REIT".into(), "개발>rust".into()]),
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap();

    let topics = list_topics_v2(root.path()).unwrap();
    // "개발>Rust" and "개발>rust" case-fold to the same topic; the flat list
    // keeps one entry (first-seen casing from the deterministic scan order)
    // and is sorted deterministically.
    assert_eq!(topics.len(), 3);
    assert!(
        topics
            .iter()
            .any(|topic| topic.eq_ignore_ascii_case("개발>Rust"))
    );
    assert!(topics.contains(&"투자>반도체".to_string()));
    assert!(topics.contains(&"투자>REIT".to_string()));
    // Deterministic: running it again gives byte-identical output.
    assert_eq!(list_topics_v2(root.path()).unwrap(), topics);
}

#[test]
fn topic_filter_matches_case_insensitively_and_by_hierarchical_prefix() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let (asset, bundle) = prepared_pasted_asset(root.path(), "some searchable evidence text");
    let evidence = evidence_ref_for(&bundle);
    write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &knowledge_response(evidence, vec!["투자>반도체".into()]),
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap();

    // Exact match, case-insensitive.
    let exact = search_records_by_perspective_v2(
        root.path(),
        "evidence",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        None,
        Some("투자>반도체"),
    )
    .unwrap();
    assert_eq!(exact.len(), 1);

    // Hierarchical prefix: the parent topic matches the child.
    let prefix = search_records_by_perspective_v2(
        root.path(),
        "evidence",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        None,
        Some("투자"),
    )
    .unwrap();
    assert_eq!(prefix.len(), 1);

    // A sibling topic, or a substring that is not a hierarchical parent,
    // must not match.
    let no_match = search_records_by_perspective_v2(
        root.path(),
        "evidence",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        None,
        Some("개발"),
    )
    .unwrap();
    assert!(no_match.is_empty());
}

fn evidence_ref_for(bundle: &mko_core::model_v2::PreparedContentV2) -> EvidenceRefV2 {
    let block = bundle
        .content_blocks
        .first()
        .expect("prepared bundle has at least one block");
    let (id, locator) = match block {
        mko_core::model_v2::ContentBlockV2::Text { id, locator, .. } => {
            (id.clone(), locator.clone())
        }
        _ => panic!("expected a text block"),
    };
    EvidenceRefV2 {
        block_id: id,
        locator,
        text_span_utf8: None,
        table_range: None,
    }
}

/// A revision written before `topics` existed has no `topics` key at all —
/// not an empty array, simply absent. It must still parse, still round-trip
/// to its exact original bytes (the byte-compare `queue_v2::scan_collection`
/// and `read_current_knowledge_revision_v2` both depend on), and still scan
/// cleanly through every read path.
#[test]
fn a_revision_written_before_topics_existed_still_round_trips_and_scans() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let (asset, bundle) = prepared_pasted_asset(root.path(), "pre-Phase-2 evidence text");
    let evidence = evidence_ref_for(&bundle);

    // Writing with empty topics is the faithful stand-in for "before this
    // field existed": `#[serde(skip_serializing_if = "Vec::is_empty")]`
    // means the key is omitted from the wire either way, so the bytes Core
    // produces here are exactly what a pre-Phase-2 Core would have written.
    let result = write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &knowledge_response(evidence, Vec::new()),
            expected_revision: None,
        },
        &SystemClock,
    )
    .unwrap();

    let bytes = fs::read(&result.revision_path).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let json = text
        .strip_prefix("# Knowledge revision\n\n    ")
        .and_then(|remaining| remaining.strip_suffix('\n'))
        .unwrap();
    assert!(
        !json.contains("\"topics\""),
        "an empty-topics revision must omit the key entirely, matching a genuinely old file: {json}"
    );

    let record_id = knowledge_record_id_v2(&asset.id).unwrap();
    let current = read_current_knowledge_revision_v2(root.path(), &record_id).unwrap();
    assert!(current.revision.response.topics.is_empty());
    // The function's own internal check already proves
    // `canonical_json_bytes(&revision) == json`; a further explicit check
    // here documents exactly the property this test exists for.
    assert_eq!(
        canonical_json_bytes(&current.revision).unwrap(),
        json.as_bytes()
    );

    // The rest of the read pipeline must not choke on a topics-less revision.
    assert!(list_topics_v2(root.path()).unwrap().is_empty());
    let queue = derive_queue_v2(root.path()).unwrap();
    assert_eq!(queue.items.len(), 1);
    let matches = search_records_by_perspective_v2(
        root.path(),
        "evidence",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(matches.len(), 1);
}
