use std::fs;

use assert_cmd::Command;
use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    front_matter::render_markdown,
    model_v2::{
        KnowledgeResponseV2, PreparedContentV2, ReviewDecisionV2, ReviewRecordTypeV2,
        ReviewRecordV2, ReviewTargetTypeV2, ReviewTargetV2, SourceResponseV2,
    },
    projection_v2::{
        ProjectionInputV2, ProjectionRecordTypeV2, ProjectionStateV2, write_projection_v2,
    },
    records_v2::{
        AssetRecordV2, WriteKnowledgeRecordRequestV2, WriteSourceRecordRequestV2,
        write_knowledge_record_v2, write_source_record_v2,
    },
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

fn clock(rfc3339: &str) -> FixedClock {
    FixedClock(rfc3339.parse().unwrap())
}

struct Fixture {
    root: tempfile::TempDir,
    asset: AssetRecordV2,
    bundle: PreparedContentV2,
    source: SourceResponseV2,
    knowledge: KnowledgeResponseV2,
}

/// One Source (left unconfirmed) and one Knowledge record (reviewed and
/// confirmed), sharing one Asset — a small but real repository for
/// exercising `mko find` end to end, matching the fixtures `queue_v2.rs`
/// already validates.
fn seeded_fixture() -> Fixture {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let asset: AssetRecordV2 =
        serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
            .unwrap();
    fs::write(
        root.path()
            .join("assets/registry")
            .join(format!("{}.json", asset.id)),
        canonical_json_bytes(&asset).unwrap(),
    )
    .unwrap();
    let mut bundle: PreparedContentV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/prepared-content.json"
    ))
    .unwrap();
    seal_bundle(&mut bundle);
    let source: SourceResponseV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-response.json"
    ))
    .unwrap();
    let knowledge: KnowledgeResponseV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/knowledge-response.json"
    ))
    .unwrap();

    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &source,
            expected_revision: None,
        },
        &clock("2026-08-01T00:00:00Z"),
    )
    .unwrap();

    let knowledge_result = write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: root.path(),
            asset: &asset,
            bundle: &bundle,
            response: &knowledge,
            expected_revision: None,
        },
        &clock("2026-08-01T00:00:00Z"),
    )
    .unwrap();

    let review_id = seed_review(
        root.path(),
        &knowledge_result.record_id,
        &knowledge_result.revision,
        "2026-08-01T01:00:00Z",
    );
    let mut tags = knowledge
        .units
        .iter()
        .flat_map(|unit| unit.tags.iter().cloned())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    write_projection_v2(
        root.path(),
        &ProjectionInputV2 {
            record_type: ProjectionRecordTypeV2::Knowledge,
            id: knowledge_result.record_id.clone(),
            title: asset.title_fallback.clone(),
            current_revision: knowledge_result.revision.clone(),
            review_head_id: Some(review_id),
            derived_state: ProjectionStateV2::Confirmed,
            domain: "uncategorized".into(),
            perspectives: Vec::new(),
            tags,
            record_link: format!("knowledge/{}/current.yaml", knowledge_result.record_id),
            asset_link: format!("assets/registry/{}.json", asset.id),
            summary: mko_core::projection_v2::knowledge_projection_summary_v2(&knowledge),
            body_markdown: mko_core::projection_v2::knowledge_projection_body_v2(
                &knowledge,
                Some(asset.provider.logical_locator.clone()),
            ),
        },
    )
    .unwrap();

    Fixture {
        root,
        asset,
        bundle,
        source,
        knowledge,
    }
}

fn seed_review(
    root: &std::path::Path,
    record_id: &str,
    revision: &str,
    created_at: &str,
) -> String {
    let targets = vec![ReviewTargetV2 {
        record_type: ReviewTargetTypeV2::Knowledge,
        record_id: record_id.into(),
        displayed_revision: revision.into(),
        decision: ReviewDecisionV2::Approve,
        feedback: None,
        supersedes_review_id: None,
    }];
    let created_at: DateTime<Utc> = created_at.parse().unwrap();
    let identity = serde_json::json!({
        "schema_version": 2,
        "record_type": ReviewRecordTypeV2::Review,
        "targets": targets,
        "created_at": created_at,
    });
    let digest = canonical_json_sha256(&identity).unwrap();
    let id = format!("personal-review-{}", digest.trim_start_matches("sha256:"));
    let record = ReviewRecordV2 {
        schema_version: 2,
        id: id.clone(),
        record_type: ReviewRecordTypeV2::Review,
        targets,
        created_at,
    };
    fs::write(
        root.join("reviews").join(format!("{id}.md")),
        render_markdown(&record, "# Review event\n").unwrap(),
    )
    .unwrap();
    id
}

fn seal_bundle(bundle: &mut PreparedContentV2) {
    let mut value = serde_json::to_value(&*bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
}

#[test]
#[allow(deprecated)]
fn find_json_v2_returns_unified_labelled_matches_and_matches_schema() {
    let fixture = seeded_fixture();
    let _ = (&fixture.asset, &fixture.bundle);

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["find", "reported example", "--format", "json-v2", "--repo"])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();

    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));

    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["command"], "find");
    assert_eq!(value["result"], "ok");
    let items = value["data"]["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        2,
        "one Source hit and one Knowledge hit: {items:?}"
    );

    let knowledge_item = items
        .iter()
        .find(|item| item["record_type"] == "knowledge")
        .unwrap();
    assert_eq!(knowledge_item["title"], "Reported result");
    assert_eq!(knowledge_item["confirmation"]["status"], "confirmed");
    assert!(knowledge_item["confirmation"]["confirmed_at"].is_string());

    let source_item = items
        .iter()
        .find(|item| item["record_type"] == "source")
        .unwrap();
    assert_eq!(source_item["layer"], "source_own_words");
    assert_eq!(source_item["confirmation"]["status"], "unconfirmed");
    assert!(source_item["confirmation"]["confirmed_at"].is_null());

    // Recall is unconditional and measured (D7): every v3 `mko find`
    // execution appends one line to logs/recall.jsonl, naming every
    // returned record ID as `surfaced`.
    let log = fs::read_to_string(fixture.root.path().join("logs/recall.jsonl")).unwrap();
    let lines = log.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let entry: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry["query"], "reported example");
    assert_eq!(entry["results"], 2);
    let surfaced = entry["surfaced"].as_array().unwrap();
    assert_eq!(surfaced.len(), 2);
    assert!(surfaced.iter().any(|id| *id == knowledge_item["record_id"]));
    assert!(surfaced.iter().any(|id| *id == source_item["record_id"]));
}

#[test]
#[allow(deprecated)]
fn find_confirmed_and_unconfirmed_filters_narrow_results_and_are_mutually_exclusive() {
    let fixture = seeded_fixture();

    let confirmed_only = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find",
            "reported example",
            "--confirmed",
            "--format",
            "json-v2",
            "--repo",
        ])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let confirmed_only: serde_json::Value = serde_json::from_slice(&confirmed_only).unwrap();
    let items = confirmed_only["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["record_type"], "knowledge");

    let unconfirmed_only = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find",
            "reported example",
            "--unconfirmed",
            "--format",
            "json-v2",
            "--repo",
        ])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let unconfirmed_only: serde_json::Value = serde_json::from_slice(&unconfirmed_only).unwrap();
    let items = unconfirmed_only["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["record_type"], "source");

    Command::cargo_bin("mko")
        .unwrap()
        .args(["find", "reported", "--confirmed", "--unconfirmed", "--repo"])
        .arg(fixture.root.path())
        .assert()
        .failure();
}

#[test]
#[allow(deprecated)]
fn find_tag_and_layer_filters_narrow_results() {
    let fixture = seeded_fixture();

    let by_tag = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find", "result", "--tag", "example", "--format", "json-v2", "--repo",
        ])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let by_tag: serde_json::Value = serde_json::from_slice(&by_tag).unwrap();
    assert!(!by_tag["data"]["items"].as_array().unwrap().is_empty());

    let by_missing_tag = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find",
            "result",
            "--tag",
            "nonexistent-tag",
            "--format",
            "json-v2",
            "--repo",
        ])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let by_missing_tag: serde_json::Value = serde_json::from_slice(&by_missing_tag).unwrap();
    assert!(
        by_missing_tag["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let by_layer = Command::cargo_bin("mko")
        .unwrap()
        .args([
            "find",
            "document",
            "--layer",
            "counterargument-uncertainty",
            "--format",
            "json-v2",
            "--repo",
        ])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let by_layer: serde_json::Value = serde_json::from_slice(&by_layer).unwrap();
    let items = by_layer["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "External validity");
}

#[test]
#[allow(deprecated)]
fn find_human_output_shows_confirmation_labels() {
    let fixture = seeded_fixture();

    let output = Command::cargo_bin("mko")
        .unwrap()
        .args(["find", "reported example", "--repo"])
        .arg(fixture.root.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let screen = String::from_utf8_lossy(&output);
    assert!(
        screen.contains("확인됨"),
        "confirmed label missing: {screen}"
    );
    assert!(
        screen.contains("AI 작성 · 미확인"),
        "unconfirmed label missing: {screen}"
    );

    let _ = (&fixture.knowledge, &fixture.source);
}

#[test]
#[allow(deprecated)]
fn find_with_no_matches_still_logs_a_zero_result_recall() {
    let fixture = seeded_fixture();

    Command::cargo_bin("mko")
        .unwrap()
        .args(["find", "no-such-term-anywhere", "--repo"])
        .arg(fixture.root.path())
        .assert()
        .success();

    let log = fs::read_to_string(fixture.root.path().join("logs/recall.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(entry["results"], 0);
    assert_eq!(entry["surfaced"].as_array().unwrap().len(), 0);
}
