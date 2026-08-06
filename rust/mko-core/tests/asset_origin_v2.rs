use std::fs;

use mko_core::{
    asset_v2::read_asset_v2,
    records_v2::{AssetOriginV2, AssetProviderBindingV2, AssetRecordTypeV2, AssetRecordV2},
    revision_v2::canonical_json_bytes,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

/// The bytes a knowledge base already holds, written before origins existed.
/// Reproduced literally rather than built from the struct, because what is
/// being defended is exactly that these bytes keep working.
const ASSET_WRITTEN_BEFORE_ORIGINS: &str = concat!(
    r#"{"fingerprint":"sha256:"#,
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    r#"","id":"personal-asset-"#,
    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    r#"","media_type":"application/pdf","provider":{"logical_locator":"paper.pdf","#,
    r#""modified_at":null,"provider_type":"google-drive-filesystem","size_bytes":1024},"#,
    r#""record_type":"asset","schema_version":2,"title_fallback":"paper.pdf"}"#
);

// An Asset registry record is read back only if it re-serializes to the exact
// bytes on disk. A new field that always serializes would therefore invalidate
// every Asset ever registered — the record would parse and then be rejected as
// "not the expected canonical immutable record". The default must be invisible
// on the wire, not merely defaulted on read.
#[test]
fn an_asset_written_before_origins_still_reads_back() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();
    let id = format!("personal-asset-{}", "a".repeat(64));
    fs::write(
        repository
            .join("assets/registry")
            .join(format!("{id}.json")),
        ASSET_WRITTEN_BEFORE_ORIGINS,
    )
    .unwrap();

    let asset = read_asset_v2(&repository, &id).unwrap();

    assert_eq!(asset.origin, AssetOriginV2::ProviderPdf);
    assert_eq!(
        canonical_json_bytes(&asset).unwrap(),
        ASSET_WRITTEN_BEFORE_ORIGINS.as_bytes(),
        "a provider PDF must round-trip to the bytes it was stored as"
    );
}

// The PDF identity contract must not loosen because snapshots arrived, and a
// snapshot must not have to pretend to be a PDF to be stored.
#[test]
fn each_origin_is_validated_against_its_own_contract() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    let snapshot = record(
        AssetOriginV2::WebSnapshot,
        "text/plain",
        "web-snapshot",
        "https://example.com/page",
        'c',
    );
    store(&repository, &snapshot);
    assert_eq!(
        read_asset_v2(&repository, &snapshot.id).unwrap().origin,
        AssetOriginV2::WebSnapshot
    );

    // A provider PDF claiming a web locator is still an invalid record.
    let pdf_with_url = record(
        AssetOriginV2::ProviderPdf,
        "application/pdf",
        "google-drive-filesystem",
        "https://example.com/page",
        'd',
    );
    store(&repository, &pdf_with_url);
    assert_eq!(
        read_asset_v2(&repository, &pdf_with_url.id)
            .unwrap_err()
            .code(),
        "asset_record_invalid"
    );

    // A snapshot claiming to be a PDF is not a snapshot.
    let snapshot_as_pdf = record(
        AssetOriginV2::WebSnapshot,
        "application/pdf",
        "web-snapshot",
        "https://example.com/page",
        'e',
    );
    store(&repository, &snapshot_as_pdf);
    assert_eq!(
        read_asset_v2(&repository, &snapshot_as_pdf.id)
            .unwrap_err()
            .code(),
        "asset_record_invalid"
    );
}

// The locator is owner-facing output and is read back into the vault, so it is
// bounded and cannot smuggle credentials or control characters.
#[test]
fn a_snapshot_locator_is_a_bounded_plain_address() {
    let root = tempdir().unwrap();
    let repository = root.path().join("kb");
    scaffold_personal_kb_v2(&repository).unwrap();

    for (index, locator) in [
        "ftp://example.com/page",
        "https://user:secret@example.com/page",
        "https://example.com/\u{7}page",
        "not-a-url",
    ]
    .into_iter()
    .enumerate()
    {
        let rejected = record(
            AssetOriginV2::WebSnapshot,
            "text/plain",
            "web-snapshot",
            locator,
            char::from(b'0' + index as u8),
        );
        store(&repository, &rejected);
        assert_eq!(
            read_asset_v2(&repository, &rejected.id).unwrap_err().code(),
            "asset_record_invalid",
            "{locator} must not be storable as a snapshot address"
        );
    }
}

fn record(
    origin: AssetOriginV2,
    media_type: &str,
    provider_type: &str,
    logical_locator: &str,
    fill: char,
) -> AssetRecordV2 {
    let hash = String::from(fill).repeat(64);
    AssetRecordV2 {
        schema_version: 2,
        id: format!("personal-asset-{hash}"),
        record_type: AssetRecordTypeV2::Asset,
        origin,
        fingerprint: format!("sha256:{hash}"),
        title_fallback: "Example page".into(),
        media_type: media_type.into(),
        provider: AssetProviderBindingV2 {
            provider_type: provider_type.into(),
            logical_locator: logical_locator.into(),
            size_bytes: 2048,
            modified_at: None,
        },
    }
}

fn store(repository: &std::path::Path, record: &AssetRecordV2) {
    fs::write(
        repository
            .join("assets/registry")
            .join(format!("{}.json", record.id)),
        canonical_json_bytes(record).unwrap(),
    )
    .unwrap();
}
