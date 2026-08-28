//! Phase 3 (§8, decided): `mko check`'s exemption for `assets/originals/` —
//! files there are exempt from the text-oriented byte budget and scans, and
//! are instead verified for their own per-form size ceiling and
//! content-addressed integrity. An originals file no Asset registry record
//! references is reported as an orphan.

use std::fs;

use chrono::Utc;
use mko_core::{
    check::{CheckRequest, check_repository},
    local_file_v2::{
        MAX_LOCAL_IMAGE_BYTES, RegisterLocalFileRequestV2, register_local_file_asset_v2,
    },
    revision_v2::sha256_digest,
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

/// A verified minimal valid 1x1 PNG (68 bytes) — see `ingestion_v2.rs` for
/// the same literal and why it is safe to duplicate here.
fn tiny_png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64,
        0xf8, 0x0f, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ]
}

#[test]
fn a_kb_with_a_5mib_png_original_passes_check() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("big-screenshot.png");
    // Real PNG signature, padded to 5 MiB — well past the old blanket 2 MiB
    // check.rs cap this exemption exists to lift, still under the 10 MiB
    // image ceiling.
    let mut png = tiny_png_bytes();
    png.resize(5 * 1024 * 1024, 0);
    assert!(png.len() as u64 <= MAX_LOCAL_IMAGE_BYTES);
    fs::write(&path, &png).unwrap();

    register_local_file_asset_v2(RegisterLocalFileRequestV2 {
        repository_root: root.path(),
        path: &path,
        title: "A big screenshot",
        modified_at: Utc::now(),
    })
    .unwrap();

    let report = check_repository(CheckRequest::new(root.path())).unwrap();
    // The scaffolded KB has no Git hooks installed yet, which is its own
    // (unrelated) reported issue; what this test exists to prove is that the
    // 5 MiB original itself raises none — no aggregate/per-file byte-budget
    // rejection, no secret/conflict scan noise, no size or integrity issue,
    // and no orphan (the Asset registry record references it).
    assert!(
        report
            .issues
            .iter()
            .all(|issue| !issue.code.starts_with("asset_original_")
                && issue.code != "check_input_too_large"),
        "unexpected originals-related issues: {:#?}",
        report.issues
    );
}

#[test]
fn a_tampered_original_fails_check() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let source_files = tempdir().unwrap();
    let path = source_files.path().join("screenshot.png");
    fs::write(&path, tiny_png_bytes()).unwrap();

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
    let original_path = root
        .path()
        .join("assets/originals")
        .join(format!("{hash}.png"));
    // Tamper with the stored original directly, bypassing the Core: the
    // filename still claims `hash`, but the bytes underneath no longer hash
    // to it.
    fs::write(&original_path, b"tampered bytes, not the registered PNG").unwrap();

    let report = check_repository(CheckRequest::new(root.path())).unwrap();
    assert!(report.has_code("asset_original_damaged"));
    assert!(!report.is_ok());
}

#[test]
fn an_orphaned_original_warns() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();

    // A stray original: correctly content-addressed (its filename hash
    // matches its own bytes), but no Asset registry record was ever written
    // for it.
    let stray = tiny_png_bytes();
    let hash = sha256_digest(&stray);
    let hash = hash.strip_prefix("sha256:").unwrap();
    fs::create_dir_all(root.path().join("assets/originals")).unwrap();
    fs::write(
        root.path()
            .join("assets/originals")
            .join(format!("{hash}.png")),
        &stray,
    )
    .unwrap();

    let report = check_repository(CheckRequest::new(root.path())).unwrap();
    assert!(report.has_code("asset_original_orphaned"));
    // Not damaged — its bytes are exactly what its filename claims; only
    // unreferenced.
    assert!(!report.has_code("asset_original_damaged"));
}
