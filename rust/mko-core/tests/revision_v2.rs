use std::fs;

use mko_core::revision_v2::{
    PublicationOutcomeV2, canonical_json_bytes, compare_and_swap_current_pointer_v2,
    create_current_pointer_v2, publish_revision_v2, sha256_digest,
};
use serde_json::json;
use tempfile::tempdir;

#[test]
fn canonical_json_is_nfc_lf_compact_and_key_sorted() {
    let value = json!({
        "z": "Cafe\u{301}\r\nsecond\rthird",
        "a": {"two": 2, "one": "e\u{301}"},
        "items": ["b", "a"]
    });

    let bytes = canonical_json_bytes(&value).expect("canonicalize JSON");

    assert_eq!(
        String::from_utf8(bytes).expect("UTF-8"),
        "{\"a\":{\"one\":\"é\",\"two\":2},\"items\":[\"b\",\"a\"],\"z\":\"Café\\nsecond\\nthird\"}"
    );
}

#[test]
fn canonical_json_rejects_keys_that_collide_after_normalization() {
    let value = json!({"é": 1, "e\u{301}": 2});

    let error = canonical_json_bytes(&value).expect_err("normalized duplicate key must fail");

    assert_eq!(error.code(), "canonical_json_invalid");
}

#[test]
fn sha256_helper_uses_prefixed_lowercase_hex() {
    assert_eq!(
        sha256_digest(b"abc"),
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn immutable_revision_publication_is_content_addressed_and_idempotent() {
    let temporary = tempdir().expect("temporary directory");
    let revisions = temporary.path().join("revisions");
    fs::create_dir(&revisions).expect("create revisions directory");
    let body = b"---\nschema_version: 2\n---\n\nbody\n";

    let first = publish_revision_v2(&revisions, body).expect("first publication");
    let second = publish_revision_v2(&revisions, body).expect("idempotent publication");

    assert_eq!(first.outcome, PublicationOutcomeV2::Created);
    assert_eq!(second.outcome, PublicationOutcomeV2::Existing);
    assert_eq!(first.revision, sha256_digest(body));
    assert_eq!(first.path, second.path);
    assert_eq!(fs::read(&first.path).expect("published revision"), body);
    assert_eq!(
        first.path.file_name().and_then(|name| name.to_str()),
        Some(&format!("{}.md", first.revision.replace(':', "-"))[..])
    );
}

#[test]
fn immutable_revision_refuses_changed_bytes_at_the_digest_path() {
    let temporary = tempdir().expect("temporary directory");
    let revisions = temporary.path().join("revisions");
    fs::create_dir(&revisions).expect("create revisions directory");
    let body = b"canonical revision";
    let digest_path = revisions.join(format!("{}.md", sha256_digest(body).replace(':', "-")));
    fs::write(&digest_path, b"tampered").expect("seed changed bytes");

    let error = publish_revision_v2(&revisions, body).expect_err("changed bytes must fail");

    assert_eq!(error.code(), "revision_conflict");
    assert_eq!(fs::read(digest_path).expect("unchanged file"), b"tampered");
}

#[test]
fn current_pointer_create_is_schema_v2_and_idempotent() {
    let temporary = tempdir().expect("temporary directory");
    let current = temporary.path().join("current.yaml");
    let pointer = json!({
        "schema_version": 2,
        "record_id": "source-1",
        "revision": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "evidence_basis": {"bundle_id": "bundle-1"}
    });

    assert_eq!(
        create_current_pointer_v2(&current, &pointer).expect("create pointer"),
        PublicationOutcomeV2::Created
    );
    assert_eq!(
        create_current_pointer_v2(&current, &pointer).expect("idempotent create"),
        PublicationOutcomeV2::Existing
    );
    assert_eq!(
        fs::read(&current).expect("pointer bytes"),
        canonical_json_bytes(&pointer).expect("canonical pointer")
    );

    let v1 = json!({"schema_version": 1, "revision": "sha256:old"});
    let v1_path = temporary.path().join("v1-current.yaml");
    let error = create_current_pointer_v2(&v1_path, &v1).expect_err("v1 pointer must fail");
    assert_eq!(error.code(), "current_pointer_invalid");
    assert!(!v1_path.exists());
}

#[test]
fn current_pointer_cas_replaces_only_the_exact_expected_bytes() {
    let temporary = tempdir().expect("temporary directory");
    let current = temporary.path().join("current.yaml");
    let first = pointer("source-1", '1');
    let second = pointer("source-1", '2');
    let stale_replacement = pointer("source-1", '3');
    create_current_pointer_v2(&current, &first).expect("create pointer");

    compare_and_swap_current_pointer_v2(&current, &first, &second).expect("CAS replace");
    let second_bytes = canonical_json_bytes(&second).expect("canonical second pointer");
    assert_eq!(fs::read(&current).expect("current pointer"), second_bytes);

    let error = compare_and_swap_current_pointer_v2(&current, &first, &stale_replacement)
        .expect_err("stale expected pointer must fail");
    assert_eq!(error.code(), "current_pointer_snapshot_changed");
    assert_eq!(fs::read(&current).expect("unchanged pointer"), second_bytes);
}

#[test]
fn non_regular_revision_and_pointer_destinations_are_rejected() {
    let temporary = tempdir().expect("temporary directory");
    let revisions = temporary.path().join("revisions");
    fs::create_dir(&revisions).expect("create revisions directory");
    let body = b"revision";
    let revision_path = revisions.join(format!("{}.md", sha256_digest(body).replace(':', "-")));
    fs::create_dir(&revision_path).expect("create non-file destination");

    let revision_error =
        publish_revision_v2(&revisions, body).expect_err("directory destination must fail");
    assert_eq!(revision_error.code(), "revision_destination_invalid");

    let current = temporary.path().join("current.yaml");
    fs::create_dir(&current).expect("create non-file pointer destination");
    let pointer_error = create_current_pointer_v2(&current, &pointer("source-1", '1'))
        .expect_err("directory pointer destination must fail");
    assert_eq!(pointer_error.code(), "current_pointer_destination_invalid");
}

#[cfg(unix)]
#[test]
fn symlink_revision_and_pointer_destinations_are_rejected_without_following() {
    use std::os::unix::fs::symlink;

    let temporary = tempdir().expect("temporary directory");
    let revisions = temporary.path().join("revisions");
    fs::create_dir(&revisions).expect("create revisions directory");
    let target = temporary.path().join("target");
    fs::write(&target, b"keep").expect("create symlink target");
    let body = b"revision";
    let revision_path = revisions.join(format!("{}.md", sha256_digest(body).replace(':', "-")));
    symlink(&target, &revision_path).expect("create revision symlink");

    let revision_error =
        publish_revision_v2(&revisions, body).expect_err("revision symlink must fail");
    assert_eq!(revision_error.code(), "revision_destination_invalid");

    let current = temporary.path().join("current.yaml");
    symlink(&target, &current).expect("create pointer symlink");
    let pointer_error = create_current_pointer_v2(&current, &pointer("source-1", '1'))
        .expect_err("pointer symlink must fail");
    assert_eq!(pointer_error.code(), "current_pointer_destination_invalid");
    assert_eq!(fs::read(target).expect("target remains unchanged"), b"keep");
}

fn pointer(record_id: &str, digit: char) -> serde_json::Value {
    json!({
        "schema_version": 2,
        "record_id": record_id,
        "revision": format!("sha256:{}", digit.to_string().repeat(64)),
        "evidence_basis": {"bundle_id": format!("bundle-{digit}")}
    })
}
