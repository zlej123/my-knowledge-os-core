use chrono::{TimeZone, Utc};
use mko_core::{
    model_v2::{ContentBlockV2, PreparedMetadataV2},
    prepared_v2::build_pdf_prepared_content_v2,
    records_v2::{AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2},
    revision_v2::canonical_json_bytes,
};

fn asset() -> AssetRecordV2 {
    AssetRecordV2 {
        schema_version: 2,
        id: format!("personal-asset-{}", "b".repeat(64)),
        record_type: AssetRecordTypeV2::Asset,
        fingerprint: format!("sha256:{}", "b".repeat(64)),
        title_fallback: "paper.pdf".into(),
        media_type: "application/pdf".into(),
        provider: AssetProviderBindingV2 {
            provider_type: "google-drive-filesystem".into(),
            logical_locator: "Inbox/paper.pdf".into(),
            size_bytes: 1024,
            modified_at: None,
        },
    }
}

fn metadata(title: &str, author: &str) -> PreparedMetadataV2 {
    PreparedMetadataV2 {
        title: Some(title.into()),
        authors: vec![author.into()],
        created_at: Some(Utc.with_ymd_and_hms(2026, 7, 22, 0, 0, 0).unwrap()),
    }
}

#[test]
fn platform_style_whitespace_and_unicode_normalize_to_identical_bytes() {
    let mac = build_pdf_prepared_content_v2(
        &asset(),
        &["Cafe\u{301}\tmodel\r\n\r\nResult   A\r\n".into()],
        metadata("Cafe\u{301} study", "A\tResearcher"),
    )
    .unwrap();
    let windows = build_pdf_prepared_content_v2(
        &asset(),
        &["Café model\n\nResult A\n".into()],
        metadata("Café study", "A Researcher"),
    )
    .unwrap();

    assert_eq!(
        canonical_json_bytes(&mac).unwrap(),
        canonical_json_bytes(&windows).unwrap()
    );
    assert_eq!(mac.content_digest, windows.content_digest);
    assert_eq!(
        mac.content_digest,
        "sha256:7c6722e7c8155defbd2fb0451e37ad563ba0d613a528d9355ed27f98a9f3c6db"
    );
    let ContentBlockV2::Text { locator, text, .. } = &mac.content_blocks[0] else {
        panic!("PDF page must produce text")
    };
    assert_eq!(locator, "page:1;chunk:1;granularity:coarse");
    assert_eq!(text, "Café model\n\nResult A");
}

#[test]
fn bundle_identity_is_bound_to_the_exact_asset() {
    let bundle = build_pdf_prepared_content_v2(
        &asset(),
        &["Evidence".into()],
        PreparedMetadataV2 {
            title: None,
            authors: Vec::new(),
            created_at: None,
        },
    )
    .unwrap();
    assert_eq!(bundle.asset_id, asset().id);
    assert_eq!(
        bundle.bundle_id,
        format!(
            "prepared-content-{}",
            bundle.content_digest.replace(':', "-")
        )
    );
}

// Real extractions carry a scattering of stray control bytes from font and
// encoding quirks. What has to hold is that canonical text contains none of
// them, and normalizing away a few dozen meaningless bytes keeps hundreds of
// readable pages that rejecting the document would have thrown away.
#[test]
fn stray_control_characters_are_normalized_away_instead_of_failing_the_document() {
    let bundle = build_pdf_prepared_content_v2(
        &asset(),
        &[
            "safe\u{0000}text\u{0001}here".into(),
            "second\u{0010}page".into(),
        ],
        PreparedMetadataV2 {
            title: Some("Title\u{0001}with noise".into()),
            authors: vec!["Author\u{0011}Name".into()],
            created_at: None,
        },
    )
    .expect("stray control bytes must not fail an otherwise readable document");

    let text = bundle
        .content_blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlockV2::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(text.contains("safetexthere"));
    assert!(text.contains("secondpage"));
    assert_eq!(bundle.metadata.title.as_deref(), Some("Titlewith noise"));
    assert_eq!(bundle.metadata.authors, vec!["AuthorName".to_owned()]);
    for character in text
        .chars()
        .chain(bundle.metadata.title.iter().flat_map(|title| title.chars()))
    {
        assert!(
            !character.is_control() || matches!(character, '\n' | '\r' | '\t' | '\u{000c}'),
            "canonical text must still carry no ambiguous control characters"
        );
    }
}

// The failure that stranded material used to be reported once, to whoever ran
// the command, and then discarded. It has to survive the session that saw it.
#[test]
fn a_failed_preparation_is_recorded_against_the_asset() {
    use mko_core::attempt_v2::{
        PreparationOutcomeV2, latest_preparation_attempt_v2, record_preparation_attempt_v2,
    };

    let root = tempfile::tempdir().unwrap();
    let repository = root.path().join("kb");
    mko_core::scaffold_v2::scaffold_personal_kb_v2(&repository).unwrap();
    let asset_id = format!("personal-asset-{}", "c".repeat(64));

    assert!(
        latest_preparation_attempt_v2(&repository, &asset_id)
            .unwrap()
            .is_none()
    );

    let first = record_preparation_attempt_v2(
        &repository,
        &asset_id,
        PreparationOutcomeV2::Failed,
        Some("pdf_text_unreadable"),
        &FixedClock("2026-08-04T00:00:00Z".parse().unwrap()),
    )
    .unwrap();
    assert_eq!(first.outcome, PreparationOutcomeV2::Failed);
    assert_eq!(first.code.as_deref(), Some("pdf_text_unreadable"));

    // The same failure at the same moment is the same fact: content addressing
    // collapses it, so a retry loop cannot grow the directory.
    let repeated = record_preparation_attempt_v2(
        &repository,
        &asset_id,
        PreparationOutcomeV2::Failed,
        Some("pdf_text_unreadable"),
        &FixedClock("2026-08-04T00:00:00Z".parse().unwrap()),
    )
    .unwrap();
    assert_eq!(repeated.id, first.id);
    assert_eq!(
        std::fs::read_dir(repository.join("assets/attempts"))
            .unwrap()
            .count(),
        1
    );

    let later = record_preparation_attempt_v2(
        &repository,
        &asset_id,
        PreparationOutcomeV2::Prepared,
        None,
        &FixedClock("2026-08-04T01:00:00Z".parse().unwrap()),
    )
    .unwrap();
    let latest = latest_preparation_attempt_v2(&repository, &asset_id)
        .unwrap()
        .expect("an attempt is on file");
    assert_eq!(latest.id, later.id);
    assert_eq!(latest.outcome, PreparationOutcomeV2::Prepared);
    assert_eq!(latest.code, None);
}

#[derive(Clone, Copy)]
struct FixedClock(chrono::DateTime<chrono::Utc>);

impl mko_core::clock::Clock for FixedClock {
    fn now_utc(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}
