use std::fs;

use chrono::{DateTime, Utc};
use mko_core::{
    clock::Clock,
    config_v2::{DomainPolicyV2, KnowledgeConfigV2, PerspectiveV2},
    model_v2::{ContentBlockV2, KnowledgeResponseV2, PreparedContentV2, SourceResponseV2},
    perspective_v2::{prepare_perspective_confirmation_v2, publish_perspective_confirmation_v2},
    records_v2::{
        AssetRecordV2, CurrentPointerV2, KnowledgeRevisionV2, RecordProjectionStatusV2,
        RecordWriteOutcomeV2, SourceRevisionV2, WriteKnowledgeRecordRequestV2,
        WriteSourceRecordRequestV2, knowledge_record_id_v2, read_current_knowledge_revision_v2,
        source_record_id_v2, write_knowledge_record_v2, write_source_record_v2,
    },
    revision_v2::{canonical_json_bytes, canonical_json_sha256},
    scaffold_v2::scaffold_personal_kb_v2,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tempfile::tempdir;

#[derive(Clone, Copy)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.0
    }
}

#[test]
fn record_contract_goldens_validate_round_trip_and_reject_unknown_fields() {
    assert_contract::<AssetRecordV2>(
        include_bytes!("../../../tests/fixtures/json-v2/asset.json"),
        include_str!("../../../schemas/v2/asset.schema.json"),
    );
    assert_contract::<CurrentPointerV2>(
        include_bytes!("../../../tests/fixtures/json-v2/current-pointer.json"),
        include_str!("../../../schemas/v2/current-pointer.schema.json"),
    );
    assert_contract::<SourceRevisionV2>(
        include_bytes!("../../../tests/fixtures/json-v2/source-revision.json"),
        include_str!("../../../schemas/v2/source-revision.schema.json"),
    );
    assert_contract::<KnowledgeRevisionV2>(
        include_bytes!("../../../tests/fixtures/json-v2/knowledge-revision.json"),
        include_str!("../../../schemas/v2/knowledge-revision.schema.json"),
    );

    let mut wrong_source_type: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-revision.json"
    ))
    .unwrap();
    wrong_source_type["record_type"] = json!("knowledge");
    assert!(serde_json::from_value::<SourceRevisionV2>(wrong_source_type).is_err());
}

#[test]
fn deterministic_ids_and_identical_source_writes_converge() {
    let environment = new_environment();
    let first = write_source(&environment, &environment.source, None).expect("first write");
    let second = write_source(&environment, &environment.source, None).expect("idempotent write");

    assert_eq!(first.outcome, RecordWriteOutcomeV2::Created);
    assert_eq!(second.outcome, RecordWriteOutcomeV2::Existing);
    assert_eq!(
        first.record_id,
        source_record_id_v2(&environment.asset.id).unwrap()
    );
    assert_eq!(first.revision, second.revision);
    assert_eq!(
        fs::read(&first.revision_path).unwrap(),
        fs::read(&second.revision_path).unwrap()
    );
    let pointer: CurrentPointerV2 =
        serde_json::from_slice(&fs::read(&first.current_path).unwrap()).unwrap();
    assert_eq!(pointer.revision, first.revision);
    assert_eq!(
        pointer.evidence_basis.bundle_id,
        environment.bundle.bundle_id
    );
    assert_eq!(
        knowledge_record_id_v2(&environment.asset.id).unwrap(),
        knowledge_record_id_v2(&environment.asset.id).unwrap()
    );
    assert_ne!(
        source_record_id_v2(&environment.asset.id).unwrap(),
        knowledge_record_id_v2(&environment.asset.id).unwrap()
    );

    fs::write(&first.revision_path, b"tampered immutable revision").unwrap();
    assert_eq!(
        write_source(&environment, &environment.source, None)
            .unwrap_err()
            .code(),
        "revision_conflict"
    );
}

#[test]
fn confirmed_investment_perspective_creates_a_pending_high_risk_revision() {
    let mut environment = new_environment();
    fs::write(
        environment
            .root
            .path()
            .join("assets/registry")
            .join(format!("{}.json", environment.asset.id)),
        canonical_json_bytes(&environment.asset).unwrap(),
    )
    .unwrap();
    let mut response = serde_json::to_value(&environment.knowledge).unwrap();
    response["units"].as_array_mut().unwrap().push(json!({
        "kind": "counterargument",
        "title": "Alternative",
        "body": "An alternative explanation remains possible.",
        "confidence": "low",
        "basis": "conflicting_evidence",
        "evidence_refs": [],
        "tags": []
    }));
    environment.knowledge = serde_json::from_value(response).unwrap();
    let first = write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle: &environment.bundle,
            response: &environment.knowledge,
            expected_revision: None,
        },
        &environment.clock,
    )
    .unwrap();
    let prepared = prepare_perspective_confirmation_v2(
        environment.root.path(),
        &first.record_id,
        vec![PerspectiveV2::Investment, PerspectiveV2::Technical],
    )
    .unwrap();

    let mismatch = publish_perspective_confirmation_v2(
        environment.root.path(),
        &prepared,
        "y",
        &environment.clock,
    )
    .unwrap_err();
    assert_eq!(mismatch.code(), "perspective_confirmation_mismatch");
    assert_eq!(
        read_current_knowledge_revision_v2(environment.root.path(), &first.record_id)
            .unwrap()
            .pointer
            .revision,
        first.revision
    );

    let replaced = publish_perspective_confirmation_v2(
        environment.root.path(),
        &prepared,
        &prepared.confirmation_phrase,
        &environment.clock,
    )
    .unwrap();
    assert_eq!(replaced.outcome, RecordWriteOutcomeV2::Replaced);
    let current =
        read_current_knowledge_revision_v2(environment.root.path(), &first.record_id).unwrap();
    assert_eq!(
        current.revision.perspectives,
        vec![PerspectiveV2::Technical, PerspectiveV2::Investment]
    );
    assert_eq!(current.revision.domain_policy, DomainPolicyV2::HighRisk);
    assert_ne!(current.pointer.revision, first.revision);
    assert!(first.revision_path.is_file());
}

#[test]
fn investment_confirmation_fails_without_counterargument_and_changes_nothing() {
    let environment = new_environment();
    let first = write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle: &environment.bundle,
            response: &environment.knowledge,
            expected_revision: None,
        },
        &environment.clock,
    )
    .unwrap();
    let error = prepare_perspective_confirmation_v2(
        environment.root.path(),
        &first.record_id,
        vec![PerspectiveV2::Investment],
    )
    .unwrap_err();
    assert_eq!(error.code(), "high_risk_knowledge_incomplete");
    assert_eq!(
        read_current_knowledge_revision_v2(environment.root.path(), &first.record_id)
            .unwrap()
            .pointer
            .revision,
        first.revision
    );
}

#[test]
fn canonical_source_write_also_publishes_its_exact_projection() {
    let environment = new_environment();

    let result = write_source(&environment, &environment.source, None).unwrap();

    let projection = match &result.projection {
        RecordProjectionStatusV2::Current(projection) => projection,
        other => panic!("expected current projection, got {other:?}"),
    };
    let text = fs::read_to_string(&projection.path).unwrap();
    assert!(text.contains(&format!("current_revision: \"{}\"", result.revision)));
    assert!(text.contains(&format!("record_id: \"{}\"", result.record_id)));
    assert!(text.contains("derived_state: unreviewed"));
    assert!(text.contains(&format!(
        "asset_link: \"assets/registry/{}.json\"",
        environment.asset.id
    )));
}

#[test]
fn derived_projection_failure_preserves_canonical_publication_and_reports_stale() {
    let environment = new_environment();
    fs::write(
        environment.root.path().join(".mko/generated-manifest.yaml"),
        b"not: [valid",
    )
    .unwrap();

    let result = write_source(&environment, &environment.source, None).unwrap();

    let RecordProjectionStatusV2::Stale { path, error } = &result.projection else {
        panic!(
            "expected stale projection result, got {:?}",
            result.projection
        );
    };
    assert_eq!(error.code(), "projection_manifest_invalid");
    assert!(!path.exists());
    assert!(result.revision_path.exists());
    let pointer: CurrentPointerV2 =
        serde_json::from_slice(&fs::read(&result.current_path).unwrap()).unwrap();
    assert_eq!(pointer.revision, result.revision);
}

#[test]
fn manual_projection_edit_is_preserved_while_canonical_replacement_remains_authoritative() {
    let environment = new_environment();
    let first = write_source(&environment, &environment.source, None).unwrap();
    let projection_path = match &first.projection {
        RecordProjectionStatusV2::Current(projection) => projection.path.clone(),
        other => panic!("expected current projection, got {other:?}"),
    };
    fs::write(&projection_path, b"manual user projection edit\n").unwrap();
    let mut changed = environment.source.clone();
    changed.general_summary = "A corrected bounded summary.".into();

    let replaced = write_source(&environment, &changed, Some(&first.revision)).unwrap();

    assert_eq!(replaced.outcome, RecordWriteOutcomeV2::Replaced);
    assert!(matches!(
        replaced.projection,
        RecordProjectionStatusV2::RepairRequired(_)
    ));
    assert_eq!(
        fs::read(&projection_path).unwrap(),
        b"manual user projection edit\n"
    );
    let pointer: CurrentPointerV2 =
        serde_json::from_slice(&fs::read(&replaced.current_path).unwrap()).unwrap();
    assert_eq!(pointer.revision, replaced.revision);
}

#[test]
fn replacement_requires_expected_revision_and_stale_cas_has_no_effect() {
    let environment = new_environment();
    let first = write_source(&environment, &environment.source, None).unwrap();
    let mut changed = environment.source.clone();
    changed.general_summary = "A corrected bounded summary.".into();

    let missing =
        write_source(&environment, &changed, None).expect_err("expected revision required");
    assert_eq!(missing.code(), "replacement_revision_required");
    assert_eq!(revision_count(&first.revision_path), 1);

    let stale = format!("sha256:{}", "0".repeat(64));
    let error = write_source(&environment, &changed, Some(&stale)).expect_err("stale CAS");
    assert_eq!(error.code(), "record_revision_stale");
    assert_eq!(revision_count(&first.revision_path), 1);
    let pointer_before: Vec<u8> = fs::read(&first.current_path).unwrap();

    let replaced = write_source(&environment, &changed, Some(&first.revision)).unwrap();
    assert_eq!(replaced.outcome, RecordWriteOutcomeV2::Replaced);
    assert_ne!(replaced.revision, first.revision);
    assert_ne!(fs::read(&first.current_path).unwrap(), pointer_before);
    assert_eq!(revision_count(&first.revision_path), 2);
    assert!(
        fs::read(&first.revision_path)
            .unwrap()
            .starts_with(b"# Source revision\n\n")
    );
}

#[cfg(unix)]
#[test]
fn current_pointer_symlink_is_rejected_without_reading_or_changing_target() {
    use std::os::unix::fs::symlink;

    let environment = new_environment();
    let first = write_source(&environment, &environment.source, None).unwrap();
    let outside = environment.root.path().join("outside-current");
    let outside_bytes = b"untrusted outside pointer bytes";
    fs::write(&outside, outside_bytes).unwrap();
    fs::remove_file(&first.current_path).unwrap();
    symlink(&outside, &first.current_path).unwrap();

    let error = write_source(&environment, &environment.source, None)
        .expect_err("pointer symlink must fail closed");

    assert_eq!(error.code(), "current_pointer_invalid");
    assert_eq!(fs::read(outside).unwrap(), outside_bytes);
    assert_eq!(revision_count(&first.revision_path), 1);
}

#[test]
fn exact_bundle_asset_and_self_digest_are_required_before_mutation() {
    let mut environment = new_environment();
    environment.bundle.content_digest = format!("sha256:{}", "0".repeat(64));
    let error = write_source(&environment, &environment.source, None).expect_err("bad digest");
    assert_eq!(error.code(), "prepared_bundle_digest_mismatch");
    assert!(
        fs::read_dir(environment.root.path().join("sources"))
            .unwrap()
            .next()
            .is_none()
    );

    environment = new_environment();
    environment.asset.fingerprint = format!("sha256:{}", "c".repeat(64));
    let error = write_source(&environment, &environment.source, None).expect_err("bad binding");
    assert_eq!(error.code(), "asset_binding_invalid");
}

#[test]
fn evidence_requires_exact_block_locator_and_valid_utf8_or_table_bounds() {
    let mut environment = new_environment();
    environment.source.key_claims[0].evidence_refs[0].locator = "page:wrong".into();
    assert_eq!(
        write_source(&environment, &environment.source, None)
            .unwrap_err()
            .code(),
        "evidence_reference_invalid"
    );

    let mut environment = new_environment();
    if let ContentBlockV2::Text { text, .. } = &mut environment.bundle.content_blocks[0] {
        text.clear();
        text.push_str("évidence");
    }
    seal_bundle(&mut environment.bundle);
    environment.source.key_claims[0].evidence_refs[0]
        .text_span_utf8
        .as_mut()
        .unwrap()
        .start = 1;
    assert_eq!(
        write_source(&environment, &environment.source, None)
            .unwrap_err()
            .code(),
        "evidence_reference_invalid"
    );

    let mut environment = new_environment();
    let range = environment.knowledge.units[0].evidence_refs[0]
        .table_range
        .as_mut()
        .unwrap();
    range.row_end = 99;
    assert_eq!(
        write_knowledge(&environment, &environment.knowledge, None)
            .unwrap_err()
            .code(),
        "evidence_reference_invalid"
    );
}

#[test]
fn source_and_knowledge_mechanical_grounding_rules_are_core_enforced() {
    let mut environment = new_environment();
    environment.source.key_claims[0].evidence_refs.clear();
    assert_eq!(
        write_source(&environment, &environment.source, None)
            .unwrap_err()
            .code(),
        "source_grounding_invalid"
    );

    let mut environment = new_environment();
    environment.knowledge.units[0].evidence_refs.clear();
    assert_eq!(
        write_knowledge(&environment, &environment.knowledge, None)
            .unwrap_err()
            .code(),
        "knowledge_grounding_invalid"
    );
}

#[test]
fn only_core_configuration_can_activate_high_risk_policy() {
    let mut standard = new_environment();
    standard.knowledge.units[0].tags.push("finance".into());
    let standard_result = write_knowledge(&standard, &standard.knowledge, None)
        .expect("untrusted tags cannot activate policy");
    assert_eq!(standard_result.outcome, RecordWriteOutcomeV2::Created);

    let mut proposed = serde_json::to_value(&standard.knowledge).unwrap();
    proposed["proposed_domains"] = json!(["finance"]);
    assert!(serde_json::from_value::<KnowledgeResponseV2>(proposed).is_err());

    let environment = new_environment();
    configure_high_risk(environment.root.path());
    let error = write_knowledge(&environment, &environment.knowledge, None)
        .expect_err("configured high risk omissions must fail");
    assert_eq!(error.code(), "high_risk_knowledge_incomplete");

    let mut response = environment.knowledge.clone();
    response.units.push(
        serde_json::from_value(json!({
            "kind": "counterargument",
            "title": "Alternative",
            "body": "An alternative remains possible.",
            "confidence": "low",
            "basis": "conflicting_evidence",
            "evidence_refs": [],
            "tags": []
        }))
        .unwrap(),
    );
    response.units.push(
        serde_json::from_value(json!({
            "kind": "open_question",
            "title": "Verification",
            "body": "What independent evidence would verify the result?",
            "confidence": "low",
            "basis": "missing_evidence",
            "evidence_refs": [],
            "tags": []
        }))
        .unwrap(),
    );
    let result = write_knowledge(&environment, &response, None).unwrap();
    assert_eq!(result.outcome, RecordWriteOutcomeV2::Created);
    let markdown = String::from_utf8(fs::read(result.revision_path).unwrap()).unwrap();
    assert!(markdown.contains("\"domain_policy\":\"high_risk\""));
    assert!(!markdown.contains("user_judgment"));
}

fn assert_contract<T>(bytes: &[u8], schema: &str)
where
    T: DeserializeOwned + Serialize,
{
    let schema: Value = serde_json::from_str(schema).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let value: Value = serde_json::from_slice(bytes).unwrap();
    assert!(validator.is_valid(&value));
    let typed: T = serde_json::from_slice(bytes).unwrap();
    assert_eq!(serde_json::to_value(typed).unwrap(), value);

    let mut unknown = value.clone();
    unknown["unexpected"] = json!(true);
    assert!(!validator.is_valid(&unknown));
    assert!(serde_json::from_value::<T>(unknown).is_err());

    let mut wrong_version = value;
    wrong_version["schema_version"] = json!(1);
    assert!(!validator.is_valid(&wrong_version));
    assert!(serde_json::from_value::<T>(wrong_version).is_err());
}

struct Environment {
    root: tempfile::TempDir,
    asset: AssetRecordV2,
    bundle: PreparedContentV2,
    source: SourceResponseV2,
    knowledge: KnowledgeResponseV2,
    clock: FixedClock,
}

fn new_environment() -> Environment {
    let root = tempdir().unwrap();
    scaffold_personal_kb_v2(root.path()).unwrap();
    let asset =
        serde_json::from_slice(include_bytes!("../../../tests/fixtures/json-v2/asset.json"))
            .unwrap();
    let mut bundle: PreparedContentV2 = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/prepared-content.json"
    ))
    .unwrap();
    seal_bundle(&mut bundle);
    Environment {
        root,
        asset,
        bundle,
        source: serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/source-response.json"
        ))
        .unwrap(),
        knowledge: serde_json::from_slice(include_bytes!(
            "../../../tests/fixtures/json-v2/knowledge-response.json"
        ))
        .unwrap(),
        clock: FixedClock("2026-07-22T00:00:00Z".parse().unwrap()),
    }
}

fn seal_bundle(bundle: &mut PreparedContentV2) {
    let mut value = serde_json::to_value(&*bundle).unwrap();
    value.as_object_mut().unwrap().remove("bundle_id");
    value.as_object_mut().unwrap().remove("content_digest");
    let digest = canonical_json_sha256(&value).unwrap();
    bundle.content_digest = digest.clone();
    bundle.bundle_id = format!("prepared-content-{}", digest.replace(':', "-"));
}

fn write_source(
    environment: &Environment,
    response: &SourceResponseV2,
    expected_revision: Option<&str>,
) -> Result<mko_core::records_v2::RecordWriteResultV2, mko_core::error::MkoError> {
    write_source_record_v2(
        WriteSourceRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle: &environment.bundle,
            response,
            expected_revision,
        },
        &environment.clock,
    )
}

fn write_knowledge(
    environment: &Environment,
    response: &KnowledgeResponseV2,
    expected_revision: Option<&str>,
) -> Result<mko_core::records_v2::RecordWriteResultV2, mko_core::error::MkoError> {
    write_knowledge_record_v2(
        WriteKnowledgeRecordRequestV2 {
            repository_root: environment.root.path(),
            asset: &environment.asset,
            bundle: &environment.bundle,
            response,
            expected_revision,
        },
        &environment.clock,
    )
}

fn configure_high_risk(repository_root: &std::path::Path) {
    let mut config = KnowledgeConfigV2::personal_default();
    config.domain_policies.default = DomainPolicyV2::HighRisk;
    fs::write(
        repository_root.join("knowledge-os.yaml"),
        config.render().unwrap(),
    )
    .unwrap();
}

fn revision_count(revision_path: &std::path::Path) -> usize {
    fs::read_dir(revision_path.parent().unwrap())
        .unwrap()
        .count()
}
