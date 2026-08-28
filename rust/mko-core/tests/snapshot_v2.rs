use chrono::Utc;
use mko_core::{
    model_v2::PreparedMetadataV2,
    prepared_v2::prepare_snapshot_asset_v2,
    scaffold_v2::scaffold_personal_kb_v2,
    snapshot_v2::{
        MAX_SNAPSHOT_BYTES, RegisterSnapshotRequestV2, RegisterVideoTranscriptRequestV2,
        read_snapshot_text_v2, register_video_transcript_v2, register_web_snapshot_v2,
    },
};
use tempfile::tempdir;

fn no_metadata() -> PreparedMetadataV2 {
    PreparedMetadataV2 {
        title: None,
        authors: Vec::new(),
        created_at: None,
    }
}

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

// Every other v2 writer goes through atomic::write_new, which opens the parent
// as a capability directory and refuses a destination that is not a regular
// file. This one used a bare `path.exists()` guard, and `exists()` follows
// symlinks — so a dangling link at the destination reported "no file here" and
// the write went through it, landing evidence outside the repository.
#[cfg(unix)]
#[test]
fn a_symlinked_destination_cannot_carry_evidence_out_of_the_repository() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let text = "The page said this.";
    let hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(text.as_bytes()));
    let outside = root.path().join("outside.txt");
    std::os::unix::fs::symlink(
        &outside,
        repository
            .join("assets/snapshots")
            .join(format!("{hash}.txt")),
    )
    .unwrap();

    let error = register_web_snapshot_v2(RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/page",
        title: "Page",
        text,
        fetched_at: Utc::now(),
    })
    .unwrap_err();

    assert_eq!(error.code(), "registry_destination_invalid");
    assert!(
        !outside.exists(),
        "evidence must never be written through a link out of the knowledge base"
    );
}

// A torn or truncated write left the stored text permanently wrong: the old
// `path.exists()` short-circuit meant re-registering the identical page
// reported success without repairing anything, and preparing it then failed
// with registered_asset_changed forever. The hash IS the identity, so bytes
// that do not match it are provably damaged and provably repairable.
#[test]
fn a_damaged_snapshot_is_repaired_by_registering_the_same_page_again() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let request = |text: &'static str| RegisterSnapshotRequestV2 {
        repository_root: &repository,
        url: "https://example.com/page",
        title: "Page",
        text,
        fetched_at: Utc::now(),
    };
    let asset = register_web_snapshot_v2(request("The page said this."))
        .unwrap()
        .asset;
    let stored = repository.join("assets/snapshots").join(format!(
        "{}.txt",
        asset.id.strip_prefix("personal-asset-").unwrap()
    ));
    std::fs::write(&stored, "The page said th").unwrap();

    let again = register_web_snapshot_v2(request("The page said this.")).unwrap();

    assert_eq!(again.asset.id, asset.id);
    assert_eq!(
        read_snapshot_text_v2(&repository, &asset.id).unwrap(),
        "The page said this.",
        "the evidence must be restored, not silently left damaged"
    );
}

// Phase 4 (§6, §10): a video transcript is registered snapshot-model, exactly
// as a web page is — identity from the transcript text's fingerprint, no
// original video bytes ever fetched or stored. Registering the same
// transcript twice must return the same Asset, not a duplicate.
#[test]
fn a_video_transcript_is_identified_by_the_text_it_stored() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let at = Utc::now();

    let first = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        title: "Example video",
        text: "The narrator said this.",
        fetched_at: at,
    })
    .unwrap()
    .asset;

    let again = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s",
        title: "Example video",
        text: "The narrator said this.",
        fetched_at: at,
    })
    .unwrap()
    .asset;
    assert_eq!(
        again.id, first.id,
        "the same transcript text is the same evidence, regardless of tracking params"
    );

    let changed = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        title: "Example video",
        text: "The narrator says something else now.",
        fetched_at: at,
    })
    .unwrap()
    .asset;
    assert_ne!(
        changed.id, first.id,
        "changed transcript text is new evidence"
    );

    assert_eq!(
        read_snapshot_text_v2(&repository, &first.id).unwrap(),
        "The narrator said this.",
    );
    assert_eq!(
        first.provider.logical_locator, "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "the read timestamp's binding stays authoritative"
    );
    assert_eq!(
        first.provider.modified_at,
        Some(at),
        "the fetched_at timestamp is recorded"
    );
    assert_eq!(first.media_type, "text/plain");
}

// The locator contract is shared with a web snapshot's (§6, reused via
// `validate_snapshot_locator`): a `youtu.be` short link and a `watch?v=` long
// link both pass, since both are plain http(s) addresses; a non-URL locator
// is refused at registration, before a bad address can reach the registry.
#[test]
fn a_video_transcript_accepts_youtube_url_shapes_and_refuses_a_non_url_locator() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let short_link = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://youtu.be/dQw4w9WgXcQ",
        title: "Short link video",
        text: "Transcript from a youtu.be short link.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;
    assert_eq!(
        short_link.provider.logical_locator,
        "https://youtu.be/dQw4w9WgXcQ"
    );

    let long_link = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://www.youtube.com/watch?v=abcdefghijk",
        title: "Watch link video",
        text: "Transcript from a watch link.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;
    assert_eq!(
        long_link.provider.logical_locator,
        "https://www.youtube.com/watch?v=abcdefghijk"
    );

    let error = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "not-a-url",
        title: "Bad locator",
        text: "Transcript text.",
        fetched_at: Utc::now(),
    })
    .unwrap_err();
    assert_eq!(error.code(), "asset_record_invalid");
}

// A video transcript is prepared through the same snapshot path a web page
// uses (§10): its evidence is already text/plain, so nothing about prepare
// needs to know it came from a video rather than a page.
#[test]
fn a_video_transcript_prepares_into_a_bundle_a_draft_can_cite() {
    use mko_core::model_v2::{ContentBlockV2, PreparedTrustV2};

    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let asset = register_video_transcript_v2(RegisterVideoTranscriptRequestV2 {
        repository_root: &repository,
        url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        title: "Conference talk",
        text: "The speaker explained the Error trait is now available in core.",
        fetched_at: Utc::now(),
    })
    .unwrap()
    .asset;

    let prepared = prepare_snapshot_asset_v2(&repository, &asset.id, no_metadata()).unwrap();

    assert_eq!(prepared.bundle.asset_id, asset.id);
    assert_eq!(prepared.bundle.asset_fingerprint, asset.fingerprint);
    assert_eq!(prepared.bundle.media_type, "text/plain");
    assert_eq!(
        prepared.bundle.trust,
        PreparedTrustV2::UntrustedDocumentContent,
        "a transcript the agent produced is data, exactly as a page is"
    );
    let ContentBlockV2::Text { text, .. } = &prepared.bundle.content_blocks[0] else {
        panic!("a video transcript is text, so its blocks are text blocks");
    };
    assert!(text.contains("Error trait"), "{text}");
}
