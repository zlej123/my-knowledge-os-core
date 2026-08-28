//! Phase 2 unified ingestion (§6, §6.1, §6.3): registration identity for the
//! new text-fingerprint and original-bytes-fingerprint origins, topics
//! normalization, `mko topics`, the `--topic` filter, and the
//! backward-compat guarantee that a revision written before `topics` existed
//! still parses and scans cleanly.

use std::fs;

use chrono::{DateTime, Utc};
use mko_core::{
    asset_v2::AssetRegistrationOutcomeV2,
    clock::{Clock, SystemClock},
    local_file_v2::{
        DOCX_MEDIA_TYPE, MAX_LOCAL_IMAGE_BYTES, RegisterLocalFileRequestV2, read_original_bytes_v2,
        read_original_text_v2, register_local_file_asset_v2,
    },
    model_v2::{
        ConfidenceV2, EvidenceRefV2, KnowledgeBasisV2, KnowledgeRecommendationOutcomeV2,
        KnowledgeRecommendationV2, KnowledgeResponseV2, KnowledgeUnitKindV2, KnowledgeUnitV2,
        PreparedMetadataV2, SourceResponseV2,
    },
    prepared_v2::{prepare_local_file_asset_v2, prepare_snapshot_asset_v2},
    queue_v2::{
        SearchConfirmationFilterV2, SearchOriginFormV2, derive_queue_v2, list_topics_v2,
        search_records_by_perspective_v2,
    },
    records_v2::{
        AssetOriginV2, AssetRecordV2, WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2,
        knowledge_record_id_v2, read_current_knowledge_revision_v2, write_knowledge_record_v2,
        write_source_record_v2,
    },
    revision_v2::{canonical_json_bytes, sha256_digest},
    scaffold_v2::scaffold_personal_kb_v2,
    snapshot_v2::{
        RegisterConversationRequestV2, RegisterPastedTextRequestV2, RegisterSnapshotRequestV2,
        register_conversation_v2, register_pasted_text_v2, register_web_snapshot_v2,
    },
};
use tempfile::tempdir;

/// A verified minimal valid 1x1 PNG (68 bytes): real magic bytes, a real
/// zlib-compressed `IDAT` chunk, a real `IEND` — small enough to embed as a
/// literal.
fn tiny_png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

/// Real zip local-file-header magic bytes (`PK\x03\x04`), which is all this
/// Core version's docx/hwpx signature check requires (§8.2/D2: the Core does
/// not parse the format, only validates the signature its extension
/// claims).
fn fake_docx_bytes() -> Vec<u8> {
    let mut bytes = b"PK\x03\x04".to_vec();
    bytes.extend(vec![0_u8; 32]);
    bytes
}

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
        prepare_local_file_asset_v2(root.path(), &first.asset.id, None, no_metadata()).unwrap();
    let prepared_twice =
        prepare_local_file_asset_v2(root.path(), &first.asset.id, None, no_metadata()).unwrap();
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

/// Like `source_response`, but with one grounded key claim — for a test that
/// wants a real evidence-bearing Source write, not an empty-claims one.
fn source_response_with_claim(
    claim_text: &str,
    evidence: EvidenceRefV2,
    topics: Vec<String>,
) -> SourceResponseV2 {
    SourceResponseV2 {
        key_claims: vec![mko_core::model_v2::SourceClaimV2 {
            text: claim_text.into(),
            evidence_refs: vec![evidence],
        }],
        ..source_response(topics)
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
        None,
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
        None,
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
        None,
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
        None,
    )
    .unwrap();
    assert_eq!(matches.len(), 1);
}

// Phase 3 (§6.1, §6.2): binary local-file originals — images and
// docx/hwpx documents — extend the same store text local files already use,
// with per-form signature validation and size ceilings.

#[test]
fn image_local_file_is_signature_validated_and_round_trips_through_the_originals_store() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("screenshot.png");
    let png = tiny_png_bytes();
    fs::write(&path, &png).unwrap();

    let registered = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "A screenshot",
        modified_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(registered.asset.origin, AssetOriginV2::LocalFile);
    assert_eq!(registered.asset.media_type, "image/png");
    assert_eq!(registered.asset.fingerprint, sha256_digest(&png));

    let hash = registered
        .asset
        .fingerprint
        .strip_prefix("sha256:")
        .unwrap();
    let stored = fs::read(
        root.path()
            .join("assets/originals")
            .join(format!("{hash}.png")),
    )
    .unwrap();
    assert_eq!(stored, png);
    assert_eq!(
        read_original_bytes_v2(root.path(), &registered.asset).unwrap(),
        png
    );
    // Text-only reading refuses a binary original rather than guessing.
    assert_eq!(
        read_original_text_v2(root.path(), &registered.asset)
            .unwrap_err()
            .code(),
        "local_file_not_text"
    );
}

#[test]
fn a_docx_local_file_is_signature_validated_by_its_zip_header() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("report.docx");
    let docx = fake_docx_bytes();
    fs::write(&path, &docx).unwrap();

    let registered = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(registered.asset.media_type, DOCX_MEDIA_TYPE);
    assert_eq!(
        read_original_bytes_v2(root.path(), &registered.asset).unwrap(),
        docx
    );
}

#[test]
fn an_image_whose_bytes_do_not_match_its_extension_is_rejected() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("not-really.png");
    fs::write(&path, b"definitely not a PNG").unwrap();

    let error = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap_err();
    assert_eq!(error.code(), "local_file_signature_invalid");
    assert!(
        fs::read_dir(root.path().join("assets/originals"))
            .map(|entries| entries.count())
            .unwrap_or(0)
            == 0
    );
}

#[test]
fn an_image_past_its_size_ceiling_is_rejected_before_it_reaches_the_originals_store() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("oversized.png");
    // Valid PNG signature, then padded well past the image ceiling — the
    // ceiling must be enforced before the (nonexistent, here) rest of the
    // image would even be parsed.
    let mut oversized = tiny_png_bytes();
    oversized.resize((MAX_LOCAL_IMAGE_BYTES + 1) as usize, 0);
    fs::write(&path, &oversized).unwrap();

    let error = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap_err();
    assert_eq!(error.code(), "local_file_invalid");
    assert!(
        fs::read_dir(root.path().join("assets/originals")).is_err()
            || fs::read_dir(root.path().join("assets/originals"))
                .unwrap()
                .count()
                == 0
    );
}

#[test]
fn prepare_requires_supplied_text_for_an_image_and_refuses_it_for_a_text_local_file() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();

    let image_path = source_files.path().join("screenshot.png");
    fs::write(&image_path, tiny_png_bytes()).unwrap();
    let image = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &image_path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();

    let missing =
        prepare_local_file_asset_v2(root.path(), &image.asset.id, None, no_metadata()).unwrap_err();
    assert_eq!(missing.code(), "local_file_extracted_text_required");

    let empty = prepare_local_file_asset_v2(
        root.path(),
        &image.asset.id,
        Some("   \n\t  "),
        no_metadata(),
    )
    .unwrap_err();
    assert_eq!(empty.code(), "local_file_extracted_text_empty");

    let prepared = prepare_local_file_asset_v2(
        root.path(),
        &image.asset.id,
        Some("OCR: quarterly revenue up 12%."),
        no_metadata(),
    )
    .unwrap();
    assert_eq!(prepared.bundle.media_type, "image/png");
    assert_eq!(
        prepared.bundle.content_blocks[0].clone(),
        mko_core::model_v2::ContentBlockV2::Text {
            id: "block-000001".into(),
            locator: "page:1;chunk:1;granularity:coarse".into(),
            text: "OCR: quarterly revenue up 12%.".into(),
        }
    );

    let text_path = source_files.path().join("note.md");
    fs::write(&text_path, "# A note\n\nSomething written down.\n").unwrap();
    let text_asset = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &text_path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    let rejected = prepare_local_file_asset_v2(
        root.path(),
        &text_asset.asset.id,
        Some("this should not be accepted"),
        no_metadata(),
    )
    .unwrap_err();
    assert_eq!(rejected.code(), "local_file_extracted_text_not_applicable");
}

/// §6.1: re-extraction (a better OCR pass) is a new Source revision of the
/// same immutable Asset, never a duplicate Asset — verifying and extending
/// Phase 2's re-registration behavior for the binary case.
#[test]
fn re_extraction_with_different_supplied_text_replaces_the_source_revision_of_the_same_asset() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("screenshot.png");
    fs::write(&path, tiny_png_bytes()).unwrap();

    let first_registration = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    // Re-registering the same bytes is the same Asset, not a duplicate.
    let second_registration = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "",
        modified_at: Utc::now(),
    })
    .unwrap();
    assert_eq!(
        second_registration.outcome,
        AssetRegistrationOutcomeV2::Existing
    );
    assert_eq!(second_registration.asset.id, first_registration.asset.id);
    let asset = first_registration.asset;

    let first_prepared = prepare_local_file_asset_v2(
        root.path(),
        &asset.id,
        Some("First-pass OCR, low quality."),
        no_metadata(),
    )
    .unwrap();
    let first_evidence = evidence_ref_for(&first_prepared.bundle);
    let first_write = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &first_prepared.bundle,
            response: &source_response_with_claim(
                "First-pass OCR, low quality.",
                first_evidence,
                Vec::new(),
            ),
            expected_revision: None,
        },
        &clock("2026-08-28T00:00:00Z"),
    )
    .unwrap();

    // A better OCR pass: same Asset, a new prepared bundle bound to the new
    // text, and a Source write that replaces the prior revision.
    let second_prepared = prepare_local_file_asset_v2(
        root.path(),
        &asset.id,
        Some("Second-pass OCR: quarterly revenue up 12%."),
        no_metadata(),
    )
    .unwrap();
    assert_ne!(
        second_prepared.bundle.bundle_id,
        first_prepared.bundle.bundle_id
    );
    let second_evidence = evidence_ref_for(&second_prepared.bundle);
    let second_write = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &second_prepared.bundle,
            response: &source_response_with_claim(
                "Second-pass OCR: quarterly revenue up 12%.",
                second_evidence,
                Vec::new(),
            ),
            expected_revision: Some(&first_write.revision),
        },
        &clock("2026-08-28T01:00:00Z"),
    )
    .unwrap();

    assert_eq!(
        second_write.outcome,
        mko_core::records_v2::RecordWriteOutcomeV2::Replaced
    );
    assert_eq!(second_write.record_id, first_write.record_id);
    assert_ne!(second_write.revision, first_write.revision);
}

#[test]
fn origin_filter_maps_display_forms_to_asset_origin_and_media_type() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();

    let paste = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "paste",
        text: "pasted evidence text",
        captured_at: Utc::now(),
    })
    .unwrap();
    let web = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: root.path(),
        url: "https://example.com/page",
        title: "web",
        text: "web evidence text",
        fetched_at: Utc::now(),
    })
    .unwrap();
    let conversation = register_conversation_v2(RegisterConversationRequestV2 {
        repository_root: root.path(),
        title: "conversation",
        text: "conversation evidence text",
        captured_at: Utc::now(),
    })
    .unwrap();
    let text_path = source_files.path().join("note.md");
    fs::write(&text_path, "local file evidence text").unwrap();
    let local_text = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &text_path,
        title: "local text",
        modified_at: Utc::now(),
    })
    .unwrap();
    let image_path = source_files.path().join("screenshot.png");
    fs::write(&image_path, tiny_png_bytes()).unwrap();
    let image = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &image_path,
        title: "image",
        modified_at: Utc::now(),
    })
    .unwrap();
    let docx_path = source_files.path().join("report.docx");
    fs::write(&docx_path, fake_docx_bytes()).unwrap();
    let document = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &docx_path,
        title: "document",
        modified_at: Utc::now(),
    })
    .unwrap();

    // A registered Asset alone is not searchable — `mko find` scans Source
    // and Knowledge revisions, not the raw registry — so each Asset needs a
    // real prepared-and-written Source before the origin filter can find it.
    for (asset, term) in [
        (&paste.asset, "pasted evidence text"),
        (&web.asset, "web evidence text"),
        (&conversation.asset, "conversation evidence text"),
    ] {
        let prepared = prepare_snapshot_asset_v2(root.path(), &asset.id, no_metadata()).unwrap();
        write_source_record_v2(
            WriteSourceRecordRequestV2 {
                repository_root: root.path(),
                asset,
                bundle: &prepared.bundle,
                response: &source_response_with_claim(
                    term,
                    evidence_ref_for(&prepared.bundle),
                    Vec::new(),
                ),
                expected_revision: None,
            },
            &clock("2026-08-28T00:00:00Z"),
        )
        .unwrap();
    }
    let prepared_local_text =
        prepare_local_file_asset_v2(root.path(), &local_text.asset.id, None, no_metadata())
            .unwrap();
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &local_text.asset,
            bundle: &prepared_local_text.bundle,
            response: &source_response_with_claim(
                "local file evidence text",
                evidence_ref_for(&prepared_local_text.bundle),
                Vec::new(),
            ),
            expected_revision: None,
        },
        &clock("2026-08-28T00:00:00Z"),
    )
    .unwrap();

    for (origin, expected_id, term) in [
        (
            SearchOriginFormV2::PastedText,
            &paste.asset.id,
            "pasted evidence text",
        ),
        (SearchOriginFormV2::Web, &web.asset.id, "web evidence text"),
        (
            SearchOriginFormV2::Conversation,
            &conversation.asset.id,
            "conversation evidence text",
        ),
        (
            SearchOriginFormV2::LocalFile,
            &local_text.asset.id,
            "local file evidence text",
        ),
    ] {
        let matches = search_records_by_perspective_v2(
            root.path(),
            term,
            None,
            SearchConfirmationFilterV2::Any,
            None,
            None,
            None,
            Some(origin),
        )
        .unwrap();
        assert_eq!(matches.len(), 1, "origin {origin:?} for term {term:?}");
        assert_eq!(&matches[0].asset_id, expected_id);
    }

    // `image` and `document` both map from `LocalFile`, discriminated by
    // media type — not each other, and not `local-file` itself.
    let prepared_image = prepare_local_file_asset_v2(
        root.path(),
        &image.asset.id,
        Some("image evidence text"),
        no_metadata(),
    )
    .unwrap();
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &image.asset,
            bundle: &prepared_image.bundle,
            response: &source_response_with_claim(
                "image evidence text",
                evidence_ref_for(&prepared_image.bundle),
                Vec::new(),
            ),
            expected_revision: None,
        },
        &clock("2026-08-28T00:00:00Z"),
    )
    .unwrap();
    let prepared_document = prepare_local_file_asset_v2(
        root.path(),
        &document.asset.id,
        Some("document evidence text"),
        no_metadata(),
    )
    .unwrap();
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &document.asset,
            bundle: &prepared_document.bundle,
            response: &source_response_with_claim(
                "document evidence text",
                evidence_ref_for(&prepared_document.bundle),
                Vec::new(),
            ),
            expected_revision: None,
        },
        &clock("2026-08-28T00:00:00Z"),
    )
    .unwrap();

    for (origin, expected_id, term) in [
        (SearchOriginFormV2::Image, &image.asset.id, "image evidence"),
        (
            SearchOriginFormV2::Document,
            &document.asset.id,
            "document evidence",
        ),
    ] {
        let matches = search_records_by_perspective_v2(
            root.path(),
            term,
            None,
            SearchConfirmationFilterV2::Any,
            None,
            None,
            None,
            Some(origin),
        )
        .unwrap();
        assert_eq!(matches.len(), 1, "origin {origin:?} for term {term:?}");
        assert_eq!(&matches[0].asset_id, expected_id);
    }

    // `video` is accepted but matches nothing until Phase 4.
    let no_video = search_records_by_perspective_v2(
        root.path(),
        "evidence",
        None,
        SearchConfirmationFilterV2::Any,
        None,
        None,
        None,
        Some(SearchOriginFormV2::Video),
    )
    .unwrap();
    assert!(no_video.is_empty());
}
