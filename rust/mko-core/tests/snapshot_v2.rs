use chrono::Utc;
use mko_core::{
    scaffold_v2::scaffold_personal_kb_v2,
    snapshot_v2::{
        MAX_SNAPSHOT_BYTES, RegisterSnapshotRequestV2, read_snapshot_text_v2,
        register_web_snapshot_v2,
    },
};
use tempfile::tempdir;

// Identity is the text, not the address. Re-reading an unchanged page must not
// create a second Asset; a page that changed must not overwrite the evidence an
// approved note already cites.
#[test]
fn a_snapshot_is_identified_by_the_text_it_stored() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let at = Utc::now();

    let first = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/a",
        title: "Example page",
        text: "The page said this.",
        fetched_at: at,
    })
    .unwrap()
    .asset;

    // A tracking parameter is a different address for the same evidence.
    let again = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/a?utm_source=newsletter",
        title: "Example page",
        text: "The page said this.",
        fetched_at: at,
    })
    .unwrap()
    .asset;
    assert_eq!(again.id, first.id, "the same text is the same evidence");

    let changed = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/a",
        title: "Example page",
        text: "The page says something else now.",
        fetched_at: at,
    })
    .unwrap()
    .asset;
    assert_ne!(changed.id, first.id, "changed text is new evidence");

    assert_eq!(
        read_snapshot_text_v2(&repository, &first.id).unwrap(),
        "The page said this.",
        "the evidence an approved note cites still resolves after the page moved on"
    );
    assert_eq!(
        first.provider.logical_locator, "https://example.com/a",
        "the first binding stays authoritative, as it does for a rediscovered PDF"
    );
}

// A page that rendered nothing readable is a failure to report, not an Asset
// holding whitespace that a later session would try to summarize.
#[test]
fn a_page_with_no_readable_text_is_refused() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let error = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/js-only",
        title: "JavaScript page",
        text: "   \n\t  ",
        fetched_at: Utc::now(),
    })
    .unwrap_err();

    assert_eq!(error.code(), "snapshot_text_empty");
}

#[test]
fn a_snapshot_larger_than_the_limit_is_refused_rather_than_truncated() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let error = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/big",
        title: "Big page",
        text: &"x".repeat(MAX_SNAPSHOT_BYTES as usize + 1),
        fetched_at: Utc::now(),
    })
    .unwrap_err();

    assert_eq!(error.code(), "snapshot_too_large");
}

// The address is validated where the record is built, not only where it is read
// back, so a bad address never reaches the registry at all.
#[test]
fn a_snapshot_address_that_could_not_be_stored_is_refused_at_registration() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let error = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://user:secret@example.com/page",
        title: "Page",
        text: "Readable text.",
        fetched_at: Utc::now(),
    })
    .unwrap_err();

    assert_eq!(error.code(), "asset_record_invalid");
}

// A title is what the owner sees in the waiting list and the vault, so it is
// always present and always bounded.
#[test]
fn a_snapshot_without_a_title_falls_back_to_its_address() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let untitled = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/untitled",
        title: "   ",
        text: "Readable text.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;
    assert_eq!(untitled.title_fallback, "https://example.com/untitled");

    let long = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/long",
        title: &"제".repeat(500),
        text: "Different readable text.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;
    assert_eq!(long.title_fallback.chars().count(), 200);
}

// "Everything downstream is the same" was claimed in the design and was not
// true: prepare inspected a provider file and rejected a snapshot's address as
// a non-portable path. A page the agent read has to reach drafting, or link
// ingestion is registration and nothing else.
#[test]
fn a_snapshot_prepares_into_a_bundle_a_draft_can_cite() {
    use mko_core::{
        model_v2::{ContentBlockV2, PreparedMetadataV2, PreparedTrustV2},
        prepared_v2::prepare_snapshot_asset_v2,
    };

    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let asset = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/release-notes",
        title: "Release notes",
        text: "The Error trait is now available in core.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;

    let prepared = prepare_snapshot_asset_v2(
        &repository,
        &asset.id,
        PreparedMetadataV2 {
            title: None,
            authors: Vec::new(),
            created_at: None,
        },
    )
    .unwrap();

    assert_eq!(prepared.bundle.asset_id, asset.id);
    assert_eq!(prepared.bundle.asset_fingerprint, asset.fingerprint);
    assert_eq!(
        prepared.bundle.trust,
        PreparedTrustV2::UntrustedDocumentContent,
        "a page the agent fetched is data, exactly as a document is"
    );
    let ContentBlockV2::Text { id, locator, text } = &prepared.bundle.content_blocks[0] else {
        panic!("a snapshot is text, so its blocks are text blocks");
    };
    assert!(id.starts_with("block-"));
    assert!(locator.starts_with("page:1;chunk:1"), "{locator}");
    assert!(text.contains("Error trait"), "{text}");
}

// The evidence a note cites must be the evidence that was read. If the stored
// text no longer hashes to the identity it was registered under, preparing it
// would bind a draft to something nobody approved.
#[test]
fn a_snapshot_whose_stored_text_changed_will_not_prepare() {
    use mko_core::{model_v2::PreparedMetadataV2, prepared_v2::prepare_snapshot_asset_v2};

    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let asset = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/page",
        title: "Page",
        text: "What was actually read.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;
    let stored = repository.join("assets/snapshots").join(format!(
        "{}.txt",
        asset.id.strip_prefix("personal-asset-").unwrap()
    ));
    std::fs::write(&stored, "Something else entirely.").unwrap();

    let error = prepare_snapshot_asset_v2(
        &repository,
        &asset.id,
        PreparedMetadataV2 {
            title: None,
            authors: Vec::new(),
            created_at: None,
        },
    )
    .unwrap_err();

    assert_eq!(error.code(), "registered_asset_changed");
}
