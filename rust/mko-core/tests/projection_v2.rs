use std::fs;

use mko_core::{
    clock::SystemClock,
    config_v2::PerspectiveV2,
    lock::{RepositoryMutationLock, StaleRepositoryLockPolicy},
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, ProjectionWriteOutcomeV2,
        projection_relative_path_v2, render_projection_v2, repair_projection_v2,
        write_projection_v2,
    },
    revision_v2::canonical_json_sha256,
};
use serde_json::{Value, json};
use tempfile::tempdir;

#[test]
fn projection_schema_fixture_and_rust_dto_agree() {
    let schema: Value =
        serde_json::from_str(include_str!("../../../schemas/v2/projection.schema.json"))
            .expect("projection schema JSON");
    let validator = jsonschema::validator_for(&schema).expect("projection schema compiles");
    let fixture = include_bytes!("../../../tests/fixtures/json-v2/projection.json");
    let value: Value = serde_json::from_slice(fixture).expect("projection fixture JSON");
    assert!(validator.is_valid(&value));
    let typed: ProjectionInputV2 = serde_json::from_slice(fixture).expect("projection Rust DTO");
    assert_eq!(serde_json::to_value(typed).expect("round trip"), value);

    let mut unknown = value.clone();
    unknown["unexpected"] = json!(true);
    assert!(!validator.is_valid(&unknown));
    assert!(serde_json::from_value::<ProjectionInputV2>(unknown).is_err());

    let mut wrong_type = value;
    wrong_type["record_type"] = json!("review");
    assert!(!validator.is_valid(&wrong_type));
    assert!(serde_json::from_value::<ProjectionInputV2>(wrong_type).is_err());

    let mut mismatched_id: Value = serde_json::from_slice(fixture).unwrap();
    mismatched_id["id"] = json!(format!("personal-knowledge-{}", "d".repeat(64)));
    assert!(!validator.is_valid(&mismatched_id));
    assert!(serde_json::from_value::<ProjectionInputV2>(mismatched_id).is_err());

    let mut missing_nullable: Value = serde_json::from_slice(fixture).unwrap();
    missing_nullable
        .as_object_mut()
        .unwrap()
        .remove("review_head_id");
    assert!(!validator.is_valid(&missing_nullable));
    assert!(serde_json::from_value::<ProjectionInputV2>(missing_nullable).is_err());
}

#[test]
fn rendering_is_deterministic_and_digest_is_over_canonical_input() {
    let input = projection("Original title");

    let first = render_projection_v2(&input).expect("first rendering");
    let second = render_projection_v2(&input).expect("second rendering");

    assert_eq!(first, second);
    let expected_digest = canonical_json_sha256(&input).expect("canonical input digest");
    let text = String::from_utf8(first.bytes).expect("projection UTF-8");
    assert_eq!(first.projection_digest, expected_digest);
    assert!(text.contains(&format!("projection_digest: \"{expected_digest}\"")));
    assert!(text.contains("record_type: source"));
    assert!(text.contains("derived_state: unreviewed"));
    assert!(text.contains("[[sources/"));
}

#[test]
fn knowledge_projection_exposes_sorted_perspectives_for_obsidian() {
    let mut input = projection("Perspective-aware knowledge");
    input.record_type = ProjectionRecordTypeV2::Knowledge;
    input.id = format!("personal-knowledge-{}", "a".repeat(64));
    input.record_link = format!(
        "knowledge/personal-knowledge-{}/current.yaml",
        "a".repeat(64)
    );
    input.domain = "investment".into();
    input.perspectives = vec![PerspectiveV2::Technical, PerspectiveV2::Investment];

    let rendered = render_projection_v2(&input).unwrap();
    let text = String::from_utf8(rendered.bytes).unwrap();

    assert!(text.contains("perspectives: [\"technical\",\"investment\"]"));
    let mut source_with_perspective = projection("Invalid Source");
    source_with_perspective.perspectives = vec![PerspectiveV2::Technical];
    assert!(render_projection_v2(&source_with_perspective).is_err());
}

#[test]
fn missing_projection_is_created_and_same_projection_is_idempotent() {
    let repository = setup_repository(&projection("Original title"));
    let input = projection("Original title");

    let first = write_projection_v2(repository.path(), &input).expect("create projection");
    let second = write_projection_v2(repository.path(), &input).expect("idempotent projection");

    assert_eq!(first.outcome, ProjectionWriteOutcomeV2::Created);
    assert_eq!(second.outcome, ProjectionWriteOutcomeV2::Existing);
    assert_eq!(
        first.path,
        repository
            .path()
            .join(projection_relative_path_v2(&input).unwrap())
    );
    assert_eq!(
        fs::read(&first.path).expect("projection bytes"),
        first.bytes
    );
}

#[test]
fn fresh_manifest_claims_new_record_before_projection_publication() {
    let input = projection("Original title");
    let repository = setup_repository_without_manifest();
    let relative = projection_relative_path_v2(&input).unwrap();
    let path = repository.path().join(&relative);
    fs::create_dir(&path).expect("temporarily block projection publication");

    let error = write_projection_v2(repository.path(), &input)
        .expect_err("projection publication should fail after manifest claim");

    assert_eq!(error.code(), "projection_destination_invalid");
    let manifest = fs::read_to_string(repository.path().join(".mko/generated-manifest.yaml"))
        .expect("claimed manifest");
    assert!(manifest.contains(&relative));
    assert!(manifest.contains("content_digest: null"));

    fs::remove_dir(&path).expect("remove publication blocker");
    let created = write_projection_v2(repository.path(), &input).expect("retry claimed projection");
    assert_eq!(created.outcome, ProjectionWriteOutcomeV2::Created);
    assert_eq!(fs::read(created.path).expect("projection"), created.bytes);
}

#[test]
fn stale_unmodified_projection_is_replaced_automatically() {
    let repository = setup_repository(&projection("Original title"));
    let original = projection("Original title");
    write_projection_v2(repository.path(), &original).expect("create original projection");
    let changed = projection("Changed title");

    let result = write_projection_v2(repository.path(), &changed).expect("safe stale update");

    assert_eq!(result.outcome, ProjectionWriteOutcomeV2::Updated);
    assert_eq!(fs::read(&result.path).expect("updated bytes"), result.bytes);
}

#[test]
fn user_modified_projection_is_backed_up_and_requires_explicit_repair() {
    let repository = setup_repository(&projection("Original title"));
    let original = projection("Original title");
    let created = write_projection_v2(repository.path(), &original).expect("create projection");
    let user_bytes = b"my exact manual projection edit\r\n";
    fs::write(&created.path, user_bytes).expect("manual edit");
    let changed = projection("Changed title");

    let pending = write_projection_v2(repository.path(), &changed).expect("detect manual edit");

    assert_eq!(pending.outcome, ProjectionWriteOutcomeV2::RepairRequired);
    assert_eq!(
        fs::read(&created.path).expect("manual bytes preserved"),
        user_bytes
    );
    let recovery = pending.recovery.as_ref().expect("recovery details");
    assert_eq!(
        fs::read(&recovery.backup_path).expect("exact backup"),
        user_bytes
    );
    assert!(
        fs::read_to_string(&recovery.diff_path)
            .expect("deterministic diff")
            .contains("+my exact manual projection edit\\r")
    );

    let repaired = repair_projection_v2(repository.path(), &changed, &recovery.modified_digest)
        .expect("explicit repair");
    assert_eq!(repaired.outcome, ProjectionWriteOutcomeV2::Repaired);
    assert_eq!(
        fs::read(&created.path).expect("repaired bytes"),
        repaired.bytes
    );
}

#[test]
fn repair_rejects_a_projection_changed_after_backup() {
    let repository = setup_repository(&projection("Original title"));
    let original = projection("Original title");
    let created = write_projection_v2(repository.path(), &original).expect("create projection");
    fs::write(&created.path, b"first manual edit").expect("first edit");
    let changed = projection("Changed title");
    let pending = write_projection_v2(repository.path(), &changed).expect("detect first edit");
    let expected = pending.recovery.unwrap().modified_digest;
    fs::write(&created.path, b"second manual edit").expect("second edit");

    let error = repair_projection_v2(repository.path(), &changed, &expected)
        .expect_err("changed repair snapshot must fail");

    assert_eq!(error.code(), "projection_snapshot_changed");
    assert_eq!(
        fs::read(created.path).expect("second edit preserved"),
        b"second manual edit"
    );
}

#[test]
fn writer_rejects_arbitrary_duplicate_escape_and_non_regular_projection_paths() {
    let repository = setup_repository(&projection("Original title"));
    let input = projection("Original title");
    write_manifest(repository.path(), &[]);
    let error = repair_projection_v2(
        repository.path(),
        &input,
        &format!("sha256:{}", "d".repeat(64)),
    )
    .expect_err("repair may not claim an unowned path");
    assert_eq!(error.code(), "projection_path_unowned");

    write_manifest(repository.path(), &["views/records/arbitrary.md".into()]);
    let error = write_projection_v2(repository.path(), &input).expect_err("arbitrary owned path");
    assert_eq!(error.code(), "projection_manifest_invalid");

    let relative = projection_relative_path_v2(&input).unwrap();
    write_manifest(repository.path(), &[relative.clone(), relative.clone()]);
    let error = write_projection_v2(repository.path(), &input).expect_err("duplicate owned path");
    assert_eq!(error.code(), "projection_manifest_invalid");

    let mut escaping = input.clone();
    escaping.id = "../outside".into();
    let error = write_projection_v2(repository.path(), &escaping).expect_err("path escape");
    assert_eq!(error.code(), "projection_invalid");
    assert!(!repository.path().join("outside.md").exists());

    write_manifest(
        repository.path(),
        &[projection_relative_path_v2(&input).unwrap()],
    );
    let path = repository
        .path()
        .join(projection_relative_path_v2(&input).unwrap());
    fs::create_dir(&path).expect("non-regular destination");
    let error = write_projection_v2(repository.path(), &input).expect_err("directory destination");
    assert_eq!(error.code(), "projection_destination_invalid");
}

#[test]
fn manifest_and_projection_reads_are_bounded() {
    let input = projection("Original title");
    let repository = setup_repository(&input);
    fs::write(
        repository.path().join(".mko/generated-manifest.yaml"),
        vec![b'a'; 1024 * 1024 + 1],
    )
    .expect("oversized manifest");
    let error = write_projection_v2(repository.path(), &input).expect_err("manifest byte limit");
    assert_eq!(error.code(), "projection_manifest_invalid");

    write_manifest(
        repository.path(),
        &[projection_relative_path_v2(&input).unwrap()],
    );
    let path = repository
        .path()
        .join(projection_relative_path_v2(&input).unwrap());
    fs::write(&path, vec![b'x'; 8 * 1024 * 1024 + 1]).expect("oversized projection");
    let error = write_projection_v2(repository.path(), &input).expect_err("projection byte limit");
    assert_eq!(error.code(), "projection_destination_invalid");
}

#[cfg(unix)]
#[test]
fn projection_destination_symlink_is_rejected_without_following() {
    use std::os::unix::fs::symlink;

    let input = projection("Original title");
    let repository = setup_repository(&input);
    let target = repository.path().join("outside-note.md");
    fs::write(&target, b"keep").expect("target note");
    let path = repository
        .path()
        .join(projection_relative_path_v2(&input).unwrap());
    symlink(&target, &path).expect("projection symlink");

    let error = write_projection_v2(repository.path(), &input).expect_err("symlink destination");

    assert_eq!(error.code(), "projection_destination_invalid");
    assert_eq!(fs::read(target).expect("target unchanged"), b"keep");
}

#[test]
fn projection_write_uses_the_repository_mutation_lock() {
    let input = projection("Original title");
    let repository = setup_repository(&input);
    let _lock = RepositoryMutationLock::acquire(
        repository.path(),
        "another v2 writer",
        &SystemClock,
        StaleRepositoryLockPolicy::Preserve,
    )
    .expect("hold repository lock");

    let error = write_projection_v2(repository.path(), &input).expect_err("writer must serialize");

    assert_eq!(error.code(), "repository_lock_held");
}

fn projection(title: &str) -> ProjectionInputV2 {
    ProjectionInputV2 {
        record_type: ProjectionRecordTypeV2::Source,
        id: format!("personal-source-{}", "a".repeat(64)),
        title: title.into(),
        current_revision: format!("sha256:{}", "b".repeat(64)),
        review_head_id: None,
        derived_state: ProjectionStateV2::Unreviewed,
        domain: "research".into(),
        perspectives: Vec::new(),
        tags: vec!["example".into(), "paper".into()],
        record_link: format!("sources/personal-source-{}/current.yaml", "a".repeat(64)),
        asset_link: format!("assets/registry/personal-asset-{}.yaml", "c".repeat(64)),
        body_markdown: String::new(),
    }
}

fn setup_repository(input: &ProjectionInputV2) -> tempfile::TempDir {
    let repository = setup_repository_without_manifest();
    write_manifest(
        repository.path(),
        &[projection_relative_path_v2(input).unwrap()],
    );
    repository
}

fn setup_repository_without_manifest() -> tempfile::TempDir {
    let repository = tempdir().expect("repository");
    for relative in [
        "views",
        "views/records",
        ".mko",
        "recovery",
        "recovery/manual-edits",
    ] {
        fs::create_dir(repository.path().join(relative)).expect("managed directory");
    }
    repository
}

fn write_manifest(repository: &std::path::Path, paths: &[String]) {
    let mut text = String::from("schema_version: 2\nprojections:\n");
    for path in paths {
        text.push_str(&format!("  - path: {path}\n    content_digest: null\n"));
    }
    fs::write(repository.join(".mko/generated-manifest.yaml"), text).expect("manifest");
}
