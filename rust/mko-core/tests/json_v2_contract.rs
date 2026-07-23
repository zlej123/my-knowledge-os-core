use mko_core::{
    json_v2::{JsonV2Failure, JsonV2Success},
    model_v2::{
        EvidenceRefV2, KnowledgeResponseV2, PreparedContentV2, ReviewRecordV2, ReviewResolutionV2,
        SourceResponseV2,
    },
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

struct ContractFixture<T> {
    name: &'static str,
    bytes: &'static [u8],
    schema: &'static str,
    _model: std::marker::PhantomData<T>,
}

impl<T> ContractFixture<T>
where
    T: DeserializeOwned + serde::Serialize,
{
    fn assert_valid_round_trip(&self) {
        let schema: Value = serde_json::from_str(self.schema).expect("schema is JSON");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        let value: Value = serde_json::from_slice(self.bytes).expect("fixture is JSON");
        if let Err(error) = validator.validate(&value) {
            panic!("{} failed schema validation: {error}", self.name);
        }
        let typed: T = serde_json::from_slice(self.bytes).expect("fixture matches Rust model");
        assert_eq!(serde_json::to_value(typed).unwrap(), value, "{}", self.name);
    }

    fn assert_rejects(&self, value: &Value) {
        let schema: Value = serde_json::from_str(self.schema).expect("schema is JSON");
        let validator = jsonschema::validator_for(&schema).expect("schema compiles");
        assert!(!validator.is_valid(value), "schema accepted {value}");
        assert!(
            serde_json::from_value::<T>(value.clone()).is_err(),
            "Rust model accepted {value}"
        );
    }
}

fn fixture<T>(
    name: &'static str,
    bytes: &'static [u8],
    schema: &'static str,
) -> ContractFixture<T> {
    ContractFixture {
        name,
        bytes,
        schema,
        _model: std::marker::PhantomData,
    }
}

#[test]
fn all_v2_artifact_goldens_validate_and_round_trip() {
    fixture::<EvidenceRefV2>(
        "evidence-ref",
        include_bytes!("../../../tests/fixtures/json-v2/evidence-ref.json"),
        include_str!("../../../schemas/v2/evidence-ref.schema.json"),
    )
    .assert_valid_round_trip();
    fixture::<PreparedContentV2>(
        "prepared-content",
        include_bytes!("../../../tests/fixtures/json-v2/prepared-content.json"),
        include_str!("../../../schemas/v2/prepared-content.schema.json"),
    )
    .assert_valid_round_trip();
    fixture::<SourceResponseV2>(
        "source-response",
        include_bytes!("../../../tests/fixtures/json-v2/source-response.json"),
        include_str!("../../../schemas/v2/source-response.schema.json"),
    )
    .assert_valid_round_trip();
    fixture::<KnowledgeResponseV2>(
        "knowledge-response",
        include_bytes!("../../../tests/fixtures/json-v2/knowledge-response.json"),
        include_str!("../../../schemas/v2/knowledge-response.schema.json"),
    )
    .assert_valid_round_trip();
    fixture::<ReviewRecordV2>(
        "review",
        include_bytes!("../../../tests/fixtures/json-v2/review.json"),
        include_str!("../../../schemas/v2/review.schema.json"),
    )
    .assert_valid_round_trip();
    fixture::<ReviewResolutionV2>(
        "review-resolution",
        include_bytes!("../../../tests/fixtures/json-v2/review-resolution.json"),
        include_str!("../../../schemas/v2/review-resolution.schema.json"),
    )
    .assert_valid_round_trip();
}

#[test]
fn machine_envelope_goldens_validate_and_round_trip() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let add = include_bytes!("../../../tests/fixtures/json-v2/add-success.json");
    let add_value: Value = serde_json::from_slice(add).unwrap();
    assert!(validator.is_valid(&add_value));
    let add_typed: JsonV2Success = serde_json::from_slice(add).unwrap();
    assert_eq!(serde_json::to_value(add_typed).unwrap(), add_value);
    let add_batch = include_bytes!("../../../tests/fixtures/json-v2/add-batch-success.json");
    let add_batch_value: Value = serde_json::from_slice(add_batch).unwrap();
    assert!(validator.is_valid(&add_batch_value));
    let add_batch_typed: JsonV2Success = serde_json::from_slice(add_batch).unwrap();
    assert_eq!(
        serde_json::to_value(add_batch_typed).unwrap(),
        add_batch_value
    );

    for bytes in [
        include_bytes!("../../../tests/fixtures/json-v2/source-prepare-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/source-write-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/knowledge-write-success.json").as_slice(),
    ] {
        let value: Value = serde_json::from_slice(bytes).unwrap();
        assert!(validator.is_valid(&value));
        let typed: JsonV2Success = serde_json::from_slice(bytes).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), value);
    }

    let success = include_bytes!("../../../tests/fixtures/json-v2/queue-success.json");
    let success_value: Value = serde_json::from_slice(success).unwrap();
    assert!(validator.is_valid(&success_value));
    let success_typed: JsonV2Success = serde_json::from_slice(success).unwrap();
    assert_eq!(serde_json::to_value(success_typed).unwrap(), success_value);

    let show = include_bytes!("../../../tests/fixtures/json-v2/show-success.json");
    let show_value: Value = serde_json::from_slice(show).unwrap();
    assert!(validator.is_valid(&show_value));
    let show_typed: JsonV2Success = serde_json::from_slice(show).unwrap();
    assert_eq!(serde_json::to_value(show_typed).unwrap(), show_value);

    for bytes in [
        include_bytes!("../../../tests/fixtures/json-v2/setup-plan-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/setup-apply-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/review-open-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/review-feedback-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/dashboard-success.json").as_slice(),
    ] {
        let value: Value = serde_json::from_slice(bytes).unwrap();
        assert!(validator.is_valid(&value));
        let typed: JsonV2Success = serde_json::from_slice(bytes).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), value);
    }

    let failure = include_bytes!("../../../tests/fixtures/json-v2/queue-error.json");
    let failure_value: Value = serde_json::from_slice(failure).unwrap();
    assert!(validator.is_valid(&failure_value));
    let failure_typed: JsonV2Failure = serde_json::from_slice(failure).unwrap();
    assert_eq!(serde_json::to_value(failure_typed).unwrap(), failure_value);
}

#[test]
fn every_v2_contract_rejects_unknown_fields_and_wrong_schema_version() {
    let prepared = fixture::<PreparedContentV2>(
        "prepared-content",
        include_bytes!("../../../tests/fixtures/json-v2/prepared-content.json"),
        include_str!("../../../schemas/v2/prepared-content.schema.json"),
    );
    let mut value: Value = serde_json::from_slice(prepared.bytes).unwrap();
    value["unexpected"] = json!(true);
    prepared.assert_rejects(&value);
    let mut value: Value = serde_json::from_slice(prepared.bytes).unwrap();
    value["schema_version"] = json!(1);
    prepared.assert_rejects(&value);

    let knowledge = fixture::<KnowledgeResponseV2>(
        "knowledge-response",
        include_bytes!("../../../tests/fixtures/json-v2/knowledge-response.json"),
        include_str!("../../../schemas/v2/knowledge-response.schema.json"),
    );
    let mut value: Value = serde_json::from_slice(knowledge.bytes).unwrap();
    value["user_judgment"] = json!("agent-authored judgment");
    knowledge.assert_rejects(&value);
    let mut value: Value = serde_json::from_slice(knowledge.bytes).unwrap();
    value["domain_policy"] = json!("high_risk");
    knowledge.assert_rejects(&value);

    let source = fixture::<SourceResponseV2>(
        "source-response",
        include_bytes!("../../../tests/fixtures/json-v2/source-response.json"),
        include_str!("../../../schemas/v2/source-response.schema.json"),
    );
    let mut value: Value = serde_json::from_slice(source.bytes).unwrap();
    value["high_risk"] = json!(true);
    source.assert_rejects(&value);
}

#[test]
fn required_nullable_fields_cannot_be_omitted() {
    let source = fixture::<SourceResponseV2>(
        "source-response",
        include_bytes!("../../../tests/fixtures/json-v2/source-response.json"),
        include_str!("../../../schemas/v2/source-response.schema.json"),
    );
    let mut value: Value = serde_json::from_slice(source.bytes).unwrap();
    value.as_object_mut().unwrap().remove("publication_date");
    source.assert_rejects(&value);

    let review = fixture::<ReviewRecordV2>(
        "review",
        include_bytes!("../../../tests/fixtures/json-v2/review.json"),
        include_str!("../../../schemas/v2/review.schema.json"),
    );
    let mut value: Value = serde_json::from_slice(review.bytes).unwrap();
    value["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("supersedes_review_id");
    review.assert_rejects(&value);
}

#[test]
fn evidence_refs_reject_unknown_fields_and_multiple_narrowing_forms() {
    let evidence = fixture::<EvidenceRefV2>(
        "evidence-ref",
        include_bytes!("../../../tests/fixtures/json-v2/evidence-ref.json"),
        include_str!("../../../schemas/v2/evidence-ref.schema.json"),
    );

    let mut value: Value = serde_json::from_slice(evidence.bytes).unwrap();
    value["unexpected"] = json!(true);
    evidence.assert_rejects(&value);

    let mut value: Value = serde_json::from_slice(evidence.bytes).unwrap();
    value["text_span_utf8"] = json!({ "start": 0, "end": 8 });
    evidence.assert_rejects(&value);
}

#[test]
fn prepared_content_requires_manifest_and_rejects_unknown_block_fields() {
    let prepared = fixture::<PreparedContentV2>(
        "prepared-content",
        include_bytes!("../../../tests/fixtures/json-v2/prepared-content.json"),
        include_str!("../../../schemas/v2/prepared-content.schema.json"),
    );

    let mut value: Value = serde_json::from_slice(prepared.bytes).unwrap();
    value.as_object_mut().unwrap().remove("artifacts");
    prepared.assert_rejects(&value);

    let mut value: Value = serde_json::from_slice(prepared.bytes).unwrap();
    value["content_blocks"][1]["unexpected"] = json!(true);
    prepared.assert_rejects(&value);
}

#[test]
fn normative_grounding_rules_are_encoded_in_schemas() {
    let source_schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/source-response.schema.json"
    ))
    .unwrap();
    let source_validator = jsonschema::validator_for(&source_schema).unwrap();
    let mut source: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-response.json"
    ))
    .unwrap();
    source["limitations"][0]["evidence_refs"] = json!([]);
    assert!(!source_validator.is_valid(&source));

    let knowledge_schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/knowledge-response.schema.json"
    ))
    .unwrap();
    let knowledge_validator = jsonschema::validator_for(&knowledge_schema).unwrap();
    let mut knowledge: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/knowledge-response.json"
    ))
    .unwrap();
    knowledge["units"][0]["basis"] = json!("missing_evidence");
    knowledge["units"][0]["evidence_refs"] = json!([]);
    assert!(!knowledge_validator.is_valid(&knowledge));
}

#[test]
fn machine_envelopes_reject_unknown_fields_and_non_v2_versions() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let bytes = include_bytes!("../../../tests/fixtures/json-v2/queue-success.json");

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["unexpected"] = json!(true);
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<JsonV2Success>(value).is_err());

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["schema_version"] = json!(1);
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<JsonV2Success>(value).is_err());

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["data"]
        .as_object_mut()
        .unwrap()
        .remove("scan_complete");
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<JsonV2Success>(value).is_err());

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["data"]["items"][0]["unexpected"] = json!(true);
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<JsonV2Success>(value).is_err());
}

#[test]
fn show_envelope_requires_exact_revision_bound_targets() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let bytes = include_bytes!("../../../tests/fixtures/json-v2/show-success.json");

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["data"]["targets"][0]
        .as_object_mut()
        .unwrap()
        .remove("review_head_id");
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<JsonV2Success>(value).is_err());

    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    value["data"]["targets"][0]["displayed_revision"] = json!("latest");
    assert!(!validator.is_valid(&value));
}

#[test]
fn semantic_write_envelopes_are_strict_and_digest_bound() {
    let schema: Value = serde_json::from_str(include_str!(
        "../../../schemas/v2/machine-output.schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    for bytes in [
        include_bytes!("../../../tests/fixtures/json-v2/source-prepare-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/source-write-success.json").as_slice(),
        include_bytes!("../../../tests/fixtures/json-v2/knowledge-write-success.json").as_slice(),
    ] {
        let mut value: Value = serde_json::from_slice(bytes).unwrap();
        value["data"]["unexpected"] = json!(true);
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<JsonV2Success>(value).is_err());

        let mut value: Value = serde_json::from_slice(bytes).unwrap();
        value["schema_version"] = json!(1);
        assert!(!validator.is_valid(&value));
        assert!(serde_json::from_value::<JsonV2Success>(value).is_err());
    }

    let mut prepared: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-prepare-success.json"
    ))
    .unwrap();
    prepared["data"]["content_digest"] = json!("latest");
    assert!(!validator.is_valid(&prepared));

    let mut written: Value = serde_json::from_slice(include_bytes!(
        "../../../tests/fixtures/json-v2/source-write-success.json"
    ))
    .unwrap();
    written["data"]["revision"] = json!("latest");
    assert!(!validator.is_valid(&written));
}
