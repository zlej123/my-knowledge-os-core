//! Extraction honesty (§6.2; owner decision 2026-09-03): the agent-read text
//! an image or document original is prepared from is persisted
//! content-addressed under `assets/extractions/`, every revision names the
//! stored text its evidence was built from, and every non-PDF bundle is
//! labelled `agent-read` rather than borrowing the PDF extractor's name.

use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use mko_core::{
    check::{CheckRequest, check_repository},
    clock::Clock,
    extraction_v2::{
        AGENT_READ_EXTRACTOR_NAME, locate_extraction_text_v2, read_extraction_text_v2,
    },
    local_file_v2::{RegisterLocalFileRequestV2, register_local_file_asset_v2},
    model_v2::{
        ContentBlockV2, EvidenceRefV2, KnowledgeRecommendationOutcomeV2, KnowledgeRecommendationV2,
        PreparedContentV2, PreparedMetadataV2, SourceClaimV2, SourceResponseV2,
    },
    prepared_v2::{
        build_pdf_prepared_content_v2, prepare_local_file_asset_v2, prepare_snapshot_asset_v2,
    },
    records_v2::{
        AssetOriginV2, AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2, CurrentPointerV2,
        RecordWriteOutcomeV2, SourceRevisionV2, WriteSourceRecordRequestV2, write_source_record_v2,
    },
    revision_v2::sha256_digest,
    scaffold_v2::scaffold_personal_kb_v2,
    snapshot_v2::{
        RegisterConversationRequestV2, RegisterPastedTextRequestV2, RegisterSnapshotRequestV2,
        RegisterVideoTranscriptRequestV2, register_conversation_v2, register_pasted_text_v2,
        register_video_transcript_v2, register_web_snapshot_v2,
    },
    version::PRODUCT_VERSION,
};
use tempfile::tempdir;

/// A verified minimal valid 1x1 PNG (68 bytes) — the same literal
/// `ingestion_v2.rs` embeds.
fn tiny_png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
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

fn hex(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap()
}

fn extraction_file(root: &Path, digest: &str) -> std::path::PathBuf {
    root.join("assets/extractions")
        .join(format!("{}.txt", hex(digest)))
}

fn register_png(root: &Path, files: &Path) -> AssetRecordV2 {
    let path = files.join("screenshot.png");
    fs::write(&path, tiny_png_bytes()).unwrap();
    register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root,
        path: &path,
        title: "A screenshot",
        modified_at: Utc::now(),
    })
    .unwrap()
    .asset
}

fn evidence_ref_for(bundle: &PreparedContentV2) -> EvidenceRefV2 {
    let ContentBlockV2::Text { id, locator, .. } = &bundle.content_blocks[0] else {
        panic!("expected a text block");
    };
    EvidenceRefV2 {
        block_id: id.clone(),
        locator: locator.clone(),
        text_span_utf8: None,
        table_range: None,
    }
}

fn source_response_with_claim(claim_text: &str, evidence: EvidenceRefV2) -> SourceResponseV2 {
    SourceResponseV2 {
        schema_version: 2,
        title: "Screenshot".into(),
        authors: Vec::new(),
        publication_date: None,
        one_sentence_summary: "A bounded summary.".into(),
        general_summary: "A grounded general summary.".into(),
        key_claims: vec![SourceClaimV2 {
            text: claim_text.into(),
            evidence_refs: vec![evidence],
        }],
        limitations: Vec::new(),
        tags: Vec::new(),
        knowledge_recommendation: KnowledgeRecommendationV2 {
            outcome: KnowledgeRecommendationOutcomeV2::ReferenceOnly,
            reasons: Vec::new(),
        },
        topics: Vec::new(),
    }
}

/// Reads a Source revision back the way the queue does: heading, then the
/// canonical JSON the revision file carries.
fn read_source_revision(path: &Path) -> SourceRevisionV2 {
    let bytes = fs::read(path).unwrap();
    let json = bytes
        .strip_prefix(b"# Source revision\n\n    ".as_slice())
        .expect("revision file carries its heading");
    serde_json::from_slice(json.strip_suffix(b"\n").unwrap_or(json)).unwrap()
}

fn read_current_pointer(root: &Path, record_id: &str) -> CurrentPointerV2 {
    let bytes = fs::read(root.join("sources").join(record_id).join("current.yaml")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn image_extraction_is_stored_content_addressed_and_the_bundle_names_it() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let text = "OCR: quarterly revenue up 12%.\n";

    let prepared =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();

    let digest = sha256_digest(text.as_bytes());
    assert_eq!(
        prepared.bundle.extraction_digest.as_deref(),
        Some(digest.as_str())
    );
    assert_eq!(prepared.bundle.extractor.name, AGENT_READ_EXTRACTOR_NAME);
    assert_eq!(prepared.bundle.extractor.version, PRODUCT_VERSION);
    assert_eq!(
        fs::read(extraction_file(root.path(), &digest)).unwrap(),
        text.as_bytes(),
        "the supplied text is stored verbatim, content-addressed"
    );
    assert_eq!(
        locate_extraction_text_v2(root.path(), &digest)
            .unwrap()
            .as_deref(),
        Some(format!("assets/extractions/{}.txt", hex(&digest)).as_str())
    );
    assert_eq!(read_extraction_text_v2(root.path(), &digest).unwrap(), text);

    // Preparing the same text again is idempotent: one file, same digest.
    let again =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    assert_eq!(again.bundle.bundle_id, prepared.bundle.bundle_id);
    assert_eq!(
        fs::read_dir(root.path().join("assets/extractions"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_damaged_extraction_is_reported_on_read_and_repaired_by_preparing_again() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let text = "OCR: the figure reads 42.";
    prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    let digest = sha256_digest(text.as_bytes());
    let path = extraction_file(root.path(), &digest);

    // Damage the stored text directly, bypassing the Core: the filename
    // still claims `digest`, but the bytes underneath no longer hash to it.
    fs::write(&path, "tampered: the figure reads 24.").unwrap();
    assert_eq!(
        read_extraction_text_v2(root.path(), &digest)
            .unwrap_err()
            .code(),
        "extraction_damaged"
    );

    // The hash is the identity, so the bytes that hash to it are provably
    // what belongs there: preparing again with the same text repairs it.
    prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), text.as_bytes());
    assert_eq!(read_extraction_text_v2(root.path(), &digest).unwrap(), text);
}

#[test]
fn evidence_resolves_after_the_local_runtime_is_removed() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let text = "OCR: quarterly revenue up 12%.";
    let prepared =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    let write = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &prepared.bundle,
            response: &source_response_with_claim(text, evidence_ref_for(&prepared.bundle)),
            expected_revision: None,
        },
        &clock("2026-09-03T00:00:00Z"),
    )
    .unwrap();

    // The prepared session — the only place the text used to live — is gone.
    fs::remove_dir_all(root.path().join(".mko/runtime")).unwrap();
    assert!(!prepared.bundle_path.exists());

    let revision = read_source_revision(&write.revision_path);
    let digest = revision
        .evidence_basis
        .extraction_digest
        .as_deref()
        .expect("the revision names the stored text its evidence was built from");
    assert_eq!(digest, sha256_digest(text.as_bytes()));
    assert_eq!(read_extraction_text_v2(root.path(), digest).unwrap(), text);
    assert_eq!(
        read_current_pointer(root.path(), &write.record_id).evidence_basis,
        revision.evidence_basis,
        "the current pointer carries the same evidence basis, digest included"
    );
    assert_eq!(revision.evidence_basis.extractor_name, "agent-read");
    assert_eq!(revision.evidence_basis.extractor_version, PRODUCT_VERSION);
}

#[test]
fn re_extraction_keeps_both_extractions_and_both_revisions_resolve() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let first_text = "First-pass OCR, low quality.";
    let second_text = "Second-pass OCR: quarterly revenue up 12%.";

    let first =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(first_text), no_metadata())
            .unwrap();
    let first_write = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &first.bundle,
            response: &source_response_with_claim(first_text, evidence_ref_for(&first.bundle)),
            expected_revision: None,
        },
        &clock("2026-09-03T00:00:00Z"),
    )
    .unwrap();
    let second =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(second_text), no_metadata())
            .unwrap();
    let second_write = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &second.bundle,
            response: &source_response_with_claim(second_text, evidence_ref_for(&second.bundle)),
            expected_revision: Some(&first_write.revision),
        },
        &clock("2026-09-03T01:00:00Z"),
    )
    .unwrap();
    assert_eq!(second_write.outcome, RecordWriteOutcomeV2::Replaced);
    fs::remove_dir_all(root.path().join(".mko/runtime")).unwrap();

    let first_digest = sha256_digest(first_text.as_bytes());
    let second_digest = sha256_digest(second_text.as_bytes());
    assert_ne!(first_digest, second_digest);
    assert!(extraction_file(root.path(), &first_digest).is_file());
    assert!(extraction_file(root.path(), &second_digest).is_file());

    // Both revision files are still there, and each one's evidence resolves
    // to exactly the text it was built from — the two passes are comparable.
    for (path, digest, text) in [
        (&first_write.revision_path, &first_digest, first_text),
        (&second_write.revision_path, &second_digest, second_text),
    ] {
        let revision = read_source_revision(path);
        assert_eq!(
            revision.evidence_basis.extraction_digest.as_deref(),
            Some(digest.as_str())
        );
        assert_eq!(read_extraction_text_v2(root.path(), digest).unwrap(), text);
    }
}

#[test]
fn writing_a_source_whose_extraction_text_is_missing_is_refused() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let text = "OCR: a claim with nothing behind it.";
    let prepared =
        prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    fs::remove_file(extraction_file(
        root.path(),
        &sha256_digest(text.as_bytes()),
    ))
    .unwrap();

    let error = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &prepared.bundle,
            response: &source_response_with_claim(text, evidence_ref_for(&prepared.bundle)),
            expected_revision: None,
        },
        &clock("2026-09-03T00:00:00Z"),
    )
    .unwrap_err();
    assert_eq!(error.code(), "extraction_not_found");
}

#[test]
fn every_non_pdf_origin_is_labelled_agent_read_and_names_its_stored_text() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let captured_at = Utc::now();

    let paste = register_pasted_text_v2(RegisterPastedTextRequestV2 {
        repository_root: root.path(),
        title: "paste",
        text: "pasted text",
        captured_at,
    })
    .unwrap()
    .asset;
    let web = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: root.path(),
        url: "https://example.com/article",
        title: "web",
        text: "web page text",
        fetched_at: captured_at,
    })
    .unwrap()
    .asset;
    let conversation = register_conversation_v2(RegisterConversationRequestV2 {
        repository_root: root.path(),
        title: "conversation",
        text: "conversation text",
        captured_at,
    })
    .unwrap()
    .asset;
    let video = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: root.path(),
        url: "https://www.youtube.com/watch?v=abc",
        title: "video",
        text: "video transcript text",
        fetched_at: captured_at,
    })
    .unwrap()
    .asset;
    let note_path = files.path().join("note.md");
    fs::write(&note_path, "# A note\n\nWritten down.\n").unwrap();
    let note = register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &note_path,
        title: "",
        modified_at: captured_at,
    })
    .unwrap()
    .asset;

    // Text-fingerprint origins and text local files: the stored text is the
    // Asset's own identity, so the bundle points at that store and no new
    // file is written.
    for (asset, expected_text, expected_store) in [
        (&paste, "pasted text", "assets/snapshots/"),
        (&web, "web page text", "assets/snapshots/"),
        (&conversation, "conversation text", "assets/snapshots/"),
        (&video, "video transcript text", "assets/snapshots/"),
        (&note, "# A note\n\nWritten down.\n", "assets/originals/"),
    ] {
        let bundle = if asset.origin == AssetOriginV2::LocalFile {
            prepare_local_file_asset_v2(root.path(), &asset.id, None, no_metadata())
        } else {
            prepare_snapshot_asset_v2(root.path(), &asset.id, no_metadata())
        }
        .unwrap()
        .bundle;
        assert_eq!(
            bundle.extractor.name, AGENT_READ_EXTRACTOR_NAME,
            "{:?}",
            asset.origin
        );
        assert_eq!(bundle.extractor.version, PRODUCT_VERSION);
        assert_eq!(
            bundle.extraction_digest.as_deref(),
            Some(asset.fingerprint.as_str()),
            "{:?} points at its own stored text",
            asset.origin
        );
        let location = locate_extraction_text_v2(root.path(), &asset.fingerprint)
            .unwrap()
            .unwrap();
        assert!(
            location.starts_with(expected_store),
            "{:?} resolved to {location}",
            asset.origin
        );
        assert_eq!(
            read_extraction_text_v2(root.path(), &asset.fingerprint).unwrap(),
            expected_text
        );
    }
    assert!(
        fs::read_dir(root.path().join("assets/extractions"))
            .unwrap()
            .next()
            .is_none(),
        "nothing above needed a second copy of its text"
    );

    // An image: agent-read too, pointing at the extraction store.
    let image = register_png(root.path(), files.path());
    let bundle = prepare_local_file_asset_v2(root.path(), &image.id, Some("OCR"), no_metadata())
        .unwrap()
        .bundle;
    assert_eq!(bundle.extractor.name, AGENT_READ_EXTRACTOR_NAME);
    assert_ne!(
        bundle.extraction_digest.as_deref(),
        Some(image.fingerprint.as_str()),
        "an image's text is not its original bytes"
    );
    assert!(
        locate_extraction_text_v2(root.path(), bundle.extraction_digest.as_deref().unwrap())
            .unwrap()
            .unwrap()
            .starts_with("assets/extractions/")
    );

    // A PDF keeps the PDF extractor's name and stores no text.
    let pdf = build_pdf_prepared_content_v2(
        &AssetRecordV2 {
            schema_version: 2,
            id: format!("personal-asset-{}", "b".repeat(64)),
            record_type: AssetRecordTypeV2::Asset,
            origin: AssetOriginV2::ProviderPdf,
            fingerprint: format!("sha256:{}", "b".repeat(64)),
            title_fallback: "paper.pdf".into(),
            media_type: "application/pdf".into(),
            provider: AssetProviderBindingV2 {
                provider_type: "google-drive-filesystem".into(),
                logical_locator: "Inbox/paper.pdf".into(),
                size_bytes: 1024,
                modified_at: None,
            },
        },
        &["page one".into()],
        no_metadata(),
    )
    .unwrap();
    assert_eq!(pdf.extractor.name, "pdf-extract");
    assert_eq!(pdf.extraction_digest, None);
    assert!(
        !String::from_utf8(mko_core::revision_v2::canonical_json_bytes(&pdf).unwrap())
            .unwrap()
            .contains("extraction_digest"),
        "an absent digest is elided, so PDF bundle digests are unchanged"
    );
}

#[test]
fn check_verifies_extraction_integrity_by_filename_hash() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let files = tempdir().unwrap();
    let asset = register_png(root.path(), files.path());
    let text = "OCR: a healthy extraction.";
    prepare_local_file_asset_v2(root.path(), &asset.id, Some(text), no_metadata()).unwrap();
    let digest = sha256_digest(text.as_bytes());

    let healthy = check_repository(CheckRequest::new(root.path())).unwrap();
    assert!(
        healthy
            .issues
            .iter()
            .all(|issue| !issue.code.starts_with("asset_extraction_")),
        "unexpected extraction issues: {:#?}",
        healthy.issues
    );

    // A prepare never followed by a write is a routine workflow, not damage:
    // no orphan report exists for the extraction store.
    assert!(!healthy.has_code("asset_extraction_orphaned"));

    fs::write(
        extraction_file(root.path(), &digest),
        "tampered after the fact",
    )
    .unwrap();
    fs::write(
        root.path().join("assets/extractions/notes.txt"),
        "not content-addressed",
    )
    .unwrap();
    let report = check_repository(CheckRequest::new(root.path())).unwrap();
    assert!(report.has_code("asset_extraction_damaged"));
    assert!(report.has_code("asset_extraction_invalid"));
    assert!(!report.is_ok());
    let damaged: Vec<&str> = report
        .issues
        .iter()
        .filter(|issue| issue.code == "asset_extraction_damaged")
        .filter_map(|issue| issue.path.as_deref())
        .collect();
    assert_eq!(
        damaged,
        [format!("assets/extractions/{}.txt", hex(&digest))]
    );
}
