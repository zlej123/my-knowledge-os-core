use std::fs;

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    dashboard_v2::{
        DashboardFileStateV2, DashboardProjectionStateV2, ensure_dashboard_v2,
        inspect_dashboard_v2, repair_dashboard_v2,
    },
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, write_projection_v2,
    },
    records_v2::{AssetRecordV2, WriteSourceRecordRequestV2, write_source_record_v2},
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
fn inspection_is_read_only_and_missing_owned_definition_has_safe_repair_state() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let path = root.path().join("views/review-queue.base");
    fs::remove_file(&path).unwrap();

    let status = inspect_dashboard_v2(root.path()).unwrap();

    let item = status
        .items
        .iter()
        .find(|item| item.path == "views/review-queue.base")
        .unwrap();
    assert!(item.manifest_owned);
    assert_eq!(item.state, DashboardFileStateV2::Missing);
    assert_eq!(
        status.projection_state,
        DashboardProjectionStateV2::RepairRequired
    );
    assert!(status.manifest_owned_drift);
    assert!(!path.exists(), "inspection must not repair implicitly");

    repair_dashboard_v2(root.path()).unwrap();
    let repaired = inspect_dashboard_v2(root.path()).unwrap();
    assert_eq!(
        repaired
            .items
            .iter()
            .find(|item| item.path == "views/review-queue.base")
            .unwrap()
            .state,
        DashboardFileStateV2::Current
    );
}

#[test]
fn user_modified_owned_definition_is_reported_and_never_overwritten() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let path = root.path().join("HOME.md");
    let user_edit = b"# My private dashboard edit\n";
    fs::write(&path, user_edit).unwrap();

    let status = inspect_dashboard_v2(root.path()).unwrap();
    let item = status
        .items
        .iter()
        .find(|item| item.path == "HOME.md")
        .unwrap();
    assert!(item.manifest_owned);
    assert_eq!(item.state, DashboardFileStateV2::UserModified);

    let error = repair_dashboard_v2(root.path()).unwrap_err();
    assert_eq!(error.code(), "dashboard_user_modified");
    assert_eq!(fs::read(&path).unwrap(), user_edit);
}

#[test]
fn repair_preflights_all_definitions_before_any_mutation() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let missing = root.path().join("HOME.md");
    fs::remove_file(&missing).unwrap();
    let modified = root.path().join("views/review-queue.base");
    let user_edit = b"my query\n";
    fs::write(&modified, user_edit).unwrap();

    let error = repair_dashboard_v2(root.path()).unwrap_err();

    assert_eq!(error.code(), "dashboard_user_modified");
    assert!(
        !missing.exists(),
        "preflight failure must not create earlier files"
    );
    assert_eq!(fs::read(modified).unwrap(), user_edit);
}

#[test]
fn manifest_digest_cannot_make_an_orphan_projection_semantically_current() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let record_id = format!("personal-source-{}", "a".repeat(64));
    write_projection_v2(
        root.path(),
        &ProjectionInputV2 {
            record_type: ProjectionRecordTypeV2::Source,
            id: record_id.clone(),
            title: "Orphan projection".into(),
            current_revision: format!("sha256:{}", "b".repeat(64)),
            review_head_id: None,
            derived_state: ProjectionStateV2::Unreviewed,
            domain: "uncategorized".into(),
            perspectives: Vec::new(),
            tags: Vec::new(),
            record_link: format!("sources/{record_id}/current.yaml"),
            asset_link: format!("assets/registry/personal-asset-{}.json", "c".repeat(64)),
            summary: String::new(),
            body_markdown: String::new(),
        },
    )
    .unwrap();

    let status = inspect_dashboard_v2(root.path()).unwrap();
    let projection = status
        .items
        .iter()
        .find(|item| item.kind == mko_core::dashboard_v2::DashboardFileKindV2::RecordProjection)
        .unwrap();

    assert!(projection.manifest_owned);
    assert_eq!(projection.state, DashboardFileStateV2::Orphaned);
    assert_eq!(
        status.projection_state,
        DashboardProjectionStateV2::RepairRequired
    );
    assert!(status.manifest_owned_drift);
    let error = repair_dashboard_v2(root.path()).unwrap_err();
    assert_eq!(error.code(), "dashboard_orphan_projection");
}

#[test]
fn repair_regenerates_missing_and_semantically_stale_unmodified_projection() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let (asset, bundle, response, record) = write_source(root.path());
    let projection = projection_path(root.path(), &record.record_id);
    fs::remove_file(&projection).unwrap();

    repair_dashboard_v2(root.path()).unwrap();
    assert_eq!(
        projection_status(root.path(), &record.record_id),
        DashboardFileStateV2::Current
    );

    write_projection_v2(
        root.path(),
        &ProjectionInputV2 {
            record_type: ProjectionRecordTypeV2::Source,
            id: record.record_id.clone(),
            title: "self-consistent but noncanonical title".into(),
            current_revision: record.revision.clone(),
            review_head_id: None,
            derived_state: ProjectionStateV2::Unreviewed,
            domain: "uncategorized".into(),
            perspectives: Vec::new(),
            tags: response.tags.clone(),
            record_link: format!("sources/{}/current.yaml", record.record_id),
            asset_link: format!("assets/registry/{}.json", asset.id),
            summary: String::new(),
            body_markdown: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        projection_status(root.path(), &record.record_id),
        DashboardFileStateV2::Stale
    );

    repair_dashboard_v2(root.path()).unwrap();
    assert_eq!(
        projection_status(root.path(), &record.record_id),
        DashboardFileStateV2::Current
    );
    assert!(
        !String::from_utf8(fs::read(projection).unwrap())
            .unwrap()
            .contains("self-consistent but noncanonical title")
    );
    let _ = bundle;
}

#[test]
fn repair_claims_safe_unowned_expected_projection() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let (_, _, _, record) = write_source(root.path());
    fs::remove_file(root.path().join(".mko/generated-manifest.yaml")).unwrap();
    assert_eq!(
        projection_status(root.path(), &record.record_id),
        DashboardFileStateV2::Unowned
    );

    repair_dashboard_v2(root.path()).unwrap();

    assert_eq!(
        projection_status(root.path(), &record.record_id),
        DashboardFileStateV2::Current
    );
}

#[test]
fn repair_preserves_user_modified_projection_and_prepares_recovery() {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    ensure_dashboard_v2(root.path()).unwrap();
    let (_, _, _, record) = write_source(root.path());
    let projection = projection_path(root.path(), &record.record_id);
    let user_edit = b"# irreplaceable user edit\n";
    fs::write(&projection, user_edit).unwrap();

    let error = repair_dashboard_v2(root.path()).unwrap_err();

    assert_eq!(error.code(), "dashboard_projection_user_modified");
    assert_eq!(fs::read(&projection).unwrap(), user_edit);
    let recovery = fs::read_dir(root.path().join("recovery/manual-edits"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(recovery.len(), 1);
    assert!(recovery[0].path().join("projection.original.md").is_file());
    assert!(recovery[0].path().join("projection.expected.md").is_file());
    assert!(recovery[0].path().join("projection.diff").is_file());
}

fn projection_path(root: &std::path::Path, record_id: &str) -> std::path::PathBuf {
    root.join(format!("views/records/source-{record_id}.md"))
}

fn projection_status(root: &std::path::Path, record_id: &str) -> DashboardFileStateV2 {
    inspect_dashboard_v2(root)
        .unwrap()
        .items
        .into_iter()
        .find(|item| item.path == format!("views/records/source-{record_id}.md"))
        .unwrap()
        .state
}

fn write_source(
    root: &std::path::Path,
) -> (
    AssetRecordV2,
    mko_core::model_v2::PreparedContentV2,
    mko_core::model_v2::SourceResponseV2,
    mko_core::records_v2::RecordWriteResultV2,
) {
    let asset: AssetRecordV2 =
        serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
            .unwrap();
    fs::write(
        root.join("assets/registry")
            .join(format!("{}.json", asset.id)),
        canonical_json_bytes(&asset).unwrap(),
    )
    .unwrap();
    let mut bundle: mko_core::model_v2::PreparedContentV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/prepared-content.json"
    ))
    .unwrap();
    let mut value = serde_json::to_value(&bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
    let response: mko_core::model_v2::SourceResponseV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-response.json"
    ))
    .unwrap();
    let record = write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root,
            asset: &asset,
            bundle: &bundle,
            response: &response,
            expected_revision: None,
        },
        &FixedClock("2026-07-23T00:00:00Z".parse().unwrap()),
    )
    .unwrap();
    (asset, bundle, response, record)
}
